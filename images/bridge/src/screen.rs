//! A computer's screens (decision 11; docs/computers.md, Ports): a page on
//! its port (6080 by convention), and a screen for each agent the bridge
//! runs, named by the agent's fragment (`agent=juniper.paul`): its own
//! display, when the image names one (screens.rs), and who drives it.
//!
//! - `GET /` and the page's files, from a directory; the page is opened at
//!   `/?agent=<agent fragment>` and names that agent to its sockets;
//! - `GET /websockify?viewer=<id>&agent=<fragment>`: that agent's RFB
//!   stream, bytes both ways. Every viewer sees the screen; input (keys,
//!   pointer, clipboard, a resize) reaches it only from the viewer holding
//!   control. The RFB client stream is parsed message by message (noVNC
//!   1.7.0's, its extensions included: `InputGate`), so input is dropped,
//!   never half-sent;
//! - `GET /control?viewer=<id>&agent=<fragment>`: a WebSocket of `{type:
//!   "take"}` and `{type: "give"}` from the page, answered to every viewer
//!   of that agent's screen with `{type: "control", agent, name, holder:
//!   <viewer>|null}`, the first at once. It answers on an image with no
//!   display too (the stub's), so the platform's lanes open a socket through
//!   a computer's port.
//!
//! An `agent` or `viewer` that is no name is 400. An agent this bridge does
//! not run, or one the image names no screen for when it names screens,
//! is 404, as is the RFB stream of an agent with no display: one agent's
//! page never shows another's desktop.
//!
//! Who holds control is the image runtime's own lease when the image names
//! its file (lease.rs: Hermes' Bot Desktop lease, which its `computer_use`
//! and browser tools read at every action and refuse while a person holds
//! it), else the screen's own. Take over writes the lease; Give back, or
//! the taker's control socket closing, gives it back to the agent; a
//! change by anyone else (the runtime stopping its desktop with `--force`)
//! reaches the viewers within `TICK_MS`; and input passes only while the
//! lease names its viewer, read as each input arrives. A lease a person
//! held in an earlier life of the bridge is given back to the agent the
//! first time this life sees that screen: no viewer survives a restart.
//!
//! Each display starts lazily: a viewer runs `start` with the agent's
//! fragment as its last argument when the RFB socket does not answer, at
//! most once a minute while it stays down (S3b: the dashboard and screen
//! lazy), and at once when it answered since the last start (it stopped or
//! restarted under its viewers, whose streams end with it: the screen's
//! page opens its stream again on its own). While a viewer watches, the
//! display's activity file (when the image names one) is touched every
//! `ACTIVITY_EVERY_MS`, so a desktop someone watches is never idle to the
//! image's idle stop (docs/computers.md).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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

use crate::lease::{self, Holder, Lease, LeaseError, LeaseFile};
use crate::net::{self, Body};
use crate::screens::{self, Display, Screens, Source};

/// The RFB server a screen shows.
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
    /// Where the image names each agent's display and its runtime's files
    /// (screens.rs): a file, or a directory by convention. None: no agent
    /// has a display (the stub's).
    pub screens: Option<Source>,
    /// What starts an agent's display, run with the agent's fragment as its
    /// last argument by a viewer that finds it down.
    pub start: Option<Vec<String>>,
}

/// An agent the bridge runs, as its screen names it to its viewers.
#[derive(Debug, Clone, PartialEq)]
pub struct Named {
    pub fragment: String,
    pub name: String,
}

/// The agents the bridge runs, from its driver: `None` until it has read
/// them, past the restore gate (no lease under `/data` is touched before).
pub type Agents = watch::Receiver<Option<Vec<Named>>>;

/// While a display stays down, its start is run again at most this often:
/// a start that came to nothing (the agent's profile not yet whole) is not
/// the screen's last word.
pub const START_AGAIN_MS: u64 = 60_000;
/// Each screen with a socket open is looked at this often: its lease, for a
/// change its viewers have not heard of; whether its agent still runs here.
pub const TICK_MS: u64 = 250;
/// A watched display's activity file is touched this often: well inside any
/// idle stop's bound (the image's is minutes), and one write per watched
/// screen at most this often.
pub const ACTIVITY_EVERY_MS: u64 = 10_000;
/// A socket asked for before the bridge has read its agents (the computer
/// still starting) waits this long for them.
pub const AGENTS_WAIT_MS: u64 = 30_000;
/// An agent's fragment's name, at most (`<label>.<username>`, each well
/// under 64).
pub const AGENT_MAX_BYTES: usize = 128;

