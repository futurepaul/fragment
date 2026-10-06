//! A computer's screen (decision 11; docs/computers.md, Ports): a page on
//! its port (6080 by convention), and, when the image has a display, a
//! WebSocket onto its RFB server with Take over / Give back kept here:
//!
//! - `GET /` and the page's files, from a directory;
//! - `GET /websockify?viewer=<id>`: the RFB stream, bytes both ways. Every
//!   viewer sees the screen; input (keys, pointer, clipboard, a resize)
//!   reaches it only from the viewer holding control. The RFB client stream
//!   is parsed message by message (noVNC 1.7.0's, its extensions included:
//!   `InputGate`), so input is dropped, never half-sent;
//! - `GET /control?viewer=<id>`: a WebSocket of `{type: "take"}` and
//!   `{type: "give"}` from the page, answered to every viewer with
//!   `{type: "control", holder: <viewer>|null}`, the first at once. It
//!   answers on an image with no display too (the stub's), so the
//!   platform's lanes open a socket through a computer's port.
//!
//! The display starts lazily: a viewer runs `start` when the RFB socket
//! does not answer, at most once a minute while it stays down (S3b: the
//! dashboard and screen lazy), and at once when it answered since the last
//! start (it stopped or restarted under its viewers, whose streams end with
//! it: the screen's page opens its stream again on its own).

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

/// The display's starts, as its viewers see them.
#[derive(Debug, Default, Clone, Copy)]
struct Starts {
    /// When a viewer last ran the start.
    last: Option<Instant>,
    /// Whether the display answered a viewer since.
    up_since: bool,
}

impl Starts {
    /// Whether a viewer that finds the display down runs its start: never
    /// started, up since the last start (so it went down since: a stop, a
    /// restart, a crash), or still down a `START_AGAIN_MS` on.
    fn may_start(self, now: Instant) -> bool {
        self.up_since || self.last.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(START_AGAIN_MS))
    }
}

/// Who holds control, shared by every viewer; and the display's starts.
struct Control {
    holder: Mutex<Option<String>>,
    changes: broadcast::Sender<Option<String>>,
    starts: Mutex<Starts>,
}

