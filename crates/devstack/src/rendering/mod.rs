//! Preview cards off Cloudflare (docs/self-host.md, seam 7): Browser
//! Rendering's three routes, as card.rs speaks them to the `BROWSER`
//! binding, served over the pinned chrome-headless-shell (browser.rs). The
//! cell's card path is the one it runs on Cloudflare; only where its
//! `fetch` goes differs (`FRAGMENT_BROWSER_URL`).
//!
//! - `POST /v1/devtools/browser?keep_alive=<ms>` starts a browser for the
//!   session, and answers `{"sessionId"}` once it answers CDP.
//! - `GET /v1/devtools/browser/<id>`, a WebSocket upgrade: the session's
//!   CDP, one client at a time, carried to and from the browser's pipe.
//! - `DELETE /v1/devtools/browser/<id>` stops it: `{"status": "closed"}`.
//!   A session with no client for its `keep_alive` (default a minute, at
//!   most ten), or alive for `SESSION_LIFE_MAX`, is stopped too.
//!
//! **Isolation.** A card's page is a stranger's code, so its browser
//! reaches fragments' origins and nothing else on the box:
//!
//! - Every connection it makes (http, https, WebSockets) goes through the
//!   gate (gate.rs), loopback and `*.localhost` included (`<-loopback>`),
//!   which lets through a fragment's origin alone and connects it to the
//!   one address fragments are served on. It resolves no name: a page
//!   cannot reach the engine, a model server, the sign-in provider, or
//!   the LAN, by name, by IP literal, or by rebinding.
//! - The browser resolves nothing itself (`--host-resolver-rules`, the
//!   gate's own address excepted), sends no QUIC, and no WebRTC UDP that
//!   bypasses its proxy; WebTransport fails outright with a proxy set.
//! - It is driven over `--remote-debugging-pipe`: it listens on no port,
//!   and only this service holds its pipe.
//! - Chrome's own sandbox is kept. Each session has a fresh profile and
//!   home, removed after it; a clean environment (no variable of this
//!   process's); and its own process group, killed whole.
//! - This service listens on loopback alone, unauthenticated: a local
//!   process may start sessions, which reach only what any visitor can.
//!
//! **A private CA.** Fragments on https under an operator's CA: the
//! browser trusts it as Chrome on Linux trusts a CA, from an NSS database
//! in its home, made once from the CA's PEM with NSS's `certutil`. The
//! certificates are checked by the browser itself, as a visitor's are.

pub mod gate;
pub mod ws;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub use gate::Origins;

/// Browsers at once (each a few hundred MB).
pub const SESSIONS_MAX: usize = 4;
/// A session's `keep_alive` when it names none, and the most it may
/// name: Browser Rendering's.
pub const KEEP_ALIVE_DEFAULT: Duration = Duration::from_secs(60);
pub const KEEP_ALIVE_MAX: Duration = Duration::from_secs(600);
/// No session outlives this, client or not.
pub const SESSION_LIFE_MAX: Duration = Duration::from_secs(15 * 60);
/// How long a browser has to answer its first CDP command.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// A CDP message either way is at most this (Chrome's pipe takes 100 MB;
/// a card's screenshot is under 1 MB).
pub const MESSAGE_MAX: usize = 64 << 20;
/// A request's head is at most this; it has this long to arrive.
const HEAD_MAX: u64 = 16 << 10;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections to the service at once.
const CONNECTIONS_MAX: usize = 64;
/// How often expired sessions are looked for.
const REAP_EVERY: Duration = Duration::from_millis(250);
/// The id of the command a new browser must answer before its session is
/// handed out (a client's own ids count up from 1).
const PROBE_ID: u64 = 2_000_000_000;
/// The CA certificates a private CA's PEM may hold.
const CA_CERTS_MAX: usize = 100;
/// The variables a browser keeps from this process: its locale, time zone
/// and fonts. Nothing else (no secret, no proxy) reaches it.
const ENV_KEPT: [&str; 5] = ["LANG", "LC_ALL", "TZ", "FONTCONFIG_FILE", "FONTCONFIG_PATH"];
/// Starts the browser with this service's pipes as its descriptors 3 (it
/// reads CDP there) and 4 (it writes), its stdin and stdout on /dev/null:
/// `--remote-debugging-pipe`'s descriptors, without a line of unsafe code.
const PIPE_EXEC: &str = r#"exec "$0" "$@" 3<&0 4>&1 0</dev/null 1>/dev/null"#;

