use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use seqforge_core::{DispatchError, ViewerRequest, ViewerResponse};

// ── Socket path lifecycle (Tier 1 hardening) ─────────────────────────────────

/// RAII guard that removes the socket file on drop. Held in
/// `AppState` so a normal process exit (window close) cleans up the
/// socket, not just the abnormal-exit path that the listener thread's
/// own cleanup covered before.
///
/// On a panic or `process::exit`, drop ordering may skip this guard —
/// per-pid socket paths (`/tmp/seqforge-<pid>.sock` /
/// `$XDG_RUNTIME_DIR/seqforge-<pid>.sock`) protect us from collisions
/// even when a stale file is left behind. Cleanup is best-effort.
pub struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── Channel type ──────────────────────────────────────────────────────────────

/// What the socket thread sends to the app's drain loop: the request plus a
/// one-shot sender the app uses to return the dispatch result.
pub type SocketRequest = (ViewerRequest, mpsc::SyncSender<Result<ViewerResponse, DispatchError>>);

// ── JSON-RPC 2.0 types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

const ERR_PARSE: i32 = -32700;
const ERR_INVALID_REQUEST: i32 = -32600;
const ERR_METHOD_NOT_FOUND: i32 = -32601;
const ERR_INVALID_PARAMS: i32 = -32602;
const ERR_DISPATCH: i32 = -32000;

const DISPATCH_TIMEOUT: Duration = Duration::from_secs(5);

fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
}

fn err_response(id: Value, code: i32, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError { code, message: message.into() }),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Open a Unix domain socket at `path`, spawn a listener thread, and return a
/// receiver for incoming `SocketRequest` values.
///
/// The caller is responsible for:
///  1. Choosing the path (use [`socket_path`]).
///  2. Setting any process-wide env vars (e.g. `SEQFORGE_SOCKET`) **before**
///     calling this function — env mutation while another thread exists is
///     UB-adjacent in Rust 2024.
pub fn start_socket_listener(
    path: PathBuf,
    ctx: egui::Context,
) -> anyhow::Result<mpsc::Receiver<SocketRequest>> {
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;

    // Hardening: explicitly chmod 0600 so only the owner can connect.
    // The default umask usually achieves this on macOS / Linux but
    // not always (e.g. umask 022 yields 0644 on some setups). Without
    // this, any local user on a multi-user host could connect and
    // drive `open` / `find` / `enzymes` against our GUI.
    let perms = std::fs::Permissions::from_mode(0o600);
    if let Err(e) = std::fs::set_permissions(&path, perms) {
        // Non-fatal — log and continue. On the typical single-user
        // dev host this never fires.
        eprintln!("[seqforge] could not set socket permissions: {e}");
    }

    let (tx, rx) = mpsc::channel::<SocketRequest>();

    let path_clone = path.clone();
    std::thread::Builder::new()
        .name("seqforge-socket".into())
        .spawn(move || accept_loop(listener, tx, ctx, path_clone))?;

    Ok(rx)
}

/// Pick a socket path. Hardening preference order:
///   1. `$XDG_RUNTIME_DIR/seqforge-<pid>.sock` — per-user, mode-0700
///      directory, the standard Linux runtime spot.
///   2. `/tmp/seqforge-<pid>.sock` — fallback for macOS (no
///      XDG_RUNTIME_DIR by default) and other systems.
///
/// The `<pid>` suffix gives per-process uniqueness — multiple GUI
/// instances won't collide, and a stale file from a crashed prior
/// process can be `unlink`ed by the new owner without confusion.
pub fn socket_path() -> PathBuf {
    let name = format!("seqforge-{}.sock", std::process::id());
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            let mut p = PathBuf::from(dir);
            p.push(&name);
            return p;
        }
    }
    PathBuf::from("/tmp").join(name)
}

// ── Listener thread ───────────────────────────────────────────────────────────

fn accept_loop(
    listener: UnixListener,
    tx: mpsc::Sender<SocketRequest>,
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
    tx: mpsc::Sender<SocketRequest>,
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
        let resp = handle_rpc_line(&line, &tx, &ctx);
        let json = serde_json::to_string(&resp).unwrap_or_default();
        let _ = stream.write_all(format!("{json}\n").as_bytes());
    }
}

