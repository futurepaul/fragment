//! `fragment computer serve`: the one server on a computer (a Sprite, or
//! any machine an agent should work on). It answers the platform over HTTP
//! with goose's developer tools (shell, write, edit, tree), and journals
//! every call on disk by the model's tool-call id: a finished call returns
//! its stored result, a running one is re-attached, and one a process death
//! orphaned comes back "interrupted". So a platform that dies and replays a
//! step never runs a tool twice, and a computer that restarts never
//! silently repeats one (ported from spikes/goose-agent, branch
//! spike/goose-agent, `computer/`).
//!
//! The agent's loop drives (phase 8's first shape): the computer holds no
//! platform credential and no model key, only its own bearer token.
//!
//! `fragment computer connect` answers the same requests without a public
//! URL: it asks its agent for them (`POST <agent>/computer/poll`, a long
//! poll with the connect token its owner was given), runs each here, and
//! posts the answer back (`/computer/answer`).
//!
//! Beside goose's tools, `screenshot {url}`: a page as headless Chrome
//! shows it, answered as an image.
//!
//!   GET  /health                   open
//!   GET  /tools                    goose's tool definitions and instructions
//!   POST /calls {id, name, arguments, cwd, wait_ms}
//!                                  start or re-attach a call; waits up to wait_ms
//!   GET  /calls/{id}?wait_ms=      a call's record
//!   POST /calls/{id}/cancel        cancel a running call (a shell command's process tree is killed)

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use goose_developer::Developer;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

// Every input and every wait is bounded.
const ID_MAX: usize = 128;
const CWD_MAX: usize = 64;
const WAIT_MS_MAX: u64 = 25_000;
const BODY_BYTES_MAX: usize = 32 * 1024 * 1024;
const TOKEN_MIN: usize = 16;

pub struct ConnectArgs {
    /// The agent's base on the platform: `https://fragment.club/api/a/<agent>`.
    pub agent: String,
    pub work: PathBuf,
    pub state: PathBuf,
    /// The connect token the agent's owner was given (`fragment agent computer --connect`).
    pub token_file: PathBuf,
}
// The journal is bounded too. A replay comes within the hour: an agent
// waits at most an hour on one call (agent/src/computer.rs), and a step
// is replayed once the watchdog finds its driver gone. Keeping a finished
// record a week keeps every replay answerable; keeping every record
// forever kept every result on disk forever.
const CALL_RETENTION_MS: u64 = 7 * 24 * 3600 * 1000;
/// Finished records kept at most, the oldest pruned first.
const CALLS_KEPT_MAX: usize = 10_000;
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
/// A pid file younger than this may belong to a call that is just
/// starting, so pruning leaves it.
const PID_FILE_GRACE_MS: u64 = 60_000;

pub struct ServeArgs {
    pub listen: SocketAddr,
    /// Projects live under this directory, one per cwd name.
    pub work: PathBuf,
    /// The call journal.
    pub state: PathBuf,
    /// The bearer token the platform presents; made (0600) if missing.
    pub token_file: PathBuf,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= ID_MAX && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn valid_cwd(cwd: &str) -> bool {
    !cwd.is_empty() && cwd.len() <= CWD_MAX && cwd.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Write-then-rename, with an fsync, so a reader sees the old or the new
/// record and never half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------- the call journal

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Running,
    Done,
    Interrupted,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CallRecord {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub status: CallStatus,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    /// How many times this computer started the tool for this id. A replay
    /// never raises it; that is the property the journal exists for.
    pub runs: u32,
    pub result: Option<CallToolResult>,
}

struct CallSlot {
    record: watch::Sender<CallRecord>,
    cancel: CancellationToken,
}

fn interrupted_result() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        "Tool call was interrupted before completing: the computer restarted while it ran. \
         Check what it changed before running it again.",
    )])
}