/// What the renderer serves with.
#[derive(Debug, Clone)]
pub struct Options {
    /// The browser's binary: the pinned chrome-headless-shell (browser.rs).
    pub browser: PathBuf,
    /// Where sessions' homes and profiles live while they run: the
    /// renderer's alone (one renderer at a time).
    pub state: PathBuf,
    /// The fragments' origins its pages may reach, and where they are served.
    pub origins: Origins,
    /// A private CA's certificates (PEM) the browser trusts besides the
    /// public roots, for fragments on https under it (Linux).
    pub ca_file: Option<PathBuf>,
    /// The service's port on loopback (0: a free one).
    pub port: u16,
}

/// Why the renderer cannot serve.
#[derive(Debug)]
pub enum RenderingError {
    Io { what: String, source: io::Error },
    /// The private CA could not be made the browser's.
    Ca(String),
}

impl fmt::Display for RenderingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderingError::Io { what, source } => write!(f, "{what}: {source}"),
            RenderingError::Ca(why) => write!(f, "the card browser's private CA: {why}"),
        }
    }
}

impl std::error::Error for RenderingError {}

fn io_error(what: impl Into<String>) -> impl FnOnce(io::Error) -> RenderingError {
    let what = what.into();
    move |source| RenderingError::Io { what, source }
}

/// The renderer, serving until dropped (its browsers stopped with it).
pub struct Rendering {
    /// What the cell's `FRAGMENT_BROWSER_URL` names.
    pub url: String,
    pub port: u16,
    inner: Arc<Inner>,
    gate: gate::Gate,
}

struct Inner {
    browser: PathBuf,
    state: PathBuf,
    /// The gate's address, for `--proxy-server`.
    proxy: String,
    /// An NSS database trusting the private CA, copied into each home.
    nssdb: Option<PathBuf>,
    sessions: Mutex<HashMap<String, Session>>,
    /// Sessions being started: counted against `SESSIONS_MAX` meanwhile.
    starting: AtomicUsize,
    connections: AtomicUsize,
    stop: AtomicBool,
}

/// One browser.
struct Session {
    child: Child,
    /// Its exit was seen (and reaped): its process group is no longer its own.
    exited: bool,
    to_browser: Arc<Mutex<ChildStdin>>,
    out: Arc<Mutex<Sink>>,
    home: PathBuf,
    keep_alive: Duration,
    born: Instant,
    /// Since when no client holds it (`None`: one does).
    idle_since: Option<Instant>,
}

