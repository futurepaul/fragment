//! The `goose` runtime: goose (github.com/aaif-goose/goose) as an agent's
//! hands, spoken to over ACP, the Agent Client Protocol
//! (agentclientprotocol.com), on its stdio (docs/optchat.md, "goose on the
//! computer"; docs/bridge.md, "goose, as the bridge speaks it").
//!
//! - **One `goose acp` per agent**, started at the agent's first turn and
//!   again at the turn after it died. Its environment is the agent's: its
//!   model calls name it (`x-fragment-agent`, goose's OpenAI provider's
//!   custom headers, to `FRAGMENT_MODEL`), its shell's `fragment` acts as it
//!   (`FRAGMENT_AS_AGENT`, `FRAGMENT_FOR`), and its credentials' placeholders
//!   are in their variables. A goose started with other credentials is
//!   started again at the agent's next turn when none of its turns runs.
//! - **A fresh session per turn** (`session/new`, cwd `/data/work`), never
//!   loaded again, and closed when the turn ends (its MCP servers with it):
//!   nothing of a session carries to the next. Every session's system
//!   prompt gets `HANDS` (the computer, its tools, and the `fragment` CLI
//!   for apps: top of mind), the same bytes every turn so its prefix
//!   caches. For a fragment that answers a `view` (a mind), it gets the
//!   subagent framing and VIEW_DOC too (OptChat's spec, §9 and §7.2,
//!   "OptChat" read "Mind"), and its prompt is the view, then the task:
//!   the turn's note, its text and its files. Any other fragment's prompt
//!   is the task alone. A mind's session also gets `fragment mcp <mind>`
//!   (read-only: view, zoom, date, search) when the image names its CLI.
//! - **Its tools, on our image** (`desktop`, the image's
//!   `fragment-desktop`): every session gets the agent's `browser` (its own
//!   desktop's Chromium, over CDP), `computer` (that desktop: keys, mouse,
//!   `screen_look`, `screen_click`) and `web` (`web_search`, `web_read`) as
//!   MCP servers, and its goose runs with `DISPLAY` naming the agent's own
//!   display. Its skills (runtime/skills.rs: the platform skill, the
//!   owner's managed set) are installed at each turn's start.
//! - **What goose says** (`session/update`): message chunks are the draft;
//!   the words before a tool call are its step's (the draft stops); each
//!   tool call, once completed or failed, is a step. The prompt's answer is
//!   the end: `end_turn`, `max_tokens` and `max_turn_requests` idle,
//!   `cancelled` stopped (after a Stop), and anything else an error.
//! - **Exactly one reply a turn, at its end** (a mind takes it as the
//!   hand-off's report, one run of its trigger): the words after the last
//!   tool call; with none, or for a turn stopped or failed, however early,
//!   `(ended: <outcome>: <why>)`.
//! - **Stop** is `session/cancel`; the turn ends when goose answers its
//!   prompt. A Stop before the session is made ends the turn at once.
//! - **goose asks nothing in words** and shows no card: in `auto` mode it
//!   asks no permission, and one it asks anyway (a security inspector's) is
//!   refused. So no `Answer` or `Tell` ever comes for its turns.
//!
//! A goose that dies ends every turn it was running as an error.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::api::{Api, ApiError};
use crate::records::{Outcome, Step};
use crate::runtime::{Agent, Command, Event, Runtime, RuntimeError, RuntimeFuture, RuntimeIo, TurnStart};

/// A line goose writes (one JSON-RPC message) is at most this many bytes:
/// a tool's result with an image in it is a few MiB of base64. Past it the
/// goose is broken, and stopped.
pub const LINE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// A goose answers `initialize` within this (its first start reads its
/// extensions' tools), and `session/new` within this too.
pub const ANSWER_MS_MAX: u64 = 60_000;
/// The view is asked this many times when the platform does not answer
/// (a refusal is final: the fragment has no view).
pub const VIEW_TRIES: u32 = 3;
/// The words a turn holds, at most (a reply is cut far below this); more
/// are dropped.
pub const SAID_MAX_BYTES: usize = 256 * 1024;
/// Tool calls of one turn open at once (asked, no result yet), at most.
pub const CALLS_OPEN_MAX: usize = 256;
/// What a step's result is cut to before the record's own cut.
pub const EXCERPT_KEPT_MAX_BYTES: usize = 4096;
/// The interception CA (docs/computers.md, "Connections and operator keys")
/// appears shortly after boot: it is waited for this long.
pub const CA_WAIT_MS: u64 = 15_000;

/// The subagent framing (OptChat's spec, §9), "OptChat" read "Mind".
pub const FRAMING: &str = "You are a subagent of Mind, an AI agent that works for one user in a
single chat that never ends. Mind gave you a task. Do it yourself, with
your tools, following the user's instructions at the end of this
prompt: they say who the user is, how their files are organized and how
they want work done.

Your first message holds the view below, then your task. The view shows
you what Mind knows: what the user wants, decided and taught. Use it as
context only, and do what your task says, not what the user's last
message says, since Mind may have given you just part of the work. Your
final reply is your report to Mind. Mind may send you more messages, even
while you work.";

/// VIEW_DOC (OptChat's spec, §7.2), "OptChat" read "Mind".
pub const VIEW_DOC: &str = "The view: the whole chat between Mind and the user, oldest first, inside
<chat> tags, as one-line summaries. Each line is

  id+n|text   the n messages from id on, summarized (newlines shown as spaces)