fn modified_ms(path: &Path) -> Result<u64> {
    let modified = std::fs::metadata(path)?.modified()?;
    Ok(modified.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

struct App {
    work: PathBuf,
    state: PathBuf,
    token: String,
    calls: Mutex<HashMap<String, Arc<CallSlot>>>,
    developers: Mutex<HashMap<String, Arc<Developer>>>,
}

impl App {
    /// The server's state, its project and journal folders made.
    fn open(work: PathBuf, state: PathBuf, token: String) -> Result<Arc<App>> {
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(state.join("calls"))?;
        std::fs::create_dir_all(state.join("pids"))?;
        Ok(Arc::new(App { work, state, token, calls: Mutex::new(HashMap::new()), developers: Mutex::new(HashMap::new()) }))
    }

    fn call_path(&self, id: &str) -> PathBuf {
        self.state.join("calls").join(format!("{id}.json"))
    }

    fn pid_path(&self, id: &str) -> PathBuf {
        self.state.join("pids").join(id)
    }

    fn read_call(&self, id: &str) -> Result<Option<CallRecord>> {
        let path = self.call_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let record = serde_json::from_slice(&std::fs::read(&path)?).with_context(|| format!("corrupt call record {}", path.display()))?;
        Ok(Some(record))
    }

    fn write_call(&self, record: &CallRecord) -> Result<()> {
        write_atomic(&self.call_path(&record.id), &serde_json::to_vec(record)?)
    }

    fn developer(&self, cwd: &str) -> Result<Arc<Developer>> {
        let mut developers = self.developers.lock().expect("developers lock");
        if let Some(developer) = developers.get(cwd) {
            return Ok(developer.clone());
        }
        let dir = self.work.join(cwd);
        std::fs::create_dir_all(&dir)?;
        let developer = Arc::new(Developer::new(Some(dir), cwd)?);
        developers.insert(cwd.to_string(), developer.clone());
        Ok(developer)
    }

    /// Start the call, or attach to it. The same id never runs twice.
    ///
    /// The slot goes in first, under the lock, and the journal is read and
    /// written once the lock is released: a call that arrives meanwhile
    /// attaches to the slot, and no other call waits behind this one's
    /// disk flush. Only a slot's own task writes its id's record, and a
    /// slot leaves the map only after its record is written, so a missing
    /// slot means the journal is final for that id.
    fn start_or_attach(self: &Arc<Self>, id: &str, name: &str, mut arguments: Option<JsonObject>, cwd: &str) -> Result<watch::Receiver<CallRecord>> {
        ensure!(valid_id(id), "call ids are 1-{ID_MAX} of [A-Za-z0-9_-]");
        ensure!(valid_cwd(cwd), "cwd is 1-{CWD_MAX} of [a-z0-9-]");
        let record = CallRecord {
            id: id.to_string(),
            name: name.to_string(),
            cwd: cwd.to_string(),
            status: CallStatus::Running,
            started_at: now_ms(),
            finished_at: None,
            runs: 1,
            result: None,
        };
        let slot = {
            let mut calls = self.calls.lock().expect("calls lock");
            if let Some(slot) = calls.get(id) {
                return Ok(slot.record.subscribe());
            }
            let (sender, _) = watch::channel(record.clone());
            let slot = Arc::new(CallSlot { record: sender, cancel: CancellationToken::new() });
            calls.insert(id.to_string(), slot.clone());
            slot
        };
        let receiver = slot.record.subscribe();
        match self.open_journal(&record) {
            Ok(None) => {}
            Ok(Some(answered)) => {
                slot.record.send_replace(answered);
                self.calls.lock().expect("calls lock").remove(id);
                return Ok(receiver);
            }
            Err(error) => {
                // Nothing ran and nothing was journaled: whoever attached
                // hears so, and the next call with this id starts afresh.
                let mut failed = record;
                failed.status = CallStatus::Interrupted;
                failed.finished_at = Some(now_ms());
                failed.result = Some(CallToolResult::error(vec![ContentBlock::text(format!("the computer could not journal the call, so it did not run: {error:#}"))]));
                slot.record.send_replace(failed);
                self.calls.lock().expect("calls lock").remove(id);
                return Err(error);
            }
        }
        if name == "shell" {
            tag_command(&mut arguments, &self.pid_path(id));
        }
        let app = self.clone();
        let cancel = slot.cancel.clone();
        let (name, cwd, id) = (name.to_string(), cwd.to_string(), id.to_string());
        tokio::spawn(async move {
            let result = match (name.as_str(), app.developer(&cwd)) {
                ("screenshot", _) => screenshot(&app.state, &id, arguments, cancel).await,
                (_, Ok(developer)) => developer.call(&name, arguments, cancel).await,
                (_, Err(error)) => CallToolResult::error(vec![ContentBlock::text(format!("the computer could not open {cwd}: {error}"))]),
            };
            let mut record = record;
            record.status = CallStatus::Done;
            record.finished_at = Some(now_ms());
            record.result = Some(result);
            if let Err(error) = app.write_call(&record) {
                eprintln!("call {id}: journal write failed: {error:#}");
            }
            let _ = std::fs::remove_file(app.pid_path(&id));
            slot.record.send_replace(record);
            app.calls.lock().expect("calls lock").remove(&id);
        });
        Ok(receiver)
    }

    /// Reads the journal for a call whose slot this thread holds: the
    /// record to answer instead of running (finished, or interrupted by the
    /// death of the process that ran it), or `None` once `fresh` is written
    /// as the new call's running record.
    fn open_journal(&self, fresh: &CallRecord) -> Result<Option<CallRecord>> {
        assert_eq!(fresh.status, CallStatus::Running, "a fresh record is running");
        match self.read_call(&fresh.id)? {
            Some(mut record) => {
                if record.status == CallStatus::Running {
                    // A running record with no live slot: the process that
                    // ran it died. Say so instead of running it again, and
                    // drop its pid file (no one can cancel it now).
                    record.status = CallStatus::Interrupted;
                    record.finished_at = Some(now_ms());
                    record.result = Some(interrupted_result());
                    self.write_call(&record)?;
                    let _ = std::fs::remove_file(self.pid_path(&record.id));
                }
                assert_ne!(record.status, CallStatus::Running, "an answered record is finished");
                Ok(Some(record))
            }
            None => {
                self.write_call(fresh)?;
                Ok(None)
            }
        }
    }

    /// Bounds the journal: deletes finished records older than
    /// `retention_ms`, and the oldest finished ones past `kept_max`, and
    /// pid files no live call owns. A running record (its process died;
    /// the next attach answers it interrupted), a record with a live slot,
    /// and an unreadable record are kept. Answers how many records went.
    fn prune(&self, now: u64, retention_ms: u64, kept_max: usize) -> Result<usize> {
        let live = || -> HashSet<String> { self.calls.lock().expect("calls lock").keys().cloned().collect() };
        let at_start = live();
        // (modified, id) of every settled record, oldest first; `.tmp`
        // files are a write in flight or a crash's leftover, left alone
        let mut records: Vec<(u64, String)> = Vec::new();
        for entry in std::fs::read_dir(self.state.join("calls"))? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().to_string();
            let Some(id) = file.strip_suffix(".json").filter(|id| valid_id(id)) else { continue };
            if !at_start.contains(id) {
                records.push((modified_ms(&entry.path())?, id.to_string()));
            }
        }
        records.sort();
        let mut remaining = records.len();
        let mut deleted = 0;
        for (modified, id) in &records {
            let expired = now.saturating_sub(*modified) > retention_ms;
            if !expired && remaining <= kept_max {
                break; // oldest first: everything after is newer, and within the cap
            }
            let finished = matches!(self.read_call(id), Ok(Some(r)) if r.status != CallStatus::Running);
            if finished && !live().contains(id) {
                std::fs::remove_file(self.call_path(id))?;
                deleted += 1;
                remaining -= 1;
            }
        }
        for entry in std::fs::read_dir(self.state.join("pids"))? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().to_string();
            let settled = now.saturating_sub(modified_ms(&entry.path())?) > PID_FILE_GRACE_MS;
            if settled && !live().contains(&id) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(deleted)
    }
}

/// goose's shell runs `$SHELL -c <command>` without a process group of its
/// own, and its cancel kills only that shell: the command's children would
/// run on. So each command first writes the shell's pid under the journal
/// (a shell that execs the command in its place keeps that pid), by which a
/// cancel finds it and kills its whole tree first.
fn tag_command(arguments: &mut Option<JsonObject>, pid_file: &Path) {
    if let Some(Value::String(command)) = arguments.as_mut().and_then(|a| a.get_mut("command")) {
        let quoted = pid_file.display().to_string().replace('\'', "'\\''");
        *command = format!("echo $$ > '{quoted}'; {command}");
    }
}

/// SIGKILLs the call's shell (or what it exec'd) and everything under it;
/// a process that detached into its own session escapes, as it would a
/// process group.
fn kill_tree(pid_file: &Path) {
    let pids = |args: &[&str]| -> Vec<String> {
        std::process::Command::new("pgrep")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    };
    let Some(root) = std::fs::read_to_string(pid_file).ok().map(|p| p.trim().to_string()).filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) else {
        return;
    };
    let mut tree = vec![root];
    let mut next = 0;
    while next < tree.len() && tree.len() < 4096 {
        let children = pids(&["-P", &tree[next]]);
        tree.extend(children);
        next += 1;
    }
    if !tree.is_empty() {
        let _ = std::process::Command::new("kill").arg("-KILL").args(&tree).output();
    }
}