const _: () = assert!(TICK_MS * 4 <= ACTIVITY_EVERY_MS, "a watched display is touched at most once every few looks");

/// Whether `a` can be an agent fragment's name (`<label>.<username>`:
/// lowercase letters, digits and `-`, one `.`). Which agent it is, if any,
/// is the bridge's agents' to say.
pub fn agent_name_ok(a: &str) -> bool {
    let chars = a.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.');
    let dots = a.bytes().filter(|b| *b == b'.').count();
    !a.is_empty() && a.len() <= AGENT_MAX_BYTES && chars && dots == 1 && !a.starts_with('.') && !a.ends_with('.')
}

fn viewer_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= lease::VIEWER_MAX_BYTES && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A socket's `viewer` and `agent`, or why not. Values are taken as they
/// are: a name needs no escape, so one that has an escape is no name.
pub fn asked(query: Option<&str>) -> Result<(String, String), String> {
    let pairs: Vec<(&str, &str)> = query.unwrap_or("").split('&').filter_map(|kv| kv.split_once('=')).collect();
    let one = |k: &str| -> Result<&str, String> {
        let mut found = pairs.iter().filter(|(n, _)| *n == k).map(|(_, v)| *v);
        match (found.next(), found.next()) {
            (Some(v), None) => Ok(v),
            (None, _) => Err(format!("?{k}= names no one")),
            (Some(_), Some(_)) => Err(format!("?{k}= is named twice")),
        }
    };
    let (viewer, agent) = (one("viewer")?, one("agent")?);
    if !viewer_ok(viewer) {
        return Err(format!("?viewer= is letters, digits, - and _, at most {} of them", lease::VIEWER_MAX_BYTES));
    }
    if !agent_name_ok(agent) {
        return Err("?agent= names an agent fragment (<label>.<username>)".into());
    }
    Ok((viewer.to_string(), agent.to_string()))
}

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

/// Who holds a screen: the runtime's lease file, or the screen's own.
enum Control {
    File(LeaseFile),
    Own(Mutex<Lease>),
}

impl Control {
    fn holder(&self) -> Holder {
        match self {
            Control::File(f) => f.read().holder,
            Control::Own(l) => l.lock().expect("a lease").holder.clone(),
        }
    }

    /// The holder after `change`, as the lease's rules have it (one
    /// semantics for both: lease.rs), and whether it changed.
    fn change(&self, change: impl FnOnce(&Lease) -> Option<Lease>) -> Result<(Holder, bool), LeaseError> {
        match self {
            Control::File(f) => f.change(change).map(|(l, changed)| (l.holder, changed)),
            Control::Own(l) => {
                let mut l = l.lock().expect("a lease");
                let next = change(&l);
                let changed = next.is_some();
                if let Some(next) = next {
                    *l = next;
                }
                Ok((l.holder.clone(), changed))
            }
        }
    }
}

/// What a screen's sockets are told.
#[derive(Debug, Clone)]
enum Word {
    Holder(Holder),
    /// Its agent no longer runs here: its sockets close.
    Gone,
}

/// One agent's screen.
struct Screen {
    agent: String,
    display: Option<Display>,
    control: Control,
    words: broadcast::Sender<Word>,
    starts: Mutex<Starts>,
    /// Sockets open on it (control and RFB), and RFB streams among them.
    sockets: AtomicUsize,
    streams: AtomicUsize,
    /// What its viewers were told last (from its first sight: a socket's
    /// first word is who holds it then), and when its activity was touched.
    told: Mutex<Holder>,
    touched: Mutex<Option<Instant>>,
}

impl Screen {
    /// A screen at its first sight in this life: a lease a person held in
    /// an earlier one is given back to the agent (no viewer survives).
    fn new(agent: &str, display: Option<Display>) -> Screen {
        let control = match display.as_ref().and_then(|d| d.lease.clone()) {
            Some(path) => {
                let file = LeaseFile::new(path);
                match file.change(|l| lease::give(l, None, lease::now_s())) {
                    Ok(_) => {}
                    // its runtime has not made the desktop's directory: no lease yet
                    Err(LeaseError::NoDirectory(_)) => {}
                    Err(e) => crate::ev!("screen.lease_failed", { "agent": agent, "why": e.to_string() }),
                }
                Control::File(file)
            }
            None => Control::Own(Mutex::new(Lease::default())),
        };
        let (words, _) = broadcast::channel(16);
        let told = Mutex::new(control.holder());
        Screen { agent: agent.to_string(), display, control, words, starts: Mutex::new(Starts::default()), sockets: AtomicUsize::new(0), streams: AtomicUsize::new(0), told, touched: Mutex::new(None) }
    }

