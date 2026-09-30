//! A small blocking HTTP/1.1 server and client over std TCP: enough for a
//! fake service (one request per connection) and for delivering webhooks.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// The query's pairs in order, a repeated key's each time.
    pub pairs: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Close the connection without writing an answer.
    pub unanswered: bool,
    /// A `101`: after its head, the connection is this one's (a WebSocket).
    pub upgrade: Option<Upgrade>,
}

/// What runs a connection a `101` handed over.
pub type Upgrade = Box<dyn FnOnce(TcpStream) + Send>;

impl Response {
    /// No answer at all: the connection closes once the request is read (a
    /// request that was handled, and an answer lost on its way back).
    pub fn unanswered() -> Response {
        Response { status: 0, headers: Vec::new(), body: Vec::new(), unanswered: true, upgrade: None }
    }

    pub fn json(status: u16, v: &serde_json::Value) -> Response {
        Response::bytes(status, "application/json", v.to_string().into_bytes())
    }

    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response { status, headers: vec![("content-type".into(), content_type.into())], body, unanswered: false, upgrade: None }
    }

    /// Accepts a WebSocket (`key`, the client's `Sec-WebSocket-Key`),
    /// choosing `protocol`; `run` then holds the connection.
    pub fn websocket(key: &str, protocol: Option<&str>, run: impl FnOnce(TcpStream) + Send + 'static) -> Response {
        use base64::Engine;
        use sha1::Digest;
        let accept = base64::engine::general_purpose::STANDARD.encode(sha1::Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
        let mut headers = vec![("upgrade".into(), "websocket".into()), ("connection".into(), "Upgrade".into()), ("sec-websocket-accept".into(), accept)];
        if let Some(p) = protocol {
            headers.push(("sec-websocket-protocol".into(), p.into()));
        }
        Response { status: 101, headers, body: Vec::new(), unanswered: false, upgrade: Some(Box::new(run)) }
    }

    pub fn with_header(mut self, k: &str, v: &str) -> Response {
        self.headers.push((k.into(), v.into()));
        self
    }
}

pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

/// Serves until dropped.
pub struct Server {
    pub url: String,
    pub port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// `port` 0 picks a free one.
    pub fn start(port: u16, handler: Handler) -> std::io::Result<Server> {
        Server::serve(TcpListener::bind(("127.0.0.1", port))?, handler)
    }

    /// Serves on a listener the caller bound, when it must know the port
    /// before the handler exists: binding once means no one else can take
    /// the port in between.
    pub fn serve(listener: TcpListener, handler: Handler) -> std::io::Result<Server> {
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || serve_one(stream, &handler));
            }
        });
        Ok(Server { url: format!("http://127.0.0.1:{port}"), port, stop })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wake the accept loop
    }
}

fn serve_one(mut stream: TcpStream, handler: &Handler) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let Some(req) = read_request(&mut stream) else { return };
    let head = req.method == "HEAD";
    let resp = handler(&req);
    if resp.unanswered {
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    }
    if let Some(run) = resp.upgrade {
        let mut out = "HTTP/1.1 101 Switching Protocols\r\n".to_string();
        for (k, v) in &resp.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str("\r\n");
        if stream.write_all(out.as_bytes()).is_ok() {
            let _ = stream.set_read_timeout(None);
            run(stream);
        }
        return;
    }
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    for (k, v) in &resp.headers {
        if !k.eq_ignore_ascii_case("content-length") {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    let declared = resp.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.clone());
    out.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n", declared.unwrap_or_else(|| resp.body.len().to_string())));
    let _ = stream.write_all(out.as_bytes());
    if !head {
        let _ = stream.write_all(&resp.body);
    }
    let _ = stream.flush();
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        _ => "Status",
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    let header_end = loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(len);
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let pairs: Vec<(String, String)> = q
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    Some(Request { method, path: decode(path), query: pairs.iter().cloned().collect(), pairs, headers, body })
}

/// Percent-decoding (and `+` as space in queries, which paths never carry).
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 3;
                }
                None => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// POSTs to an `http://` URL; the answer's status, or why it failed.
pub fn post(url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<u16, String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// URLs")?;
    let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let mut stream = TcpStream::connect(authority).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    let mut req = format!("POST {path} HTTP/1.1\r\nhost: {authority}\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).map_err(|e| e.to_string())?;
    let line = String::from_utf8_lossy(&answer);
    line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| format!("no HTTP status in {:?}", line.chars().take(80).collect::<String>()))
}
