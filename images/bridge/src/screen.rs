//! A computer's screen (decision 11; docs/computers.md, Ports): a page on
//! its port (6080 by convention), and, when the image has a display, a
//! WebSocket onto its RFB server with Take over / Give back kept here:
//!
//! - `GET /` and the page's files, from a directory;
//! - `GET /websockify?viewer=<id>`: the RFB stream, bytes both ways. Every
//!   viewer sees the screen; input (keys, pointer, clipboard) reaches it
//!   only from the viewer holding control. The RFB client stream is parsed
//!   message by message, so input is dropped, never half-sent;
//! - `GET /control?viewer=<id>`: a WebSocket of `{type: "take"}` and
//!   `{type: "give"}` from the page, answered to every viewer with
//!   `{type: "control", holder: <viewer>|null}`.
//!
//! The display starts lazily: a viewer runs `start` when the RFB socket
//! does not answer, at most once a minute while it stays down (S3b: the
//! dashboard and screen lazy).

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::net::{self, Body};

/// The RFB server the screen shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Unix(PathBuf),
    Tcp(String),
}

impl Target {
    /// `unix:<path>` or `tcp:<host:port>`.
    pub fn parse(s: &str) -> Result<Target, String> {
        if let Some(p) = s.strip_prefix("unix:") {
            return Ok(Target::Unix(PathBuf::from(p)));
        }
        if let Some(a) = s.strip_prefix("tcp:") {
            return Ok(Target::Tcp(a.to_string()));
        }
        Err(format!("{s}: the RFB target is unix:<path> or tcp:<host:port>"))
    }
}

#[derive(Debug, Clone)]
pub struct ScreenConfig {
    pub listen: SocketAddr,
    /// The page and its files.
    pub dir: PathBuf,
    /// No target: the page alone (the stub's).
    pub target: Option<Target>,
    /// What starts the display, run once by the first viewer that finds it
    /// down.
    pub start: Option<Vec<String>>,
}

/// While the display stays down, its start is run again at most this often:
/// what it starts may change (the image's first agent, while it runs), and
/// a start that came to nothing is not the screen's last word.
pub const START_AGAIN_MS: u64 = 60_000;

/// Whether a viewer that finds the display down runs its start, given the
/// last start's time.
fn may_start(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(START_AGAIN_MS))
}

/// Who holds control, shared by every viewer; and when the display was
/// last started.
struct Control {
    holder: Mutex<Option<String>>,
    changes: broadcast::Sender<Option<String>>,
    started: Mutex<Option<Instant>>,
}

pub async fn serve(cfg: ScreenConfig, stop: watch::Receiver<bool>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.map_err(|e| format!("screen listen {}: {e}", cfg.listen))?;
    crate::ev!("screen.listening", { "listen": cfg.listen.to_string(), "rfb": cfg.target.is_some() });
    let (changes, _) = broadcast::channel(16);
    let control = Arc::new(Control { holder: Mutex::new(None), changes, started: Mutex::new(None) });
    let cfg = Arc::new(cfg);
    let handler = move |req: Request<Incoming>, _peer: SocketAddr| {
        let (cfg, control) = (cfg.clone(), control.clone());
        async move { handle(req, cfg, control).await }
    };
    net::serve(listener, handler, stop).await;
    Ok(())
}

fn viewer_of(req: &Request<Incoming>) -> Option<String> {
    let q = req.uri().query()?;
    let v = q.split('&').find_map(|kv| kv.strip_prefix("viewer="))?;
    let ok = !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then(|| v.to_string())
}