A summary tags each item with its kind: user (the user's words), talk
(Mind's replies), tool (Mind's tool calls), echo (their results), note
(notes from the user's other agents), or work (a computer task's report,
starting \"[id] \"). A short message is its own line, word for word. A
text too long for one message is split over several in a row. Recent lines
cover one message each; the older the messages, the more a line covers.
A message not summarized yet shows as \"(not summarized yet: zoom it)\".
No message appears in full, not even the last ones.

Navigating: zoom(id, n) opens line id+n into the two lines of n/2
messages it was made from; zoom(id, 1) gives message id in full. Zoom
whenever a summary only mentions something you need, such as what your
last reply said, a decision, a past attempt or where a file is, before
you act, guess or ask. date(id) gives the date and time of message id.";

/// What every session's system prompt adds under goose's own: the
/// computer, its tools, and fragments top of mind (the platform skill has
/// the rest: runtime/computer.md).
pub const HANDS: &str = "You work on your owner's Fragment computer: a Linux machine with your shell, your own desktop and a browser on it, which your owner can watch.

Fragments are how you make things for people: apps, sites, pages, dashboards, trackers, brains. You make, publish and update your owner's fragments with the `fragment` CLI in your shell (it acts as you; no login). Before any app, site, page or fragment work, load the `fragment` skill, then `apps-finite`; give your owner the link to what you made.

Your tools: web_search and web_read read the web fast with no browser: use them first for reading, and never curl or scrape a web page's HTML in the shell. When your task says to use the browser, use it. The browser tools drive the Chromium on your desktop: browser_snapshot reads the page as elements, each with a ref ([ref=f1e5]), and you act on one by passing its ref as `target` (browser_click, browser_type, browser_fill_form); browser_find searches a long page. The computer tools drive the whole desktop; you read no images, so screen_look asks a vision model what is on the screen and screen_click finds what you describe. If a tool answers human_has_control, your owner has taken over your screen: wait for them.";

#[derive(Debug, Clone)]
pub struct GooseConfig {
    /// goose (`BRIDGE_GOOSE_BIN`).
    pub command: PathBuf,
    /// Its arguments: `acp --with-builtin developer`.
    pub args: Vec<String>,
    /// Each session's cwd, what its tools write (`/data/work`: docs/computers.md, the seam).
    pub work: PathBuf,
    /// Its tools' `HOME` (`/data/work/home`): what they write under `~` is work too.
    pub home: PathBuf,
    /// Each agent's goose keeps its own state under `<root>/<agent>`
    /// (`GOOSE_PATH_ROOT`): scratch, never `/data` (no session is loaded
    /// again).
    pub root: PathBuf,
    /// `FRAGMENT_API`: the view, and its CLI's.
    pub api: String,
    /// `FRAGMENT_MODEL`: goose's OpenAI provider's host.
    pub model: String,
    /// The model tier its calls name (`medium`).
    pub tier: String,
    /// The fragment CLI, for a mind's `fragment mcp <mind>`; none, no such
    /// extension.
    pub cli: Option<PathBuf>,
    /// The interception CA to append to the system's bundle once it appears.
    pub ca: Option<(PathBuf, PathBuf)>,
    /// The image's `fragment-desktop` (`BRIDGE_GOOSE_DESKTOP`): each agent's
    /// desktop, and its browser, computer and web tools; none, no such tools.
    pub desktop: Option<PathBuf>,
    /// Whether each turn installs its agent's skills (`BRIDGE_GOOSE_SKILLS`).
    pub skills: bool,
}

/// An agent's goose, as a pipe: its stdout, its stdin, and what keeps it
/// alive (dropped, it dies).
pub struct Pipe {
    pub read: Box<dyn AsyncRead + Send + Unpin>,
    pub write: Box<dyn AsyncWrite + Send + Unpin>,
    pub keep: Box<dyn Send>,
}

/// What starts an agent's goose: the process (`Process`), or a test's fake.
pub trait Spawn: Send + Sync + 'static {
    fn spawn(&self, agent: &Agent) -> Result<Pipe, String>;
}

pub struct Goose {
    pub config: GooseConfig,
    pub spawn: Arc<dyn Spawn>,
}

impl Goose {
    /// The runtime with goose's process as each agent's.
    pub fn new(config: GooseConfig) -> Goose {
        let spawn = Arc::new(Process { config: config.clone() });
        Goose { config, spawn }
    }
}

impl Runtime for Goose {
    fn name(&self) -> &'static str {
        "goose"
    }

    fn run(self: Box<Self>, io: RuntimeIo) -> RuntimeFuture {
        Box::pin(run(self.config, self.spawn, io))
    }
}

// ---- the process ----

/// `goose acp` as the agent, with the agent's environment.
pub struct Process {
    pub config: GooseConfig,
}

/// The environment an agent's goose runs with (and its tools, which
/// inherit it): `display`, the agent's own desktop's, when it has one.
pub fn environment(cfg: &GooseConfig, a: &Agent, display: Option<u32>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("GOOSE_PROVIDER", "openai".to_string()),
        ("GOOSE_MODEL", cfg.tier.clone()),
        // the computer's model intercept, as the agent (docs/computers.md,
        // "Models"): plain HTTP, no key of the guest's
        ("OPENAI_HOST", cfg.model.clone()),
        ("OPENAI_BASE_PATH", "v1/chat/completions".to_string()),
        // goose reads its custom headers from where it found the key: an
        // empty key in the environment sends no authorization at all
        ("OPENAI_API_KEY", String::new()),
        ("OPENAI_CUSTOM_HEADERS", format!("{}={}", crate::api::AGENT_HEADER, a.fragment)),
        // a known limit, not asked of `/v1/models` (404 at the intercept)
        ("GOOSE_CONTEXT_LIMIT", "128000".to_string()),
        // a turn is a fresh session: nothing of goose's own compacts it
        // (the threshold, any goose; at an overflow too, our fork), and its
        // system prompt never changes within it (our fork: its prefix caches)
        ("GOOSE_AUTO_COMPACT_THRESHOLD", "0".to_string()),
        ("GOOSE_NO_COMPACTION", "1".to_string()),
        ("GOOSE_STABLE_SYSTEM_PROMPT", "1".to_string()),
        // no extension of goose's config: `developer` (its builtin) and a
        // session's own (`mcpServers`) alone; no subagents, scheduler, or
        // memory of goose's own
        ("EXTENSIONS", "{}".to_string()),
        ("GOOSE_MODE", "auto".to_string()),
        ("GOOSE_DISABLE_KEYRING", "1".to_string()),
        ("GOOSE_DISABLE_SESSION_NAMING", "true".to_string()),
        ("GOOSE_TELEMETRY_OFF", "1".to_string()),
        ("GOOSE_PATH_ROOT", cfg.root.join(&a.fragment).display().to_string()),
        ("HOME", cfg.home.display().to_string()),
        // its shell's `fragment` acts as the agent, for its owner (cli/GUIDE.md, "As an agent")
        ("FRAGMENT_API", cfg.api.clone()),
        ("FRAGMENT_AS_AGENT", a.fragment.clone()),
        ("FRAGMENT_FOR", a.owner.clone()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for c in &a.credentials {
        for name in &c.env {
            env.push((name.clone(), c.placeholder.clone()));
        }
    }
    if let Some(n) = display {
        env.push(("DISPLAY".into(), format!(":{n}")));
    }
    env
}

/// The agent's display, as the image's desktop gives it out
/// (`fragment-desktop display <agent>`): none when the image has no
/// desktop, or it did not answer.
fn display_of(cfg: &GooseConfig, a: &Agent) -> Option<u32> {
    let desktop = cfg.desktop.as_ref()?;
    let out = std::process::Command::new(desktop).args(["display", &a.fragment]).stdin(std::process::Stdio::null()).output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().parse().ok(),
        Ok(o) => {
            crate::ev!("goose.no_display", { "agent": a.fragment, "why": String::from_utf8_lossy(&o.stderr).trim() });
            None
        }
        Err(e) => {
            crate::ev!("goose.no_display", { "agent": a.fragment, "why": e.to_string() });
            None
        }
    }
}

/// What an agent's goose was started with that can change while it runs:
/// a goose started with another is stale.
fn fingerprint(a: &Agent) -> String {
    let creds: Vec<(&str, &Vec<String>, &str)> = a.credentials.iter().map(|c| (c.provider.as_str(), &c.env, c.placeholder.as_str())).collect();
    json!([a.owner, creds]).to_string()
}