/// Parse one newline-delimited JSON-RPC request, enqueue it with a one-shot
/// response channel, block until the app dispatches it, then return the result.
fn handle_rpc_line(
    line: &str,
    tx: &mpsc::Sender<SocketRequest>,
    ctx: &egui::Context,
) -> JsonRpcResponse {
    let rpc: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return err_response(Value::Null, ERR_PARSE, format!("parse error: {e}")),
    };

    let id = rpc.id.clone();

    let mut obj = match rpc.params {
        Value::Object(m) => m,
        Value::Null => serde_json::Map::new(),
        _ => return err_response(id, ERR_INVALID_PARAMS, "params must be an object or null"),
    };
    obj.insert("method".into(), Value::String(rpc.method));

    let req: ViewerRequest = match serde_json::from_value(Value::Object(obj)) {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unknown variant") {
                return err_response(id, ERR_METHOD_NOT_FOUND, "method not found");
            }
            return err_response(id, ERR_INVALID_PARAMS, format!("invalid params: {msg}"));
        }
    };

    // Create a one-shot channel for the dispatch result.
    let (resp_tx, resp_rx) = mpsc::sync_channel(1);
    if tx.send((req, resp_tx)).is_err() {
        return err_response(id, ERR_INVALID_REQUEST, "viewer no longer running");
    }
    ctx.request_repaint();

    // Block until the app's drain loop dispatches the request and sends back.
    match resp_rx.recv_timeout(DISPATCH_TIMEOUT) {
        Ok(Ok(resp)) => {
            match serde_json::to_value(&resp) {
                Ok(v) => ok_response(id, v),
                Err(e) => err_response(id, ERR_DISPATCH, format!("serialisation error: {e}")),
            }
        }
        Ok(Err(e)) => err_response(id, ERR_DISPATCH, e.to_string()),
        Err(_) => err_response(id, ERR_DISPATCH, "viewer did not respond within timeout"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    use seqforge_core::{DispatchError, ViewerRequest, ViewerResponse};

    use super::SocketRequest;

    fn fake_app(rx: mpsc::Receiver<SocketRequest>) {
        std::thread::spawn(move || {
            while let Ok((req, resp_tx)) = rx.recv() {
                let resp: Result<ViewerResponse, DispatchError> = match req {
                    ViewerRequest::GoTo { position, .. } => {
                        Ok(ViewerResponse::Navigated { position })
                    }
                    ViewerRequest::Close => Ok(ViewerResponse::Ok),
                    _ => Ok(ViewerResponse::Ok),
                };
                let _ = resp_tx.send(resp);
            }
        });
    }

    #[test]
    fn jsonrpc_goto_round_trip() {
        let (tx, rx) = mpsc::channel::<SocketRequest>();
        fake_app(rx);

        let (mut client, server) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            let mut server = server;
            for line in reader.lines().map_while(Result::ok) {
                let resp = super::handle_rpc_line(&line, &tx, &egui::Context::default());
                let json = serde_json::to_string(&resp).unwrap();
                let _ = server.write_all(format!("{json}\n").as_bytes());
            }
        });

        let req = r#"{"jsonrpc":"2.0","id":1,"method":"goto","params":{"position":42}}"#;
        client.write_all(format!("{req}\n").as_bytes()).unwrap();

        let mut resp_line = String::new();
        BufReader::new(&client).read_line(&mut resp_line).unwrap();
        let resp: serde_json::Value = serde_json::from_str(resp_line.trim()).unwrap();

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["kind"], "navigated");
        assert_eq!(resp["result"]["position"], 42);
    }

    #[test]
    fn jsonrpc_parse_error_returns_minus_32700() {
        let (tx, _rx) = mpsc::channel::<SocketRequest>();
        let resp = super::handle_rpc_line("not json", &tx, &egui::Context::default());
        assert_eq!(resp.error.as_ref().unwrap().code, -32700);
    }

    #[test]
    fn jsonrpc_unknown_method_returns_minus_32601() {
        let (tx, _rx) = mpsc::channel::<SocketRequest>();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"unknown","params":{}}"#;
        let resp = super::handle_rpc_line(line, &tx, &egui::Context::default());
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn jsonrpc_id_preserved_in_response() {
        let (tx, rx) = mpsc::channel::<SocketRequest>();
        fake_app(rx);
        let line = r#"{"jsonrpc":"2.0","id":"abc","method":"close","params":{}}"#;
        let resp = super::handle_rpc_line(line, &tx, &egui::Context::default());
        assert_eq!(resp.id, serde_json::json!("abc"));
        assert!(resp.result.is_some());
    }

    #[test]
    fn jsonrpc_dispatch_error_returns_minus_32000() {
        let (tx, rx) = mpsc::channel::<SocketRequest>();
        // App always returns NoActiveView error
        std::thread::spawn(move || {
            while let Ok((_, resp_tx)) = rx.recv() {
                let _ = resp_tx.send(Err(DispatchError::NoActiveView));
            }
        });
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"goto","params":{"position":1}}"#;
        let resp = super::handle_rpc_line(line, &tx, &egui::Context::default());
        assert_eq!(resp.error.as_ref().unwrap().code, -32000);
    }

    #[test]
    fn file_command_needs_no_socket() {
        let cmd = seqforge_core::FileCommand::Digest {
            input: std::path::PathBuf::from("in.gb"),
            enzymes: vec!["EcoRI".into()],
            output: std::path::PathBuf::from("out.gb"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("EcoRI"));
    }
}