async fn handle(mut req: Request<Incoming>, cfg: Arc<ScreenConfig>, control: Arc<Control>) -> Response<Body> {
    let path = req.uri().path().to_string();
    match path.as_str() {
        "/websockify" | "/control" => {
            let Some(target) = cfg.target.clone() else { return net::refusal(StatusCode::NOT_FOUND, "not_found", "this computer has no display") };
            let Some(viewer) = viewer_of(&req) else { return net::refusal(StatusCode::BAD_REQUEST, "invalid", "?viewer=<id>") };
            let Some((response, socket)) = net::accept_ws(&mut req) else { return net::refusal(StatusCode::BAD_REQUEST, "invalid", "a WebSocket") };
            let rfb = path == "/websockify";
            tokio::spawn(async move {
                let Some(ws) = socket.await else { return };
                if rfb {
                    viewer_rfb(ws, target, viewer, cfg, control).await;
                } else {
                    viewer_control(ws, viewer, control).await;
                }
            });
            response
        }
        _ => file(&cfg.dir, &path).await,
    }
}

async fn file(dir: &Path, path: &str) -> Response<Body> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() || rel.ends_with('/') { format!("{rel}index.html") } else { rel.to_string() };
    let p = Path::new(&rel);
    let safe = p.components().all(|c| matches!(c, Component::Normal(_)));
    if !safe {
        return net::refusal(StatusCode::NOT_FOUND, "not_found", "no such file");
    }
    let full = dir.join(p);
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("");
            let ty = match ext {
                "html" => "text/html; charset=utf-8",
                "js" | "mjs" => "text/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                "json" => "application/json",
                _ => "application/octet-stream",
            };
            Response::builder().status(StatusCode::OK).header("content-type", ty).header("cache-control", "no-cache").body(http_body_util::Full::new(Bytes::from(bytes))).expect("a well-formed answer")
        }
        Err(_) => net::refusal(StatusCode::NOT_FOUND, "not_found", "no such file"),
    }
}

