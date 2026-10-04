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

/// The most of an answer `post` reads, head and body: a webhook's answer
/// is a few hundred bytes, and one past this is a bug, not a slow server.
pub const ANSWER_BYTES_MAX: usize = 1024 * 1024;

/// POSTs to an `http://` URL; the answer's status, or why it failed.
///
/// It returns as soon as the answer is complete (its head, then its body
/// by Content-Length, or to the chunked terminator) and drops the socket.
/// Reading to the end of the stream is not "complete": workerd's HTTP
/// server holds a connection open for a next request for 5 s after an
/// answer (KJ's pipeline timeout), whatever `connection: close` asked, so a
/// client that waits for the server to close waits 5 s on every delivery.
/// Only an answer that declares no length is read to the end, as HTTP/1.1
/// says it must be.
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
    read_answer(&mut stream)
}

/// How an answer's body ends (RFC 9112, 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// No body: a 1xx, 204 or 304.
    Empty,
    Length(usize),
    Chunked,
    /// No length declared: the body ends when the server closes.
    Close,
}

/// Reads one answer from `stream`, to its last byte and no further: its
/// status, or why it is not a whole HTTP/1.1 answer.
fn read_answer(stream: &mut impl Read) -> Result<u16, String> {
    let mut answer = Answer { buf: Vec::new(), at: 0, stream };
    let head_end = answer.until(b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&answer.buf[..head_end]).into_owned();
    answer.at = head_end;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = match status_line.split_whitespace().collect::<Vec<_>>().as_slice() {
        [version, code, ..] if version.starts_with("HTTP/1.") => code.parse().map_err(|_| format!("no HTTP status in {status_line:?}"))?,
        _ => return Err(format!("no HTTP status in {:?}", status_line.chars().take(80).collect::<String>())),
    };
    let header = |name: &str| lines.clone().filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string());
    let framing = match (status, header("transfer-encoding"), header("content-length")) {
        (100..=199 | 204 | 304, _, _) => Framing::Empty,
        (_, Some(te), _) if te.to_ascii_lowercase().ends_with("chunked") => Framing::Chunked,
        (_, _, Some(n)) => Framing::Length(n.parse().map_err(|_| format!("a Content-Length that is no number: {n:?}"))?),
        (_, _, None) => Framing::Close,
    };
    match framing {
        Framing::Empty => {}
        Framing::Length(n) => answer.take(n)?,
        Framing::Chunked => answer.chunks()?,
        Framing::Close => answer.to_end()?,
    }
    assert!(answer.buf.len() <= ANSWER_BYTES_MAX, "an answer is read within its bound");
    Ok(status)
}

/// An answer as it arrives: what was read (`buf`), and how far it is parsed (`at`).
struct Answer<'a, R: Read> {
    buf: Vec<u8>,
    at: usize,
    stream: &'a mut R,
}

