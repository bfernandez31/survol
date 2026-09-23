//! JSON-RPC over a byte stream, as spoken by language servers: messages
//! framed by a `Content-Length` header. One reader thread routes responses
//! to their pending request, answers the server's own requests (with
//! neutral results) and queues notifications.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::LspError;

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;
type Pending = Arc<Mutex<HashMap<i64, Sender<Result<Value, LspError>>>>>;

/// A notification sent by the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// The client side of a JSON-RPC connection.
pub struct Client {
    writer: Writer,
    pending: Pending,
    notifications: Receiver<Notification>,
    next_id: AtomicI64,
}

impl Client {
    /// Speaks JSON-RPC over `reader` (the server's output) and `writer`
    /// (its input).
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        let writer: Writer = Arc::new(Mutex::new(Box::new(writer)));
        let pending: Pending = Arc::default();
        let (tx, notifications) = mpsc::channel();
        {
            let writer = writer.clone();
            let pending = pending.clone();
            std::thread::spawn(move || read_loop(BufReader::new(reader), &writer, &pending, &tx));
        }
        Self {
            writer,
            pending,
            notifications,
            next_id: AtomicI64::new(1),
        }
    }

    /// Sends a request and waits at most `timeout` for its result. On
    /// timeout the request is cancelled (`$/cancelRequest`).
    pub fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, LspError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(id, tx);
        let sent = write_message(
            &self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        if let Err(e) = sent {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(res) => res,
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                let _ = self.notify("$/cancelRequest", json!({"id": id}));
                Err(LspError::Timeout(method.to_string()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(LspError::Closed),
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        write_message(
            &self.writer,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
    }

    /// Next notification, waiting at most `timeout`. `Ok(None)`: none came.
    pub fn next_notification(&self, timeout: Duration) -> Result<Option<Notification>, LspError> {
        match self.notifications.recv_timeout(timeout) {
            Ok(n) => Ok(Some(n)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(LspError::Closed),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read_loop(
    mut reader: impl BufRead,
    writer: &Writer,
    pending: &Pending,
    notifications: &Sender<Notification>,
) {
    while let Ok(Some(msg)) = read_message(&mut reader) {
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").filter(|v| !v.is_null());
        match (method, id) {
            // A request of the server: answer so that it does not wait.
            (Some(method), Some(id)) => {
                let result = server_request_result(method, msg.get("params"));
                let _ = write_message(
                    writer,
                    &json!({"jsonrpc": "2.0", "id": id, "result": result}),
                );
            }
            (Some(method), None) => {
                let _ = notifications.send(Notification {
                    method: method.to_string(),
                    params: msg.get("params").cloned().unwrap_or(Value::Null),
                });
            }
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { continue };
                let Some(tx) = lock(pending).remove(&id) else {
                    continue;
                };
                let res = match msg.get("error") {
                    Some(err) => Err(LspError::Rpc {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: err
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    }),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(res);
            }
            (None, None) => {}
        }
    }
    // The server is gone: fail every waiting request.
    lock(pending).clear();
}

/// What to answer to a request of the server: an empty configuration for
/// each item asked, `null` otherwise (progress tokens, registrations...).
fn server_request_result(method: &str, params: Option<&Value>) -> Value {
    match method {
        "workspace/configuration" => {
            let n = params
                .and_then(|p| p.get("items"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Value::Array(vec![Value::Null; n])
        }
        "workspace/workspaceFolders" => Value::Array(Vec::new()),
        _ => Value::Null,
    }
}

/// Reads one framed message; `Ok(None)` at the end of the stream.
pub(crate) fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = header.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().ok();
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Writes one framed message.
pub(crate) fn write_frame(w: &mut dyn Write, msg: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

fn write_message(writer: &Writer, msg: &Value) -> Result<(), LspError> {
    let mut w = lock(writer);
    write_frame(&mut **w, msg).map_err(LspError::Io)
}