// ---------------------------------------------------------------- screenshots

/// Its size, and how long one may take.
const SHOT_SIZE: (u32, u32) = (1024, 640);
const SHOT_TIMEOUT: Duration = Duration::from_secs(45);

fn screenshot_tool() -> Value {
    json!({
        "name": "screenshot",
        "description": "Opens a page (an http(s) or data: URL) in this computer's headless Chrome and answers what it shows, \
                        as an image (1024×640). The chat you answer in shows it too.",
        "inputSchema": { "type": "object", "required": ["url"], "properties": { "url": { "type": "string" } } },
    })
}

/// Chrome or Chromium on this computer (`CHROME_BIN` names another).
fn chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CHROME_BIN") {
        return Some(PathBuf::from(p));
    }
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

async fn screenshot(state: &Path, id: &str, arguments: Option<JsonObject>, cancel: CancellationToken) -> CallToolResult {
    let fail = |why: String| CallToolResult::error(vec![ContentBlock::text(why)]);
    let Some(url) = arguments.as_ref().and_then(|a| a.get("url")).and_then(Value::as_str).map(str::to_string) else {
        return fail("url is required".into());
    };
    if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("data:")) {
        return fail("the url is http(s) or data:".into());
    }
    let Some(bin) = chrome() else { return fail("this computer has no Chrome: install chromium (or set CHROME_BIN)".into()) };
    let dir = state.join("shots");
    let (out, profile) = (dir.join(format!("{id}.png")), dir.join(format!("{id}.profile")));
    if let Err(e) = std::fs::create_dir_all(&profile) {
        return fail(format!("the computer could not make {}: {e}", profile.display()));
    }
    let mut command = tokio::process::Command::new(bin);
    command
        .args(["--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run", "--no-default-browser-check"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={},{}", SHOT_SIZE.0, SHOT_SIZE.1))
        .arg(format!("--screenshot={}", out.display()))
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // Chrome refuses to sandbox as root, which is how a Sprite runs it
    if std::env::var("USER").as_deref() == Ok("root") {
        command.arg("--no-sandbox");
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("Chrome did not start: {e}")),
    };
    // Chrome writes the screenshot but can linger after (its updater): the
    // file, once it stops growing, is the answer, and Chrome is stopped
    let written = async {
        let mut last = 0u64;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            if size > 0 && size == last {
                return true;
            }
            last = size;
            if let Ok(Some(_)) = child.try_wait() {
                return std::fs::metadata(&out).is_ok_and(|m| m.len() > 0);
            }
        }
    };
    let ran = tokio::select! {
        r = tokio::time::timeout(SHOT_TIMEOUT, written) => r,
        _ = cancel.cancelled() => Ok(false),
    };
    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(&profile);
    match ran {
        Err(_) => return fail(format!("the page did not finish in {SHOT_TIMEOUT:?}")),
        Ok(false) if cancel.is_cancelled() => return fail("the screenshot was cancelled".into()),
        Ok(false) => return fail("Chrome stopped without a screenshot".into()),
        Ok(true) => {}
    }
    let bytes = match std::fs::read(&out) {
        Ok(b) => b,
        Err(e) => return fail(format!("Chrome made no screenshot: {e}")),
    };
    let _ = std::fs::remove_file(&out);
    use base64::Engine;
    CallToolResult::success(vec![
        ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(bytes), "image/png"),
        ContentBlock::text(format!("A screenshot of {url} ({}×{}).", SHOT_SIZE.0, SHOT_SIZE.1)),
    ])
}