/// Where the browser's messages go.
enum Sink {
    /// Its first answer, awaited by the request that starts it.
    Probe(mpsc::Sender<Vec<u8>>),
    /// The client's socket (its writing half).
    Client(TcpStream),
    /// No one: what it says between clients is dropped.
    Nobody,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // a thread that panicked holding it left nothing half-written that matters here
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Rendering {
    /// Serves from `opts.state`, which is the renderer's alone: homes a
    /// renderer before it left (it crashed) are removed first.
    pub fn start(opts: Options) -> Result<Rendering, RenderingError> {
        fs::create_dir_all(&opts.state).map_err(io_error(format!("create {}", opts.state.display())))?;
        let entries = fs::read_dir(&opts.state).map_err(io_error(format!("read {}", opts.state.display())))?;
        for stale in entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("session-")) {
            fs::remove_dir_all(stale.path()).map_err(io_error(format!("remove {}", stale.path().display())))?;
        }
        let nssdb = match &opts.ca_file {
            Some(ca) => Some(nss_trusting(ca, &opts.state)?),
            None => None,
        };
        let gate = gate::Gate::start(opts.origins.clone()).map_err(io_error("start the card browser's gate"))?;
        let listener = TcpListener::bind(("127.0.0.1", opts.port)).map_err(io_error(format!("listen on 127.0.0.1:{}", opts.port)))?;
        let port = listener.local_addr().map_err(io_error("the renderer's port"))?.port();
        let inner = Arc::new(Inner {
            browser: opts.browser,
            state: opts.state,
            proxy: gate.proxy(),
            nssdb,
            sessions: Mutex::new(HashMap::new()),
            starting: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let accepting = Arc::clone(&inner);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if accepting.stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                if accepting.connections.load(Ordering::Relaxed) >= CONNECTIONS_MAX {
                    continue;
                }
                accepting.connections.fetch_add(1, Ordering::Relaxed);
                let inner = Arc::clone(&accepting);
                std::thread::spawn(move || {
                    serve(&inner, stream);
                    inner.connections.fetch_sub(1, Ordering::Relaxed);
                });
            }
        });
        let reaping = Arc::clone(&inner);
        std::thread::spawn(move || {
            while !reaping.stop.load(Ordering::Relaxed) {
                std::thread::sleep(REAP_EVERY);
                reap(&reaping);
            }
        });
        Ok(Rendering { url: format!("http://127.0.0.1:{port}"), port, inner, gate })
    }

    /// The gate's port (its origins' only way out).
    pub fn gate_port(&self) -> u16 {
        self.gate.port
    }

    /// Sessions running now.
    pub fn sessions(&self) -> usize {
        locked(&self.inner.sessions).len()
    }
}

impl Drop for Rendering {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wake the accept loop
        let all: Vec<Session> = locked(&self.inner.sessions).drain().map(|(_, s)| s).collect();
        for s in all {
            close(s);
        }
    }
}

/// The browser's command line, less its binary: the isolation above.
pub fn flags(proxy: &str, profile: &Path) -> Vec<String> {
    let proxy_host = proxy.rsplit_once("://").map(|(_, a)| a).and_then(|a| a.rsplit_once(':')).map(|(h, _)| h).unwrap_or("127.0.0.1");
    vec![
        "--remote-debugging-pipe".into(),
        format!("--user-data-dir={}", profile.display()),
        format!("--proxy-server={proxy}"),
        "--proxy-bypass-list=<-loopback>".into(),
        format!("--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE {proxy_host}"),
        "--disable-quic".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
        // nothing of its own on the network (the gate would refuse it anyway)
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-domain-reliability".into(),
        "--disable-client-side-phishing-detection".into(),
        "--disable-sync".into(),
        "--disable-breakpad".into(),
        "--disable-features=Translate,MediaRouter,OptimizationHints".into(),
        "--no-pings".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-extensions".into(),
        "--disable-default-apps".into(),
        "--mute-audio".into(),
        "--hide-scrollbars".into(),
        "--force-color-profile=srgb".into(),
        "about:blank".into(),
    ]
}

/// A request's head: method, path, query, headers.
struct Head {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn query(&self, name: &str) -> Option<&str> {
        self.query.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v)
    }
}

/// The head, at most `HEAD_MAX` bytes; `None` for anything else.
fn read_head(r: &mut impl BufRead) -> Option<Head> {
    let mut lines = vec![];
    let mut left = HEAD_MAX;
    // bounded: each line takes at least its "\n" from HEAD_MAX
    loop {
        let mut line = String::new();
        let n = r.by_ref().take(left).read_line(&mut line).ok()?;
        if n == 0 || !line.ends_with('\n') {
            return None;
        }
        left -= n as u64;
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut first = lines.first()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers = lines[1..].iter().filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    Some(Head { method, path: path.to_string(), query: query.to_string(), headers })
}

fn respond(s: &mut TcpStream, status: u16, body: &serde_json::Value) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        _ => "Error",
    };
    let body = body.to_string();
    let head = format!("HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
    let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body.as_bytes())).and_then(|_| s.flush());
}

