use std::path::Path;

use anyhow::Context;
use seqforge_core::{Strand, ViewerRequest};

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

// ── File commands ─────────────────────────────────────────────────────────────

pub fn run_info(path: &Path) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let info = serde_json::json!({
        "kind": "document_info",
        "name": doc.name,
        "length": doc.len(),
        "topology": format!("{:?}", doc.topology).to_lowercase(),
        "features": doc.features.len(),
        "primers": doc.primers.len(),
        "path": path,
    });
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}

/// Translate a (sub)range of a sequence file to protein — a local, read-only
/// derivation that needs no running GUI. `start`/`end` are 0-based half-open
/// (default: whole sequence); `strand` is `+`/`-`; `frame` is the GenBank
/// codon_start convention (1, 2, or 3).
pub fn run_translate(
    path: &Path,
    start: Option<usize>,
    end: Option<usize>,
    strand: &str,
    frame: usize,
) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let len = doc.len();
    let start = start.unwrap_or(0);
    let end = end.unwrap_or(len);
    if start >= end || end > len {
        anyhow::bail!("range {start}..{end} is invalid for a sequence of length {len}");
    }
    let strand = match strand.trim() {
        "-" | "reverse" | "Reverse" => Strand::Reverse,
        _ => Strand::Forward,
    };
    let protein = seqforge_bio::translate(&doc.sequence[start..end], strand, frame);
    let out = serde_json::json!({
        "kind": "translation",
        "name": doc.name,
        "start": start,
        "end": end,
        "strand": format!("{strand:?}").to_lowercase(),
        "frame": frame,
        "protein": protein,
        "length": protein.chars().count(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Find open reading frames in a sequence file — a local analysis (no GUI).
/// `min_aa` filters by protein length; forward + reverse frames by default.
pub fn run_orfs(
    path: &Path,
    min_aa: usize,
    stop_to_stop: bool,
    forward_only: bool,
) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let orfs = seqforge_bio::find_orfs(&doc.sequence, min_aa, !stop_to_stop, !forward_only);
    let items: Vec<_> = orfs
        .iter()
        .map(|o| {
            serde_json::json!({
                "start": o.start,
                "end": o.end,
                "strand": format!("{:?}", o.strand).to_lowercase(),
                "frame": o.frame,
                "aa_len": o.aa_len,
            })
        })
        .collect();
    let out = serde_json::json!({
        "kind": "orfs",
        "name": doc.name,
        "count": orfs.len(),
        "orfs": items,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Melting temperature + GC of an oligo — a pure, local derivation (no running
/// GUI, no file). Reaches the vendored seqfold engine through `seqforge-bio`'s
/// thin `tm`/`gc` surface (`bio → thermo`; `core` never sees thermo). Tm is the
/// nearest-neighbour model (SantaLucia NN + Owczarzy-2008 salt), in °C.
pub fn run_tm(oligo: &str) -> anyhow::Result<()> {
    let tm = seqforge_bio::tm(oligo)
        .map_err(|e| anyhow::anyhow!("cannot compute Tm for {oligo:?}: {}", e.0))?;
    let hairpin = seqforge_bio::hairpin_dg(oligo, seqforge_bio::DEFAULT_FOLD_TEMP_C);
    let dimer = seqforge_bio::self_dimer_dg(oligo, seqforge_bio::DEFAULT_FOLD_TEMP_C);
    let out = serde_json::json!({
        "kind": "oligo_tm",
        "oligo": oligo.to_uppercase(),
        "length": oligo.len(),
        "tm": tm,
        "gc": seqforge_bio::gc(oligo),
        "hairpin_dg": hairpin.ok(),
        "self_dimer_dg": dimer.ok(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

// ── Viewer command socket dispatch ────────────────────────────────────────────

/// Send a `ViewerRequest` to a running SeqForge GUI via the Unix domain socket
/// using the JSON-RPC 2.0 wire format.
///
/// Reads `SEQFORGE_SOCKET` from the environment. If unset, the command cannot
/// be delivered and an error is returned.
///
/// On non-Unix platforms (Windows), returns an error explaining that the
/// agent-IPC transport isn't supported in v0.1 (Tier 1 #5). File commands
/// (`info`, `digest`, `annotate`) work everywhere; viewer commands are
/// Unix-only until/unless we adopt `interprocess` for cross-platform sockets.
#[cfg(not(unix))]
pub fn dispatch_viewer_cmd(_req: ViewerRequest) -> anyhow::Result<()> {
    anyhow::bail!(
        "viewer commands (open/close/goto/find/enzymes) require a Unix \
         domain socket; not supported on this platform"
    )
}

#[cfg(unix)]
pub fn dispatch_viewer_cmd(req: ViewerRequest) -> anyhow::Result<()> {
    let socket_path = std::env::var("SEQFORGE_SOCKET").map_err(|_| {
        anyhow::anyhow!("no SeqForge instance running (SEQFORGE_SOCKET is not set)")
    })?;

    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("could not connect to SeqForge socket at {socket_path}"))?;

    // Serialize the ViewerRequest as a JSON-RPC 2.0 request.
    // ViewerRequest uses serde tag="method", so we merge method into params.
    let req_value = serde_json::to_value(&req)?;
    let method = req_value
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let mut params = match req_value {
        serde_json::Value::Object(mut m) => {
            m.remove("method");
            serde_json::Value::Object(m)
        }
        _ => serde_json::Value::Null,
    };
    if matches!(params, serde_json::Value::Object(ref m) if m.is_empty()) {
        params = serde_json::Value::Null;
    }

    let rpc = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let json = serde_json::to_string(&rpc)?;
    stream.write_all(format!("{json}\n").as_bytes())?;
    stream.flush()?;

    // Read and parse the JSON-RPC response.
    let mut response_line = String::new();
    BufReader::new(&stream).read_line(&mut response_line)?;
    let response: serde_json::Value = serde_json::from_str(response_line.trim())
        .context("invalid JSON-RPC response from SeqForge")?;

    if let Some(err) = response.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("SeqForge rejected command: {msg}");
    }

    // Pretty-print the result so agents can consume it.
    if let Some(result) = response.get("result") {
        println!("{}", serde_json::to_string_pretty(result)?);
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use seqforge_core::ViewerRequest;

    #[test]
    fn viewer_cmd_fails_without_socket_env() {
        // Safety: test binary is single-threaded by default; no concurrent env reads.
        unsafe { std::env::remove_var("SEQFORGE_SOCKET") };
        let err = super::dispatch_viewer_cmd(ViewerRequest::Close).unwrap_err();
        assert!(err.to_string().contains("SEQFORGE_SOCKET"));
    }
}
