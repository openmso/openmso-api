// SPDX-License-Identifier: Apache-2.0
//! Capture-server side of OCP: JSON-RPC dispatch loop over a MessageStream.
//!
//! Implement the [`CaptureServer`] trait and hand an instance to
//! [`run_from_argv`]. Requests are handled on the serve loop in arrival
//! order; long-running acquisition belongs in a worker thread that emits
//! notifications through the shared [`Ctx`] (writes are thread-safe).

use serde_json::{json, Value};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::framing::MessageStream;
use crate::PROTOCOL_VERSION;

// JSON-RPC error codes
pub const PARSE_ERROR: i64 = -32700;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
// OCP capture-server error codes (>= 1000)
pub const DEVICE_ERROR: i64 = 1000;
pub const DEVICE_DISCONNECTED: i64 = 1001;
pub const BUSY: i64 = 1002;
pub const UNSUPPORTED: i64 = 1003;

#[derive(Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError { code, message: message.into(), data: None }
    }

    pub fn method_not_found(method: &str) -> Self {
        RpcError::new(METHOD_NOT_FOUND, format!("method not found: {method}"))
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

/// Shared server context: the outgoing stream plus the shutdown latch.
/// Clone the `Arc<Ctx>` into acquisition worker threads.
pub struct Ctx {
    stream: Arc<MessageStream>,
    shutdown: AtomicBool,
}

impl Ctx {
    pub fn notify(&self, method: &str, params: Value, payload: Option<&[u8]>) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        if let Err(e) = self.stream.write_message(&msg, payload) {
            eprintln!("[error] notify {method} failed: {e}");
        }
    }

    pub fn log(&self, level: &str, message: &str) {
        self.notify("log", json!({"level": level, "message": message}), None);
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

pub trait CaptureServer {
    fn info(&self) -> Value;

    fn capabilities(&self) -> Value {
        json!({})
    }

    fn handle(&mut self, method: &str, params: &Value, payload: Option<Vec<u8>>,
              ctx: &Arc<Ctx>) -> Result<Value, RpcError>;

    /// Called when the serve loop ends; release hardware here.
    fn on_disconnect(&mut self) {}
}

pub fn serve(server: &mut dyn CaptureServer, stream: MessageStream) {
    let stream = Arc::new(stream);
    let ctx = Arc::new(Ctx { stream: stream.clone(), shutdown: AtomicBool::new(false) });
    while !ctx.shutdown.load(Ordering::SeqCst) {
        let (msg, payload) = match stream.read_message() {
            Ok(Some(item)) => item,
            Ok(None) => break, // EOF: frontend went away
            Err(e) => {
                eprintln!("protocol error: {e}");
                break;
            }
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue; // responses from client: none expected in v0
        };
        let method = method.to_string();
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method.as_str() {
            "initialize" => {
                let client_version = params.get("protocol_version")
                    .and_then(Value::as_i64).unwrap_or(0);
                Ok(json!({
                    "protocol_version": PROTOCOL_VERSION.min(client_version),
                    "plugin": server.info(),
                    "capabilities": server.capabilities(),
                }))
            }
            "shutdown" => {
                ctx.request_shutdown();
                Ok(json!({}))
            }
            _ => server.handle(&method, &params, payload, &ctx),
        };
        let Some(id) = id else { continue };
        let reply = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => {
                let mut err = json!({"code": e.code, "message": e.message});
                if let Some(data) = e.data {
                    err["data"] = data;
                }
                json!({"jsonrpc": "2.0", "id": id, "error": err})
            }
        };
        if let Err(e) = stream.write_message(&reply, None) {
            eprintln!("write failed: {e}");
            break;
        }
    }
    server.on_disconnect();
}

pub fn run_stdio(server: &mut dyn CaptureServer) {
    let stream = MessageStream::new(Box::new(std::io::stdin()),
                                    Box::new(std::io::stdout()));
    serve(server, stream);
}

pub fn run_tcp(server: &mut dyn CaptureServer, host: &str, port: u16) {
    let listener = TcpListener::bind((host, port))
        .unwrap_or_else(|e| panic!("cannot listen on {host}:{port}: {e}"));
    eprintln!("[info] listening on {host}:{port}");
    let (conn, addr) = listener.accept().expect("accept failed");
    eprintln!("[info] client connected from {addr}");
    let reader = conn.try_clone().expect("socket clone failed");
    serve(server, MessageStream::new(Box::new(reader), Box::new(conn)));
}

pub fn run_from_argv(server: &mut dyn CaptureServer) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--listen") {
        let hostport = args.get(i + 1).map(String::as_str)
            .expect("--listen requires HOST:PORT or PORT");
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (if h.is_empty() { "127.0.0.1" } else { h }, p),
            None => ("127.0.0.1", hostport),
        };
        let port: u16 = port.parse().expect("bad --listen port");
        run_tcp(server, host, port);
    } else {
        run_stdio(server);
    }
}
