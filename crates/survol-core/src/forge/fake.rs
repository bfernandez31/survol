//! A fake HTTP server for forge tests: serves canned responses in order and
//! reports each request (method, path with query, headers of interest, body).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path and query, e.g. `/api/v4/version?x=1`.
    pub path: String,
    pub authorization: String,
    pub content_type: String,
    pub body: String,
}

impl Request {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

/// A canned response: status line (`200 OK`), extra headers, body.
pub struct Response {
    pub status: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

pub fn ok(body: &str) -> Response {
    Response {
        status: "200 OK",
        headers: Vec::new(),
        body: body.to_string(),
    }
}

pub fn status(status: &'static str, body: &str) -> Response {
    Response {
        status,
        headers: Vec::new(),
        body: body.to_string(),
    }
}

/// Serves `responses`, one per connection; returns the base URL and the
/// requests received.
pub fn serve(responses: Vec<Response>) -> (String, mpsc::Receiver<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for resp in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let (mut len, mut authorization, mut content_type) = (0, String::new(), String::new());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let (k, v) = line.split_once(':').unwrap_or((&line, ""));
                let v = v.trim().to_string();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => len = v.parse().unwrap_or(0),
                    "authorization" => authorization = v,
                    "content-type" => content_type = v,
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            tx.send(Request {
                method,
                path,
                authorization,
                content_type,
                body: String::from_utf8_lossy(&body).into_owned(),
            })
            .unwrap();
            let extra: String = resp
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\r\n"))
                .collect();
            write!(
                stream,
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                resp.status,
                resp.body.len(),
                resp.body
            )
            .unwrap();
        }
    });
    (addr, rx)
}