    /// Tells its viewers `holder` when it is news, or `always`: whether it
    /// told them.
    fn tell(&self, holder: Holder, always: bool) -> bool {
        let mut told = self.told.lock().expect("told");
        let news = *told != holder;
        if news || always {
            *told = holder.clone();
            let _ = self.words.send(Word::Holder(holder));
        }
        news || always
    }

    /// Changes who holds it, and tells its viewers; a lease that could not
    /// be changed is said, and its viewers told who holds it still.
    fn change(&self, why: &str, change: impl FnOnce(&Lease) -> Option<Lease>) {
        match self.control.change(change) {
            Ok((holder, changed)) => {
                if changed {
                    crate::ev!("screen.control", { "agent": self.agent, "why": why, "holder": holder.said() });
                }
                self.tell(holder, false);
            }
            Err(e) => {
                crate::ev!("screen.lease_failed", { "agent": self.agent, "why": e.to_string() });
                self.tell(self.control.holder(), true);
            }
        }
    }

    /// Its display was used now (a viewer, a take over), as its runtime
    /// counts use: its activity file's time, when the file is there (the
    /// runtime makes it as the display starts, and removes it as it stops).
    fn touch(&self) {
        *self.touched.lock().expect("touched") = Some(Instant::now());
        let Some(path) = self.display.as_ref().and_then(|d| d.activity.as_ref()) else { return };
        let now = std::time::SystemTime::now();
        let touched = std::fs::OpenOptions::new().write(true).open(path).and_then(|f| f.set_times(std::fs::FileTimes::new().set_accessed(now).set_modified(now)));
        if let Err(e) = touched {
            if e.kind() != std::io::ErrorKind::NotFound {
                crate::ev!("screen.touch_failed", { "agent": self.agent, "error": e.to_string() });
            }
        }
    }

    fn touch_due(&self, now: Instant) -> bool {
        self.touched.lock().expect("touched").is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(ACTIVITY_EVERY_MS))
    }
}

/// A socket counted on its screen while it lives.
struct Open {
    screen: Arc<Screen>,
    stream: bool,
}

impl Open {
    fn new(screen: &Arc<Screen>, stream: bool) -> Open {
        screen.sockets.fetch_add(1, Ordering::SeqCst);
        if stream {
            screen.streams.fetch_add(1, Ordering::SeqCst);
        }
        Open { screen: screen.clone(), stream }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let before = self.screen.sockets.fetch_sub(1, Ordering::SeqCst);
        assert!(before > 0, "a socket is counted once");
        if self.stream {
            let before = self.screen.streams.fetch_sub(1, Ordering::SeqCst);
            assert!(before > 0, "a stream is counted once");
        }
    }
}

/// Where the screens are named, as the screen reads them.
enum Naming {
    /// No agent has a display.
    None,
    /// The image's screens file, read again when it changes.
    File(Mutex<Screens>),
    /// Every agent has a display, in a directory of its own here.
    Dir(PathBuf),
}

struct Shared {
    cfg: ScreenConfig,
    agents: Agents,
    naming: Naming,
    by_agent: Mutex<HashMap<String, Arc<Screen>>>,
}

impl Shared {
    /// What the image names for `agent`: `Some(None)` when it names no
    /// screens at all (every agent has one, with no display), `None` when it
    /// names screens and none for this agent.
    fn display_of(&self, agent: &str) -> Option<Option<Display>> {
        match &self.naming {
            Naming::None => Some(None),
            Naming::File(file) => {
                let mut file = file.lock().expect("the screens file");
                file.refresh();
                file.get(agent).cloned().map(Some)
            }
            Naming::Dir(dir) => Some(Some(screens::in_dir(dir, agent))),
        }
    }