async fn wait_finished(receiver: &mut watch::Receiver<CallRecord>, wait: Duration) -> CallRecord {
    let _ = tokio::time::timeout(wait, receiver.wait_for(|r| r.status != CallStatus::Running)).await;
    receiver.borrow().clone()
}

fn clamp_wait(wait_ms: u64) -> Duration {
    Duration::from_millis(wait_ms.min(WAIT_MS_MAX))
}

// ---------------------------------------------------------------- http

type Shared = State<Arc<App>>;

struct Failure(StatusCode, String);

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure(StatusCode::BAD_REQUEST, format!("{error:#}"))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn authorize(State(app): Shared, request: Request, next: Next) -> Response {
    let presented = request.headers().get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    if !constant_time_eq(presented.as_bytes(), app.token.as_bytes()) {
        return Failure(StatusCode::UNAUTHORIZED, "bad token".into()).into_response();
    }
    next.run(request).await
}

fn tools_answer() -> Value {
    let mut tools = serde_json::to_value(Developer::tools()).unwrap_or_else(|_| json!([]));
    if let Some(list) = tools.as_array_mut() {
        list.push(screenshot_tool());
    }
    json!({ "tools": tools, "instructions": goose_developer::instructions() })
}

async fn tools() -> Json<Value> {
    Json(tools_answer())
}

