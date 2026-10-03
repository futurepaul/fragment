//! The bridge's I/O around its pure engine (engine.rs):
//!
//! - the restore gate (`RESTORE_PENDING=1`: wait for
//!   `/run/computer/restored` before reading `/data`);
//! - the state file (`<state>/state.json`), written whole and renamed into
//!   place after every step that changed it, before the step's effects;
//! - followers: for each agent and each fragment it follows (its chats'
//!   `chat`, its own `tasks`), a wake subscription, a catch-up from the
//!   cursor over `GET …/channels/{c}?after=`, then `__live`, reconnecting
//!   with jitter (lesson 5);
//! - lanes: each fragment's posts and drafts in order, retried by id;
//! - the runtime's commands (a turn's attachments downloaded first) and
//!   events;
//! - the keepalive socket, held while the engine says so (decision 39).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch, Notify};
use tokio_tungstenite::tungstenite::Message;

use crate::api::{self, Api, ApiError};
use crate::engine::{self, ChatView, Effect, Engine, Input, Settings, State};
use crate::limits;
use crate::net::Backoff;
use crate::records::{self, AttachmentRef, Record};
use crate::runtime::{Agent, Command, Event, LocalFile, Runtime, RuntimeIo};

/// What the bridge is given.
#[derive(Debug, Clone)]
pub struct Config {
    /// `FRAGMENT_API`.
    pub api: String,
    /// Where its state lives (`/data/bridge`).
    pub state_dir: PathBuf,
    /// Scratch for attachments, never kept across a sleep.
    pub media_dir: PathBuf,
    /// `RESTORE_PENDING=1`: wait for `restored` before reading `/data`.
    pub restore_pending: bool,
    pub restored: PathBuf,
    pub settings: Settings,
}

/// Why the bridge stopped.
#[derive(Debug)]
pub enum BridgeError {
    /// Its state file contradicts itself: refused, never repaired in place.
    Corrupt(String),
    /// Its state could not be read or written.
    Disk(String),
    /// Its runtime failed for good, or it was misconfigured.
    Runtime(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::Corrupt(m) => write!(f, "corrupt state: {m}"),
            BridgeError::Disk(m) => write!(f, "state: {m}"),
            BridgeError::Runtime(m) => write!(f, "{m}"),
        }
    }
}

// ---- state on disk ----

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

/// The state as last written, or a fresh one.
pub fn load(dir: &Path) -> Result<State, BridgeError> {
    let path = state_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > limits::STATE_FILE_MAX_BYTES {
                return Err(BridgeError::Corrupt(format!("{} is {} bytes, past the bound", path.display(), bytes.len())));
            }
            let state: State = serde_json::from_slice(&bytes).map_err(|e| BridgeError::Corrupt(format!("{}: {e}", path.display())))?;
            engine::check(&state).map_err(|c| BridgeError::Corrupt(c.0))?;
            Ok(state)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(BridgeError::Disk(format!("{}: {e}", path.display()))),
    }
}

/// Writes the state whole: a temporary file, synced, renamed over the old,
/// and the directory synced, so a crash leaves the old state or the new.
pub fn save(dir: &Path, state: &State) -> Result<(), BridgeError> {
    let bytes = serde_json::to_vec(state).map_err(|e| BridgeError::Disk(e.to_string()))?;
    assert!(bytes.len() <= limits::STATE_FILE_MAX_BYTES, "the engine bounds its state: {} bytes", bytes.len());
    let path = state_path(dir);
    let tmp = dir.join("state.json.tmp");
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    };
    write().map_err(|e| BridgeError::Disk(format!("{}: {e}", path.display())))?;
    Ok(())
}

/// Waits for the platform's restore of `/data` (docs/computers.md, the
/// restore gate). False when told to stop first.
pub async fn restore_gate(cfg: &Config, mut stop: watch::Receiver<bool>) -> bool {
    if !cfg.restore_pending {
        return true;
    }
    crate::ev!("gate.waiting", { "marker": cfg.restored.display().to_string() });
    let started = Instant::now();
    // bounded by the platform: it touches the marker once /data is back, or
    // stops the container
    loop {
        if cfg.restored.exists() {
            crate::ev!("gate.open", { "waitedMs": started.elapsed().as_millis() as u64 });
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            _ = crate::net::stopped(&mut stop) => return false,
        }
    }
}

// ---- the engine's inbox ----

