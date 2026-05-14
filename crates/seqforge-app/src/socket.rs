use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;

use seqforge_core::ViewerCommand;

// ── Public entry point ────────────────────────────────────────────────────────

/// Open a Unix domain socket, spawn a listener thread, and return the socket
/// path + a receiver for incoming `ViewerCommand` values.
///
/// The socket path is `/tmp/seqforge-{pid}.sock`. Any stale file from a prior
/// crash is removed before binding.
pub fn start_socket_listener(
    ctx: egui::Context,
) -> anyhow::Result<(PathBuf, mpsc::Receiver<ViewerCommand>)> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // remove stale socket from prior crash

    let listener = UnixListener::bind(&path)?;
    let (tx, rx) = mpsc::channel::<ViewerCommand>();

    let path_clone = path.clone();
    std::thread::Builder::new()
        .name("seqforge-socket".into())
        .spawn(move || accept_loop(listener, tx, ctx, path_clone))?;

    Ok((path, rx))
}

pub fn socket_path() -> PathBuf {
    PathBuf::from(format!("/tmp/seqforge-{}.sock", std::process::id()))
}

// ── Listener thread ───────────────────────────────────────────────────────────

fn accept_loop(
    listener: UnixListener,
    tx: mpsc::Sender<ViewerCommand>,
    ctx: egui::Context,
    path: PathBuf,
) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => break,
        };
        let tx = tx.clone();
        let ctx = ctx.clone();
        let _ = std::thread::spawn(move || handle_connection(stream, tx, ctx));
    }
    let _ = std::fs::remove_file(path);
}

fn handle_connection(
    mut stream: std::os::unix::net::UnixStream,
    tx: mpsc::Sender<ViewerCommand>,
    ctx: egui::Context,
) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        // stub: CommandPolicy validation goes here (post-MVP sandboxing hook)

        match serde_json::from_str::<ViewerCommand>(&line) {
            Ok(cmd) => {
                let _ = stream.write_all(b"ok\n");
                let _ = tx.send(cmd);
                ctx.request_repaint();
            }
            Err(e) => {
                let _ = stream.write_all(format!("error: {e}\n").as_bytes());
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    use seqforge_core::ViewerCommand;

    /// Serialise → send → receive → deserialise round-trip using a connected
    /// UnixStream pair (no real socket file needed).
    #[test]
    fn viewer_command_socket_round_trip() {
        let (tx, rx) = std::sync::mpsc::channel::<ViewerCommand>();

        // Pair of connected streams: client <-> server
        let (mut client, server) = UnixStream::pair().unwrap();

        // Spawn a minimal handler thread (same logic as handle_connection).
        std::thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            let mut server = server;
            for line in reader.lines().flatten() {
                if let Ok(cmd) = serde_json::from_str::<ViewerCommand>(&line) {
                    let _ = server.write_all(b"ok\n");
                    let _ = tx.send(cmd);
                }
            }
        });

        let cmd = ViewerCommand::GoTo { position: 42 };
        let json = serde_json::to_string(&cmd).unwrap();
        client.write_all(format!("{json}\n").as_bytes()).unwrap();

        // Read "ok" response
        let mut resp = String::new();
        BufReader::new(&client).read_line(&mut resp).unwrap();
        assert_eq!(resp.trim(), "ok");

        // Assert the received command
        let received = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert!(matches!(received, ViewerCommand::GoTo { position: 42 }));
    }

    #[test]
    fn file_command_needs_no_socket() {
        // FileCommand::Digest runs locally in the CLI process — no socket needed.
        // Here we just assert it serialises correctly (the actual dispatch is in seqforge-cli).
        let cmd = seqforge_core::FileCommand::Digest {
            input: std::path::PathBuf::from("in.gb"),
            enzymes: vec!["EcoRI".into()],
            output: std::path::PathBuf::from("out.gb"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("EcoRI"));
    }
}