#[derive(Deserialize)]
struct CallBody {
    id: String,
    name: String,
    arguments: Option<JsonObject>,
    cwd: String,
    #[serde(default)]
    wait_ms: u64,
}

#[derive(Deserialize)]
struct WaitQuery {
    #[serde(default)]
    wait_ms: u64,
}

// The routes' work, shared by `serve`'s HTTP and `connect`'s tunnel.

async fn start_call(app: &Arc<App>, body: CallBody) -> Result<CallRecord, Failure> {
    let mut receiver = app.start_or_attach(&body.id, &body.name, body.arguments, &body.cwd)?;
    Ok(wait_finished(&mut receiver, clamp_wait(body.wait_ms)).await)
}

async fn call_record(app: &Arc<App>, id: &str, wait_ms: u64) -> Result<CallRecord, Failure> {
    if !valid_id(id) {
        return Err(Failure(StatusCode::BAD_REQUEST, "bad call id".into()));
    }
    let slot = app.calls.lock().expect("calls lock").get(id).cloned();
    match slot {
        Some(slot) => {
            let mut receiver = slot.record.subscribe();
            Ok(wait_finished(&mut receiver, clamp_wait(wait_ms)).await)
        }
        None => match app.read_call(id)? {
            Some(record) => Ok(record),
            None => Err(Failure(StatusCode::NOT_FOUND, format!("no call {id}"))),
        },
    }
}

fn cancel_call(app: &Arc<App>, id: &str) -> Value {
    let slot = app.calls.lock().expect("calls lock").get(id).cloned();
    if let Some(slot) = &slot {
        if valid_id(id) && slot.record.borrow().name == "shell" {
            kill_tree(&app.pid_path(id));
        }
        slot.cancel.cancel();
    }
    json!({ "cancelled": slot.is_some() })
}

async fn call(State(app): Shared, Json(body): Json<CallBody>) -> Result<Json<CallRecord>, Failure> {
    Ok(Json(start_call(&app, body).await?))
}

async fn call_status(State(app): Shared, UrlPath(id): UrlPath<String>, Query(query): Query<WaitQuery>) -> Result<Json<CallRecord>, Failure> {
    Ok(Json(call_record(&app, &id, query.wait_ms).await?))
}

async fn call_cancel(State(app): Shared, UrlPath(id): UrlPath<String>) -> Json<Value> {
    Json(cancel_call(&app, &id))
}

/// The bearer token: read from its file, or made there (0600) the first time.
fn token(path: &Path) -> Result<String> {
    if !path.exists() {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let token = hex::encode(rand::random::<[u8; 32]>());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(path).with_context(|| format!("making {}", path.display()))?;
        writeln!(file, "{token}")?;
        eprintln!("made a new token in {}", path.display());
    }
    let value = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?.trim().to_string();
    ensure!(value.len() >= TOKEN_MIN, "{} holds no usable token (at least {TOKEN_MIN} characters)", path.display());
    Ok(value)
}

fn router(app: Arc<App>) -> Router {
    let authed = Router::new()
        .route("/tools", get(tools))
        .route("/calls", post(call))
        .route("/calls/{id}", get(call_status))
        .route("/calls/{id}/cancel", post(call_cancel))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize));
    Router::new().route("/health", get(|| async { "ok" })).merge(authed).layer(DefaultBodyLimit::max(BODY_BYTES_MAX)).with_state(app)
}

// ---------------------------------------------------------------- connect

/// A request the agent would have sent to `serve`, tunneled.
#[derive(Deserialize)]
struct Tunneled {
    rid: String,
    method: String,
    path: String,
    #[serde(default)]
    body: Value,
}