enum Msg {
    Input(Input),
    /// `GET /api/computer` as read (in a task of its own), and the agent
    /// whose fragments to list now (`None`: those due).
    Computer(api::Computer, Option<String>),
}

/// What followers read of the engine without asking it: the cursors, as of
/// the last step that was persisted.
#[derive(Default)]
struct Shared {
    cursors: Mutex<BTreeMap<String, u64>>,
}

impl Shared {
    fn cursor(&self, agent: &str, fragment: &str, channel: &str) -> u64 {
        self.cursors.lock().expect("cursors").get(&engine::cursor_key(agent, fragment, channel)).copied().unwrap_or(0)
    }
}

/// Runs the bridge until `stop` turns true.
pub async fn run(cfg: Config, runtime: Box<dyn Runtime>, stop: watch::Receiver<bool>) -> Result<(), BridgeError> {
    if !restore_gate(&cfg, stop.clone()).await {
        return Ok(());
    }
    let api = Api::new(&cfg.api).map_err(BridgeError::Runtime)?;
    std::fs::create_dir_all(&cfg.media_dir).map_err(|e| BridgeError::Disk(format!("{}: {e}", cfg.media_dir.display())))?;
    let state = load(&cfg.state_dir)?;
    let mut engine = Engine::new(state, cfg.settings).map_err(|c| BridgeError::Corrupt(c.0))?;

    let (inbox_tx, mut inbox) = mpsc::channel::<Msg>(limits::INBOX_MAX);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(256);
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(1024);
    let (keep_tx, keep_rx) = watch::channel(false);
    let shared = Arc::new(Shared::default());

    // The runtime starts at once (Hermes dials it while the bridge reads the
    // platform), and its events go to the inbox.
    let name = runtime.name();
    let mut runtime_task = tokio::spawn(runtime.run(RuntimeIo { commands: cmd_rx, events: event_tx, shutdown: stop.clone() }));
    {
        let inbox_tx = inbox_tx.clone();
        tokio::spawn(async move {
            // bounded by the runtime: ends when it drops its sender
            while let Some(e) = event_rx.recv().await {
                if inbox_tx.send(Msg::Input(Input::Runtime(e))).await.is_err() {
                    return;
                }
            }
        });
    }

    // The agents, before any record: a turn is handed to an agent it names.
    let computer = tokio::select! {
        c = ask_until(&api, stop.clone()) => c,
        r = &mut runtime_task => return runtime_result(r, name),
    };
    let Some(computer) = computer else { return Ok(()) };
    crate::ev!("bridge.start", { "computer": computer.computer, "agents": computer.agents.len(), "runtime": name, "boot": engine.state().boot + 1 });

    let lanes = Lanes::new(api.clone(), stop.clone());
    let runtime_lane = RuntimeLane::spawn(api.clone(), cmd_tx, cfg.media_dir.clone());
    tokio::spawn(keepalive(api.clone(), keep_rx, stop.clone()));
    let follows = Follows { api: api.clone(), inbox: inbox_tx.clone(), shared: shared.clone(), stop: stop.clone(), running: Arc::new(Mutex::new(HashMap::new())), last: Mutex::new(HashMap::new()) };

    let act = |engine: &Engine, step: engine::Step| -> Result<(), BridgeError> {
        if step.dirty {
            save(&cfg.state_dir, engine.state())?;
            *shared.cursors.lock().expect("cursors") = engine.state().cursors.clone();
        }
        for e in step.effects {
            match e {
                Effect::Post { agent, fragment, channel, id, body, files } => lanes.push(&fragment, Job::Post { agent, channel, id, body, files }),
                Effect::Draft { agent, fragment, turn, text } => lanes.push(&fragment, Job::Draft { agent, turn, text }),
                Effect::Runtime(c) => runtime_lane.push(c),
                Effect::Keepalive(on) => {
                    crate::ev!("keepalive", { "hold": on });
                    let _ = keep_tx.send(on);
                }
                Effect::Discover { agent } => reread(&api, &inbox_tx, Some(agent)),
            }
        }
        Ok(())
    };

    let s = engine.step(Input::Agents(computer.agents.clone()), crate::log::now_ms());
    act(&engine, s)?;
    let s = engine.recover(crate::log::now_ms());
    act(&engine, s)?;
    for a in &computer.agents {
        follows.discover(a.clone());
    }

    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut computer_read = Instant::now();
    let mut stop_rx = stop.clone();
    // bounded by the bridge's life: one input per pass, ended by `stop`
    let result = loop {
        tokio::select! {
            biased;
            _ = crate::net::stopped(&mut stop_rx) => break Ok(()),
            m = inbox.recv() => {
                let Some(m) = m else { break Ok(()) };
                let step = match m {
                    Msg::Input(input) => engine.step(input, crate::log::now_ms()),
                    Msg::Computer(c, only) => {
                        let s = engine.step(Input::Agents(c.agents.clone()), crate::log::now_ms());
                        // An agent that left this computer is followed no more.
                        follows.keep_only(&c.agents);
                        match only {
                            Some(agent) => c.agents.iter().filter(|a| a.fragment == agent).for_each(|a| follows.discover(a.clone())),
                            None => follows.rediscover_due(&c.agents),
                        }
                        s
                    }
                };
                if let Err(e) = act(&engine, step) {
                    break Err(e);
                }
            }
            _ = tick.tick() => {
                let s = engine.step(Input::Tick, crate::log::now_ms());
                if let Err(e) = act(&engine, s) {
                    break Err(e);
                }
                // The computer's agents are read again this often: one assigned
                // to it while it is awake is followed from then.
                if computer_read.elapsed() >= Duration::from_millis(limits::COMPUTER_EVERY_MS) {
                    computer_read = Instant::now();
                    reread(&api, &inbox_tx, None);
                }
            }
            r = &mut runtime_task => break runtime_result(r, name),
        }
    };
    crate::ev!("bridge.stop", { "ok": result.is_ok() });
    result
}