    /// The screen of `agent`, made at its first sight (or anew, once the
    /// image names another display for it): none when the image names none
    /// for it.
    fn screen(&self, agent: &str) -> Option<Arc<Screen>> {
        let display = self.display_of(agent)?;
        let mut by = self.by_agent.lock().expect("the screens");
        if let Some(s) = by.get(agent).filter(|s| s.display == display) {
            return Some(s.clone());
        }
        let s = Arc::new(Screen::new(agent, display));
        by.insert(agent.to_string(), s.clone());
        Some(s)
    }
}

pub async fn serve(cfg: ScreenConfig, agents: Agents, stop: watch::Receiver<bool>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.map_err(|e| format!("screen listen {}: {e}", cfg.listen))?;
    crate::ev!("screen.listening", { "listen": cfg.listen.to_string(), "screens": format!("{:?}", cfg.screens) });
    let naming = match cfg.screens.clone() {
        None => Naming::None,
        Some(Source::File(p)) => Naming::File(Mutex::new(Screens::new(p))),
        Some(Source::Dir(d)) => Naming::Dir(d),
    };
    let shared = Arc::new(Shared { cfg, agents, naming, by_agent: Mutex::new(HashMap::new()) });
    tokio::spawn(tick(shared.clone(), stop.clone()));
    let handler = move |req: Request<Incoming>, _peer: SocketAddr| {
        let shared = shared.clone();
        async move { handle(req, shared).await }
    };
    net::serve(listener, handler, stop).await;
    Ok(())
}

/// Every `TICK_MS`, once the bridge has its agents: each screen the image
/// names seen (its first sight gives back an earlier life's lease); and
/// each screen with a socket open told of a change of its lease, touched
/// while watched, or closed once its agent no longer runs here.
async fn tick(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut every = tokio::time::interval(Duration::from_millis(TICK_MS));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // bounded by the bridge's life: one look per TICK_MS, ended by `stop`
    loop {
        tokio::select! {
            _ = net::stopped(&mut stop) => return,
            _ = every.tick() => {}
        }
        let Some(running) = shared.agents.borrow().clone() else { continue };
        // the agents the image names a screen for: by convention, all of them
        let named: Option<Vec<String>> = match &shared.naming {
            Naming::File(file) => {
                let mut file = file.lock().expect("the screens file");
                file.refresh();
                Some(file.agents().cloned().collect())
            }
            Naming::Dir(_) => Some(running.iter().map(|a| a.fragment.clone()).collect()),
            Naming::None => None,
        };
        for agent in named.iter().flatten() {
            let _ = shared.screen(agent);
        }
        let here = |agent: &str| running.iter().any(|a| a.fragment == agent) && named.as_ref().is_none_or(|n| n.iter().any(|n| n == agent));
        let open: Vec<Arc<Screen>> = {
            let mut by = shared.by_agent.lock().expect("the screens");
            // a screen of an agent gone, with no socket left, is let go
            by.retain(|agent, s| here(agent) || s.sockets.load(Ordering::SeqCst) > 0);
            by.values().filter(|s| s.sockets.load(Ordering::SeqCst) > 0).cloned().collect()
        };
        let now = Instant::now();
        for s in open {
            if !here(&s.agent) {
                let _ = s.words.send(Word::Gone);
                continue;
            }
            s.tell(s.control.holder(), false);
            if s.streams.load(Ordering::SeqCst) > 0 && s.touch_due(now) {
                s.touch();
            }
        }
    }
}

/// The agents, once the bridge has read them (within `AGENTS_WAIT_MS`).
async fn running(agents: &Agents) -> Option<Vec<Named>> {
    let mut rx = agents.clone();
    let read = tokio::time::timeout(Duration::from_millis(AGENTS_WAIT_MS), rx.wait_for(Option::is_some)).await;
    match read {
        Ok(Ok(agents)) => agents.clone(),
        _ => None,
    }
}