/// An agent fragment's name is a path part here (`<label>.<username>`).
fn path_part_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name != "." && name != ".." && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Spawn for Process {
    fn spawn(&self, a: &Agent) -> Result<Pipe, String> {
        if !path_part_ok(&a.fragment) {
            return Err(format!("{:?} is no agent fragment's name", a.fragment));
        }
        let cfg = &self.config;
        let made = std::fs::create_dir_all(cfg.root.join(&a.fragment)).and_then(|()| std::fs::create_dir_all(&cfg.work)).and_then(|()| std::fs::create_dir_all(&cfg.home));
        made.map_err(|e| format!("its directories: {e}"))?;
        let mut child = tokio::process::Command::new(&cfg.command)
            .args(&cfg.args)
            .current_dir(&cfg.work)
            .envs(environment(cfg, a, display_of(cfg, a)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("{}: {e}", cfg.command.display()))?;
        let read = child.stdout.take().ok_or("no stdout")?;
        let write = child.stdin.take().ok_or("no stdin")?;
        crate::ev!("goose.started", { "agent": a.fragment, "pid": child.id() });
        Ok(Pipe { read: Box::new(read), write: Box::new(write), keep: Box::new(child) })
    }
}

/// Appends the interception CA to the system's bundle once it appears,
/// once (the bundle is the image's: rebuilt at every start).
async fn trust_ca(ca: PathBuf, bundle: PathBuf) {
    let t = std::time::Instant::now();
    // bounded by CA_WAIT_MS
    while t.elapsed() < Duration::from_millis(CA_WAIT_MS) {
        if let Ok(pem) = tokio::fs::read_to_string(&ca).await {
            let have = tokio::fs::read_to_string(&bundle).await.unwrap_or_default();
            if have.contains(pem.trim()) {
                return;
            }
            let appended = async {
                let mut f = tokio::fs::OpenOptions::new().append(true).open(&bundle).await?;
                f.write_all(format!("\n{}\n", pem.trim()).as_bytes()).await?;
                f.flush().await
            };
            match appended.await {
                Ok(()) => crate::ev!("goose.ca", { "appended": true, "ms": t.elapsed().as_millis() as u64 }),
                Err(e) => crate::ev!("goose.ca", { "appended": false, "why": e.to_string() }),
            }
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    crate::ev!("goose.ca", { "appended": false, "why": "no CA appeared" });
}

// ---- ACP, one goose ----

/// Why a call to goose failed.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpError {
    /// goose could not be started.
    Spawn(String),
    /// goose answered with a JSON-RPC error.
    Refused { code: i64, message: String },
    /// goose's stdio closed (it died) before it answered.
    Gone,
    /// goose did not answer `method` in time.
    Late(&'static str),
    /// An answer that is not what the method returns.
    Decode(String),
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpError::Spawn(m) => write!(f, "it did not start: {m}"),
            AcpError::Refused { code, message } => write!(f, "{message} ({code})"),
            AcpError::Gone => write!(f, "it stopped"),
            AcpError::Late(method) => write!(f, "it did not answer {method} within {} s", ANSWER_MS_MAX / 1000),
            AcpError::Decode(m) => write!(f, "an answer that does not decode: {m}"),
        }
    }
}

type Answer = Result<Value, AcpError>;

/// A JSON-RPC error, said whole: goose puts what went wrong in its `data`.
fn refusal(e: &Value) -> AcpError {
    let message = e["message"].as_str().unwrap_or("");
    let message = match &e["data"] {
        Value::Null => message.to_string(),
        Value::String(d) if message.is_empty() => d.clone(),
        Value::String(d) => format!("{message}: {d}"),
        d => format!("{message}: {d}"),
    };
    AcpError::Refused { code: e["code"].as_i64().unwrap_or(0), message }
}

/// One goose: the calls it owes an answer, and the sessions whose updates
/// go to their turns.
struct Conn {
    agent: String,
    fingerprint: String,
    out: mpsc::UnboundedSender<String>,
    calls: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
    sessions: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    next: AtomicU64,
    dead: AtomicBool,
    keep: Mutex<Option<Box<dyn Send>>>,
}

/// Reads one line of at most `max` bytes (its `\n` dropped): `None` at the
/// end, an error past the bound.
async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> std::io::Result<Option<()>> {
    buf.clear();
    // bounded by `max`: each pass takes bytes or ends
    loop {
        let available = r.fill_buf().await?;
        if available.is_empty() {
            return Ok((!buf.is_empty()).then_some(()));
        }
        let (take, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (available.len(), false),
        };
        if buf.len() + take > max {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("a line past {max} bytes")));
        }
        buf.extend_from_slice(&available[..take]);
        r.consume(if done { take + 1 } else { take });
        if done {
            return Ok(Some(()));
        }
    }
}

impl Conn {
    /// Starts `agent`'s goose and greets it (`initialize`).
    async fn start(spawn: &dyn Spawn, agent: &Agent) -> Result<Arc<Conn>, AcpError> {
        let pipe = spawn.spawn(agent).map_err(AcpError::Spawn)?;
        let (out, mut lines) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(Conn {
            agent: agent.fragment.clone(),
            fingerprint: fingerprint(agent),
            out,
            calls: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            keep: Mutex::new(Some(pipe.keep)),
        });
        let mut write = pipe.write;
        tokio::spawn(async move {
            // bounded by the goose's life: its stdin closes with it
            while let Some(mut line) = lines.recv().await {
                line.push('\n');
                if write.write_all(line.as_bytes()).await.is_err() || write.flush().await.is_err() {
                    break;
                }
            }
        });
        let reader = conn.clone();
        let read = pipe.read;
        tokio::spawn(async move { reader.read(read).await });
        let hello = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "fragment-bridge", "version": env!("CARGO_PKG_VERSION") },
        });
        let greeted = conn.call("initialize", hello, Some(Duration::from_millis(ANSWER_MS_MAX))).await;
        match greeted {
            Ok(v) if v["protocolVersion"].as_u64() == Some(1) => Ok(conn),
            Ok(v) => {
                conn.kill();
                Err(AcpError::Decode(format!("initialize answered protocol {}", v["protocolVersion"])))
            }
            Err(e) => {
                conn.kill();
                Err(e)
            }
        }
    }

    /// goose's stdout, until it ends: answers to their calls, updates to
    /// their sessions, and goose's own asks answered.
    async fn read(self: Arc<Conn>, read: Box<dyn AsyncRead + Send + Unpin>) {
        let mut r = BufReader::new(read);
        let mut buf = Vec::new();
        // bounded by the goose's life: one message per pass, ended by EOF
        loop {
            match read_line(&mut r, &mut buf, LINE_MAX_BYTES).await {
                Ok(Some(())) => {}
                Ok(None) => break,
                Err(e) => {
                    crate::ev!("goose.unreadable", { "agent": self.agent, "error": e.to_string() });
                    break;
                }
            }
            if buf.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Value>(&buf) {
                Ok(m) => self.dispatch(m),
                Err(e) => crate::ev!("goose.unreadable", { "agent": self.agent, "error": e.to_string() }),
            }
        }
        self.dead.store(true, Ordering::SeqCst);
        crate::ev!("goose.stopped", { "agent": self.agent });
        for (_, tx) in self.calls.lock().expect("calls").drain() {
            let _ = tx.send(Err(AcpError::Gone));
        }
        self.sessions.lock().expect("sessions").clear();
        self.kill();
    }

    fn dispatch(&self, m: Value) {
        let method = m["method"].as_str();
        match (method, m.get("id")) {
            // an answer to a call of ours
            (None, Some(id)) => {
                let Some(tx) = id.as_u64().and_then(|id| self.calls.lock().expect("calls").remove(&id)) else { return };
                let answer = match m.get("error") {
                    Some(e) => Err(refusal(e)),
                    None => Ok(m["result"].clone()),
                };
                let _ = tx.send(answer);
            }
            (Some("session/update"), None) => {
                let session = m["params"]["sessionId"].as_str().unwrap_or("");
                if let Some(tx) = self.sessions.lock().expect("sessions").get(session) {
                    let _ = tx.send(m["params"]["update"].clone());
                }
            }
            // goose asks: a permission is refused (in auto mode it asks
            // none; one it asks anyway is a security inspector's, and no
            // one is there to answer it), anything else is not offered
            (Some(method), Some(id)) => {
                let answer = if method == "session/request_permission" {
                    let reject = m["params"]["options"].as_array().and_then(|o| o.iter().find(|o| o["kind"] == "reject_once")).and_then(|o| o["optionId"].as_str());
                    crate::ev!("goose.permission_refused", { "agent": self.agent, "tool": m["params"]["toolCall"]["title"] });
                    match reject {
                        Some(option) => json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "selected", "optionId": option } } }),
                        None => json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "cancelled" } } }),
                    }
                } else {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} is not offered") } })
                };
                let _ = self.out.send(answer.to_string());
            }
            // other notifications (goose's own) say nothing a turn needs
            (Some(_), None) | (None, None) => {}
        }
    }

    /// Calls `method`: its answer, or why none came (within `wait`, when given).
    async fn call(&self, method: &'static str, params: Value, wait: Option<Duration>) -> Answer {
        if self.dead.load(Ordering::SeqCst) {
            return Err(AcpError::Gone);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.calls.lock().expect("calls").insert(id, tx);
        if self.out.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()).is_err() {
            self.calls.lock().expect("calls").remove(&id);
            return Err(AcpError::Gone);
        }
        let answered = match wait {
            Some(wait) => match tokio::time::timeout(wait, rx).await {
                Ok(a) => a,
                Err(_) => {
                    self.calls.lock().expect("calls").remove(&id);
                    return Err(AcpError::Late(method));
                }
            },
            None => rx.await,
        };
        answered.unwrap_or(Err(AcpError::Gone))
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string());
    }

    /// `session`'s updates, from now until `forget`.
    fn follow(&self, session: &str) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        if !self.dead.load(Ordering::SeqCst) {
            self.sessions.lock().expect("sessions").insert(session.to_string(), tx);
        }
        rx
    }

    fn forget(&self, session: &str) {
        self.sessions.lock().expect("sessions").remove(session);
    }

    /// Whether a turn runs in it now.
    fn busy(&self) -> bool {
        !self.sessions.lock().expect("sessions").is_empty()
    }

    fn alive(&self) -> bool {
        !self.dead.load(Ordering::SeqCst)
    }

    /// Ends it: its process is killed (its stdout then ends, and the reader
    /// with it).
    fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
        drop(self.keep.lock().expect("keep").take());
    }
}

