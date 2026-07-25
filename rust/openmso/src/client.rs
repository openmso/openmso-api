// SPDX-License-Identifier: Apache-2.0
//! Frontend-side OCP client: launches (or connects to) a capture server.
//!
//! A background reader thread resolves request responses and hands
//! notifications to a user callback. The callback runs on the reader thread —
//! keep it quick (append to buffers; do heavy processing elsewhere).
//!
//! Rust counterpart of `python/openmso/client.py`.

use std::collections::HashMap;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::framing::MessageStream;
use crate::PROTOCOL_VERSION;

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Error returned by the remote capture server, or a local transport failure.
#[derive(Debug, Clone)]
pub struct CaptureError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl CaptureError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        CaptureError { code, message: message.into(), data: None }
    }
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for CaptureError {}

/// Called for each notification (a message with `method` but no `id`).
pub type NotificationHandler =
    Box<dyn Fn(&str, &Value, Option<&[u8]>) + Send + 'static>;

struct Inner {
    next_id: i64,
    pending: HashMap<i64, Option<Value>>, // id -> response message once it arrives
    eof: bool,
}

struct Shared {
    stream: Arc<MessageStream>,
    inner: Mutex<Inner>,
    cvar: Condvar,
    handler: Mutex<Option<NotificationHandler>>,
}

pub struct CaptureClient {
    shared: Arc<Shared>,
    proc: Option<Child>,
    reader: Option<JoinHandle<()>>,
}

impl CaptureClient {
    fn start(stream: MessageStream, proc: Option<Child>,
             handler: Option<NotificationHandler>) -> Self {
        let shared = Arc::new(Shared {
            stream: Arc::new(stream),
            inner: Mutex::new(Inner { next_id: 0, pending: HashMap::new(), eof: false }),
            cvar: Condvar::new(),
            handler: Mutex::new(handler),
        });
        let reader = {
            let shared = shared.clone();
            std::thread::spawn(move || read_loop(shared))
        };
        CaptureClient { shared, proc, reader: Some(reader) }
    }

    // -- constructors -----------------------------------------------------
    /// Spawn a capture-server subprocess speaking OCP on its stdio.
    ///
    /// The server's stderr is inherited so its diagnostics reach the user.
    pub fn launch(argv: &[String], handler: Option<NotificationHandler>)
        -> std::io::Result<Self>
    {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut proc = cmd.spawn()?;
        let stdout = proc.stdout.take().expect("child stdout piped");
        let stdin = proc.stdin.take().expect("child stdin piped");
        let stream = MessageStream::new(Box::new(stdout), Box::new(stdin));
        Ok(Self::start(stream, Some(proc), handler))
    }

    /// Connect to a capture server already listening on `host:port`.
    pub fn connect(host: &str, port: u16, handler: Option<NotificationHandler>)
        -> std::io::Result<Self>
    {
        let sock = TcpStream::connect((host, port))?;
        let reader = sock.try_clone()?;
        let stream = MessageStream::new(Box::new(reader), Box::new(sock));
        Ok(Self::start(stream, None, handler))
    }

    // -- API --------------------------------------------------------------
    pub fn set_notification_handler(&self, handler: NotificationHandler) {
        *self.shared.handler.lock().unwrap() = Some(handler);
    }

    /// Send a request and block until the response arrives or `timeout` elapses.
    pub fn request(&self, method: &str, params: Value, timeout: Duration)
        -> Result<Value, CaptureError>
    {
        let id = {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.next_id += 1;
            let id = inner.next_id;
            inner.pending.insert(id, None);
            id
        };
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method,
                         "params": params});
        if let Err(e) = self.shared.stream.write_message(&msg, None) {
            self.shared.inner.lock().unwrap().pending.remove(&id);
            return Err(CaptureError::new(-1, format!("write failed: {e}")));
        }

        let deadline = Instant::now() + timeout;
        let mut inner = self.shared.inner.lock().unwrap();
        loop {
            if let Some(Some(_)) = inner.pending.get(&id) {
                break;
            }
            if inner.eof {
                inner.pending.remove(&id);
                return Err(CaptureError::new(
                    -1, "capture server exited before responding"));
            }
            let now = Instant::now();
            if now >= deadline {
                inner.pending.remove(&id);
                return Err(CaptureError::new(
                    -1, format!("no response to {method:?} within {timeout:?}")));
            }
            let (guard, _) = self.shared.cvar
                .wait_timeout(inner, deadline - now).unwrap();
            inner = guard;
        }
        let response = inner.pending.remove(&id).flatten().unwrap();
        drop(inner);

        if let Some(err) = response.get("error") {
            return Err(CaptureError {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(-1),
                message: err.get("message").and_then(Value::as_str)
                    .unwrap_or("?").to_string(),
                data: err.get("data").cloned(),
            });
        }
        Ok(response.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    pub fn initialize(&self, client_name: &str) -> Result<Value, CaptureError> {
        self.request("initialize", json!({
            "protocol_version": PROTOCOL_VERSION,
            "client": {"name": client_name, "version": CLIENT_VERSION},
        }), Duration::from_secs(60))
    }

    /// Block until the reader thread observes EOF (the server went away).
    pub fn wait_closed(&self) {
        let inner = self.shared.inner.lock().unwrap();
        drop(self.shared.cvar.wait_while(inner, |i| !i.eof).unwrap());
    }

    pub fn close(&mut self) {
        let _ = self.request("shutdown", json!({}), Duration::from_secs(5));
        if let Some(mut proc) = self.proc.take() {
            let _ = proc.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for CaptureClient {
    fn drop(&mut self) {
        if self.proc.is_some() || self.reader.is_some() {
            self.close();
        }
    }
}

fn read_loop(shared: Arc<Shared>) {
    loop {
        match shared.stream.read_message() {
            Ok(Some((msg, payload))) => {
                let has_id = msg.get("id").map(|v| !v.is_null()).unwrap_or(false);
                let method = msg.get("method").and_then(Value::as_str);
                match method {
                    // Response to one of our requests.
                    None if has_id => {
                        let id = msg["id"].as_i64().unwrap_or(-1);
                        let mut inner = shared.inner.lock().unwrap();
                        if inner.pending.contains_key(&id) {
                            inner.pending.insert(id, Some(msg));
                            shared.cvar.notify_all();
                        }
                    }
                    // Notification from the server.
                    Some(method) => {
                        let params = msg.get("params").cloned()
                            .unwrap_or_else(|| json!({}));
                        if let Some(h) = shared.handler.lock().unwrap().as_ref() {
                            h(method, &params, payload.as_deref());
                        }
                    }
                    None => {} // id-less response: none expected in v0
                }
            }
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("omso: capture server stream error: {e}");
                break;
            }
        }
    }
    let mut inner = shared.inner.lock().unwrap();
    inner.eof = true;
    shared.cvar.notify_all();
}
