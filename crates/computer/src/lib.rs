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
//!   GET  /health                   open
//!   GET  /tools                    goose's tool definitions and instructions
//!   POST /calls {id, name, arguments, cwd, wait_ms}
//!                                  start or re-attach a call; waits up to wait_ms
//!   GET  /calls/{id}?wait_ms=      a call's record
//!   POST /calls/{id}/cancel        cancel a running call (a shell command's process tree is killed)

use std::collections::HashMap;
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

struct App {
    work: PathBuf,
    state: PathBuf,
    token: String,
    calls: Mutex<HashMap<String, Arc<CallSlot>>>,
    developers: Mutex<HashMap<String, Arc<Developer>>>,
}

impl App {
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
    fn start_or_attach(self: &Arc<Self>, id: &str, name: &str, mut arguments: Option<JsonObject>, cwd: &str) -> Result<watch::Receiver<CallRecord>> {
        ensure!(valid_id(id), "call ids are 1-{ID_MAX} of [A-Za-z0-9_-]");
        ensure!(valid_cwd(cwd), "cwd is 1-{CWD_MAX} of [a-z0-9-]");
        let mut calls = self.calls.lock().expect("calls lock");
        if let Some(slot) = calls.get(id) {
            return Ok(slot.record.subscribe());
        }
        if let Some(mut record) = self.read_call(id)? {
            if record.status == CallStatus::Running {
                // No live slot but a running record: this process died
                // while the tool ran. Say so instead of running it again.
                record.status = CallStatus::Interrupted;
                record.finished_at = Some(now_ms());
                record.result = Some(interrupted_result());
                self.write_call(&record)?;
            }
            let (_, receiver) = watch::channel(record);
            return Ok(receiver);
        }
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
        self.write_call(&record)?;
        if name == "shell" {
            tag_command(&mut arguments, &self.pid_path(id));
        }
        let (sender, receiver) = watch::channel(record.clone());
        let cancel = CancellationToken::new();
        let slot = Arc::new(CallSlot { record: sender, cancel: cancel.clone() });
        calls.insert(id.to_string(), slot.clone());
        drop(calls);

        let app = self.clone();
        let (name, cwd, id) = (name.to_string(), cwd.to_string(), id.to_string());
        tokio::spawn(async move {
            let result = match app.developer(&cwd) {
                Ok(developer) => developer.call(&name, arguments, cancel).await,
                Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("the computer could not open {cwd}: {error}"))]),
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

async fn wait_finished(receiver: &mut watch::Receiver<CallRecord>, wait: Duration) -> CallRecord {
    let _ = tokio::time::timeout(wait, receiver.wait_for(|r| r.status != CallStatus::Running)).await;
    let record = receiver.borrow().clone();
    record
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

async fn tools() -> Json<Value> {
    Json(json!({ "tools": Developer::tools(), "instructions": goose_developer::instructions() }))
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

async fn call(State(app): Shared, Json(body): Json<CallBody>) -> Result<Json<CallRecord>, Failure> {
    let mut receiver = app.start_or_attach(&body.id, &body.name, body.arguments, &body.cwd)?;
    Ok(Json(wait_finished(&mut receiver, clamp_wait(body.wait_ms)).await))
}

async fn call_status(State(app): Shared, UrlPath(id): UrlPath<String>, Query(query): Query<WaitQuery>) -> Result<Json<CallRecord>, Failure> {
    if !valid_id(&id) {
        return Err(Failure(StatusCode::BAD_REQUEST, "bad call id".into()));
    }
    let slot = app.calls.lock().expect("calls lock").get(&id).cloned();
    match slot {
        Some(slot) => {
            let mut receiver = slot.record.subscribe();
            Ok(Json(wait_finished(&mut receiver, clamp_wait(query.wait_ms)).await))
        }
        None => match app.read_call(&id)? {
            Some(record) => Ok(Json(record)),
            None => Err(Failure(StatusCode::NOT_FOUND, format!("no call {id}"))),
        },
    }
}

async fn call_cancel(State(app): Shared, UrlPath(id): UrlPath<String>) -> Json<Value> {
    let slot = app.calls.lock().expect("calls lock").get(&id).cloned();
    if let Some(slot) = &slot {
        if valid_id(&id) && slot.record.borrow().name == "shell" {
            kill_tree(&app.pid_path(&id));
        }
        slot.cancel.cancel();
    }
    Json(json!({ "cancelled": slot.is_some() }))
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

/// Serves until the process is stopped.
pub fn serve(args: ServeArgs) -> Result<()> {
    std::fs::create_dir_all(&args.work)?;
    std::fs::create_dir_all(args.state.join("calls"))?;
    std::fs::create_dir_all(args.state.join("pids"))?;
    let app = Arc::new(App {
        work: args.work,
        state: args.state,
        token: token(&args.token_file)?,
        calls: Mutex::new(HashMap::new()),
        developers: Mutex::new(HashMap::new()),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
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

    #[tokio::test]
    async fn cancelling_a_shell_call_kills_its_children() {
        cancel_kills_children("{marker} && echo late > late.txt").await;
    }

    /// A shell may exec the last command of `-c` in place of itself (bash
    /// and zsh do); that command's own children must still die.
    #[tokio::test]
    async fn cancelling_kills_the_children_of_an_execed_command() {
        cancel_kills_children("sh -c '{marker}; echo late > late.txt'").await;
    }

    async fn cancel_kills_children(template: &str) {
        let dir = std::env::temp_dir().join(format!("fragment-computer-test-{}", hex::encode(rand::random::<[u8; 4]>())));
        let app = Arc::new(App {
            work: dir.join("work"),
            state: dir.join("state"),
            token: "x".repeat(32),
            calls: Mutex::new(HashMap::new()),
            developers: Mutex::new(HashMap::new()),
        });
        std::fs::create_dir_all(app.state.join("calls")).unwrap();
        std::fs::create_dir_all(app.state.join("pids")).unwrap();
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

    #[test]
    fn a_token_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("fragment-computer-test-{}", hex::encode(rand::random::<[u8; 4]>())));
        let path = dir.join("token");
        let first = token(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(token(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&path, "short\n").unwrap();
        assert!(token(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