fn runtime_result(r: Result<Result<(), crate::runtime::RuntimeError>, tokio::task::JoinError>, name: &str) -> Result<(), BridgeError> {
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(BridgeError::Runtime(e.to_string())),
        Err(e) => Err(BridgeError::Runtime(format!("the {name} runtime panicked: {e}"))),
    }
}

/// Reads `GET /api/computer` in a task of its own (a slow platform never
/// holds the engine), into the inbox.
fn reread(api: &Api, inbox: &mpsc::Sender<Msg>, only: Option<String>) {
    let (api, inbox) = (api.clone(), inbox.clone());
    tokio::spawn(async move {
        if let Some(c) = ask_once(&api).await {
            let _ = inbox.send(Msg::Computer(c, only)).await;
        }
    });
}

async fn ask_once(api: &Api) -> Option<api::Computer> {
    match api.computer().await {
        Ok(c) => Some(c),
        Err(e) => {
            crate::ev!("computer.unread", { "error": e.to_string() });
            None
        }
    }
}

/// `GET /api/computer` until it answers.
async fn ask_until(api: &Api, stop: watch::Receiver<bool>) -> Option<api::Computer> {
    let mut backoff = Backoff::default();
    // bounded by the platform's answer, or `stop`
    loop {
        if let Some(c) = ask_once(api).await {
            return Some(c);
        }
        if !backoff.wait(stop.clone()).await {
            return None;
        }
    }
}

// ---- followers ----

type FollowKey = (String, String, String);

/// The fragments each agent follows: one task per `(agent, fragment,
/// channel)`.
struct Follows {
    api: Api,
    inbox: mpsc::Sender<Msg>,
    shared: Arc<Shared>,
    stop: watch::Receiver<bool>,
    running: Arc<Mutex<HashMap<FollowKey, tokio::task::JoinHandle<()>>>>,
    last: Mutex<HashMap<String, Instant>>,
}

impl Follows {
    /// Stops following for agents no longer on this computer.
    fn keep_only(&self, agents: &[Agent]) {
        let mut running = self.running.lock().expect("followers");
        running.retain(|(agent, fragment, channel), h| {
            let keep = agents.iter().any(|a| &a.fragment == agent);
            if !keep {
                crate::ev!("follow.dropped", { "agent": agent, "fragment": fragment, "channel": channel, "why": "the agent left this computer" });
                h.abort();
            }
            keep
        });
        self.last.lock().expect("last").retain(|agent, _| agents.iter().any(|a| &a.fragment == agent));
    }

    fn rediscover_due(&self, agents: &[Agent]) {
        let every = Duration::from_millis(limits::DISCOVER_EVERY_MS);
        let due: Vec<Agent> = {
            let last = self.last.lock().expect("last");
            agents.iter().filter(|a| last.get(&a.fragment).is_none_or(|t| t.elapsed() >= every)).cloned().collect()
        };
        for a in due {
            self.discover(a);
        }
    }