fn error(s: &mut TcpStream, status: u16, why: &str) {
    respond(s, status, &serde_json::json!({ "error": why }));
}

/// One connection: a route, then (for the socket) the session's CDP.
fn serve(inner: &Arc<Inner>, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(HEAD_TIMEOUT));
    let Ok(reading) = stream.try_clone() else { return };
    let mut r = BufReader::new(reading);
    let Some(head) = read_head(&mut r) else {
        error(&mut stream, 400, "not an HTTP/1.1 request");
        return;
    };
    let Some(rest) = head.path.strip_prefix("/v1/devtools/browser") else {
        error(&mut stream, 404, "no such route: the renderer answers /v1/devtools/browser");
        return;
    };
    match (head.method.as_str(), rest.strip_prefix('/')) {
        ("POST", None) if rest.is_empty() => acquire(inner, &head, &mut stream),
        ("GET", Some(id)) if valid_id(id) => connect(inner, id, &head, stream, r),
        ("DELETE", Some(id)) if valid_id(id) => {
            let session = locked(&inner.sessions).remove(id);
            match session {
                Some(s) => {
                    close(s);
                    respond(&mut stream, 200, &serde_json::json!({ "status": "closed" }));
                }
                None => error(&mut stream, 404, "no such session (closed, or never made)"),
            }
        }
        (_, Some(_)) => error(&mut stream, 404, "no such session"),
        _ => error(&mut stream, 405, "POST /v1/devtools/browser starts a session"),
    }
}

/// A session's id, as this service makes them (32 hex).
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn keep_alive(head: &Head) -> Result<Duration, String> {
    match head.query("keep_alive") {
        None => Ok(KEEP_ALIVE_DEFAULT),
        Some(ms) => match ms.parse::<u64>() {
            Ok(ms) if Duration::from_millis(ms) <= KEEP_ALIVE_MAX => Ok(Duration::from_millis(ms)),
            _ => Err(format!("keep_alive is milliseconds, at most {}", KEEP_ALIVE_MAX.as_millis())),
        },
    }
}

/// `POST /v1/devtools/browser`: a browser of the session's own, handed
/// out once it answers CDP.
fn acquire(inner: &Arc<Inner>, head: &Head, stream: &mut TcpStream) {
    let keep_alive = match keep_alive(head) {
        Ok(k) => k,
        Err(why) => return error(stream, 400, &why),
    };
    {
        let sessions = locked(&inner.sessions);
        if sessions.len() + inner.starting.load(Ordering::Relaxed) >= SESSIONS_MAX {
            return error(stream, 429, &format!("{SESSIONS_MAX} browsers are running"));
        }
        inner.starting.fetch_add(1, Ordering::Relaxed);
    }
    let id = crate::random_hex(16);
    let started = launch(inner, &id, keep_alive);
    inner.starting.fetch_sub(1, Ordering::Relaxed);
    match started {
        Ok(session) => {
            locked(&inner.sessions).insert(id.clone(), session);
            respond(stream, 200, &serde_json::json!({ "sessionId": id }));
        }
        Err(why) => {
            eprintln!("card browser: a session did not start: {why}");
            error(stream, 500, &format!("the browser did not start: {why}"));
        }
    }
}