async fn handle(mut req: Request<Incoming>, shared: Arc<Shared>) -> Response<Body> {
    let path = req.uri().path().to_string();
    match path.as_str() {
        "/websockify" | "/control" => {
            let rfb = path == "/websockify";
            let (viewer, agent) = match asked(req.uri().query()) {
                Ok(asked) => asked,
                Err(why) => return net::refusal(StatusCode::BAD_REQUEST, "invalid", &why),
            };
            let Some(running) = running(&shared.agents).await else {
                return net::refusal(StatusCode::SERVICE_UNAVAILABLE, "starting", "this computer is starting: open the screen again in a moment");
            };
            let Some(named) = running.into_iter().find(|a| a.fragment == agent) else {
                return net::refusal(StatusCode::NOT_FOUND, "not_found", &format!("no agent {agent} runs on this computer"));
            };
            let Some(screen) = shared.screen(&agent) else {
                return net::refusal(StatusCode::NOT_FOUND, "not_found", &format!("this computer names no screen for {agent}"));
            };
            if rfb && screen.display.is_none() {
                return net::refusal(StatusCode::NOT_FOUND, "not_found", &format!("{agent} has no display on this computer"));
            }
            let Some((response, socket)) = net::accept_ws(&mut req) else { return net::refusal(StatusCode::BAD_REQUEST, "invalid", "a WebSocket") };
            tokio::spawn(async move {
                let Some(ws) = socket.await else { return };
                match rfb {
                    true => viewer_rfb(ws, viewer, screen, shared).await,
                    false => viewer_control(ws, viewer, screen, named).await,
                }
            });
            response
        }
        _ => file(&shared.cfg.dir, &path).await,
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

fn said(named: &Named, holder: &Holder) -> Message {
    Message::text(json!({ "type": "control", "agent": named.fragment, "name": named.name, "holder": holder.said() }).to_string())
}

async fn viewer_control(ws: net::ServerWs, viewer: String, screen: Arc<Screen>, named: Named) {
    let _open = Open::new(&screen, false);
    let (mut sink, mut stream) = ws.split();
    let mut words = screen.words.subscribe();
    // its first word: who holds it now, told to every viewer when that is
    // news to them (a change the next look would have told), else to it alone
    let now = screen.control.holder();
    if !screen.tell(now.clone(), false) {
        let _ = sink.send(said(&named, &now)).await;
    }
    // bounded by the socket
    loop {
        tokio::select! {
            m = stream.next() => {
                let Some(Ok(m)) = m else { break };
                let Message::Text(t) = m else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { continue };
                match v["type"].as_str() {
                    Some("take") => {
                        // a take over is a use of the display, as its runtime counts one
                        screen.change("take", |l| lease::take(l, &viewer, lease::now_s()));
                        screen.touch();
                    }
                    Some("give") => screen.change("give", |l| lease::give(l, Some(&viewer), lease::now_s())),
                    _ => {}
                }
            }
            w = words.recv() => match w {
                Ok(Word::Holder(h)) => {
                    if sink.send(said(&named, &h)).await.is_err() {
                        break;
                    }
                }
                Ok(Word::Gone) => break,
                // a word missed: what holds it now is what the viewer needs
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if sink.send(said(&named, &screen.control.holder())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    let _ = sink.close().await;
    // A viewer that leaves gives control back (only the holder's leaving does).
    screen.change("left", |l| lease::give(l, Some(&viewer), lease::now_s()));
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
async fn open(target: &Target, screen: &Screen, start: Option<&Vec<String>>) -> Option<Box<dyn Stream>> {
    let up = || screen.starts.lock().expect("starts").up_since = true;
    if let Ok(s) = dial(target).await {
        up();
        return Some(s);
    }
    let starting = {
        let mut starts = screen.starts.lock().expect("starts");
        let now = Instant::now();
        let start = starts.may_start(now);
        if start {
            *starts = Starts { last: Some(now), up_since: false };
        }
        start
    };
    if starting {
        if let Some(cmd) = start.filter(|c| !c.is_empty()) {
            crate::ev!("screen.starting", { "agent": screen.agent, "cmd": cmd[0] });
            let _ = tokio::process::Command::new(&cmd[0]).args(&cmd[1..]).arg(&screen.agent).kill_on_drop(false).spawn();
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

async fn viewer_rfb(ws: net::ServerWs, viewer: String, screen: Arc<Screen>, shared: Arc<Shared>) {
    let target = screen.display.as_ref().map(|d| d.rfb.clone()).expect("a stream is opened only onto a display");
    let _open = Open::new(&screen, true);
    let Some(rfb) = open(&target, &screen, shared.cfg.start.as_ref()).await else {
        let (mut sink, _) = ws.split();
        let _ = sink.send(Message::Close(None)).await;
        crate::ev!("screen.down", { "agent": screen.agent });
        return;
    };
    crate::ev!("screen.viewer", { "agent": screen.agent, "viewer": viewer });
    screen.touch();
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
    let mut gate = InputGate::default();
    let mut words = screen.words.subscribe();
    // bounded by the viewer's socket
    loop {
        tokio::select! {
            m = stream.next() => {
                let Some(Ok(m)) = m else { break };
                let Message::Binary(b) = m else { continue };
                // the lease as each input arrives: a person who gave back, or
                // was given back by the runtime, sends no more
                let out = match gate.push(&b, || screen.control.holder().is(&viewer)) {
                    Ok(out) => out,
                    Err(e) => {
                        crate::ev!("screen.refused", { "agent": screen.agent, "why": e.why() });
                        break;
                    }
                };
                if !out.is_empty() && rfb_write.write_all(&out).await.is_err() {
                    break;
                }
            }
            w = words.recv() => {
                if matches!(w, Ok(Word::Gone) | Err(broadcast::error::RecvError::Closed)) {
                    break;
                }
            }
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

/// The longest clipboard a viewer may send: Xvnc's `-MaxCutText` as a
/// desktop sets it.
pub const CUT_TEXT_MAX: usize = 256 * 1024;

impl InputGate {
    /// The bytes to forward for `bytes` from the viewer. `holds_control` is
    /// asked once, and only when `bytes` hold input: whether the viewer
    /// holds control now.
    pub fn push(&mut self, bytes: &[u8], holds_control: impl FnOnce() -> bool) -> Result<Vec<u8>, Unframed> {
        self.buf.extend_from_slice(bytes);
        let mut holds_control = Some(holds_control);
        let mut holds: Option<bool> = None;
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
            let passes = !input || *holds.get_or_insert_with(|| holds_control.take().expect("asked once")());
            if passes {
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
        assert_eq!(g.push(&handshake(), || panic!("the handshake is no input")).unwrap(), handshake(), "the handshake always passes");
        let update = [3u8, 0, 0, 0, 0, 0, 0, 10, 0, 10];
        let key = [4u8, 1, 0, 0, 0, 0, 0, 0x61];
        let pointer = [5u8, 1, 0, 10, 0, 10];
        let mut both = update.to_vec();
        both.extend_from_slice(&key);
        both.extend_from_slice(&pointer);
        assert_eq!(g.push(&both, || false).unwrap(), update.to_vec(), "a viewer without control sends no input");
        assert_eq!(g.push(&both, || true).unwrap(), both, "the holder's input passes");
        assert_eq!(g.push(&update, || panic!("asked only when there is input")).unwrap(), update.to_vec());
    }

    #[test]
    fn messages_split_across_frames() {
        let mut g = InputGate::default();
        g.push(&handshake(), || true).unwrap();
        let enc = [2u8, 0, 0, 2, 0, 0, 0, 7, 0xff, 0xff, 0xff, 0x21];
        assert!(g.push(&enc[..3], || true).unwrap().is_empty(), "not whole yet");
        assert_eq!(g.push(&enc[3..], || true).unwrap(), enc.to_vec());
        let cut = [6u8, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c'];
        assert!(g.push(&cut[..9], || false).unwrap().is_empty());
        assert!(g.push(&cut[9..], || false).unwrap().is_empty(), "clipboard is input");
        assert_eq!(g.push(&[9], || false), Err(Unframed::Unknown(9)), "an unknown message closes the viewer");
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
        g.push(&handshake(), || true).unwrap();
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
        assert_eq!(g.push(&stream, || true).unwrap(), want, "the holder's caps, update request and pointer all pass, in order");
        let mut watched = caps;
        watched.extend_from_slice(&update);
        watched.extend_from_slice(&extended_pointer);
        assert_eq!(g.push(&watched, || false).unwrap(), update.to_vec(), "a watcher's update request passes; its clipboard and pointer do not");
        // a viewer resizing the agent's screen is input
        let resize = [251u8, 0, 3, 32, 2, 88, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 32, 2, 88, 0, 0, 0, 0];
        assert!(g.push(&resize, || false).unwrap().is_empty(), "a watcher does not resize the screen");
        assert_eq!(g.push(&resize, || true).unwrap(), resize.to_vec());
        // a clipboard longer than Xvnc takes closes the viewer, either sign
        for len in [CUT_TEXT_MAX as i32 + 1, -(CUT_TEXT_MAX as i32) - 1] {
            let mut g = InputGate::default();
            g.push(&handshake(), || true).unwrap();
            let mut long = vec![6u8, 0, 0, 0];
            long.extend_from_slice(&len.to_be_bytes());
            assert_eq!(g.push(&long, || true), Err(Unframed::TooLong(CUT_TEXT_MAX + 1)));
        }
    }

    /// A display that stays down is started again, at most once a minute:
    /// a viewer before the agent's profile was whole is not the screen's
    /// last word; a burst of viewers is one start.
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
        assert_eq!(g.push(&h, || false).unwrap(), h[..h.len() - 6].to_vec());
        assert_eq!(Target::parse("unix:/a/rfb.sock").unwrap(), Target::Unix("/a/rfb.sock".into()));
        assert_eq!(Target::parse("tcp:127.0.0.1:5901").unwrap(), Target::Tcp("127.0.0.1:5901".into()));
        assert!(Target::parse("/a").is_err());
    }

    /// Valid: a socket names its viewer and its agent. Invalid: either
    /// missing, named twice, escaped or out of shape, which is no name.
    #[test]
    fn a_socket_names_its_viewer_and_agent() {
        assert_eq!(asked(Some("viewer=v_1-a&agent=juniper.paul")), Ok(("v_1-a".into(), "juniper.paul".into())));
        assert_eq!(asked(Some("agent=fred-2.ann&viewer=x&other=1")), Ok(("x".into(), "fred-2.ann".into())), "in any order, among others");
        for bad in [
            None,
            Some("viewer=v"),
            Some("agent=juniper.paul"),
            Some("viewer=&agent=juniper.paul"),
            Some("viewer=v&agent="),
            Some("viewer=v&agent=juniper"),
            Some("viewer=v&agent=juniper.paul.x"),
            Some("viewer=v&agent=Juniper.paul"),
            Some("viewer=v&agent=juniper%2Epaul"),
            Some("viewer=v&agent=../x.paul"),
            Some("viewer=v&agent=juniper.paul&agent=fred.paul"),
            Some("viewer=v v&agent=juniper.paul"),
        ] {
            assert!(asked(bad).is_err(), "{bad:?}");
        }
        assert!(asked(Some(&format!("viewer={}&agent=a.b", "v".repeat(lease::VIEWER_MAX_BYTES + 1)))).is_err());
        assert!(asked(Some(&format!("viewer=v&agent={}.b", "a".repeat(AGENT_MAX_BYTES)))).is_err());
    }

    /// Take over on a screen with no lease file of its runtime's follows the
    /// lease's rules all the same: the last to take it holds it, only the
    /// holder gives it back, and taking or giving again changes nothing.
    #[test]
    fn a_screens_own_lease_follows_the_leases_rules() {
        let s = Screen::new("juniper.paul", None);
        assert_eq!(s.control.holder(), Holder::Agent);
        s.change("take", |l| lease::take(l, "v1", 1.0));
        assert!(s.control.holder().is("v1"));
        s.change("give", |l| lease::give(l, Some("v2"), 2.0));
        assert!(s.control.holder().is("v1"), "only the holder gives back");
        s.change("take", |l| lease::take(l, "v2", 3.0));
        s.change("left", |l| lease::give(l, Some("v1"), 4.0));
        assert!(s.control.holder().is("v2"), "the first viewer leaving takes nothing from the second");
        s.change("give", |l| lease::give(l, Some("v2"), 5.0));
        assert_eq!(s.control.holder(), Holder::Agent);
    }

    /// Restart: a screen first seen in a life gives back a lease a person
    /// held in the last (no viewer of it survives), and leaves the agent's
    /// alone; its directory not there yet, nothing is made.
    #[test]
    fn a_lease_held_in_an_earlier_life_is_given_back() {
        let d = std::env::temp_dir().join(format!("bridge-screen-life-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("bot-desktop")).unwrap();
        let path = d.join("bot-desktop/lease.json");
        let file = LeaseFile::new(path.clone());
        file.change(|l| lease::take(l, "gone", 1.0)).unwrap();
        assert!(file.read().holder.is("gone"));
        let display = |lease: PathBuf| Some(Display { rfb: Target::Unix(d.join("rfb.sock")), lease: Some(lease), activity: None });
        let s = Screen::new("juniper.paul", display(path.clone()));
        assert_eq!(s.control.holder(), Holder::Agent);
        assert_eq!(file.read().epoch, 2, "given back once");
        let _ = Screen::new("juniper.paul", display(path.clone()));
        assert_eq!(file.read().epoch, 2, "the agent's already: nothing written");
        let _ = Screen::new("fred.paul", display(d.join("missing/lease.json")));
        assert!(!d.join("missing").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