// ---- the runtime ----

struct Ctx {
    config: GooseConfig,
    api: Api,
    spawn: Arc<dyn Spawn>,
    gooses: tokio::sync::Mutex<HashMap<String, Arc<Conn>>>,
    /// The platform skill, made once from the CLI's (`fragment skill`).
    platform: tokio::sync::OnceCell<Option<String>>,
}

impl Ctx {
    /// The agent's goose: its running one, or a new one (when it has none,
    /// its own died, or it is stale and idle).
    async fn goose_for(&self, a: &Agent) -> Result<Arc<Conn>, AcpError> {
        let mut gooses = self.gooses.lock().await;
        if let Some(c) = gooses.get(&a.fragment) {
            let stale = c.fingerprint != fingerprint(a);
            if c.alive() && (!stale || c.busy()) {
                return Ok(c.clone());
            }
            crate::ev!("goose.restart", { "agent": a.fragment, "why": if c.alive() { "its credentials changed" } else { "it stopped" } });
            c.kill();
            gooses.remove(&a.fragment);
        }
        let c = Conn::start(&*self.spawn, a).await?;
        gooses.insert(a.fragment.clone(), c.clone());
        Ok(c)
    }
}

/// What a running turn hears from the runtime's loop.
enum Heard {
    Stop,
    Forget,
}

async fn run(config: GooseConfig, spawn: Arc<dyn Spawn>, mut io: RuntimeIo) -> Result<(), RuntimeError> {
    let api = Api::new(&config.api).map_err(RuntimeError::Setup)?;
    if let Some((ca, bundle)) = config.ca.clone() {
        tokio::spawn(trust_ca(ca, bundle));
    }
    let ctx = Arc::new(Ctx { config, api, spawn, gooses: tokio::sync::Mutex::new(HashMap::new()), platform: tokio::sync::OnceCell::new() });
    let mut turns: HashMap<String, mpsc::Sender<Heard>> = HashMap::new();
    let mut shutdown = io.shutdown.clone();
    // Each agent's goose starts at its first turn, so turns can be taken now.
    let _ = io.events.send(Event::Connected(true)).await;
    // bounded by the bridge's life: one command per pass
    loop {
        let c = tokio::select! {
            c = io.commands.recv() => c,
            _ = crate::net::stopped(&mut shutdown) => break,
        };
        let Some(c) = c else { break };
        turns.retain(|_, tx| !tx.is_closed());
        match c {
            Command::Start(ts) => {
                let (tx, rx) = mpsc::channel(4);
                turns.insert(ts.turn.clone(), tx);
                tokio::spawn(turn(ctx.clone(), *ts, rx, io.events.clone()));
            }
            Command::Stop { turn } => {
                if let Some(tx) = turns.get(&turn) {
                    let _ = tx.try_send(Heard::Stop);
                }
            }
            Command::Forget { turn } => {
                if let Some(tx) = turns.remove(&turn) {
                    let _ = tx.try_send(Heard::Forget);
                }
            }
            Command::Answer { turn, .. } | Command::Tell { turn, .. } => {
                crate::ev!("goose.unasked", { "turn": turn, "why": "goose asks nothing a person answers" });
            }
        }
    }
    for c in ctx.gooses.lock().await.values() {
        c.kill();
    }
    Ok(())
}

/// The fragment's view (a mind's), as the turn's agent: `None` when it has
/// none (no such operation, or one the agent may not call), or when the
/// platform did not answer in `VIEW_TRIES` (said in the log).
async fn view_of(api: &Api, ts: &TurnStart) -> Option<String> {
    let id = format!("{}-view", ts.turn);
    for attempt in 1..=VIEW_TRIES {
        match api.op(&ts.agent.fragment, &ts.fragment, "view", &id, &json!({})).await {
            Ok(result) => return result["text"].as_str().map(str::to_string),
            Err(ApiError::Refused { status: 400 | 403 | 404, .. }) => return None,
            Err(e) if e.retryable() && attempt < VIEW_TRIES => tokio::time::sleep(Duration::from_millis(1000 * u64::from(attempt))).await,
            Err(e) => {
                crate::ev!("goose.view_unread", { "turn": ts.turn, "fragment": ts.fragment, "error": e.to_string() });
                return None;
            }
        }
    }
    None
}

/// A session's system prompt, appended under goose's own: `HANDS`, and for
/// a mind the framing and VIEW_DOC; the same bytes every turn.
pub fn system_prompt(mind: bool) -> String {
    match mind {
        true => format!("{HANDS}\n\n{FRAMING}\n\n{VIEW_DOC}"),
        false => HANDS.to_string(),
    }
}

