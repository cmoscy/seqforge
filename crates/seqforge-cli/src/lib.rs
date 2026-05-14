use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::Context;
use seqforge_core::ViewerRequest;

// ── File commands ─────────────────────────────────────────────────────────────

pub fn run_info(path: &Path) -> anyhow::Result<()> {
    let doc = seqforge_bio::load(path)
        .with_context(|| format!("Failed to load {}", path.display()))?;
    let info = serde_json::json!({
        "kind": "document_info",
        "name": doc.name,
        "length": doc.len(),
        "topology": format!("{:?}", doc.topology).to_lowercase(),
        "features": doc.features.len(),
        "path": path,
    });
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}

// ── Viewer command socket dispatch ────────────────────────────────────────────

/// Send a `ViewerRequest` to a running SeqForge GUI via the Unix domain socket
/// using the JSON-RPC 2.0 wire format.
///
/// Reads `SEQFORGE_SOCKET` from the environment. If unset, the command cannot
/// be delivered and an error is returned.
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
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
        anyhow::bail!("SeqForge rejected command: {msg}");
    }

    // Pretty-print the result so agents can consume it.
    if let Some(result) = response.get("result") {
        println!("{}", serde_json::to_string_pretty(result)?);
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
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