    /// Lists the agent's fragments, and follows each chat and its tasks it
    /// does not follow yet (in a task of its own: a slow listing never
    /// holds the engine).
    fn discover(&self, agent: Agent) {
        self.last.lock().expect("last").insert(agent.fragment.clone(), Instant::now());
        let (api, inbox, shared, stop, running) = (self.api.clone(), self.inbox.clone(), self.shared.clone(), self.stop.clone(), self.running.clone());
        tokio::spawn(async move {
            let follows = match list_follows(&api, &agent).await {
                Ok(f) => f,
                Err(e) => {
                    crate::ev!("discover.failed", { "agent": agent.fragment, "error": e.to_string() });
                    return;
                }
            };
            crate::ev!("discover", { "agent": agent.fragment, "follows": follows.len() });
            let mut running = running.lock().expect("followers");
            running.retain(|_, h| !h.is_finished());
            for (fragment, channel) in follows {
                let key = (agent.fragment.clone(), fragment.clone(), channel.clone());
                if running.contains_key(&key) {
                    continue;
                }
                let h = tokio::spawn(follow(api.clone(), agent.clone(), fragment, channel, inbox.clone(), shared.clone(), stop.clone()));
                running.insert(key, h);
            }
        });
    }
}

/// What an agent follows: `chat` of every fragment it is in that has a
/// postable `chat` channel, and `tasks` of its own fragment if it has one.
async fn list_follows(api: &Api, agent: &Agent) -> Result<Vec<(String, String)>, ApiError> {
    let mut fragments = api.fragments(&agent.fragment).await?;
    fragments.truncate(limits::DISCOVER_FRAGMENTS_MAX);
    if !fragments.iter().any(|f| f.name == agent.fragment) {
        fragments.push(api::FragmentEntry { name: agent.fragment.clone(), role: String::new() });
    }
    let mut out = Vec::new();
    for f in fragments {
        if out.len() >= limits::FOLLOWS_PER_AGENT_MAX {
            crate::ev!("discover.bounded", { "agent": agent.fragment, "max": limits::FOLLOWS_PER_AGENT_MAX });
            break;
        }
        let channels = match api.channels(&agent.fragment, &f.name).await {
            Ok(c) => c,
            Err(e) if e.gone() => continue,
            Err(e) => return Err(e),
        };
        let own = f.name == agent.fragment;
        for c in channels {
            let chat = c.name == records::CHAT && c.post.is_some() && !own;
            let tasks = c.name == records::TASKS && own;
            if chat || tasks {
                out.push((f.name.clone(), c.name.clone()));
            }
        }
    }
    Ok(out)
}

/// A chat's members and names, read again after `VIEW_TTL_MS`.
struct ViewCache {
    view: ChatView,
    since: i64,
    read_at: Option<Instant>,
}

impl ViewCache {
    fn fresh(&self) -> bool {
        self.read_at.is_some_and(|t| t.elapsed() < Duration::from_millis(limits::VIEW_TTL_MS))
    }

    /// Reads the chat's agents (in the order they were added), its writers'
    /// names, and when this agent joined: records from before are history.
    async fn read(&mut self, api: &Api, agent: &Agent, fragment: &str, writer: Option<&str>) -> Result<(), ApiError> {
        let mut members = api.members(&agent.fragment, fragment).await?;
        members.sort_by(|a, b| (a.added_at, &a.principal).cmp(&(b.added_at, &b.principal)));
        self.view.agents = members.iter().filter(|m| m.kind == "agent").map(|m| m.principal.clone()).collect();
        self.since = members.iter().find(|m| m.principal == agent.identity).map(|m| m.added_at).unwrap_or(0);
        let mut ids: Vec<String> = members.iter().filter(|m| m.kind != "agent" && m.principal.starts_with("id:")).map(|m| m.principal.clone()).collect();
        if let Some(w) = writer.filter(|w| w.starts_with("id:") && !ids.iter().any(|i| i == w)) {
            ids.push(w.to_string());
        }
        match api.people(&agent.fragment, fragment, &ids).await {
            Ok(names) => self.view.names = names,
            Err(e) => crate::ev!("people.unread", { "fragment": fragment, "error": e.to_string() }),
        }
        // A writer no one could name is "someone" until the next read, not
        // a reason to read again at each of its records.
        for id in ids {
            self.view.names.entry(id).or_insert_with(|| "someone".into());
        }
        self.read_at = Some(Instant::now());
        Ok(())
    }
}