/// Answers a tunneled request as `serve`'s routes would: (status, body).
async fn dispatch(app: &Arc<App>, method: &str, path: &str, body: Value) -> (u16, Value) {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let wait_ms = query.split('&').find_map(|kv| kv.strip_prefix("wait_ms=")).and_then(|v| v.parse().ok()).unwrap_or(0);
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let answered = match (method, segments.as_slice()) {
        ("GET", ["tools"]) => Ok(tools_answer()),
        ("POST", ["calls"]) => match serde_json::from_value::<CallBody>(body) {
            Ok(b) => start_call(app, b).await.map(|r| json!(r)),
            Err(e) => Err(Failure(StatusCode::BAD_REQUEST, format!("body: {e}"))),
        },
        ("GET", ["calls", id]) => call_record(app, id, wait_ms).await.map(|r| json!(r)),
        ("POST", ["calls", id, "cancel"]) => Ok(cancel_call(app, id)),
        _ => Err(Failure(StatusCode::NOT_FOUND, format!("no route {method} {path}"))),
    };
    match answered {
        Ok(v) => (200, v),
        Err(Failure(status, why)) => (status.as_u16(), json!({ "error": why })),
    }
}

/// Answers its agent's requests until the process is stopped: a long poll
/// out to the platform, each request run here, its answer posted back.
pub fn connect(args: ConnectArgs) -> Result<()> {
    let token = std::fs::read_to_string(&args.token_file)
        .with_context(|| format!("reading {} (the connect token its owner was given)", args.token_file.display()))?
        .trim()
        .to_string();
    ensure!(token.len() >= TOKEN_MIN, "{} holds no usable token", args.token_file.display());
    let app = App::open(args.work, args.state, token.clone())?;
    let base = args.agent.trim_end_matches('/').to_string();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
        eprintln!("fragment computer answering {base} (Ctrl-C stops it)");
        let mut backoff = 1u64;
        loop {
            let polled = client.post(format!("{base}/computer/poll")).header("x-computer-token", &token).header("content-type", "application/json").body("{}").send().await;
            let requests: Vec<Tunneled> = match polled {
                Ok(r) if r.status().is_success() => {
                    backoff = 1;
                    let bytes = r.bytes().await.unwrap_or_default();
                    serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| serde_json::from_value(v["requests"].clone()).ok()).unwrap_or_default()
                }
                Ok(r) if matches!(r.status().as_u16(), 401 | 403 | 404) => {
                    anyhow::bail!("{base} refused this computer ({}): ask the agent's owner for a new connect token", r.status());
                }
                other => {
                    let why = other.map_or_else(|e| format!("the platform did not answer ({e})"), |r| format!("the platform answered {}", r.status()));
                    eprintln!("{why}; again in {backoff} s");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(30);
                    continue;
                }
            };
            for t in requests {
                let (app, client, base, token) = (app.clone(), client.clone(), base.clone(), token.clone());
                tokio::spawn(async move {
                    let (status, body) = dispatch(&app, &t.method, &t.path, t.body).await;
                    let answer = json!({ "rid": t.rid, "status": status, "body": body }).to_string();
                    // a lost answer is asked again (the calls are idempotent by id)
                    let sent = client.post(format!("{base}/computer/answer")).header("x-computer-token", &token).header("content-type", "application/json").body(answer).send().await;
                    if let Err(e) = sent {
                        eprintln!("answering {}: {e}", t.rid);
                    }
                });
            }
        }
    })
}

