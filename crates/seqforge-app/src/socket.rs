use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use seqforge_core::ViewerRequest;

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

// JSON-RPC 2.0 standard error codes + application range
const ERR_PARSE: i32 = -32700;
const ERR_INVALID_REQUEST: i32 = -32600;
const ERR_METHOD_NOT_FOUND: i32 = -32601;
const ERR_INVALID_PARAMS: i32 = -32602;
#[allow(dead_code)] // reserved for dispatch errors surfaced in Stage 5+
const ERR_DISPATCH: i32 = -32000;

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

/// Open a Unix domain socket, spawn a listener thread, and return the socket
/// path + a receiver for incoming `ViewerRequest` values.
///
/// The socket path is `/tmp/seqforge-{pid}.sock`. Any stale file from a prior
/// crash is removed before binding.
pub fn start_socket_listener(
    ctx: egui::Context,
) -> anyhow::Result<(PathBuf, mpsc::Receiver<ViewerRequest>)> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // remove stale socket from prior crash

    let listener = UnixListener::bind(&path)?;
    let (tx, rx) = mpsc::channel::<ViewerRequest>();

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
    tx: mpsc::Sender<ViewerRequest>,
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
    tx: mpsc::Sender<ViewerRequest>,
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

/// Parse one newline-delimited JSON-RPC request, enqueue it, and return the response envelope.
fn handle_rpc_line(
    line: &str,
    tx: &mpsc::Sender<ViewerRequest>,
    ctx: &egui::Context,
) -> JsonRpcResponse {
    // Parse outer JSON-RPC envelope.
    let rpc: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return err_response(Value::Null, ERR_PARSE, format!("parse error: {e}"));
        }
    };

    let id = rpc.id.clone();

    // Merge method + params into a tagged-enum object that ViewerRequest deserialises from.
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
            // Distinguish "unknown method" from malformed params.
            if msg.contains("unknown variant") {
                return err_response(id, ERR_METHOD_NOT_FOUND, "method not found");
            }
            return err_response(id, ERR_INVALID_PARAMS, format!("invalid params: {msg}"));
        }
    };

    if tx.send(req).is_err() {
        return err_response(id, ERR_INVALID_REQUEST, "viewer no longer running");
    }
    ctx.request_repaint();

    // Acknowledge immediately; the actual dispatch result is async (next frame).
    // Stage 5+ can introduce a response channel for synchronous results.
    ok_response(id, serde_json::json!({"kind": "ok"}))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    use seqforge_core::ViewerRequest;

    /// JSON-RPC round-trip over a connected UnixStream pair.
    #[test]
    fn jsonrpc_goto_round_trip() {
        let (tx, rx) = std::sync::mpsc::channel::<ViewerRequest>();

        let (mut client, server) = UnixStream::pair().unwrap();

        std::thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            let mut server = server;
            for line in reader.lines().flatten() {
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
        assert_eq!(resp["result"]["kind"], "ok");

        let received = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert!(matches!(received, ViewerRequest::GoTo { position: 42 }));
    }

    #[test]
    fn jsonrpc_parse_error_returns_minus_32700() {
        let (tx, _rx) = std::sync::mpsc::channel::<ViewerRequest>();
        let resp = super::handle_rpc_line("not json", &tx, &egui::Context::default());
        assert_eq!(resp.error.as_ref().unwrap().code, -32700);
    }

    #[test]
    fn jsonrpc_unknown_method_returns_minus_32601() {
        let (tx, _rx) = std::sync::mpsc::channel::<ViewerRequest>();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"unknown","params":{}}"#;
        let resp = super::handle_rpc_line(line, &tx, &egui::Context::default());
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn jsonrpc_id_preserved_in_response() {
        let (tx, _rx) = std::sync::mpsc::channel::<ViewerRequest>();
        let line = r#"{"jsonrpc":"2.0","id":"abc","method":"close","params":{}}"#;
        let resp = super::handle_rpc_line(line, &tx, &egui::Context::default());
        assert_eq!(resp.id, serde_json::json!("abc"));
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