async fn viewer_control(ws: net::ServerWs, viewer: String, control: Arc<Control>) {
    let (mut sink, mut stream) = ws.split();
    let mut changes = control.changes.subscribe();
    let now = control.holder.lock().expect("control").clone();
    let _ = sink.send(Message::text(json!({ "type": "control", "holder": now }).to_string())).await;
    // bounded by the socket
    loop {
        tokio::select! {
            m = stream.next() => {
                let Some(Ok(m)) = m else { break };
                let Message::Text(t) = m else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { continue };
                let mut holder = control.holder.lock().expect("control");
                let changed = match v["type"].as_str() {
                    Some("take") => {
                        *holder = Some(viewer.clone());
                        true
                    }
                    Some("give") if holder.as_deref() == Some(viewer.as_str()) => {
                        *holder = None;
                        true
                    }
                    _ => false,
                };
                if changed {
                    crate::ev!("screen.control", { "holder": holder.clone() });
                    let _ = control.changes.send(holder.clone());
                }
            }
            c = changes.recv() => {
                let Ok(h) = c else { continue };
                if sink.send(Message::text(json!({ "type": "control", "holder": h }).to_string())).await.is_err() {
                    break;
                }
            }
        }
    }
    // A viewer that leaves gives control back.
    let mut holder = control.holder.lock().expect("control");
    if holder.as_deref() == Some(viewer.as_str()) {
        *holder = None;
        let _ = control.changes.send(None);
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

async fn dial(target: &Target) -> std::io::Result<Box<dyn Stream>> {
    match target {
        Target::Unix(p) => Ok(Box::new(tokio::net::UnixStream::connect(p).await?)),
        Target::Tcp(a) => Ok(Box::new(tokio::net::TcpStream::connect(a.as_str()).await?)),
    }
}

/// The display, started if it is down (at most once a `START_AGAIN_MS`);
/// then its socket.
async fn open(target: &Target, cfg: &ScreenConfig, control: &Control) -> Option<Box<dyn Stream>> {
    if let Ok(s) = dial(target).await {
        return Some(s);
    }
    let start = {
        let mut started = control.started.lock().expect("started");
        let now = Instant::now();
        let start = may_start(*started, now);
        if start {
            *started = Some(now);
        }
        start
    };
    if start {
        if let Some(cmd) = cfg.start.as_ref().filter(|c| !c.is_empty()) {
            crate::ev!("screen.starting", { "cmd": cmd[0] });
            let _ = tokio::process::Command::new(&cmd[0]).args(&cmd[1..]).kill_on_drop(false).spawn();
        }
    }
    // bounded: about 15 s of tries, while the display comes up
    for _ in 0..75 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Ok(s) = dial(target).await {
            return Some(s);
        }
    }
    None
}

async fn viewer_rfb(ws: net::ServerWs, target: Target, viewer: String, cfg: Arc<ScreenConfig>, control: Arc<Control>) {
    let Some(rfb) = open(&target, &cfg, &control).await else {
        let (mut sink, _) = ws.split();
        let _ = sink.send(Message::Close(None)).await;
        crate::ev!("screen.down");
        return;
    };
    crate::ev!("screen.viewer", { "viewer": viewer });
    let (mut rfb_read, mut rfb_write) = tokio::io::split(rfb);
    let (mut sink, mut stream) = ws.split();
    let down = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        // bounded by the RFB socket
        loop {
            match rfb_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sink.send(Message::binary(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    });
    let mut filter = InputGate::default();
    // bounded by the viewer's socket
    while let Some(Ok(m)) = stream.next().await {
        let Message::Binary(b) = m else { continue };
        let holds = control.holder.lock().expect("control").as_deref() == Some(viewer.as_str());
        let Ok(out) = filter.push(&b, holds) else {
            crate::ev!("screen.refused", { "why": "an RFB message this screen does not know" });
            break;
        };
        if !out.is_empty() && rfb_write.write_all(&out).await.is_err() {
            break;
        }
    }
    down.abort();
}

/// The RFB client stream, message by message: input passes only while the
/// viewer holds control. RFB 3.8 with no authentication or VNC auth.
#[derive(Debug, Default)]
pub struct InputGate {
    buf: Vec<u8>,
    stage: Stage,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Stage {
    #[default]
    Version,
    Security,
    Auth,
    Init,
    Messages,
}

/// A client message the gate cannot size: the stream can no longer be
/// followed, so the viewer is closed.
#[derive(Debug, PartialEq)]
pub struct Unknown(pub u8);

impl InputGate {
    /// The bytes to forward for `bytes` from the viewer.
    pub fn push(&mut self, bytes: &[u8], holds_control: bool) -> Result<Vec<u8>, Unknown> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        // bounded by the buffer: each pass consumes a whole message or stops
        loop {
            let need = match self.stage {
                Stage::Version => 12,
                Stage::Security => 1,
                Stage::Auth => 16,
                Stage::Init => 1,
                Stage::Messages => match message_len(&self.buf)? {
                    Some(n) => n,
                    None => break,
                },
            };
            if self.buf.len() < need {
                break;
            }
            let msg: Vec<u8> = self.buf.drain(..need).collect();
            let input = self.stage == Stage::Messages && is_input(&msg);
            self.stage = match self.stage {
                Stage::Version => Stage::Security,
                // VNC authentication (2) answers a 16-byte challenge.
                Stage::Security if msg[0] == 2 => Stage::Auth,
                Stage::Security | Stage::Auth => Stage::Init,
                Stage::Init | Stage::Messages => Stage::Messages,
            };
            if !input || holds_control {
                out.extend_from_slice(&msg);
            }
        }
        Ok(out)
    }
}

fn is_input(msg: &[u8]) -> bool {
    match msg[0] {
        4..=6 => true,
        // QEMU's extended key event
        255 => msg.get(1) == Some(&0),
        _ => false,
    }
}

/// A client message's length, once enough of it is here to know it.
fn message_len(b: &[u8]) -> Result<Option<usize>, Unknown> {
    let Some(&t) = b.first() else { return Ok(None) };
    let at = |i: usize| b.get(i).copied();
    let u16_at = |i: usize| Some(u16::from_be_bytes([at(i)?, at(i + 1)?]) as usize);
    let u32_at = |i: usize| Some(u32::from_be_bytes([at(i)?, at(i + 1)?, at(i + 2)?, at(i + 3)?]) as usize);
    Ok(match t {
        0 => Some(20),
        2 => u16_at(2).map(|n| 4 + 4 * n),
        3 => Some(10),
        4 => Some(8),
        5 => Some(6),
        6 => u32_at(4).map(|n| 8 + n.min(1 << 20)),
        150 => Some(10),
        248 => at(8).map(|n| 9 + n as usize),
        250 => Some(4),
        251 => at(6).map(|n| 8 + 16 * n as usize),
        255 => match at(1) {
            None => None,
            Some(0) => Some(12),
            Some(other) => return Err(Unknown(other)),
        },
        other => return Err(Unknown(other)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Vec<u8> {
        let mut h = b"RFB 003.008\n".to_vec();
        h.push(1); // security: none
        h.push(1); // ClientInit: shared
        h
    }

    #[test]
    fn input_passes_only_with_control() {
        let mut g = InputGate::default();
        assert_eq!(g.push(&handshake(), false).unwrap(), handshake(), "the handshake always passes");
        let update = [3u8, 0, 0, 0, 0, 0, 0, 10, 0, 10];
        let key = [4u8, 1, 0, 0, 0, 0, 0, 0x61];
        let pointer = [5u8, 1, 0, 10, 0, 10];
        let mut both = update.to_vec();
        both.extend_from_slice(&key);
        both.extend_from_slice(&pointer);
        assert_eq!(g.push(&both, false).unwrap(), update.to_vec(), "a viewer without control sends no input");
        assert_eq!(g.push(&both, true).unwrap(), both, "the holder's input passes");
    }

    #[test]
    fn messages_split_across_frames() {
        let mut g = InputGate::default();
        g.push(&handshake(), true).unwrap();
        let enc = [2u8, 0, 0, 2, 0, 0, 0, 7, 0xff, 0xff, 0xff, 0x21];
        assert!(g.push(&enc[..3], true).unwrap().is_empty(), "not whole yet");
        assert_eq!(g.push(&enc[3..], true).unwrap(), enc.to_vec());
        let cut = [6u8, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c'];
        assert!(g.push(&cut[..9], false).unwrap().is_empty());
        assert!(g.push(&cut[9..], false).unwrap().is_empty(), "clipboard is input");
        assert_eq!(g.push(&[9], false), Err(Unknown(9)), "an unknown message closes the viewer");
    }

    /// A display that stays down is started again, at most once a minute:
    /// a viewer before the image had an agent, or after its first agent
    /// changed, is not the screen's last word; a burst of viewers is one
    /// start.
    #[test]
    fn a_display_down_is_started_again_once_a_minute() {
        let t = Instant::now();
        assert!(may_start(None, t), "never started: start it");
        assert!(!may_start(Some(t), t), "just started: wait for it");
        assert!(!may_start(Some(t), t + Duration::from_millis(START_AGAIN_MS - 1)));
        assert!(may_start(Some(t), t + Duration::from_millis(START_AGAIN_MS)), "still down a minute on: start it again");
        assert!(!may_start(Some(t + Duration::from_secs(5)), t), "a clock read before the last start never starts twice");
    }

    #[test]
    fn vnc_auth_and_targets() {
        let mut g = InputGate::default();
        let mut h = b"RFB 003.008\n".to_vec();
        h.push(2);
        h.extend_from_slice(&[7u8; 16]);
        h.push(0);
        h.extend_from_slice(&[5u8, 0, 0, 1, 0, 1]);
        assert_eq!(g.push(&h, false).unwrap(), h[..h.len() - 6].to_vec());
        assert_eq!(Target::parse("unix:/a/rfb.sock").unwrap(), Target::Unix("/a/rfb.sock".into()));
        assert_eq!(Target::parse("tcp:127.0.0.1:5901").unwrap(), Target::Tcp("127.0.0.1:5901".into()));
        assert!(Target::parse("/a").is_err());
    }
}