/// A turn's prompt, as its text blocks: for a mind (a fragment with a
/// view), the view, then the task; for any other, the task alone. The task
/// is what the turn is told first (a cut turn's note), its text, and where
/// its files are.
pub fn prompt(view: Option<&str>, ts: &TurnStart) -> Vec<String> {
    let mut task: Vec<String> = Vec::new();
    if let Some(note) = &ts.note {
        task.push(note.clone());
    }
    task.push(ts.text.clone());
    if !ts.files.is_empty() {
        let files: Vec<String> = ts.files.iter().map(|f| format!("- {} ({}, {} bytes)", f.path.display(), f.media_type, f.size)).collect();
        task.push(format!("Its files, on this computer:\n{}", files.join("\n")));
    }
    let mut blocks: Vec<String> = view.map(str::to_string).into_iter().collect();
    blocks.push(task.join("\n\n"));
    blocks
}

/// The session a turn runs in: the agent's browser, computer and web tools
/// when the image has its desktop; `fragment mcp <mind>` among them for a
/// mind, when the image has the CLI.
fn new_session(cfg: &GooseConfig, ts: &TurnStart, mind: bool) -> Value {
    let mut servers = Vec::new();
    if let Some(desktop) = &cfg.desktop {
        for (name, args) in [("browser", vec!["mcp", "browser", ts.agent.fragment.as_str()]), ("computer", vec!["mcp", "computer", ts.agent.fragment.as_str()]), ("web", vec!["mcp", "web"])] {
            servers.push(json!({ "name": name, "command": desktop, "args": args, "env": [] }));
        }
    }
    if let (true, Some(cli)) = (mind, &cfg.cli) {
        let env = [("FRAGMENT_AS_AGENT", ts.agent.fragment.as_str()), ("FRAGMENT_FOR", ts.agent.owner.as_str()), ("FRAGMENT_API", cfg.api.as_str())];
        servers.push(json!({
            "name": "mind",
            "command": cli,
            "args": ["mcp", ts.fragment],
            "env": env.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect::<Vec<_>>(),
        }));
    }
    // a title of ours: goose names no session itself (no model call for it)
    json!({ "cwd": cfg.work, "mcpServers": servers, "_meta": { "sessionTitle": format!("turn {}", ts.turn) } })
}

/// The platform skill: the computer's page and the CLI's own skill, made
/// once (none when the image has no CLI, or it printed no skill).
async fn platform_skill(ctx: &Ctx) -> Option<String> {
    let made = ctx.platform.get_or_init(|| async {
        let cli = ctx.config.cli.as_ref()?;
        let out = tokio::process::Command::new(cli).arg("skill").stdin(std::process::Stdio::null()).output().await.ok()?;
        match super::skills::platform_skill(&String::from_utf8_lossy(&out.stdout)) {
            Ok(s) => Some(s),
            Err(e) => {
                crate::ev!("goose.no_platform_skill", { "why": e });
                None
            }
        }
    });
    made.await.clone()
}

/// Installs the turn's agent's skills, waiting at most `INSTALL_MS_MAX`:
/// what was installed before serves a turn whose install is slow.
async fn install_skills(ctx: &Ctx, ts: &TurnStart) {
    let platform = platform_skill(ctx).await;
    let (dir, manifest) = super::skills::dirs(&ctx.config.root, &ts.agent.fragment);
    let install = super::skills::install(&ctx.api, &ts.agent, &dir, &manifest, platform.as_deref());
    match tokio::time::timeout(Duration::from_millis(super::skills::INSTALL_MS_MAX), install).await {
        Ok(Ok(d)) => crate::ev!("goose.skills", { "agent": ts.agent.fragment, "fragment": d.fragment, "fetched": d.fetched, "removed": d.removed, "refused": d.refused, "skills": d.skills }),
        Ok(Err(e)) => crate::ev!("goose.skills_failed", { "agent": ts.agent.fragment, "error": e.to_string() }),
        Err(_) => crate::ev!("goose.skills_failed", { "agent": ts.agent.fragment, "error": "not done in time: the next turn finishes it" }),
    }
}

/// The turn's session, made: its goose, its id, its updates, and its prompt.
async fn prepare(ctx: &Ctx, ts: &TurnStart) -> Result<(Arc<Conn>, String, mpsc::UnboundedReceiver<Value>, Vec<String>), String> {
    let (view, ()) = tokio::join!(view_of(&ctx.api, ts), async {
        if ctx.config.skills {
            install_skills(ctx, ts).await;
        }
    });
    let conn = ctx.goose_for(&ts.agent).await.map_err(|e| format!("goose: {e}"))?;
    let made = conn.call("session/new", new_session(&ctx.config, ts, view.is_some()), Some(Duration::from_millis(ANSWER_MS_MAX))).await;
    let made = made.map_err(|e| format!("goose made no session: {e}"))?;
    let session = made["sessionId"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| format!("goose made no session: {made}"))?.to_string();
    let framed = json!({ "sessionId": session, "mode": "append", "key": "fragment", "text": system_prompt(view.is_some()) });
    if let Err(e) = conn.call("_goose/unstable/session/system-prompt/set", framed, Some(Duration::from_millis(ANSWER_MS_MAX))).await {
        close(&conn, &session);
        return Err(format!("goose took no system prompt: {e}"));
    }
    let updates = conn.follow(&session);
    crate::ev!("goose.session", { "turn": ts.turn, "agent": ts.agent.fragment, "session": session, "view": view.as_ref().map(String::len) });
    Ok((conn, session, updates, prompt(view.as_deref(), ts)))
}

/// Closes a turn's session (and its MCP servers), never waiting on it.
fn close(conn: &Arc<Conn>, session: &str) {
    conn.forget(session);
    let (conn, params) = (conn.clone(), json!({ "sessionId": session }));
    tokio::spawn(async move {
        if let Err(e) = conn.call("session/close", params, Some(Duration::from_millis(ANSWER_MS_MAX))).await {
            crate::ev!("goose.unclosed", { "agent": conn.agent, "error": e.to_string() });
        }
    });
}

async fn turn(ctx: Arc<Ctx>, ts: TurnStart, mut heard: mpsc::Receiver<Heard>, events: mpsc::Sender<Event>) {
    let id = ts.turn.clone();
    let emit = |out: Vec<Event>| {
        let events = events.clone();
        async move {
            for e in out {
                let _ = events.send(e).await;
            }
        }
    };
    let prepared = tokio::select! {
        p = prepare(&ctx, &ts) => p,
        h = heard.recv() => {
            if !matches!(h, Some(Heard::Forget)) {
                emit(ended(&id, Outcome::Stopped)).await;
            }
            return;
        }
    };
    let (conn, session, mut updates, blocks) = match prepared {
        Ok(p) => p,
        Err(e) => {
            crate::ev!("goose.turn_failed", { "turn": id, "error": e });
            emit(ended(&id, Outcome::Error(e))).await;
            return;
        }
    };
    let mut map = Mapper::new(&id);
    let blocks: Vec<Value> = blocks.into_iter().map(|text| json!({ "type": "text", "text": text })).collect();
    let prompt = conn.call("session/prompt", json!({ "sessionId": session, "prompt": blocks }), None);
    tokio::pin!(prompt);
    let (mut stopping, mut following, mut hearing) = (false, true, true);
    // bounded by the prompt: it answers, or its goose dies (and it answers Gone)
    let answer = loop {
        tokio::select! {
            biased;
            u = updates.recv(), if following => match u {
                Some(u) => emit(map.update(&u)).await,
                None => following = false,
            },
            a = &mut prompt => break a,
            h = heard.recv(), if hearing => match h {
                Some(Heard::Stop) if !stopping => {
                    stopping = true;
                    conn.notify("session/cancel", json!({ "sessionId": session }));
                }
                Some(Heard::Stop) => {}
                Some(Heard::Forget) => {
                    conn.notify("session/cancel", json!({ "sessionId": session }));
                    close(&conn, &session);
                    return;
                }
                None => hearing = false,
            },
        }
    };
    // what goose said before it answered, in order
    while let Ok(u) = updates.try_recv() {
        emit(map.update(&u)).await;
    }
    close(&conn, &session);
    emit(map.end(answer.as_ref(), stopping)).await;
}

