use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::Context;
use seqforge_core::ViewerCommand;

// ── File commands ─────────────────────────────────────────────────────────────

pub fn run_info(path: &Path) -> anyhow::Result<()> {
    let doc = seqforge_bio::load(path)
        .with_context(|| format!("Failed to load {}", path.display()))?;
    println!("Name:     {}", doc.name);
    println!("Length:   {} bp", doc.len());
    println!("Topology: {:?}", doc.topology);
    println!("Features: {}", doc.features.len());
    Ok(())
}

// ── Viewer command socket dispatch ────────────────────────────────────────────

/// Send a `ViewerCommand` to a running SeqForge GUI via the Unix domain socket.
///
/// Reads `SEQFORGE_SOCKET` from the environment. If unset, the command cannot
/// be delivered and an error is returned. `FileCommand`s always run in-process
/// and never call this function.
pub fn dispatch_viewer_cmd(cmd: ViewerCommand) -> anyhow::Result<()> {
    let socket_path = std::env::var("SEQFORGE_SOCKET").map_err(|_| {
        anyhow::anyhow!("no SeqForge instance running (SEQFORGE_SOCKET is not set)")
    })?;

    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("could not connect to SeqForge socket at {socket_path}"))?;

    let json = serde_json::to_string(&cmd)?;
    stream.write_all(format!("{json}\n").as_bytes())?;
    stream.flush()?;

    // Print the response line ("ok" or an error message).
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response)?;
    let response = response.trim();
    if response.starts_with("error:") {
        anyhow::bail!("SeqForge rejected command: {response}");
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use seqforge_core::ViewerCommand;

    #[test]
    fn viewer_cmd_fails_without_socket_env() {
        // Safety: test binary is single-threaded by default; no concurrent env reads.
        unsafe { std::env::remove_var("SEQFORGE_SOCKET") };
        let err = super::dispatch_viewer_cmd(ViewerCommand::Close).unwrap_err();
        assert!(err.to_string().contains("SEQFORGE_SOCKET"));
    }
}
