use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use interprocess::local_socket::{
    GenericFilePath, Listener, ListenerOptions, Stream, ToFsName, prelude::*,
};
use seqforge_core::{DispatchError, ViewerRequest, ViewerResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Socket path lifecycle (Tier 1 hardening) ─────────────────────────────────

/// RAII guard that removes a filesystem socket file on drop. Held in
/// `AppState` so a normal process exit (window close) cleans up the
/// socket, not just the abnormal-exit path that the listener thread's
/// own cleanup covered before.
///
/// On Windows the endpoint is a named pipe (`\\.\pipe\…`); there is no
/// filesystem object to unlink, so drop is a no-op. On Unix, per-pid
/// paths under `$XDG_RUNTIME_DIR` / `/tmp` protect us from collisions
/// even when a stale file is left behind after a panic. Cleanup is
/// best-effort.
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
        // Unix: unlink the filesystem socket. Windows named pipes have no
        // filesystem object — remove_file fails harmlessly.
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── Channel type ──────────────────────────────────────────────────────────────

/// What the socket thread sends to the app's drain loop: the request plus a
/// one-shot sender the app uses to return the dispatch result.
pub type SocketRequest = (
    ViewerRequest,
    mpsc::SyncSender<Result<ViewerResponse, DispatchError>>,
);

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
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn err_response(id: Value, code: i32, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
        }),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Open a local socket at `path`, spawn a listener thread, and return a
/// receiver for incoming `SocketRequest` values.
///
/// On Unix `path` is a filesystem Unix-domain socket. On Windows it is a
/// named-pipe path of the form `\\.\pipe\seqforge-<pid>`. Both are published
/// in `SEQFORGE_SOCKET` and speak the same newline-delimited JSON-RPC.
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
    let name = path
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| anyhow::anyhow!("invalid socket name {}: {e}", path.display()))?;

    #[cfg(unix)]
    let opts = {
        // Hardening: mode 0600 so only the owner can connect. The default
        // umask usually achieves this on macOS / Linux but not always
        // (e.g. umask 022 yields 0644). Without this, any local user on a
        // multi-user host could drive `open` / `find` / `enzymes` against
        // our GUI. Named pipes on Windows already default to the creating
        // user's ACL — no equivalent step.
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        ListenerOptions::new().name(name).mode(0o600)
    };
    #[cfg(not(unix))]
    let opts = ListenerOptions::new().name(name);

    let listener = opts
        .create_sync()
        .map_err(|e| anyhow::anyhow!("could not bind socket at {}: {e}", path.display()))?;

    let (tx, rx) = mpsc::channel::<SocketRequest>();

    let path_clone = path.clone();
    std::thread::Builder::new()
        .name("seqforge-socket".into())
        .spawn(move || accept_loop(listener, tx, ctx, path_clone))?;

    Ok(rx)
}

/// Pick a local-socket endpoint for this GUI process.
///
/// **Unix** preference order:
///   1. `$XDG_RUNTIME_DIR/seqforge-<pid>.sock` — per-user, mode-0700
///      directory, the standard Linux runtime spot.
///   2. `/tmp/seqforge-<pid>.sock` — fallback for macOS (no
///      XDG_RUNTIME_DIR by default) and other systems.
///
/// **Windows:** `\\.\pipe\seqforge-<pid>` — a per-user named pipe (the
/// creating user's ACL is the access boundary).
///
/// The `<pid>` suffix gives per-process uniqueness — multiple GUI
/// instances won't collide, and a stale Unix socket file from a crashed
/// prior process can be reclaimed by the new owner without confusion.
pub fn socket_path() -> PathBuf {
    let pid = std::process::id();
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\pipe\seqforge-{pid}"))
    }
    #[cfg(unix)]
    {
        let name = format!("seqforge-{pid}.sock");
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            if !dir.is_empty() {
                let mut p = PathBuf::from(dir);
                p.push(&name);
                return p;
            }
        }
        PathBuf::from("/tmp").join(name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        PathBuf::from(format!("seqforge-{pid}.sock"))
    }
}

// ── Listener thread ───────────────────────────────────────────────────────────

fn accept_loop(
    listener: Listener,
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
    #[cfg(unix)]
    let _ = std::fs::remove_file(path);
    #[cfg(not(unix))]
    let _ = path; // named pipes have nothing to unlink
}

fn handle_connection(stream: Stream, tx: mpsc::Sender<SocketRequest>, ctx: egui::Context) {
    // `Stream` implements `Read` / `Write` for `&Stream`, so one connection
    // handles both directions without cloning.
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let resp = handle_rpc_line(line.trim_end_matches(['\r', '\n']), &tx, &ctx);
        let json = serde_json::to_string(&resp).unwrap_or_default();
        if writer.write_all(format!("{json}\n").as_bytes()).is_err() {
            break;
        }
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
        Ok(Ok(resp)) => match serde_json::to_value(&resp) {
            Ok(v) => ok_response(id, v),
            Err(e) => err_response(id, ERR_DISPATCH, format!("serialisation error: {e}")),
        },
        Ok(Err(e)) => err_response(id, ERR_DISPATCH, e.to_string()),
        Err(_) => err_response(id, ERR_DISPATCH, "viewer did not respond within timeout"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc;

    use interprocess::local_socket::{
        GenericFilePath, ListenerOptions, Stream, ToFsName, prelude::*,
    };
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

    /// A unique local-socket endpoint for one test (filesystem path on Unix,
    /// named pipe on Windows).
    fn test_endpoint(tag: &str) -> std::path::PathBuf {
        let tag = tag.replace(':', "-");
        #[cfg(windows)]
        {
            std::path::PathBuf::from(format!(
                r"\\.\pipe\seqforge-test-{}-{}",
                tag,
                std::process::id()
            ))
        }
        #[cfg(not(windows))]
        {
            std::env::temp_dir().join(format!("seqforge-test-{}-{}.sock", tag, std::process::id()))
        }
    }

    #[test]
    fn jsonrpc_goto_round_trip() {
        let (tx, rx) = mpsc::channel::<SocketRequest>();
        fake_app(rx);

        let endpoint = test_endpoint("goto");
        let name = endpoint
            .as_os_str()
            .to_fs_name::<GenericFilePath>()
            .expect("test endpoint name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .expect("bind test listener");

        let endpoint_for_client = endpoint.clone();
        std::thread::spawn(move || {
            let stream = match listener.incoming().next() {
                Some(Ok(s)) => s,
                _ => return,
            };
            let mut reader = BufReader::new(&stream);
            let mut writer = &stream;
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let resp = super::handle_rpc_line(line.trim_end(), &tx, &egui::Context::default());
                let json = serde_json::to_string(&resp).unwrap();
                let _ = writer.write_all(format!("{json}\n").as_bytes());
            }
        });

        // Give the accept thread a moment to be waiting.
        std::thread::sleep(std::time::Duration::from_millis(20));

        let name = endpoint_for_client
            .as_os_str()
            .to_fs_name::<GenericFilePath>()
            .expect("client name");
        let mut client = Stream::connect(name).expect("connect to test listener");

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
}