/// Serves until the process is stopped.
pub fn serve(args: ServeArgs) -> Result<()> {
    let app = App::open(args.work, args.state, token(&args.token_file)?)?;
    let pruned = app.prune(now_ms(), CALL_RETENTION_MS, CALLS_KEPT_MAX)?;
    if pruned > 0 {
        eprintln!("pruned {pruned} finished call record(s) from the journal");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let pruner = app.clone();
        // lives as long as the server; each pass is bounded by the journal
        tokio::spawn(async move {
            let mut every = tokio::time::interval(PRUNE_INTERVAL);
            every.tick().await; // the first tick is now, and startup just pruned
            loop {
                every.tick().await;
                let app = pruner.clone();
                match tokio::task::spawn_blocking(move || app.prune(now_ms(), CALL_RETENTION_MS, CALLS_KEPT_MAX)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => eprintln!("pruning the journal failed: {error:#}"),
                    Err(error) => eprintln!("pruning the journal failed: {error}"),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind(args.listen).await.with_context(|| format!("binding {}", args.listen))?;
        eprintln!("fragment computer serving on {}", listener.local_addr()?);
        axum::serve(listener, router(app)).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_cwds_are_bounded() {
        assert!(valid_id("tc-0123abc_DEF"));
        assert!(!valid_id("tc:abc") && !valid_id("") && !valid_id(&"a".repeat(ID_MAX + 1)));
        assert!(valid_cwd("work") && valid_cwd("my-app-2"));
        assert!(!valid_cwd("../etc") && !valid_cwd("Work") && !valid_cwd("a/b") && !valid_cwd(""));
    }

    #[test]
    fn a_shell_command_first_writes_its_pid() {
        let mut arguments: Option<JsonObject> = serde_json::from_value(json!({ "command": "sleep 1 && echo done" })).ok();
        tag_command(&mut arguments, Path::new("/state/it's/pids/tc-abc"));
        assert_eq!(arguments.unwrap()["command"], "echo $$ > '/state/it'\\''s/pids/tc-abc'; sleep 1 && echo done");
    }

    /// A shell may exec the last command of `-c` in place of itself (bash
    /// and zsh do); that command's own children must still die.
    #[tokio::test]
    async fn cancelling_kills_the_children_of_an_execed_command() {
        cancel_kills_children("sh -c '{marker}; echo late > late.txt'").await;
    }

    async fn cancel_kills_children(template: &str) {
        let (dir, app) = test_app();
        let marker = format!("sleep 29.{}", rand::random::<u16>());
        let arguments = serde_json::from_value(json!({ "command": template.replace("{marker}", &marker) })).ok();
        let mut receiver = app.start_or_attach("tc-cancel", "shell", arguments, "work").unwrap();
        let alive = || std::process::Command::new("pgrep").args(["-f", &marker]).output().unwrap().status.success();
        let t0 = std::time::Instant::now();
        while !alive() && t0.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(alive(), "the command started");
        let _ = call_cancel(State(app.clone()), UrlPath("tc-cancel".into())).await;
        let record = wait_finished(&mut receiver, Duration::from_secs(5)).await;
        assert_eq!(record.status, CallStatus::Done);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!alive(), "the command's child was killed");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn test_app() -> (PathBuf, Arc<App>) {
        let dir = std::env::temp_dir().join(format!("fragment-computer-test-{}", hex::encode(rand::random::<[u8; 4]>())));
        let app = App::open(dir.join("work"), dir.join("state"), "x".repeat(32)).unwrap();
        (dir, app)
    }

    fn record(id: &str, status: CallStatus) -> CallRecord {
        let finished = status != CallStatus::Running;
        CallRecord {
            id: id.into(),
            name: "shell".into(),
            cwd: "work".into(),
            status,
            started_at: 1,
            finished_at: finished.then_some(2),
            runs: 1,
            result: finished.then(|| CallToolResult::success(vec![ContentBlock::text(format!("the result of {id}"))])),
        }
    }

    fn age(path: &Path, ms: u64) {
        let at = SystemTime::now() - Duration::from_millis(ms);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
    }

    const DAY_MS: u64 = 24 * 3600 * 1000;

    /// Goal: pruning bounds the journal without losing what a replay
    /// needs. Method: records of each kind, some older than the retention;
    /// only the old finished ones go, and only pid files no call owns.
    #[test]
    fn pruning_keeps_running_and_recent_records() {
        let (dir, app) = test_app();
        for (id, status, days) in [
            ("old-done", CallStatus::Done, 8),
            ("old-interrupted", CallStatus::Interrupted, 8),
            ("old-running", CallStatus::Running, 8),
            ("old-live", CallStatus::Done, 8),
            ("recent-done", CallStatus::Done, 6),
            ("fresh-done", CallStatus::Done, 0),
        ] {
            app.write_call(&record(id, status)).unwrap();
            age(&app.call_path(id), days * DAY_MS);
        }
        // a slot for old-live: a call re-attached to its old record right now
        let (sender, _) = watch::channel(record("old-live", CallStatus::Running));
        app.calls.lock().unwrap().insert("old-live".into(), Arc::new(CallSlot { record: sender, cancel: CancellationToken::new() }));
        for (id, ms) in [("old-done", DAY_MS), ("old-live", DAY_MS), ("just-started", 0)] {
            std::fs::write(app.pid_path(id), "123").unwrap();
            age(&app.pid_path(id), ms);
        }
        std::fs::write(app.state.join("calls").join("half-written.tmp"), "{").unwrap();

        let deleted = app.prune(now_ms(), 7 * DAY_MS, 100).unwrap();
        assert_eq!(deleted, 2);
        let kept = |id: &str| app.call_path(id).exists();
        assert!(!kept("old-done") && !kept("old-interrupted"), "finished records past the retention go");
        assert!(kept("old-running"), "a running record stays for its next attach to answer");
        assert!(kept("old-live"), "a record with a live slot stays");
        assert!(kept("recent-done") && kept("fresh-done"), "records within the retention stay");
        assert!(app.state.join("calls").join("half-written.tmp").exists(), "a write in flight is not a record");
        assert!(!app.pid_path("old-done").exists(), "a pid file no call owns goes");
        assert!(app.pid_path("old-live").exists(), "a live call's pid file stays");
        assert!(app.pid_path("just-started").exists(), "a pid file a starting call may own stays");
        assert_eq!(app.prune(now_ms(), 7 * DAY_MS, 100).unwrap(), 0, "a second pass finds nothing");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pruning_keeps_at_most_the_newest_finished_records() {
        let (dir, app) = test_app();
        for i in 0..5u64 {
            let id = format!("done-{i}");
            app.write_call(&record(&id, CallStatus::Done)).unwrap();
            age(&app.call_path(&id), (10 - i) * 60_000); // done-4 is the newest
        }
        app.write_call(&record("running", CallStatus::Running)).unwrap();
        age(&app.call_path("running"), 3_600_000);
        assert_eq!(app.prune(now_ms(), 7 * DAY_MS, 3).unwrap(), 3);
        let left: Vec<bool> = (0..5).map(|i| app.call_path(&format!("done-{i}")).exists()).collect();
        assert_eq!(left, [false, false, false, true, true], "the oldest finished go first");
        assert!(app.call_path("running").exists(), "never a running record");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A replay of a finished call answers its record and runs nothing; a
    /// call whose process died comes back interrupted, its pid file gone;
    /// neither leaves a slot behind.
    #[tokio::test]
    async fn the_journal_answers_replays_without_running() {
        let (dir, app) = test_app();
        let marker = app.work.join("work").join("ran.txt");
        let arguments = || serde_json::from_value(json!({ "command": format!("touch '{}'", marker.display()) })).ok();
        app.write_call(&record("tc-done", CallStatus::Done)).unwrap();
        let mut receiver = app.start_or_attach("tc-done", "shell", arguments(), "work").unwrap();
        let answered = wait_finished(&mut receiver, Duration::from_secs(5)).await;
        assert_eq!(answered.status, CallStatus::Done);
        assert_eq!(serde_json::to_value(&answered.result).unwrap(), serde_json::to_value(&record("tc-done", CallStatus::Done).result).unwrap());
        assert!(!app.calls.lock().unwrap().contains_key("tc-done"));

        app.write_call(&record("tc-died", CallStatus::Running)).unwrap();
        std::fs::write(app.pid_path("tc-died"), "123").unwrap();
        let mut receiver = app.start_or_attach("tc-died", "shell", arguments(), "work").unwrap();
        let answered = wait_finished(&mut receiver, Duration::from_secs(5)).await;
        assert_eq!(answered.status, CallStatus::Interrupted);
        assert_eq!(app.read_call("tc-died").unwrap().unwrap().status, CallStatus::Interrupted, "journaled as interrupted");
        assert!(!app.pid_path("tc-died").exists(), "the dead call's pid file is gone");
        assert!(!app.calls.lock().unwrap().contains_key("tc-died"));
        assert!(!marker.exists(), "neither ran the command");

        // a new call runs once, journaled as done; its replay is the record
        let mut receiver = app.start_or_attach("tc-new", "shell", arguments(), "work").unwrap();
        let done = wait_finished(&mut receiver, Duration::from_secs(10)).await;
        assert_eq!((done.status, done.runs), (CallStatus::Done, 1));
        assert!(marker.exists(), "the new call ran");
        std::fs::remove_file(&marker).unwrap();
        let t0 = std::time::Instant::now();
        while app.calls.lock().unwrap().contains_key("tc-new") && t0.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut receiver = app.start_or_attach("tc-new", "shell", arguments(), "work").unwrap();
        assert_eq!(wait_finished(&mut receiver, Duration::from_secs(5)).await.status, CallStatus::Done);
        assert!(!marker.exists(), "the replay did not run it again");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_token_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("fragment-computer-test-{}", hex::encode(rand::random::<[u8; 4]>())));
        let path = dir.join("token");
        let first = token(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(token(&path).unwrap(), first);
        std::fs::write(&path, "short\n").unwrap();
        assert!(token(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