/// Why a follower stopped following.
enum Ended {
    /// The agent is no longer in the fragment.
    Gone,
    /// The bridge is stopping.
    Stopped,
    /// The connection ended: follow again after a wait.
    Again(String),
}

/// Follows one channel of one fragment as one agent until the bridge stops
/// or the agent leaves it.
async fn follow(api: Api, agent: Agent, fragment: String, channel: String, inbox: mpsc::Sender<Msg>, shared: Arc<Shared>, stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    let mut view = ViewCache { view: ChatView::default(), since: 0, read_at: None };
    let tasks = channel == records::TASKS;
    let mut subscribed = false;
    // bounded by the bridge's life (or the agent's membership): one
    // connection per pass, each pass waiting a jittered backoff
    loop {
        if *stop.borrow() {
            return;
        }
        let ended = follow_once(&api, &agent, &fragment, &channel, &inbox, &shared, &stop, &mut view, &mut subscribed, tasks, &mut backoff).await;
        match ended {
            Ended::Stopped => return,
            Ended::Gone => {
                crate::ev!("follow.gone", { "agent": agent.fragment, "fragment": fragment, "channel": channel });
                let _ = inbox.send(Msg::Input(Input::Gone { agent: agent.fragment.clone(), fragment: fragment.clone() })).await;
                return;
            }
            Ended::Again(why) => {
                crate::ev!("follow.reconnect", { "agent": agent.fragment, "fragment": fragment, "channel": channel, "why": why });
                if !backoff.wait(stop.clone()).await {
                    return;
                }
            }
        }
    }
}

fn ended_by(e: ApiError) -> Ended {
    if e.gone() {
        Ended::Gone
    } else {
        Ended::Again(e.to_string())
    }
}