/// Starts a browser in a home of its own and waits for its first answer.
fn launch(inner: &Inner, id: &str, keep_alive: Duration) -> Result<Session, String> {
    let home = inner.state.join(format!("session-{id}"));
    let profile = home.join("profile");
    fs::create_dir_all(&profile).map_err(|e| format!("create {}: {e}", profile.display()))?;
    if let Some(nssdb) = &inner.nssdb {
        copy_dir(nssdb, &home.join(".pki/nssdb")).map_err(|e| format!("copy the CA's NSS database: {e}"))?;
    }
    let log_path = home.join("browser.log");
    let log = fs::File::create(&log_path).map_err(|e| format!("create {}: {e}", log_path.display()))?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(PIPE_EXEC).arg(&inner.browser).args(flags(&inner.proxy, &profile));
    cmd.env_clear().env("HOME", &home).env("PATH", "/usr/bin:/bin");
    for var in ENV_KEPT {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(log).process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("start {}: {e}", inner.browser.display()))?;
    let (to, from) = (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"));
    let (tx, rx) = mpsc::channel();
    let out = Arc::new(Mutex::new(Sink::Probe(tx)));
    let pumping = Arc::clone(&out);
    std::thread::spawn(move || pump(from, &pumping));
    let to_browser = Arc::new(Mutex::new(to));
    let mut session = Session { child, exited: false, to_browser, out, home, keep_alive, born: Instant::now(), idle_since: Some(Instant::now()) };
    let probe = serde_json::json!({ "id": PROBE_ID, "method": "Browser.getVersion" }).to_string();
    let asked = write_message(&session.to_browser, probe.as_bytes());
    let deadline = Instant::now() + START_TIMEOUT;
    // bounded: each message read moves toward the deadline or answers
    let answered = asked.is_ok()
        && loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(m) if serde_json::from_slice::<serde_json::Value>(&m).is_ok_and(|v| v["id"] == PROBE_ID) => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        };
    if !answered {
        let said = fs::read_to_string(&log_path).unwrap_or_default();
        let tail: Vec<&str> = said.lines().rev().take(3).collect();
        session.exited = matches!(session.child.try_wait(), Ok(Some(_)));
        close(session);
        return Err(format!("no answer to CDP within {START_TIMEOUT:?} (it said: {})", tail.into_iter().rev().collect::<Vec<_>>().join(" | ")));
    }
    *locked(&session.out) = Sink::Nobody;
    Ok(session)
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        fs::copy(entry.path(), to.join(entry.file_name()))?;
    }
    Ok(())
}

/// One CDP message to the browser: its bytes, then the NUL that ends it.
fn write_message(to: &Mutex<ChildStdin>, msg: &[u8]) -> io::Result<()> {
    assert!(!msg.contains(&0), "a message to the browser holds no NUL: it ends one");
    let mut to = locked(to);
    to.write_all(msg)?;
    to.write_all(&[0])?;
    to.flush()
}

/// The browser's messages, each to whoever its sink names, until it ends
/// its pipe (it exited) or says something past `MESSAGE_MAX`.
fn pump(from: ChildStdout, out: &Mutex<Sink>) {
    let mut r = BufReader::new(from);
    // bounded: each message is at most MESSAGE_MAX, and the loop ends with the pipe
    loop {
        let mut msg = Vec::new();
        match r.by_ref().take(MESSAGE_MAX as u64 + 1).read_until(0, &mut msg) {
            Ok(n) if n > 0 && msg.last() == Some(&0) => {
                msg.pop();
            }
            // the pipe ended, mid-message or not, or a message ran past the limit
            _ => break,
        }
        let mut sink = locked(out);
        match &mut *sink {
            Sink::Client(s) => {
                if ws::write_frame(s, ws::TEXT, &msg).is_err() {
                    let _ = s.shutdown(Shutdown::Both);
                    *sink = Sink::Nobody;
                }
            }
            Sink::Probe(tx) => {
                let _ = tx.send(msg);
            }
            Sink::Nobody => {}
        }
    }
    // the browser is gone: so is its client's socket, and a start that
    // waits for its first answer hears now that none will come
    let mut sink = locked(out);
    if let Sink::Client(s) = &mut *sink {
        let _ = ws::write_close(s, 1011);
        let _ = s.shutdown(Shutdown::Both);
    }
    *sink = Sink::Nobody;
}