// ---- what goose says, as the bridge's events ----

/// A tool call asked, its result not yet in.
struct Call {
    tool: String,
    args: String,
    text: String,
}

/// One turn's `session/update`s and its prompt's answer, as the bridge's
/// runtime events (pure: the tests drive it).
pub struct Mapper {
    turn: String,
    /// The words since the last tool call.
    said: String,
    /// The message they are of (ACP's `messageId`).
    message: Option<String>,
    /// A draft of them is shown.
    drafted: bool,
    calls: HashMap<String, Call>,
}

/// A turn's end, said: its one reply, `(ended: <outcome>: <why>)`, then the
/// end (a turn with final words says those instead: `Mapper::end`).
pub fn ended(turn: &str, outcome: Outcome) -> Vec<Event> {
    let said = match &outcome {
        Outcome::Idle => "(ended: idle: goose said nothing after its last step)".to_string(),
        Outcome::Stopped => "(ended: stopped: its asker stopped it)".to_string(),
        Outcome::Error(why) => format!("(ended: error: {why})"),
    };
    vec![Event::Reply { turn: turn.to_string(), part: 1, text: said }, Event::End { turn: turn.to_string(), outcome }]
}

/// A text cut to at most `max` bytes, on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A tool call's name as goose's extension knows it (`developer__shell`
/// is `shell`), else its title.
fn tool_name(u: &Value) -> String {
    let named = u["_meta"]["goose"]["toolCall"]["toolName"].as_str().map(|n| n.rsplit("__").next().unwrap_or(n).to_string());
    named.or_else(|| u["title"].as_str().map(str::to_string)).unwrap_or_else(|| "tool".into())
}

/// A call's arguments, shortest first: its command, its path, else all of
/// them.
fn args_of(input: &Value) -> String {
    for key in ["command", "path", "query", "url"] {
        if let Some(s) = input[key].as_str() {
            return cut(s, EXCERPT_KEPT_MAX_BYTES).to_string();
        }
    }
    match input {
        Value::Null => String::new(),
        v => cut(&v.to_string(), EXCERPT_KEPT_MAX_BYTES).to_string(),
    }
}

/// What a call's result said: its text content, else its structured
/// output (a shell's stdout and stderr).
fn excerpt_of(u: &Value) -> String {
    let texts: Vec<&str> = u["content"].as_array().map(|c| c.iter().filter_map(|c| c["content"]["text"].as_str()).collect()).unwrap_or_default();
    let mut text = texts.join("\n");
    if text.trim().is_empty() {
        let out = &u["rawOutput"];
        text = match (out["stdout"].as_str(), out["stderr"].as_str()) {
            (Some(o), Some(e)) => format!("{o}{e}"),
            (Some(o), None) => o.to_string(),
            _ if out.is_null() => String::new(),
            _ => out.to_string(),
        };
    }
    cut(text.trim(), EXCERPT_KEPT_MAX_BYTES).to_string()
}

impl Mapper {
    pub fn new(turn: &str) -> Mapper {
        Mapper { turn: turn.to_string(), said: String::new(), message: None, drafted: false, calls: HashMap::new() }
    }