#[allow(clippy::too_many_arguments)]
async fn follow_once(
    api: &Api,
    agent: &Agent,
    fragment: &str,
    channel: &str,
    inbox: &mpsc::Sender<Msg>,
    shared: &Shared,
    stop: &watch::Receiver<bool>,
    view: &mut ViewCache,
    subscribed: &mut bool,
    tasks: bool,
    backoff: &mut Backoff,
) -> Ended {
    // A record on the channel wakes this computer from now on.
    if !*subscribed {
        match ensure_wake(api, agent, fragment, channel).await {
            Ok(()) => *subscribed = true,
            Err(e) => return ended_by(e),
        }
    }
    if !tasks && !view.fresh() {
        if let Err(e) = view.read(api, agent, fragment, None).await {
            return ended_by(e);
        }
    }
    // Catch up from the cursor, then follow live from where that left off.
    let mut last = shared.cursor(&agent.fragment, fragment, channel);
    for _ in 0..limits::CATCHUP_PAGES_MAX {
        let page = match api.records(&agent.fragment, fragment, channel, last, limits::CATCHUP_PAGE_RECORDS).await {
            Ok(p) => p,
            Err(e) => return ended_by(e),
        };
        let full = page.records.len() as u32 >= limits::CATCHUP_PAGE_RECORDS;
        for r in page.records {
            last = last.max(r.seq);
            if !feed(api, agent, fragment, r, inbox, view, tasks).await {
                return Ended::Stopped;
            }
        }
        if !full {
            break;
        }
    }
    let ws = match api.live(&agent.fragment, fragment).await {
        Ok(ws) => ws,
        Err(e) => return Ended::Again(e),
    };
    let (mut sink, mut stream) = ws.split();
    let subscribe = |after: u64| Message::text(json!({ "type": "subscribe", "channel": channel, "after": after }).to_string());
    if sink.send(subscribe(last)).await.is_err() {
        return Ended::Again("the live socket closed".into());
    }
    crate::ev!("follow.live", { "agent": agent.fragment, "fragment": fragment, "channel": channel, "after": last });
    let mut ping = tokio::time::interval(Duration::from_millis(limits::LIVE_PING_MS));
    ping.tick().await;
    let mut heard = Instant::now();
    let mut stop = stop.clone();
    // bounded by the socket: ends when it closes, goes quiet, or `stop`
    loop {
        tokio::select! {
            _ = crate::net::stopped(&mut stop) => {
                let _ = sink.close().await;
                return Ended::Stopped;
            }
            _ = ping.tick() => {
                if heard.elapsed() > Duration::from_millis(2 * limits::LIVE_PING_MS) {
                    return Ended::Again("the live socket went quiet".into());
                }
                if sink.send(Message::text(json!({ "type": "ping" }).to_string())).await.is_err() {
                    return Ended::Again("the live socket closed".into());
                }
            }
            m = stream.next() => {
                heard = Instant::now();
                let text = match m {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(1005);
                        // 4003: access revoked; 4004: the fragment was deleted.
                        if matches!(code, 4003 | 4004) {
                            return Ended::Gone;
                        }
                        return Ended::Again(format!("closed {code}"));
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Ended::Again(e.to_string()),
                    None => return Ended::Again("the live socket ended".into()),
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match v["type"].as_str() {
                    Some("hello") => backoff.reset(),
                    Some("record") if v["channel"] == channel => {
                        let Ok(r) = serde_json::from_value::<Record>(v.clone()) else { continue };
                        if r.seq <= last {
                            continue;
                        }
                        last = r.seq;
                        if !feed(api, agent, fragment, r, inbox, view, tasks).await {
                            return Ended::Stopped;
                        }
                    }
                    Some("subscribed") if v["more"] == json!(true) => {
                        let next = v["next"].as_u64().unwrap_or(last);
                        if sink.send(subscribe(next.max(last))).await.is_err() {
                            return Ended::Again("the live socket closed".into());
                        }
                    }
                    Some("error") => crate::ev!("follow.refused_frame", { "fragment": fragment, "message": v["message"].as_str().unwrap_or("") }),
                    _ => {}
                }
            }
        }
    }
}

/// One record into the engine, with the chat's view (fresh enough, and
/// knowing its writer). False when the engine is gone.
async fn feed(api: &Api, agent: &Agent, fragment: &str, record: Record, inbox: &mpsc::Sender<Msg>, view: &mut ViewCache, tasks: bool) -> bool {
    let (input_view, since) = if tasks {
        // A routine that fired while the computer was gone longer than this
        // is skipped, as cron skips a missed run.
        let since = i64::try_from(crate::log::now_ms().saturating_sub(limits::TASKS_BACKLOG_MS)).expect("ms fit");
        (None, since)
    } else {
        let unknown = record.principal.starts_with("id:") && !view.view.names.contains_key(&record.principal) && !view.view.agents.contains(&record.principal);
        if !view.fresh() || unknown {
            if let Err(e) = view.read(api, agent, fragment, Some(&record.principal)).await {
                crate::ev!("view.unread", { "fragment": fragment, "error": e.to_string() });
            }
        }
        (Some(view.view.clone()), view.since)
    };
    inbox.send(Msg::Input(Input::Record { agent: agent.fragment.clone(), fragment: fragment.to_string(), record, view: input_view, since })).await.is_ok()
}

/// The agent's wake subscription on the channel: made unless it has one.
async fn ensure_wake(api: &Api, agent: &Agent, fragment: &str, channel: &str) -> Result<(), ApiError> {
    let subs = api.subscriptions(&agent.fragment, fragment).await?;
    let have = subs.iter().any(|s| s.channel == channel && s.wake && s.principal.as_deref().is_none_or(|p| p == agent.identity));
    if have {
        return Ok(());
    }
    let s = api.subscribe_wake(&agent.fragment, fragment, channel).await?;
    crate::ev!("subscribed", { "agent": agent.fragment, "fragment": fragment, "channel": channel, "id": s.id });
    Ok(())
}

// ---- lanes: each fragment's posts and drafts, in order ----

#[derive(Debug, Clone)]
enum Job {
    Post { agent: String, channel: &'static str, id: String, body: Value, files: Vec<LocalFile> },
    Draft { agent: String, turn: String, text: Option<String> },
}

struct Lane {
    queue: Mutex<VecDeque<Job>>,
    ready: Notify,
}

#[derive(Clone)]
struct Lanes {
    api: Api,
    stop: watch::Receiver<bool>,
    lanes: Arc<Mutex<HashMap<String, Arc<Lane>>>>,
}

impl Lanes {
    fn new(api: Api, stop: watch::Receiver<bool>) -> Lanes {
        Lanes { api, stop, lanes: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn push(&self, fragment: &str, job: Job) {
        let lane = {
            let mut lanes = self.lanes.lock().expect("lanes");
            match lanes.get(fragment) {
                Some(l) => l.clone(),
                None => {
                    let l = Arc::new(Lane { queue: Mutex::new(VecDeque::new()), ready: Notify::new() });
                    lanes.insert(fragment.to_string(), l.clone());
                    tokio::spawn(work(self.api.clone(), fragment.to_string(), l.clone(), self.stop.clone()));
                    l
                }
            }
        };
        {
            let mut q = lane.queue.lock().expect("lane");
            // A lane this long is a platform that stopped answering; the
            // oldest drafts go first (the next carries the whole text).
            if q.len() >= limits::LANE_MAX {
                if let Some(i) = q.iter().position(|j| matches!(j, Job::Draft { .. })) {
                    q.remove(i);
                }
            }
            assert!(q.len() < limits::LANE_MAX, "a fragment's lane holds at most {} posts", limits::LANE_MAX);
            q.push_back(job);
        }
        lane.ready.notify_one();
    }
}

/// A fragment's lane: its jobs in order, each post retried until the
/// platform takes it (its id makes a retry the same record).
async fn work(api: Api, fragment: String, lane: Arc<Lane>, mut stop: watch::Receiver<bool>) {
    let mut drafted: HashMap<String, Instant> = HashMap::new();
    // bounded by the bridge's life: one job per pass
    loop {
        let job = {
            let mut q = lane.queue.lock().expect("lane");
            let job = q.pop_front();
            // A draft a later draft of the same turn replaces is skipped.
            match job {
                Some(Job::Draft { ref turn, text: Some(_), .. }) if q.iter().any(|j| matches!(j, Job::Draft { turn: t, .. } if t == turn)) => continue,
                j => j,
            }
        };
        let Some(job) = job else {
            tokio::select! {
                _ = lane.ready.notified() => continue,
                _ = crate::net::stopped(&mut stop) => return,
            }
        };
        match job {
            Job::Draft { agent, turn, text } => {
                if text.is_some() {
                    let wait = drafted.get(&turn).map(|t| Duration::from_millis(limits::DRAFT_INTERVAL_MS).saturating_sub(t.elapsed())).unwrap_or_default();
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                        // A newer draft may have come meanwhile.
                        let newer = lane.queue.lock().expect("lane").iter().any(|j| matches!(j, Job::Draft { turn: t, .. } if *t == turn));
                        if newer {
                            continue;
                        }
                    }
                    drafted.insert(turn.clone(), Instant::now());
                } else {
                    drafted.remove(&turn);
                }
                match api.draft(&agent, &fragment, records::CHAT, &turn, text.as_deref()).await {
                    Ok(()) => {}
                    Err(e) if api::rate_limited(&e) => {}
                    Err(e) => crate::ev!("draft.failed", { "fragment": fragment, "turn": turn, "error": e.to_string() }),
                }
            }
            Job::Post { agent, channel, id, body, files } => post(&api, &fragment, &agent, channel, &id, body, &files, stop.clone()).await,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn post(api: &Api, fragment: &str, agent: &str, channel: &str, id: &str, mut body: Value, files: &[LocalFile], stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    for attempt in 1..=limits::POST_TRIES_MAX {
        let uploaded = upload(api, fragment, agent, files).await;
        let result = match uploaded {
            Ok(refs) => {
                if !refs.is_empty() {
                    body["attachments"] = json!(refs);
                }
                api.post(agent, fragment, channel, id, &body).await
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(replayed) => {
                crate::ev!("posted", { "fragment": fragment, "channel": channel, "id": id, "replayed": replayed, "attempt": attempt });
                return;
            }
            Err(e) if e.retryable() && attempt < limits::POST_TRIES_MAX => {
                crate::ev!("post.retry", { "fragment": fragment, "id": id, "attempt": attempt, "error": e.to_string() });
                if !backoff.wait(stop.clone()).await {
                    return;
                }
            }
            Err(e) => {
                // A 409 is an id this bridge posted before with another
                // body: a bug in the ids, never retried. A 403/404: it is no
                // longer the agent's to post in.
                crate::ev!("post.failed", { "fragment": fragment, "channel": channel, "id": id, "error": e.to_string() });
                return;
            }
        }
    }
}

/// Uploads a reply's files as the fragment's blobs: their refs.
async fn upload(api: &Api, fragment: &str, agent: &str, files: &[LocalFile]) -> Result<Vec<AttachmentRef>, ApiError> {
    let mut refs = Vec::new();
    for f in files.iter().take(limits::ATTACHMENTS_MAX) {
        let bytes = match tokio::fs::read(&f.path).await {
            Ok(b) if !b.is_empty() && b.len() as u64 <= limits::ATTACHMENT_MAX_BYTES => Bytes::from(b),
            Ok(_) => {
                crate::ev!("attachment.skipped", { "name": f.name, "why": "empty, or past the bound" });
                continue;
            }
            Err(e) => {
                crate::ev!("attachment.skipped", { "name": f.name, "why": e.to_string() });
                continue;
            }
        };
        let sha = records::hex(&Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        api.put_blob(agent, fragment, &sha, &f.media_type, bytes).await?;
        refs.push(AttachmentRef { sha256: sha, size, media_type: f.media_type.clone(), name: f.name.clone() });
    }
    Ok(refs)
}

// ---- the runtime's commands ----

/// Commands to the runtime in order; a turn's attachments are downloaded
/// before it is handed (so a Stop never overtakes its Start).
struct RuntimeLane {
    tx: mpsc::UnboundedSender<Command>,
}

impl RuntimeLane {
    fn spawn(api: Api, to_runtime: mpsc::Sender<Command>, media_dir: PathBuf) -> RuntimeLane {
        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
        tokio::spawn(async move {
            // bounded by the engine: ends when it drops its sender
            while let Some(mut c) = rx.recv().await {
                if let Command::Start(ts) = &mut c {
                    ts.files = download(&api, &ts.agent.fragment, &ts.fragment, &ts.attachments, &media_dir).await;
                }
                if to_runtime.send(c).await.is_err() {
                    return;
                }
            }
        });
        RuntimeLane { tx }
    }

    fn push(&self, c: Command) {
        // The engine bounds its open turns, and so the commands in flight.
        let _ = self.tx.send(c);
    }
}

/// A message's attachments, from the chat's blobs to scratch files.
async fn download(api: &Api, agent: &str, fragment: &str, refs: &[AttachmentRef], dir: &Path) -> Vec<LocalFile> {
    let mut out = Vec::new();
    for r in refs.iter().take(limits::ATTACHMENTS_MAX) {
        let path = dir.join(&r.sha256);
        let have = tokio::fs::metadata(&path).await.is_ok_and(|m| m.len() == r.size);
        if !have {
            match api.get_blob(agent, fragment, &r.sha256).await {
                Ok(bytes) if records::hex(&Sha256::digest(&bytes)) == r.sha256 => {
                    if let Err(e) = tokio::fs::write(&path, &bytes).await {
                        crate::ev!("attachment.unsaved", { "sha256": r.sha256, "error": e.to_string() });
                        continue;
                    }
                }
                Ok(_) => {
                    crate::ev!("attachment.refused", { "sha256": r.sha256, "why": "its bytes are not its hash" });
                    continue;
                }
                Err(e) => {
                    crate::ev!("attachment.unread", { "sha256": r.sha256, "error": e.to_string() });
                    continue;
                }
            }
        }
        out.push(LocalFile { path, media_type: r.media_type.clone(), name: r.name.clone(), size: r.size });
    }
    out
}

// ---- keepalive ----

/// Holds `GET /api/computer/keepalive` open while `hold` is true, and only
/// then (decision 39: the computer stays awake while it is open).
async fn keepalive(api: Api, mut hold: watch::Receiver<bool>, stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    // bounded by the bridge's life: one connection per pass
    loop {
        if *stop.borrow() {
            return;
        }
        if !*hold.borrow() {
            let mut stop2 = stop.clone();
            tokio::select! {
                r = hold.changed() => if r.is_err() { return },
                _ = crate::net::stopped(&mut stop2) => return,
            }
            continue;
        }
        let ws = match api.keepalive().await {
            Ok(ws) => ws,
            Err(e) => {
                crate::ev!("keepalive.failed", { "error": e });
                if !backoff.wait(stop.clone()).await {
                    return;
                }
                continue;
            }
        };
        backoff.reset();
        crate::ev!("keepalive.held");
        let (mut sink, mut stream) = ws.split();
        let mut stop2 = stop.clone();
        // bounded by the socket, `hold`, or `stop`
        loop {
            tokio::select! {
                r = hold.changed() => {
                    if r.is_err() || !*hold.borrow() {
                        let _ = sink.close().await;
                        crate::ev!("keepalive.dropped");
                        break;
                    }
                }
                m = stream.next() => match m {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        crate::ev!("keepalive.lost");
                        break;
                    }
                    Some(Ok(_)) => {}
                },
                _ = crate::net::stopped(&mut stop2) => {
                    let _ = sink.close().await;
                    return;
                }
            }
        }
    }
}