/// `GET /v1/devtools/browser/<id>`, upgraded: the client's messages to the
/// browser, the browser's (`pump`) to the client, until either ends.
fn connect(inner: &Arc<Inner>, id: &str, head: &Head, mut stream: TcpStream, mut r: BufReader<TcpStream>) {
    let upgrade = head.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let key = head.header("sec-websocket-key").filter(|k| k.len() == 24);
    let (true, Some(key), Some("13")) = (upgrade, key, head.header("sec-websocket-version")) else {
        return error(&mut stream, 426, "the session's CDP is a WebSocket (version 13)");
    };
    let Ok(writing) = stream.try_clone() else { return };
    let (to_browser, out) = {
        let mut sessions = locked(&inner.sessions);
        let Some(s) = sessions.get_mut(id) else { return error(&mut stream, 404, "no such session (closed, or never made)") };
        if s.idle_since.is_none() {
            return error(&mut stream, 409, "the session has a client already");
        }
        s.idle_since = None;
        (Arc::clone(&s.to_browser), Arc::clone(&s.out))
    };
    let accept = format!("HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n", ws::accept_key(key));
    {
        // the 101 goes out before any message of the browser's can
        let mut sink = locked(&out);
        if stream.write_all(accept.as_bytes()).is_err() {
            drop(sink);
            return detach(inner, id, &out);
        }
        *sink = Sink::Client(writing);
    }
    let _ = stream.set_read_timeout(Some(SESSION_LIFE_MAX));
    let mut reader = ws::Reader::new(MESSAGE_MAX);
    // bounded: each event is read off the socket, which closes with the session
    loop {
        match reader.next(&mut r) {
            Ok(ws::Event::Message(_, msg)) => {
                if msg.contains(&0) {
                    control(&out, |s| ws::write_close(s, 1007));
                    break;
                }
                if write_message(&to_browser, &msg).is_err() {
                    control(&out, |s| ws::write_close(s, 1011));
                    break;
                }
            }
            Ok(ws::Event::Ping(p)) => control(&out, |s| ws::write_frame(s, ws::PONG, &p)),
            Ok(ws::Event::Pong) => {}
            Ok(ws::Event::Close(_)) => {
                control(&out, |s| ws::write_close(s, 1000));
                break;
            }
            Err(ws::WsError::Refused { code, .. }) => {
                control(&out, |s| ws::write_close(s, code));
                break;
            }
            Err(ws::WsError::Io(_)) => break,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    detach(inner, id, &out);
}

/// A frame of the service's own to the client, if it is still there.
fn control(out: &Mutex<Sink>, write: impl FnOnce(&mut TcpStream) -> io::Result<()>) {
    if let Sink::Client(s) = &mut *locked(out) {
        let _ = write(s);
    }
}

/// The client is gone: the session waits its `keep_alive` for another.
fn detach(inner: &Inner, id: &str, out: &Mutex<Sink>) {
    *locked(out) = Sink::Nobody;
    if let Some(s) = locked(&inner.sessions).get_mut(id) {
        s.idle_since = Some(Instant::now());
    }
}

/// Stops the sessions whose time is up: no client for their `keep_alive`,
/// alive past `SESSION_LIFE_MAX`, or a browser that exited.
fn reap(inner: &Inner) {
    let now = Instant::now();
    let doomed: Vec<Session> = {
        let mut sessions = locked(&inner.sessions);
        let ids: Vec<String> = sessions
            .iter_mut()
            .filter_map(|(id, s)| {
                s.exited = s.exited || matches!(s.child.try_wait(), Ok(Some(_)));
                let idle = s.idle_since.is_some_and(|t| now.duration_since(t) > s.keep_alive);
                (s.exited || idle || now.duration_since(s.born) > SESSION_LIFE_MAX).then(|| id.clone())
            })
            .collect();
        ids.iter().filter_map(|id| sessions.remove(id)).collect()
    };
    for s in doomed {
        close(s);
    }
}

/// A session's end: its client's socket shut, its browser's whole process
/// group killed (while its leader is unreaped, so the group is still its
/// own), the leader reaped, its home removed.
fn close(mut s: Session) {
    if let Sink::Client(c) = &mut *locked(&s.out) {
        let _ = ws::write_close(c, 1000);
        let _ = c.shutdown(Shutdown::Both);
    }
    if !s.exited {
        let group = format!("-{}", s.child.id());
        let _ = Command::new("kill").args(["-KILL", "--", &group]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = s.child.kill();
    let _ = s.child.wait();
    let _ = fs::remove_dir_all(&s.home);
}

/// An NSS database (`<state>/nssdb`) trusting each CA certificate in
/// `ca` to identify servers, made with NSS's `certutil`, as Chrome on
/// Linux reads a home's `.pki/nssdb`.
fn nss_trusting(ca: &Path, state: &Path) -> Result<PathBuf, RenderingError> {
    if cfg!(target_os = "macos") {
        return Err(RenderingError::Ca("on macOS the browser trusts the system's keychain: add the CA there, and name no CA file".into()));
    }
    let pem = fs::read_to_string(ca).map_err(|e| RenderingError::Ca(format!("read {}: {e}", ca.display())))?;
    let certs = pem_certificates(&pem).map_err(RenderingError::Ca)?;
    let db = state.join("nssdb");
    let _ = fs::remove_dir_all(&db);
    fs::create_dir_all(&db).map_err(io_error(format!("create {}", db.display())))?;
    let dir = format!("sql:{}", db.display());
    let certutil = |args: &[&str]| -> Result<(), RenderingError> {
        let out = Command::new("certutil").args(args).stdin(Stdio::null()).output().map_err(|e| {
            RenderingError::Ca(format!("run certutil (NSS's tools: Debian's libnss3-tools, Fedora's nss-tools, Arch's nss): {e}"))
        })?;
        if !out.status.success() {
            return Err(RenderingError::Ca(format!("certutil {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim())));
        }
        Ok(())
    };
    certutil(&["-N", "-d", &dir, "--empty-password"])?;
    for (i, cert) in certs.iter().enumerate() {
        let file = state.join(format!("ca-{i}.pem"));
        fs::write(&file, cert).map_err(io_error(format!("write {}", file.display())))?;
        let name = format!("fragment-ca-{i}");
        let added = certutil(&["-A", "-d", &dir, "-n", &name, "-t", "C,,", "-i", &file.display().to_string()]);
        let _ = fs::remove_file(&file);
        added?;
    }
    Ok(db)
}

/// The certificates a PEM bundle holds, each its own PEM block.
fn pem_certificates(pem: &str) -> Result<Vec<String>, String> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut certs = vec![];
    let mut rest = pem;
    // bounded: each block found moves past its END
    while let Some(at) = rest.find(BEGIN) {
        let body = &rest[at + BEGIN.len()..];
        let Some(end) = body.find(END) else { return Err("a certificate's PEM block has no END line".into()) };
        let b64: String = body[..end].chars().filter(|c| !c.is_whitespace()).collect();
        use base64::Engine;
        if base64::engine::general_purpose::STANDARD.decode(&b64).map_or(true, |der| der.is_empty()) {
            return Err(format!("certificate {} is not base64", certs.len() + 1));
        }
        certs.push(format!("{BEGIN}\n{b64}\n{END}\n"));
        if certs.len() > CA_CERTS_MAX {
            return Err(format!("more than {CA_CERTS_MAX} certificates"));
        }
        rest = &body[end + END.len()..];
    }
    if certs.is_empty() {
        return Err("it holds no certificate (a PEM `BEGIN CERTIFICATE` block)".into());
    }
    Ok(certs)
}

#[cfg(test)]
mod tests;