    /// One `session/update`'s `update`.
    pub fn update(&mut self, u: &Value) -> Vec<Event> {
        let turn = self.turn.clone();
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let Some(text) = u["content"]["text"].as_str().filter(|_| u["content"]["type"] == "text") else { return Vec::new() };
                let message = u["messageId"].as_str();
                // another message without a tool call between: a paragraph
                if message.is_some() && self.message.as_deref() != message && !self.said.trim().is_empty() {
                    self.said.push_str("\n\n");
                }
                if message.is_some() {
                    self.message = message.map(str::to_string);
                }
                if self.said.len() + text.len() <= SAID_MAX_BYTES {
                    self.said.push_str(text);
                }
                if self.said.trim().is_empty() {
                    return Vec::new();
                }
                self.drafted = true;
                vec![Event::Draft { turn, text: self.said.trim_start().to_string() }]
            }
            Some("tool_call") => {
                let words = std::mem::take(&mut self.said).trim().to_string();
                self.message = None;
                let mut out = Vec::new();
                if std::mem::take(&mut self.drafted) {
                    // its words are the step's now
                    out.push(Event::Draft { turn, text: String::new() });
                }
                let Some(id) = u["toolCallId"].as_str() else { return out };
                if self.calls.len() >= CALLS_OPEN_MAX {
                    crate::ev!("goose.calls_bounded", { "turn": self.turn, "max": CALLS_OPEN_MAX });
                    return out;
                }
                self.calls.insert(id.to_string(), Call { tool: tool_name(u), args: args_of(&u["rawInput"]), text: words });
                out
            }
            Some("tool_call_update") => {
                let ok = match u["status"].as_str() {
                    Some("completed") => true,
                    Some("failed") => false,
                    _ => return Vec::new(),
                };
                let Some(call) = u["toolCallId"].as_str().and_then(|id| self.calls.remove(id)) else { return Vec::new() };
                vec![Event::Step { turn, step: Step { tool: call.tool, args: call.args, ok, excerpt: excerpt_of(u), text: call.text } }]
            }
            _ => Vec::new(),
        }
    }

    /// The prompt's answer (`stopReason`), or why none came; `stopped`: the
    /// asker pressed Stop. The turn's one reply is its final words when it
    /// ended idle with some, else what ended it.
    pub fn end(&mut self, answer: Result<&Value, &AcpError>, stopped: bool) -> Vec<Event> {
        let turn = self.turn.clone();
        let outcome = match answer {
            Ok(a) => match a["stopReason"].as_str() {
                Some("end_turn" | "max_tokens" | "max_turn_requests") => Outcome::Idle,
                Some("cancelled") if stopped => Outcome::Stopped,
                Some("cancelled") => Outcome::Error("goose cancelled the turn".into()),
                Some("refusal") => Outcome::Error("the model refused".into()),
                _ => Outcome::Error(format!("goose ended the turn: {a}")),
            },
            Err(e) => Outcome::Error(format!("goose: {e}")),
        };
        let words = self.said.trim();
        if outcome != Outcome::Idle || words.is_empty() {
            return ended(&turn, outcome);
        }
        vec![Event::Reply { turn: turn.clone(), part: 1, text: words.to_string() }, Event::End { turn, outcome }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str, message: &str) -> Value {
        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text }, "messageId": message })
    }

    fn call(id: &str, tool: &str, input: Value) -> Value {
        json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("developer: {tool}"), "status": "pending", "rawInput": input, "_meta": { "goose": { "toolCall": { "toolName": format!("developer__{tool}"), "extensionName": "developer" } } } })
    }

    fn done(id: &str, status: &str, text: &str) -> Value {
        json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": status, "content": [{ "type": "content", "content": { "type": "text", "text": text } }] })
    }

    fn draft(text: &str) -> Event {
        Event::Draft { turn: "t".into(), text: text.into() }
    }

    /// Goal: words stream as drafts, the words before a tool call are its
    /// step's (the draft stopped), a step posts once its result is in, and
    /// the words after the last call are the one reply, then the end.
    #[test]
    fn chunks_calls_and_the_end_are_drafts_steps_and_one_reply() {
        let mut m = Mapper::new("t");
        assert_eq!(m.update(&chunk("Let me ", "m1")), vec![draft("Let me ")]);
        assert_eq!(m.update(&chunk("look.", "m1")), vec![draft("Let me look.")]);
        assert_eq!(m.update(&call("c1", "shell", json!({ "command": "ls -la" }))), vec![draft("")]);
        assert_eq!(m.update(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "in_progress" })), vec![], "a call in progress is no step yet");
        let step = Step { tool: "shell".into(), args: "ls -la".into(), ok: true, excerpt: "a.txt".into(), text: "Let me look.".into() };
        assert_eq!(m.update(&done("c1", "completed", "a.txt\n")), vec![Event::Step { turn: "t".into(), step }]);
        assert_eq!(m.update(&done("c1", "completed", "again")), vec![], "a call's result counts once");
        // a call with no words before it, failed; its result's text from rawOutput
        assert_eq!(m.update(&call("c2", "shell", json!({ "command": "false" }))), vec![], "nothing drafted, nothing stopped");
        let failed = m.update(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c2", "status": "failed", "rawOutput": { "stdout": "", "stderr": "boom" } }));
        assert_eq!(failed, vec![Event::Step { turn: "t".into(), step: Step { tool: "shell".into(), args: "false".into(), ok: false, excerpt: "boom".into(), text: String::new() } }]);
        // thoughts, usage and goose's own notices say nothing
        for quiet in [json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "hmm" } }), json!({ "sessionUpdate": "usage_update", "used": 1 }), json!({ "sessionUpdate": "session_info_update" })] {
            assert_eq!(m.update(&quiet), vec![]);
        }
        assert_eq!(m.update(&chunk("Found ", "m2")), vec![draft("Found ")]);
        assert_eq!(m.update(&chunk("a.txt.", "m2")), vec![draft("Found a.txt.")]);
        let end = m.end(Ok(&json!({ "stopReason": "end_turn" })), false);
        assert_eq!(end, vec![Event::Reply { turn: "t".into(), part: 1, text: "Found a.txt.".into() }, Event::End { turn: "t".into(), outcome: Outcome::Idle }]);
    }

    /// Goal: two messages with no call between are two paragraphs of one
    /// reply; a turn whose words were all a step's still says one reply, of
    /// how it ended, never those words.
    #[test]
    fn messages_join_and_a_silent_end_says_how_it_ended() {
        let mut m = Mapper::new("t");
        m.update(&chunk("One.", "m1"));
        assert_eq!(m.update(&chunk("Two.", "m2")), vec![draft("One.\n\nTwo.")]);
        let mut m = Mapper::new("t");
        m.update(&chunk("Writing it now.", "m1"));
        m.update(&call("c1", "write", json!({ "path": "/data/work/a.txt", "content": "x" })));
        let steps = m.update(&done("c1", "completed", "wrote"));
        assert!(matches!(&steps[..], [Event::Step { step, .. }] if step.args == "/data/work/a.txt" && step.tool == "write" && step.text == "Writing it now."));
        let end = m.end(Ok(&json!({ "stopReason": "end_turn" })), false);
        assert_eq!(end, vec![Event::Reply { turn: "t".into(), part: 1, text: "(ended: idle: goose said nothing after its last step)".into() }, Event::End { turn: "t".into(), outcome: Outcome::Idle }]);
    }

    /// Goal: each way a prompt ends, each with exactly one reply. A Stop is
    /// `stopped`; a cancel nobody asked for, a refusal, an unknown reason and
    /// a goose that died are errors; each says so as its reply, whatever it
    /// said before. A cut answer is idle, its words the reply.
    #[test]
    fn every_end() {
        let ended = |answer: Result<Value, AcpError>, stopped: bool, said: &str| {
            let mut m = Mapper::new("t");
            if !said.is_empty() {
                m.update(&chunk(said, "m1"));
            }
            m.end(answer.as_ref(), stopped)
        };
        let end = |o: Outcome| Event::End { turn: "t".into(), outcome: o };
        let reply = |t: &str| Event::Reply { turn: "t".into(), part: 1, text: t.into() };
        assert_eq!(ended(Ok(json!({ "stopReason": "cancelled" })), true, "half"), vec![reply("(ended: stopped: its asker stopped it)"), end(Outcome::Stopped)]);
        assert_eq!(ended(Ok(json!({ "stopReason": "cancelled" })), false, ""), vec![reply("(ended: error: goose cancelled the turn)"), end(Outcome::Error("goose cancelled the turn".into()))]);
        assert_eq!(ended(Ok(json!({ "stopReason": "refusal" })), false, "no"), vec![reply("(ended: error: the model refused)"), end(Outcome::Error("the model refused".into()))]);
        assert_eq!(ended(Ok(json!({ "stopReason": "max_tokens" })), false, "cut"), vec![reply("cut"), end(Outcome::Idle)]);
        assert_eq!(ended(Ok(json!({ "stopReason": "max_turn_requests" })), false, ""), vec![reply("(ended: idle: goose said nothing after its last step)"), end(Outcome::Idle)]);
        assert!(matches!(&ended(Ok(json!({ "stopReason": "weird" })), false, "")[..], [Event::Reply { .. }, Event::End { outcome: Outcome::Error(e), .. }] if e.contains("weird")));
        assert_eq!(ended(Err(AcpError::Gone), false, "so far"), vec![reply("(ended: error: goose: it stopped)"), end(Outcome::Error("goose: it stopped".into()))]);
        let refused = AcpError::Refused { code: -32603, message: "Error in agent response stream: 402".into() };
        assert_eq!(ended(Err(refused), false, ""), vec![reply("(ended: error: goose: Error in agent response stream: 402 (-32603))"), end(Outcome::Error("goose: Error in agent response stream: 402 (-32603)".into()))]);
        // a turn ended before goose ran it (a Stop while it was prepared, a goose that did not start)
        assert_eq!(super::ended("t", Outcome::Error("goose: it did not start: no such file".into())), vec![reply("(ended: error: goose: it did not start: no such file)"), end(Outcome::Error("goose: it did not start: no such file".into()))]);
    }

    fn ts(text: &str) -> TurnStart {
        let agent = Agent { fragment: "hands.paul".into(), identity: "id:hands".into(), name: "hands".into(), owner: "id:paul".into(), credentials: vec![] };
        TurnStart { turn: "t1".into(), agent, fragment: "mind.paul".into(), chat_name: "mind".into(), seq: 3, asker: "fragment:mind.paul".into(), asker_name: "mind".into(), text: text.into(), attachments: vec![], files: vec![], routine: false, claim_seq: None, note: None }
    }

    /// Goal: a mind's turn is its view, then its task, under a system prompt
    /// of the framing and VIEW_DOC; any other fragment's is its text alone;
    /// a cut turn's note and its files are said around the text.
    #[test]
    fn the_prompt() {
        let sys = system_prompt(true);
        assert!(sys.starts_with(HANDS) && sys.contains("You are a subagent of Mind, an AI agent") && sys.contains("The view: the whole chat between Mind") && !sys.contains("OptChat"), "{sys}");
        assert_eq!(system_prompt(true), sys, "the same bytes every turn");
        assert_eq!(system_prompt(false), HANDS, "any other turn: the computer and its tools alone");
        assert!(HANDS.contains("`fragment` CLI") && HANDS.contains("load the `fragment` skill"), "fragments, top of mind");
        assert_eq!(prompt(Some("<chat>\n0+1|user: hi\n</chat>"), &ts("Find my notes")), vec!["<chat>\n0+1|user: hi\n</chat>", "Find my notes"]);
        assert_eq!(prompt(None, &ts("hello")), vec!["hello"]);
        let mut cut = ts("hello");
        cut.note = Some("Your turn before this one was cut short.".into());
        cut.files = vec![crate::runtime::LocalFile { path: "/tmp/bridge-media/x/cat.png".into(), media_type: "image/png".into(), name: "cat.png".into(), size: 3 }];
        assert_eq!(prompt(None, &cut), vec!["Your turn before this one was cut short.\n\nhello\n\nIts files, on this computer:\n- /tmp/bridge-media/x/cat.png (image/png, 3 bytes)"]);
    }

    fn config() -> GooseConfig {
        GooseConfig { command: "/usr/local/bin/goose".into(), args: vec![], work: "/data/work".into(), home: "/data/work/home".into(), root: "/tmp/goose".into(), api: "http://api.fragment.internal".into(), model: "http://model.fragment.internal".into(), tier: "medium".into(), cli: Some("/usr/local/bin/fragment".into()), ca: None, desktop: None, skills: false }
    }

    /// Goal: a mind's session gets `fragment mcp <mind>` as the agent, any
    /// other's none, and every session a title of ours (no naming call).
    #[test]
    fn a_minds_session_has_its_mcp() {
        let s = new_session(&config(), &ts("x"), true);
        assert_eq!(s["cwd"], "/data/work");
        assert_eq!(s["_meta"]["sessionTitle"], "turn t1");
        assert_eq!(s["mcpServers"], json!([{ "name": "mind", "command": "/usr/local/bin/fragment", "args": ["mcp", "mind.paul"], "env": [{ "name": "FRAGMENT_AS_AGENT", "value": "hands.paul" }, { "name": "FRAGMENT_FOR", "value": "id:paul" }, { "name": "FRAGMENT_API", "value": "http://api.fragment.internal" }] }]));
        assert_eq!(new_session(&config(), &ts("x"), false)["mcpServers"], json!([]));
        let mut no_cli = config();
        no_cli.cli = None;
        assert_eq!(new_session(&no_cli, &ts("x"), true)["mcpServers"], json!([]));
    }

    /// Goal: on an image with a desktop, every session gets the agent's own
    /// browser, computer and web tools (the browser's and the computer's
    /// naming the agent), a mind's its view's besides.
    #[test]
    fn every_session_has_the_desktops_tools() {
        let mut cfg = config();
        cfg.desktop = Some("/usr/local/bin/fragment-desktop".into());
        let s = new_session(&cfg, &ts("x"), false);
        let servers = s["mcpServers"].as_array().unwrap();
        let named: Vec<(&str, Vec<&str>)> = servers.iter().map(|s| (s["name"].as_str().unwrap(), s["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect())).collect();
        assert_eq!(named, vec![("browser", vec!["mcp", "browser", "hands.paul"]), ("computer", vec!["mcp", "computer", "hands.paul"]), ("web", vec!["mcp", "web"])]);
        assert!(servers.iter().all(|s| s["command"] == "/usr/local/bin/fragment-desktop"));
        assert_eq!(new_session(&cfg, &ts("x"), true)["mcpServers"].as_array().unwrap().len(), 4, "a mind's view too");
        let env: HashMap<String, String> = environment(&cfg, &ts("x").agent, Some(12)).into_iter().collect();
        assert_eq!(env.get("DISPLAY").map(String::as_str), Some(":12"), "its goose and tools on its own display");
        assert!(!environment(&cfg, &ts("x").agent, None).iter().any(|(k, _)| k == "DISPLAY"));
    }

    /// Goal: an agent's goose calls the model as the agent, its shell's CLI
    /// acts as it, its credentials are in their variables, and nothing of
    /// goose's own compacts, names, asks or reports.
    #[test]
    fn an_agents_environment() {
        let mut a = ts("x").agent;
        a.credentials = vec![crate::runtime::Credential { provider: "perplexity".into(), kind: "operator".into(), env: vec!["PERPLEXITY_API_KEY".into()], placeholder: "fck_perplexity_00".into(), hosts: vec![] }];
        let env: HashMap<String, String> = environment(&config(), &a, None).into_iter().collect();
        for (k, v) in [
            ("OPENAI_HOST", "http://model.fragment.internal"),
            ("OPENAI_CUSTOM_HEADERS", "x-fragment-agent=hands.paul"),
            ("OPENAI_API_KEY", ""),
            ("GOOSE_MODEL", "medium"),
            ("GOOSE_MODE", "auto"),
            ("GOOSE_AUTO_COMPACT_THRESHOLD", "0"),
            ("GOOSE_NO_COMPACTION", "1"),
            ("GOOSE_STABLE_SYSTEM_PROMPT", "1"),
            ("EXTENSIONS", "{}"),
            ("GOOSE_DISABLE_SESSION_NAMING", "true"),
            ("GOOSE_PATH_ROOT", "/tmp/goose/hands.paul"),
            ("FRAGMENT_AS_AGENT", "hands.paul"),
            ("FRAGMENT_FOR", "id:paul"),
            ("PERPLEXITY_API_KEY", "fck_perplexity_00"),
        ] {
            assert_eq!(env.get(k).map(String::as_str), Some(v), "{k}");
        }
        let b = Agent { credentials: vec![], ..a.clone() };
        assert_ne!(fingerprint(&a), fingerprint(&b), "a goose with other credentials is stale");
        assert!(path_part_ok("hands.paul") && !path_part_ok("../x") && !path_part_ok("a/b") && !path_part_ok(".."));
    }

    /// Goal: a line is read whole up to its bound, and one past it is an
    /// error, never a partial message.
    #[tokio::test]
    async fn lines_are_bounded() {
        let mut r = BufReader::with_capacity(4, &b"{\"a\":1}\n\n0123456789\nlast"[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line(&mut r, &mut buf, 16).await.unwrap(), Some(()));
        assert_eq!(buf, b"{\"a\":1}");
        assert_eq!(read_line(&mut r, &mut buf, 16).await.unwrap(), Some(()));
        assert!(buf.is_empty());
        assert!(read_line(&mut r, &mut buf, 8).await.is_err(), "past its bound");
        let mut r = BufReader::new(&b"last"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 16).await.unwrap(), Some(()));
        assert_eq!(buf, b"last");
        assert_eq!(read_line(&mut r, &mut buf, 16).await.unwrap(), None);
    }
}