pub async fn serve(cfg: ScreenConfig, stop: watch::Receiver<bool>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.map_err(|e| format!("screen listen {}: {e}", cfg.listen))?;
    crate::ev!("screen.listening", { "listen": cfg.listen.to_string(), "rfb": cfg.target.is_some() });
    let (changes, _) = broadcast::channel(16);
    let control = Arc::new(Control { holder: Mutex::new(None), changes, starts: Mutex::new(Starts::default()) });
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
            // the control socket answers on any image (the stub's has no
            // display): the platform's lanes open it through a computer's port
            let rfb = path == "/websockify";
            let target = cfg.target.clone();
            if rfb && target.is_none() {
                return net::refusal(StatusCode::NOT_FOUND, "not_found", "this computer has no display");
            }
            let Some(viewer) = viewer_of(&req) else { return net::refusal(StatusCode::BAD_REQUEST, "invalid", "?viewer=<id>") };
            let Some((response, socket)) = net::accept_ws(&mut req) else { return net::refusal(StatusCode::BAD_REQUEST, "invalid", "a WebSocket") };
            tokio::spawn(async move {
                let Some(ws) = socket.await else { return };
                match target {
                    Some(target) if rfb => viewer_rfb(ws, target, viewer, cfg, control).await,
                    _ => viewer_control(ws, viewer, control).await,
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

/// The display, started if it is down (`Starts::may_start`); then its
/// socket.
async fn open(target: &Target, cfg: &ScreenConfig, control: &Control) -> Option<Box<dyn Stream>> {
    let up = || control.starts.lock().expect("starts").up_since = true;
    if let Ok(s) = dial(target).await {
        up();
        return Some(s);
    }
    let start = {
        let mut starts = control.starts.lock().expect("starts");
        let now = Instant::now();
        let start = starts.may_start(now);
        if start {
            *starts = Starts { last: Some(now), up_since: false };
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
            up();
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
        let out = match filter.push(&b, holds) {
            Ok(out) => out,
            Err(e) => {
                crate::ev!("screen.refused", { "why": e.why() });
                break;
            }
        };
        if !out.is_empty() && rfb_write.write_all(&out).await.is_err() {
            break;
        }
    }
    down.abort();
}

/// The RFB client stream, message by message: input passes only while the
/// viewer holds control. RFB 3.8 with no authentication or VNC auth, and
/// every message noVNC 1.7.0 (the screen page's) sends, its extensions
/// included: TigerVNC's extended clipboard (a negative length), the
/// extended pointer event (its marker bit), QEMU's extended key event.
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

/// A client message the gate cannot follow, so the viewer is closed.
#[derive(Debug, PartialEq)]
pub enum Unframed {
    /// A message type the gate cannot size.
    Unknown(u8),
    /// A clipboard longer than `CUT_TEXT_MAX`, as Xvnc would refuse it.
    TooLong(usize),
}

impl Unframed {
    fn why(&self) -> String {
        match self {
            Unframed::Unknown(t) => format!("an RFB message this screen does not know ({t})"),
            Unframed::TooLong(n) => format!("a clipboard of {n} bytes, more than {CUT_TEXT_MAX}"),
        }
    }
}

/// The longest clipboard a viewer may send: Xvnc's `-MaxCutText`, as Hermes'
/// desktop launcher sets it.
pub const CUT_TEXT_MAX: usize = 256 * 1024;

impl InputGate {
    /// The bytes to forward for `bytes` from the viewer.
    pub fn push(&mut self, bytes: &[u8], holds_control: bool) -> Result<Vec<u8>, Unframed> {
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
        // keys, the pointer, the clipboard; and SetDesktopSize, which
        // resizes the agent's screen (Xvnc runs -AcceptSetDesktopSize)
        4..=6 | 251 => true,
        // QEMU's extended key event
        255 => msg.get(1) == Some(&0),
        _ => false,
    }
}

/// A client message's length, once enough of it is here to know it.
fn message_len(b: &[u8]) -> Result<Option<usize>, Unframed> {
    let Some(&t) = b.first() else { return Ok(None) };
    let at = |i: usize| b.get(i).copied();
    let u16_at = |i: usize| Some(u16::from_be_bytes([at(i)?, at(i + 1)?]) as usize);
    let i32_at = |i: usize| Some(i32::from_be_bytes([at(i)?, at(i + 1)?, at(i + 2)?, at(i + 3)?]));
    Ok(match t {
        0 => Some(20),
        2 => u16_at(2).map(|n| 4 + 4 * n),
        3 => Some(10),
        4 => Some(8),
        // its marker bit: the extended pointer event, a byte of buttons more
        5 => at(1).map(|mask| if mask & 0x80 != 0 { 7 } else { 6 }),
        // a negative length is the extended clipboard's: as many bytes follow
        6 => match i32_at(4).map(|n| n.unsigned_abs() as usize) {
            Some(n) if n > CUT_TEXT_MAX => return Err(Unframed::TooLong(n)),
            n => n.map(|n| 8 + n),
        },
        150 => Some(10),
        248 => at(8).map(|n| 9 + n as usize),
        250 => Some(4),
        251 => at(6).map(|n| 8 + 16 * n as usize),
        255 => match at(1) {
            None => None,
            Some(0) => Some(12),
            Some(other) => return Err(Unframed::Unknown(other)),
        },
        other => return Err(Unframed::Unknown(other)),
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
        assert_eq!(g.push(&[9], false), Err(Unframed::Unknown(9)), "an unknown message closes the viewer");
    }

    /// What noVNC 1.7.0 sends once Xvnc offers its extensions (Paul,
    /// 2026-10-05: "I can't remote control the desktop"): its extended
    /// clipboard caps, a ClientCutText whose length is negative, came right
    /// after the first update request, and the gate read the length as a
    /// u32 and waited for a megabyte, so no input of the viewer's, nor its
    /// next update request, ever reached the screen after Take over.
    #[test]
    fn novncs_extensions_are_followed() {
        let mut g = InputGate::default();
        g.push(&handshake(), true).unwrap();
        // extendedClipboardCaps: flags (caps, five actions; text) and text's max size
        let caps_body = [0x1fu8, 0, 0, 1, 0, 0, 0, 0];
        let mut caps = vec![6u8, 0, 0, 0];
        caps.extend_from_slice(&(-(caps_body.len() as i32)).to_be_bytes());
        caps.extend_from_slice(&caps_body);
        let update = [3u8, 1, 0, 0, 0, 0, 5, 160, 3, 132];
        let pointer = [5u8, 1, 0, 101, 0, 57];
        let extended_pointer = [5u8, 0x80, 0, 9, 0, 9, 1];
        let mut stream = caps.clone();
        stream.extend_from_slice(&update);
        stream.extend_from_slice(&pointer);
        stream.extend_from_slice(&extended_pointer);
        let mut want = caps.clone();
        want.extend_from_slice(&update);
        want.extend_from_slice(&pointer);
        want.extend_from_slice(&extended_pointer);
        assert_eq!(g.push(&stream, true).unwrap(), want, "the holder's caps, update request and pointer all pass, in order");
        let mut watched = caps;
        watched.extend_from_slice(&update);
        watched.extend_from_slice(&extended_pointer);
        assert_eq!(g.push(&watched, false).unwrap(), update.to_vec(), "a watcher's update request passes; its clipboard and pointer do not");
        // a viewer resizing the agent's screen is input
        let resize = [251u8, 0, 3, 32, 2, 88, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 32, 2, 88, 0, 0, 0, 0];
        assert!(g.push(&resize, false).unwrap().is_empty(), "a watcher does not resize the screen");
        assert_eq!(g.push(&resize, true).unwrap(), resize.to_vec());
        // a clipboard longer than Xvnc takes closes the viewer, either sign
        for len in [CUT_TEXT_MAX as i32 + 1, -(CUT_TEXT_MAX as i32) - 1] {
            let mut g = InputGate::default();
            g.push(&handshake(), true).unwrap();
            let mut long = vec![6u8, 0, 0, 0];
            long.extend_from_slice(&len.to_be_bytes());
            assert_eq!(g.push(&long, true), Err(Unframed::TooLong(CUT_TEXT_MAX + 1)));
        }
    }

    /// A display that stays down is started again, at most once a minute:
    /// a viewer before the image had an agent, or after its first agent
    /// changed, is not the screen's last word; a burst of viewers is one
    /// start.
    #[test]
    fn a_display_down_is_started_again_once_a_minute() {
        let t = Instant::now();
        let down = |last| Starts { last, up_since: false };
        assert!(down(None).may_start(t), "never started: start it");
        assert!(!down(Some(t)).may_start(t), "just started: wait for it");
        assert!(!down(Some(t)).may_start(t + Duration::from_millis(START_AGAIN_MS - 1)));
        assert!(down(Some(t)).may_start(t + Duration::from_millis(START_AGAIN_MS)), "still down a minute on: start it again");
        assert!(!down(Some(t + Duration::from_secs(5))).may_start(t), "a clock read before the last start never starts twice");
    }

    /// A display that answered since its last start and is down now was
    /// stopped or restarted under its viewers (p5, 2026-10-05: the screen
    /// showed the agent's browser only once it was reopened): the next
    /// viewer, the page opening its stream again, starts it at once, and
    /// the viewers after it wait for that start.
    #[test]
    fn a_display_that_went_down_is_started_at_once() {
        let t = Instant::now();
        let went_down = Starts { last: Some(t), up_since: true };
        assert!(went_down.may_start(t + Duration::from_secs(1)), "up since the last start: start it now, not a minute on");
        let starting = Starts { last: Some(t + Duration::from_secs(1)), up_since: false };
        assert!(!starting.may_start(t + Duration::from_secs(2)), "one start for a burst of viewers");
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