impl<R: Read> Answer<'_, R> {
    /// Reads more; an error when the server closed first, or the answer
    /// outgrew its bound.
    fn more(&mut self, what: &str) -> Result<(), String> {
        let mut tmp = [0u8; 16384];
        let n = self.stream.read(&mut tmp).map_err(|e| format!("reading {what}: {e}"))?;
        if n == 0 {
            return Err(format!("the server closed the connection before {what} ended"));
        }
        self.buf.extend_from_slice(&tmp[..n]);
        if self.buf.len() > ANSWER_BYTES_MAX {
            return Err(format!("an answer of more than {ANSWER_BYTES_MAX} bytes"));
        }
        Ok(())
    }

    /// The end of the first `marker` at or after `at`, reading until it comes.
    fn until(&mut self, marker: &[u8]) -> Result<usize, String> {
        // bounded: each pass reads at least a byte, and `more` stops at ANSWER_BYTES_MAX
        loop {
            if let Some(i) = self.buf[self.at..].windows(marker.len()).position(|w| w == marker) {
                return Ok(self.at + i + marker.len());
            }
            self.more("the answer's head")?;
        }
    }

    /// `n` more bytes past `at`, which then ends past them.
    fn take(&mut self, n: usize) -> Result<(), String> {
        if n > ANSWER_BYTES_MAX {
            return Err(format!("a body of {n} bytes, more than {ANSWER_BYTES_MAX}"));
        }
        // bounded: each pass reads at least a byte, and `more` stops at ANSWER_BYTES_MAX
        while self.buf.len() < self.at + n {
            self.more("the body")?;
        }
        self.at += n;
        Ok(())
    }

    /// A chunked body, to its last chunk and the trailers' blank line.
    fn chunks(&mut self) -> Result<(), String> {
        // bounded: a chunk is a byte at least, and `more` stops at ANSWER_BYTES_MAX
        loop {
            let line_end = self.until(b"\r\n")?;
            let line = String::from_utf8_lossy(&self.buf[self.at..line_end - 2]).into_owned();
            self.at = line_end;
            let size = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size, 16).map_err(|_| format!("a chunk size that is no number: {line:?}"))?;
            if size == 0 {
                break;
            }
            self.take(size)?;
            let end = self.until(b"\r\n")?;
            if end != self.at + 2 {
                return Err("a chunk longer than its size".into());
            }
            self.at = end;
        }
        // the trailers, each a line, then a blank one
        loop {
            let line_end = self.until(b"\r\n")?;
            let blank = line_end == self.at + 2;
            self.at = line_end;
            if blank {
                return Ok(());
            }
        }
    }

    /// A body with no length: to the end of the stream, which the server ends.
    fn to_end(&mut self) -> Result<(), String> {
        let mut tmp = [0u8; 16384];
        // bounded: each pass reads at least a byte, and the check stops at ANSWER_BYTES_MAX
        loop {
            let n = self.stream.read(&mut tmp).map_err(|e| format!("reading the body: {e}"))?;
            if n == 0 {
                self.at = self.buf.len();
                return Ok(());
            }
            self.buf.extend_from_slice(&tmp[..n]);
            if self.buf.len() > ANSWER_BYTES_MAX {
                return Err(format!("an answer of more than {ANSWER_BYTES_MAX} bytes"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! `post` against servers of the tests' own, each of which answers and
    //! then holds the connection open, as workerd does for its pipeline
    //! timeout: a whole answer returns at once, however it is framed, and
    //! one cut short is an error, never a status.
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    /// A server that answers one request with `answer`, written in pieces
    /// of at most `piece` bytes, then holds the socket until the test ends
    /// (`hold`) or, with `close`, closes it.
    fn server(answer: &'static [u8], piece: usize, close: bool) -> (String, mpsc::Sender<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}/hook", listener.local_addr().unwrap().port());
        let (hold, held) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream).expect("a request");
            for part in answer.chunks(piece) {
                stream.write_all(part).unwrap();
                stream.flush().unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
            if !close {
                // held until the test drops its end: never a close that ends the answer
                let _ = held.recv_timeout(Duration::from_secs(20));
            }
        });
        (url, hold)
    }

    fn timed(url: &str) -> (Result<u16, String>, Duration) {
        let t0 = Instant::now();
        let r = post(url, &[("content-type", "application/json")], b"{}");
        (r, t0.elapsed())
    }

    #[test]
    fn a_whole_answer_returns_at_once_on_a_connection_held_open() {
        let answers: [&'static [u8]; 4] = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\ncontent-type: application/json\r\n\r\n{\"ok\":true}",
            b"HTTP/1.1 202 Accepted\r\nTransfer-Encoding: chunked\r\n\r\n4;ext=1\r\n{\"ok\r\n7\r\n\":true}\r\n0\r\nx-trailer: 1\r\n\r\n",
            b"HTTP/1.1 204 No Content\r\n\r\n",
            b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n",
        ];
        for (answer, status) in answers.into_iter().zip([200, 202, 204, 401]) {
            // in one write, and a byte at a time
            for piece in [answer.len(), 1] {
                let (url, hold) = server(answer, piece, false);
                let (r, took) = timed(&url);
                assert_eq!(r, Ok(status), "{}", String::from_utf8_lossy(answer));
                assert!(took < Duration::from_secs(3), "{took:?}: post waited for the server to close");
                drop(hold);
            }
        }
    }

    #[test]
    fn an_answer_without_a_length_is_read_until_the_server_closes() {
        let (url, _hold) = server(b"HTTP/1.1 200 OK\r\n\r\nall of it", 4, true);
        assert_eq!(timed(&url).0, Ok(200));
    }

    #[test]
    fn an_answer_cut_short_is_an_error() {
        let cut: [&'static [u8]; 4] = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n{\"ok\":",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nshort",
            b"HTTP/1.1 200 OK\r\ncontent-len",
            b"",
        ];
        for answer in cut {
            let (url, _hold) = server(answer, 64, true);
            let r = timed(&url).0;
            assert!(r.is_err(), "{r:?} for {}", String::from_utf8_lossy(answer));
        }
        let (url, _hold) = server(b"SMTP ready\r\n\r\n", 64, true);
        assert!(timed(&url).0.is_err_and(|e| e.contains("no HTTP status")));
    }
}
