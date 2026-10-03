//! The `Computer` cell: one Durable Object per computer (docs/computers.md).
//! It is the only thing that talks to its container: it starts and stops
//! it, saves and restores its `/data`, arms its egress intercepts, proxies
//! its ports, and wakes it. What it does next is decided by its lifecycle,
//! a pure state machine (`fragment_core::computer`); this file performs its
//! actions through the container calls entry.mjs's `ContainerHost` makes
//! (`ctx.computerHost`), and feeds back what happened. It knows nothing of
//! what the image runs.
//!
//! - **Its owner** (`/api/computers…`, the router: `route`) makes it, wakes
//!   it, puts it to sleep, assigns agent fragments to it, and mints the
//!   one-time tickets that sign a browser in to its ports.
//! - **Its origin** (`<24 hex>--computer.<suffix>`: `serve_host`) serves
//!   its ports to its owner, cross-site from the platform, so a page the
//!   guest serves can act as no one; in a tab of its own, or in a frame of
//!   the platform's page (the shell's), the only page that may frame it.
//! - **Its guest** reaches the platform only through the intercepts
//!   (`ComputerEgress`): the API as its agents (each request signed by the
//!   agent fragment it names, which checks it runs here), its own view,
//!   and the keepalive socket that holds it awake. A request to a
//!   connection's or an operator key's host has its placeholders swapped
//!   for the credential (`egress_swap`, decisions 22 and 37), which this
//!   cell resolves: for an agent that runs here (any of its owner's
//!   connections, unless its owner narrowed them: decision 44), and a
//!   connection's token from WorkOS Pipes, held until shortly before it
//!   expires.
//! - **Its agents' new fragments** (`computer/joined`, from a fragment an
//!   agent of its was added to): the agent's own fragment posts `joined`
//!   on its `tasks`, and the fragment that added it then wakes the
//!   computer (`Wake::Joined`; docs/computers.md).

use std::cell::RefCell;
use std::collections::BTreeMap;

use fragment_core::computer::{Action, Event, Lifecycle, Phase, Socket, Step, Wake};
use fragment_core::ledger::{Meter, MeterRow, Spend};
use fragment_core::price::Usage;
use fragment_core::swap::{self, Credential};
use fragment_proto::computer::{valid_computer_id, AgentConnections, ComputerAgent, ComputerPhase, ComputerView, PortTicket};
use fragment_proto::{ErrorCode, IdentityKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::wasm_bindgen::{self, prelude::*, JsCast};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::{body_json, json_response};
use crate::js;

/// The marker an internal route's caller carries (routed.rs `marker`): the
/// router never passes it.
pub const INTERNAL_HEADER: &str = "x-fragment-computer-internal";
/// What an egress or a port request asks the cell (set by Rust inside the
/// Worker only: the router drops every client `x-fragment-*`).
const KIND_HEADER: &str = "x-fragment-computer";
const PORT_HEADER: &str = "x-fragment-computer-port";
const SESSION_HEADER: &str = "x-fragment-computer-session";
const SIGNER_HEADER: &str = "x-fragment-computer-signer";
/// The header the guest names the agent it acts as with (docs/computers.md).
const AGENT_HEADER: &str = "x-fragment-agent";
/// The cookies that sign a browser in to a computer's origin: a top-level
/// visit's (SameSite=Lax), and a frame's in the platform's page (the
/// shell's tab onto a port: SameSite=None, partitioned). Each is
/// `__Host-` over https (`auth::set_cookie`): a fragment's page may set a
/// cookie for the whole suffix, never one of those.
const SESSION_COOKIE: &str = "fragment_computer";
const FRAME_COOKIE: &str = "fragment_computer_frame";

/// `agents.connections` is JSON: `null`, every connection the owner has
/// (decision 44), or a list that narrows it. An agent's row names it as
/// it is made (`computer/assign`), so the column's default is never read.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS agents (fragment TEXT PRIMARY KEY, identity TEXT NOT NULL, owner TEXT NOT NULL, added_at INTEGER NOT NULL, connections TEXT NOT NULL DEFAULT 'null');
CREATE TABLE IF NOT EXISTS awake (from_ms INTEGER PRIMARY KEY, to_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tickets (hash TEXT PRIMARY KEY, port INTEGER NOT NULL, identity TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (hash TEXT PRIMARY KEY, identity TEXT NOT NULL, expires_at INTEGER NOT NULL);
";

/// Agent fragments one computer runs.
const AGENTS_MAX: u64 = 16;
/// A ticket is spent within this, once.
const TICKET_TTL_MS: i64 = 2 * 60_000;
/// A browser's session on a computer's origin.
const SESSION_TTL_MS: i64 = 12 * 3_600_000;
/// Tickets and sessions kept at once (the oldest go first).
const TICKETS_MAX: u64 = 64;
const SESSIONS_MAX: u64 = 64;
/// The runtime's idle stop: only the safety net under the cell's own sleep
/// (spike S3: the container gets SIGTERM about 70 s past it).
const RUNTIME_IDLE_MS: i64 = fragment_core::computer::IDLE_MS + 10 * 60_000;
/// How long a start waits for the container to take an exec before it
/// restores into it.
const EXEC_READY_MS: i64 = 120_000;
/// A signalled guest exits within this (docs/computers.md: five seconds).
const EXIT_WAIT_MS: i64 = 5_000;
/// The largest request body the egress signs and hands on (it is hashed
/// for NIP-98 first, so it is read whole).
const EGRESS_BODY_MAX_BYTES: usize = 32 * 1024 * 1024;
/// A port number the platform proxies to.
const PORT_MAX: u16 = 65_535;
/// A connection's token is used until this long before it expires, and
/// for at most `TOKEN_HOLD_MAX_MS` (WorkOS refreshes it; asking again is
/// cheap, so a revoked connection stops within minutes).
const TOKEN_MARGIN_MS: i64 = 60_000;
const TOKEN_HOLD_MAX_MS: i64 = 10 * 60_000;
/// Tokens held at once (all go when it is full: there are few owners).
const TOKENS_MAX: usize = 64;
/// Awake intervals one flush sends (one is made every five minutes awake).
const METER_ROWS_MAX: u64 = 64;

#[derive(Clone, Copy)]
enum MetaKey {
    Id,
    Owner,
    Image,
    CreatedAt,
    Lifecycle,
    /// The last `/data` backup (DirectoryBackup's record).
    Backup,
    /// The last snapshot and the image it was taken of.
    Snapshot,
    /// Why the last wake was refused, or the last sleep failed to save.
    Note,
}

impl MetaKey {
    fn key(self) -> &'static str {
        match self {
            MetaKey::Id => "id",
            MetaKey::Owner => "owner",
            MetaKey::Image => "image",
            MetaKey::CreatedAt => "created_at",
            MetaKey::Lifecycle => "lifecycle",
            MetaKey::Backup => "backup",
            MetaKey::Snapshot => "snapshot",
            MetaKey::Note => "note",
        }
    }
}

/// What a snapshot is of: the image's reference (its name can stand for
/// another image after a redeploy).
#[derive(Serialize, Deserialize)]
struct Snapshot {
    id: String,
    image: String,
}

#[durable_object]
pub struct ComputerCell {
    state: State,
    raw: JsValue,
    cfg: &'static Config,
    env: Env,
    /// Whether this isolate has looked for a container an earlier one left
    /// running (lesson 6: `adopt`).
    adopted: std::cell::Cell<bool>,
    /// Connections' tokens by (owner, provider): the token and until when
    /// it is used. In memory only: a new isolate asks WorkOS again.
    tokens: RefCell<BTreeMap<(String, String), (String, i64)>>,
}

impl DurableObject for ComputerCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        state.storage().sql().exec(SCHEMA, None).expect("the Computer schema applies");
        let cfg = Config::from_env(&env);
        ComputerCell { state, raw, cfg, env, adopted: std::cell::Cell::new(false), tokens: RefCell::default() }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(r) => Ok(r),
            Err(e) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        if self.meta(MetaKey::Id).ok().flatten().is_some() {
            if let Err(e) = self.drive(Event::Alarm).await {
                console_error!("{{\"computer\":\"alarm\",\"error\":{}}}", json!(e.message));
            }
        }
        Response::ok("")
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        // the keepalive says nothing the cell needs; a ping is answered
        if let WebSocketIncomingMessage::String(s) = message {
            if serde_json::from_str::<Value>(&s).is_ok_and(|v| v["type"] == "ping") {
                ws.send_with_str(r#"{"type":"pong"}"#)?;
            }
        }
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, code: usize, reason: String, _clean: bool) -> Result<()> {
        let code = if code == 1005 || code == 1006 { 1000 } else { code as u16 };
        let _ = ws.close(Some(code), Some(reason));
        self.keepalive_closed(&ws).await;
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: worker::Error) -> Result<()> {
        self.keepalive_closed(&ws).await;
        Ok(())
    }
}

/// `computer/init`'s body.
#[derive(Deserialize)]
struct Init {
    id: String,
    owner: String,
}

#[derive(Deserialize)]
struct WakeBody {
    why: Wake,
}

#[derive(Deserialize)]
struct Assign {
    fragment: String,
    identity: String,
    owner: String,
}

#[derive(Deserialize)]
struct Unassign {
    fragment: String,
}

#[derive(Deserialize)]
struct SetConnections {
    fragment: String,
    /// `None`: every connection the owner has (decision 44).
    connections: Option<Vec<String>>,
}

/// What an agent may have swapped in, as `agents.connections` keeps it.
fn stored_connections(text: &str, fragment: &str) -> CellResult<Option<Vec<String>>> {
    serde_json::from_str(text).map_err(|e| CellError::host(format!("{fragment}'s stored connections {text:?}: {e}")))
}

/// `computer/joined`'s body: the agent `identity` was added to `fragment`,
/// its membership made at `at` (which keys the notice: runs_on.rs).
#[derive(Deserialize)]
struct JoinedAsk {
    identity: String,
    fragment: String,
    at: i64,
}

/// `computer/credential`'s body: what `agent` asked to have swapped in.
#[derive(Deserialize)]
struct CredentialAsk {
    agent: String,
    connection: Option<String>,
    key: Option<String>,
}

#[derive(Deserialize)]
struct Pin {
    image: String,
}

#[derive(Deserialize)]
struct TicketAsk {
    port: u16,
    identity: String,
}

#[derive(Deserialize)]
struct Redeem {
    ticket: String,
}

#[derive(Deserialize)]
struct Exited {
    generation: u64,
}

#[derive(Deserialize)]
struct Tab {
    open: bool,
}

fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

impl ComputerCell {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.sql().exec(q, binds)?.to_array::<Value>()?)
    }

    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.sql().exec(q, binds)?;
        Ok(())
    }

    fn meta(&self, k: MetaKey) -> CellResult<Option<String>> {
        let rows = self.rows("SELECT value FROM meta WHERE key = ?", vec![k.key().into()])?;
        Ok(rows.first().and_then(|r| r["value"].as_str()).map(str::to_string))
    }

    fn must(&self, k: MetaKey) -> CellResult<String> {
        self.meta(k)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, "no such computer"))
    }

    fn set_meta(&self, k: MetaKey, v: &str) -> CellResult<()> {
        self.exec("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![k.key().into(), v.into()])
    }

    fn del_meta(&self, k: MetaKey) -> CellResult<()> {
        self.exec("DELETE FROM meta WHERE key = ?", vec![k.key().into()])
    }

    fn lifecycle(&self) -> CellResult<Lifecycle> {
        match self.meta(MetaKey::Lifecycle)? {
            Some(text) => serde_json::from_str(&text).map_err(|e| CellError::host(format!("the stored lifecycle: {e}"))),
            None => Ok(Lifecycle::new()),
        }
    }

    fn host(&self) -> CellResult<JsValue> {
        let host = js::property(&self.raw, "computerHost")?;
        if host.is_undefined() {
            return Err(CellError::host("this Durable Object has no container host (entry.mjs's Computer class)"));
        }
        Ok(host)
    }

    async fn call(&self, method: &str, args: &[JsValue]) -> CellResult<JsValue> {
        js::invoke(&self.host()?, method, args).await
    }

    /// Applies `first` and every event its actions report back, each
    /// against the state as stored: a handler running beside this one (a
    /// wake while a start is under way) applies its own event between, and
    /// the generations keep a late report from changing anything.
    async fn drive(&self, first: Event) -> CellResult<Option<String>> {
        let mut queue = vec![first];
        if !self.adopted.replace(true) {
            if let Some(exited) = self.adopt().await? {
                // the event that brought this isolate up applies after the exit
                queue.push(exited);
            }
        }
        let mut refused = None;
        while let Some(event) = queue.pop() {
            let mut life = self.lifecycle()?;
            let logged = json!(event);
            let step = life.apply(event, js::now_ms());
            // one line per event (lesson 14): what happened, and what it led to
            console_log!("{}", json!({ "computer": self.meta(MetaKey::Id)?, "event": logged, "phase": life.phase, "actions": step.actions }));
            self.set_meta(MetaKey::Lifecycle, &serde_json::to_string(&life).map_err(|e| CellError::host(e.to_string()))?)?;
            self.alarm_at(step.alarm_ms).await?;
            refused = refused.or(step.refused.clone());
            for next in self.perform(step).await? {
                queue.push(next);
            }
        }
        Ok(refused)
    }

    /// A new isolate's first look (lesson 6): a container its lifecycle says
    /// is running is taken over (watched again, its intercepts and idle
    /// stop armed again, never destroyed: `/data` may be unsaved), or, gone,
    /// reported as exited.
    async fn adopt(&self) -> CellResult<Option<Event>> {
        let generation = match self.lifecycle()?.phase {
            Phase::Starting { generation, .. } | Phase::Awake { generation, .. } | Phase::Sleeping { generation, .. } => generation,
            Phase::Asleep | Phase::Failed { .. } => return Ok(None),
        };
        let g = JsValue::from_f64(generation as f64);
        if self.call("adopt", std::slice::from_ref(&g)).await?.as_bool() != Some(true) {
            return Ok(Some(Event::Exited { generation }));
        }
        let id = self.must(MetaKey::Id)?;
        self.call("arm", &[g, id.as_str().into(), JsValue::from_f64(RUNTIME_IDLE_MS as f64), self.swap_hosts()]).await?;
        Ok(None)
    }

    async fn alarm_at(&self, at: Option<i64>) -> CellResult<()> {
        let storage = self.state.storage();
        match at {
            Some(ms) => storage.set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(ms as f64)))).await?,
            None => storage.delete_alarm().await?,
        }
        Ok(())
    }

    /// Performs a step's actions: the events they report, in order.
    async fn perform(&self, step: Step) -> CellResult<Vec<Event>> {
        let mut reported = vec![];
        for action in step.actions {
            match action {
                Action::Start { generation } => reported.push(self.start(generation).await),
                Action::Sleep { generation } => reported.push(self.sleep(generation).await),
                Action::Meter { from_ms, to_ms } => {
                    // kept until the owner's ledger has it: a flush that fails
                    // is tried again at the next interval, or the next wake
                    self.exec("INSERT INTO awake (from_ms, to_ms) VALUES (?, ?) ON CONFLICT (from_ms) DO NOTHING", vec![from_ms.into(), to_ms.into()])?;
                    self.flush_awake().await;
                }
            }
        }
        Ok(reported)
    }

    /// A wake, unless the owner's ledger refuses it (decision 27: at zero
    /// credit, no wakes). A computer already up is not asked about. A
    /// ledger that does not answer lets it wake (docs/ledger.md).
    async fn wake(&self, why: Wake) -> CellResult<()> {
        let owner = self.must(MetaKey::Owner)?;
        if !matches!(self.lifecycle()?.phase, Phase::Awake { .. } | Phase::Starting { .. }) {
            let may = crate::ledger::MaySpend { spend: Spend::Wake, fragment: None, by_owner: true };
            if let Err(e) = crate::ledger::ask(&self.env, &owner, &may).await {
                if e.refused.is_some() {
                    self.set_meta(MetaKey::Note, &e.message)?;
                    console_log!("{}", json!({ "computer": self.meta(MetaKey::Id)?, "wake": why, "refused": e.message }));
                    return Err(CellError::new(e.code, e.message));
                }
                console_error!("{}", json!({ "computer": self.meta(MetaKey::Id)?, "wake": why, "ledger": e.message }));
            }
            self.flush_awake().await;
        }
        // a wake that started nothing says why
        if let Some(why) = self.drive(Event::Wake { why }).await? {
            return Err(CellError::new(ErrorCode::WontWake, why));
        }
        Ok(())
    }

    /// Sends the awake intervals the ledger has not taken to the owner's
    /// ledger (each once, by its reference), and forgets those it took. A
    /// failure is logged; they go with the next flush.
    async fn flush_awake(&self) {
        if let Err(e) = self.try_flush_awake().await {
            console_error!("{}", json!({ "computer": "awake-flush", "error": e.message }));
        }
    }

    async fn try_flush_awake(&self) -> CellResult<()> {
        let (id, owner) = (self.must(MetaKey::Id)?, self.must(MetaKey::Owner)?);
        let rows = self.rows("SELECT from_ms, to_ms FROM awake ORDER BY from_ms LIMIT ?", vec![(METER_ROWS_MAX as i64).into()])?;
        let intervals: Vec<(i64, i64)> = rows.iter().filter_map(|r| Some((r["from_ms"].as_i64()?, r["to_ms"].as_i64()?))).collect();
        let Some((first, _)) = intervals.first().copied() else { return Ok(()) };
        let last = intervals.last().map_or(first, |(from, _)| *from);
        let meter_rows = intervals
            .iter()
            .filter(|(from, to)| to > from)
            .map(|(from, to)| MeterRow {
                reference: format!("awake:{id}:{from}"),
                usage: Usage::Awake { instance: self.cfg.computer_instance.clone(), ms: (to - from) as u64 },
                fragment: None,
                agent: None,
                computer: Some(id.clone()),
                at_ms: *to,
            })
            .collect::<Vec<_>>();
        if !meter_rows.is_empty() {
            let meter = Meter { batch: format!("awake:{id}:{first}-{last}"), rows: meter_rows };
            let metered = crate::ledger::ask(&self.env, &owner, &meter).await.map_err(CellError::from)?;
            for r in &metered.refused {
                // a row the ledger refused changed nothing there: it is logged, not kept
                console_error!("{}", json!({ "computer": id, "awake": meter.rows.get(r.index as usize).map(|m| &m.reference), "refused": r.why }));
            }
        }
        self.exec("DELETE FROM awake WHERE from_ms >= ? AND from_ms <= ?", vec![first.into(), last.into()])
    }

    /// The hosts the swap intercepts: every connection's and operator key's.
    fn swap_hosts(&self) -> JsValue {
        js::to_js(&json!(swap::all_hosts([&self.cfg.connections, &self.cfg.operator_keys])))
    }

    /// The environment every image may rely on (docs/computers.md).
    fn guest_env(&self, id: &str, image: &str, restoring: bool) -> Value {
        let mut env = json!({
            "FRAGMENT_COMPUTER": id,
            "FRAGMENT_API": "http://api.fragment.internal",
            "FRAGMENT_MODEL": "http://model.fragment.internal",
            "FRAGMENT_STORAGE": "http://storage.fragment.internal",
            "FRAGMENT_IMAGE": image,
        });
        if restoring {
            env["RESTORE_PENDING"] = json!("1");
        }
        env
    }

    /// One start: from a snapshot of the pinned image when there is one,
    /// else the image (restoring `/data` from the last backup, behind the
    /// restore gate). Any failure destroys what started.
    async fn start(&self, generation: u64) -> Event {
        match self.try_start(generation).await {
            Ok(()) => Event::Ready { generation },
            Err(e) => {
                let _ = self.call("destroy", &[JsValue::from_f64(generation as f64), "the start failed".into()]).await;
                Event::StartFailed { generation, why: e.message }
            }
        }
    }

    async fn try_start(&self, generation: u64) -> CellResult<()> {
        let (id, image) = (self.must(MetaKey::Id)?, self.must(MetaKey::Image)?);
        let g = JsValue::from_f64(generation as f64);
        // lesson 6: a container a new isolate found running is never ours to
        // reuse half-known; it goes before this one starts
        if js::invoke(&self.host()?, "running", &[]).await?.as_bool() == Some(true) {
            self.call("destroy", &[JsValue::from_f64(0.0), "a new start".into()]).await?;
        }
        let reference = self.call("imageRef", &[image.as_str().into()]).await?.as_string();
        let snapshot = match (self.meta(MetaKey::Snapshot)?, &reference) {
            (Some(text), Some(r)) if self.cfg.computer_snapshots => serde_json::from_str::<Snapshot>(&text).ok().filter(|s| &s.image == r).map(|s| s.id),
            _ => None,
        };
        let backup = if snapshot.is_none() { self.meta(MetaKey::Backup)? } else { None };
        let env = js::to_js(&self.guest_env(&id, &image, backup.is_some()));
        let snapshot_js = snapshot.as_deref().map(JsValue::from_str).unwrap_or(JsValue::NULL);
        self.call("start", &[g.clone(), image.as_str().into(), snapshot_js, env, JsValue::NULL]).await?;
        let armed = self.call("arm", &[g.clone(), id.as_str().into(), JsValue::from_f64(RUNTIME_IDLE_MS as f64), self.swap_hosts()]).await?;
        if armed.as_bool() != Some(true) {
            return Err(CellError::host(format!("start {generation} was superseded before it was armed")));
        }
        if let Some(record) = backup {
            let up = self.call("execReady", &[g.clone(), JsValue::from_f64(EXEC_READY_MS as f64)]).await?;
            if up.as_bool() != Some(true) {
                return Err(CellError::host(format!("the container took no exec within {}s", EXEC_READY_MS / 1000)));
            }
            let record: Value = serde_json::from_str(&record).map_err(|e| CellError::host(format!("the stored backup: {e}")))?;
            self.call("restore", &[g.clone(), js::to_js(&record)]).await?;
            let argv = js::to_js(&json!(["sh", "-c", "mkdir -p /run/computer && touch /run/computer/restored"]));
            let out = js::from_js(&self.call("exec", &[g, argv]).await?).map_err(CellError::host)?;
            if out["exitCode"] != 0 {
                return Err(CellError::host(format!("opening the restore gate: {}", out["output"])));
            }
        }
        Ok(())
    }

    /// The sleep sequence (docs/computers.md): save `/data`, snapshot,
    /// signal, give the guest five seconds, destroy. A save that fails
    /// keeps the backup before it and is noted; the sleep goes on.
    async fn sleep(&self, generation: u64) -> Event {
        let g = JsValue::from_f64(generation as f64);
        if let Ok(true) = self.call("running", &[]).await.map(|r| r.as_bool() == Some(true)) {
            match self.call("backup", std::slice::from_ref(&g)).await.and_then(|r| js::from_js(&r).map_err(CellError::host)) {
                Ok(record) => {
                    let previous = self.meta(MetaKey::Backup).ok().flatten();
                    let _ = self.set_meta(MetaKey::Backup, &record.to_string());
                    if let Some(old) = previous.and_then(|p| serde_json::from_str::<Value>(&p).ok()) {
                        let _ = self.call("forget", &[js::to_js(&old)]).await;
                    }
                }
                Err(e) => {
                    let _ = self.set_meta(MetaKey::Note, &format!("the last sleep could not save /data: {}", e.message));
                }
            }
            if self.cfg.computer_snapshots {
                let name = format!("{}-{generation}", self.meta(MetaKey::Id).ok().flatten().unwrap_or_default().replace(':', "-"));
                match self.call("snapshot", &[g.clone(), name.into()]).await.map(|s| s.as_string()) {
                    Ok(Some(id)) => {
                        let name = self.meta(MetaKey::Image).ok().flatten().unwrap_or_default();
                        match self.call("imageRef", &[name.as_str().into()]).await.ok().and_then(|r| r.as_string()) {
                            Some(image) => {
                                let _ = self.set_meta(MetaKey::Snapshot, &serde_json::to_string(&Snapshot { id, image }).unwrap_or_default());
                            }
                            None => {
                                let _ = self.del_meta(MetaKey::Snapshot);
                            }
                        }
                    }
                    // a computer without a snapshot wakes from its image and backup
                    _ => {
                        let _ = self.del_meta(MetaKey::Snapshot);
                    }
                }
            }
            let _ = self.call("signal", &[g.clone(), JsValue::from_f64(15.0)]).await;
            let _ = self.call("exited", &[JsValue::from_f64(EXIT_WAIT_MS as f64)]).await;
        }
        let _ = self.call("destroy", &[g, "asleep".into()]).await;
        Event::Asleep { generation }
    }

    fn agents(&self) -> CellResult<Vec<ComputerAgent>> {
        let rows = self.rows("SELECT fragment, identity, owner, connections FROM agents ORDER BY added_at", vec![])?;
        assert!(rows.len() as u64 <= AGENTS_MAX, "a computer runs at most AGENTS_MAX agents");
        let mut agents = Vec::with_capacity(rows.len());
        for r in rows {
            let fragment = r["fragment"].as_str().unwrap_or_default().to_string();
            let name = fragment_proto::split_fragment_name(&fragment).map(|(label, _)| label.to_string()).unwrap_or_default();
            // a stored value that does not read is a host fault, never "all"
            let connections = stored_connections(r["connections"].as_str().unwrap_or_default(), &fragment)?;
            agents.push(ComputerAgent {
                identity: r["identity"].as_str().unwrap_or_default().to_string(),
                owner: r["owner"].as_str().unwrap_or_default().to_string(),
                fragment,
                name,
                connections,
            });
        }
        Ok(agents)
    }

    fn view(&self) -> CellResult<ComputerView> {
        let id = self.must(MetaKey::Id)?;
        let life = self.lifecycle()?;
        let (phase, why) = match &life.phase {
            Phase::Asleep => (ComputerPhase::Asleep, None),
            Phase::Starting { .. } => (ComputerPhase::Starting, None),
            Phase::Awake { .. } => (ComputerPhase::Awake, None),
            Phase::Sleeping { .. } => (ComputerPhase::Sleeping, None),
            Phase::Failed { why } => (ComputerPhase::WontWake, Some(why.clone())),
        };
        let origin = self.cfg.computer_origin(&id).ok_or_else(|| CellError::host("a computer's origin needs FRAGMENT_HOST_SUFFIX"))?;
        Ok(ComputerView {
            computer: id,
            owner: self.must(MetaKey::Owner)?,
            image: self.must(MetaKey::Image)?,
            phase,
            why: why.or(self.meta(MetaKey::Note)?),
            agents: self.agents()?,
            origin,
        })
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        let path = req.path();
        match req.headers().get(KIND_HEADER)?.as_deref() {
            Some("keepalive") => return self.keepalive(&req).await,
            Some("port") => return self.port(req).await,
            _ => {}
        }
        if req.headers().get(INTERNAL_HEADER)?.is_none() {
            return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}")));
        }
        match path.trim_start_matches('/') {
            "computer/init" => {
                let b: Init = body_json(&mut req).await?;
                if !valid_computer_id(&b.id) {
                    return Err(CellError::invalid("a computer's id is computer:<24 hex>"));
                }
                match self.meta(MetaKey::Owner)? {
                    Some(owner) if owner != b.owner => return Err(CellError::new(ErrorCode::Forbidden, "this computer is someone else's")),
                    Some(_) => {}
                    None => {
                        let image = self.cfg.computer_image.clone().ok_or_else(|| CellError::host("this deployment makes no computers (FRAGMENT_COMPUTER_IMAGE)"))?;
                        self.set_meta(MetaKey::Id, &b.id)?;
                        self.set_meta(MetaKey::Owner, &b.owner)?;
                        self.set_meta(MetaKey::Image, &image)?;
                        self.set_meta(MetaKey::CreatedAt, &js::now_ms().to_string())?;
                    }
                }
                json_response(&self.view()?)
            }
            "computer/view" => json_response(&self.view()?),
            "computer/wake" => {
                let b: WakeBody = body_json(&mut req).await?;
                self.must(MetaKey::Id)?;
                self.wake(b.why).await?;
                json_response(&self.view()?)
            }
            "computer/sleep" => {
                self.must(MetaKey::Id)?;
                self.drive(Event::Sleep).await?;
                json_response(&self.view()?)
            }
            "computer/assign" => {
                let b: Assign = body_json(&mut req).await?;
                if self.must(MetaKey::Owner)? != b.owner {
                    return Err(CellError::new(ErrorCode::Forbidden, "an agent runs on its owner's computer"));
                }
                let n = self.rows("SELECT COUNT(*) AS n FROM agents WHERE fragment != ?", vec![b.fragment.as_str().into()])?;
                if n.first().and_then(|r| r["n"].as_u64()).unwrap_or(0) >= AGENTS_MAX {
                    return Err(CellError::invalid(format!("a computer runs at most {AGENTS_MAX} agents")));
                }
                // a new agent may use every connection its owner has (decision
                // 44); assigned again, it keeps what its owner narrowed it to
                self.exec(
                    "INSERT INTO agents (fragment, identity, owner, added_at, connections) VALUES (?, ?, ?, ?, 'null') ON CONFLICT (fragment) DO UPDATE SET identity = excluded.identity",
                    vec![b.fragment.as_str().into(), b.identity.as_str().into(), b.owner.as_str().into(), js::now_ms().into()],
                )?;
                json_response(&self.view()?)
            }
            "computer/pin" => {
                let b: Pin = body_json(&mut req).await?;
                let images = js::from_js(&self.call("images", &[]).await?).map_err(CellError::host)?;
                if !images.as_array().is_some_and(|names| names.iter().any(|n| n == b.image.as_str())) {
                    return Err(CellError::invalid(format!("no image {:?} on this deployment (its images: {images})", b.image)));
                }
                // the next start takes it: a snapshot of the old image no longer
                // matches, so it starts from this one and restores /data (decision 19)
                self.set_meta(MetaKey::Image, &b.image)?;
                json_response(&self.view()?)
            }
            "computer/unassign" => {
                let b: Unassign = body_json(&mut req).await?;
                self.exec("DELETE FROM agents WHERE fragment = ?", vec![b.fragment.as_str().into()])?;
                json_response(&self.view()?)
            }
            "computer/connections" => {
                let mut b: SetConnections = body_json(&mut req).await?;
                if let Some(list) = b.connections.as_mut() {
                    list.sort();
                    list.dedup();
                    if let Some(c) = list.iter().find(|c| !self.cfg.connections.contains_key(c.as_str())) {
                        let offered: Vec<&String> = self.cfg.connections.keys().collect();
                        return Err(CellError::invalid(format!("no connection {c:?} on this deployment (its connections: {offered:?})")));
                    }
                }
                if self.rows("SELECT fragment FROM agents WHERE fragment = ?", vec![b.fragment.as_str().into()])?.is_empty() {
                    return Err(CellError::new(ErrorCode::NotFound, format!("{} does not run on this computer", b.fragment)));
                }
                let text = serde_json::to_string(&b.connections).map_err(|e| CellError::host(e.to_string()))?;
                assert_eq!(stored_connections(&text, &b.fragment)?, b.connections, "connections read back as they are written");
                self.exec("UPDATE agents SET connections = ? WHERE fragment = ?", vec![text.into(), b.fragment.as_str().into()])?;
                json_response(&self.view()?)
            }
            "computer/credential" => {
                let b: CredentialAsk = body_json(&mut req).await?;
                json_response(&self.credential(b).await?)
            }
            "computer/joined" => {
                let b: JoinedAsk = body_json(&mut req).await?;
                json_response(&self.joined(b).await?)
            }
            "computer/ticket" => {
                let b: TicketAsk = body_json(&mut req).await?;
                if b.identity != self.must(MetaKey::Owner)? {
                    return Err(CellError::new(ErrorCode::Forbidden, "only the computer's owner opens its ports"));
                }
                let token = js::random_hex::<24>();
                let expires = js::now_ms() + TICKET_TTL_MS;
                self.exec(
                    "INSERT INTO tickets (hash, port, identity, expires_at) VALUES (?, ?, ?, ?)",
                    vec![sha_hex(&token).into(), i64::from(b.port).into(), b.identity.as_str().into(), expires.into()],
                )?;
                self.trim("tickets", TICKETS_MAX)?;
                json_response(&json!({ "ticket": token, "port": b.port, "expiresAt": expires }))
            }
            "computer/redeem" => {
                let b: Redeem = body_json(&mut req).await?;
                let now = js::now_ms();
                let rows = self.rows("SELECT port, identity, expires_at FROM tickets WHERE hash = ?", vec![sha_hex(&b.ticket).into()])?;
                self.exec("DELETE FROM tickets WHERE hash = ? OR expires_at < ?", vec![sha_hex(&b.ticket).into(), now.into()])?;
                let Some(t) = rows.first().filter(|t| t["expires_at"].as_i64().unwrap_or(0) >= now) else {
                    return Err(CellError::new(ErrorCode::Unauthenticated, "this link was used or is too old: open the computer again"));
                };
                // 32 bytes, as every session's: its cookie is read as one (`auth::cookie_of`)
                let session = js::random_hex::<32>();
                let identity = t["identity"].as_str().unwrap_or_default();
                self.exec(
                    "INSERT INTO sessions (hash, identity, expires_at) VALUES (?, ?, ?)",
                    vec![sha_hex(&session).into(), identity.into(), (now + SESSION_TTL_MS).into()],
                )?;
                self.trim("sessions", SESSIONS_MAX)?;
                json_response(&json!({ "session": session, "port": t["port"], "maxAgeS": SESSION_TTL_MS / 1000 }))
            }
            "computer/guest" => json_response(&self.view()?),
            "computer/exited" => {
                let b: Exited = body_json(&mut req).await?;
                self.drive(Event::Exited { generation: b.generation }).await?;
                json_response(&json!({ "ok": true }))
            }
            "computer/tab" => {
                let b: Tab = body_json(&mut req).await?;
                let socket = Socket::Tab;
                self.drive(if b.open { Event::Opened { socket } } else { Event::Closed { socket } }).await?;
                json_response(&json!({ "ok": true }))
            }
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}"))),
        }
    }

    /// The secret to swap in for `b`'s placeholder, when its agent runs here
    /// and may use it: `{secret, owner, identity}` (the agent's), so the
    /// egress meters a key's call to its owner. An agent may use every
    /// connection its owner has unless its owner narrowed it to a list
    /// (decision 44: a person's agents are not fenced from each other, so
    /// the list is a role's specialization, not a wall).
    async fn credential(&self, b: CredentialAsk) -> CellResult<Value> {
        let rows = self.rows("SELECT owner, identity, connections FROM agents WHERE fragment = ?", vec![b.agent.as_str().into()])?;
        let agent = rows.first().ok_or_else(|| CellError::new(ErrorCode::Forbidden, format!("{} does not run on this computer", b.agent)))?;
        let owner = agent["owner"].as_str().unwrap_or_default().to_string();
        let identity = agent["identity"].as_str().unwrap_or_default().to_string();
        let secret = match (b.connection, b.key) {
            (Some(provider), None) => {
                let narrowed = stored_connections(agent["connections"].as_str().unwrap_or_default(), &b.agent)?;
                if let Some(list) = narrowed.filter(|list| !list.contains(&provider)) {
                    return Err(CellError::new(
                        ErrorCode::Forbidden,
                        format!(
                            "{} may not use the {provider} connection: its owner narrowed it to {} on their computer (null lets it use every connection its owner has)",
                            b.agent,
                            if list.is_empty() { "none".to_string() } else { list.join(", ") }
                        ),
                    ));
                }
                self.connection_token(&owner, &provider).await
            }
            (None, Some(name)) => {
                if !self.cfg.operator_keys.contains_key(&name) {
                    return Err(CellError::invalid(format!("no operator key {name:?} on this deployment")));
                }
                if !self.cfg.key_prices.iter().any(|p| p.key == name) {
                    return Err(CellError::new(ErrorCode::Forbidden, format!("the operator key {name} has no price on this deployment, so it is not lent")));
                }
                // a paid call its owner's ledger would refuse is never made (decision 27)
                let may = crate::ledger::MaySpend { spend: Spend::AgentTurn, fragment: None, by_owner: true };
                if let Err(e) = crate::ledger::ask(&self.env, &owner, &may).await {
                    if e.refused.is_some() {
                        return Err(CellError::new(e.code, e.message));
                    }
                    console_error!("{}", json!({ "computer": "key", "ledger": e.message }));
                }
                crate::keys::operator_key(&self.env, &name).ok_or_else(|| CellError::host(format!("{} is not set", swap::key_secret_name(&name))))
            }
            _ => Err(CellError::invalid("ask for one connection or one key")),
        }?;
        Ok(json!({ "secret": secret, "owner": owner, "identity": identity }))
    }

    /// The agent `b.identity` was added to `b.fragment` (runs_on.rs, from
    /// that fragment's outbox): when it runs here, its own fragment posts
    /// `joined` on its `tasks` (once by the membership: a retry posts
    /// nothing twice), so a guest that is awake lists its fragments again.
    /// Answers `{runs}`: whether it runs here, and so whether there is a
    /// computer to wake (its caller wakes it, on its own: a member's change
    /// never waits for a start). Here is no one's computer, or the
    /// identity no agent of it: nothing to tell.
    async fn joined(&self, b: JoinedAsk) -> CellResult<Value> {
        if !fragment_core::npub::is_identity(&b.identity) || !fragment_proto::valid_fragment_name(&b.fragment) || b.at <= 0 {
            return Err(CellError::invalid("a join names an identity, a fragment and when it joined"));
        }
        let Some(id) = self.meta(MetaKey::Id)? else { return Ok(json!({ "runs": false })) };
        let rows = self.rows("SELECT fragment FROM agents WHERE identity = ?", vec![b.identity.as_str().into()])?;
        let Some(agent) = rows.first().and_then(|r| r["fragment"].as_str()).map(str::to_string) else { return Ok(json!({ "runs": false })) };
        let posted = crate::fragment::ask(&self.env, &agent, "computer/joined", &json!({ "computer": id, "fragment": b.fragment, "at": b.at })).await?;
        console_log!("{}", json!({ "computer": id, "joined": b.fragment, "agent": agent, "posted": posted["posted"] }));
        Ok(json!({ "runs": true, "computer": id, "agent": agent, "posted": posted["posted"] }))
    }

    /// `owner`'s token for `provider`: the one held, or WorkOS Pipes' for
    /// the WorkOS user `owner` signed in as.
    async fn connection_token(&self, owner: &str, provider: &str) -> CellResult<String> {
        let now = js::now_ms();
        let held = (owner.to_string(), provider.to_string());
        if let Some((token, until)) = self.tokens.borrow().get(&held) {
            if *until > now {
                return Ok(token.clone());
            }
        }
        let workos = self.cfg.workos()?;
        let call = crate::registry::calls::SubjectOf { identity: owner.into(), issuer: workos.issuer() };
        let user = crate::ask_registry(&self.env, &call)
            .await?
            .subject
            .ok_or_else(|| CellError::new(ErrorCode::NotConnected, format!("{owner} has no WorkOS account to connect {provider} with")))?;
        let (status, answer) = crate::keys::pipes_token(&self.env, &workos.api, provider, &user).await?;
        if status != 200 {
            let why = answer["message"].as_str().unwrap_or("no reason given");
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS refused {provider}'s token ({status}): {why}")));
        }
        if answer["active"] != true {
            let why = match answer["error"].as_str() {
                Some("needs_reauthorization") => format!("connect {provider} again: its account needs authorizing again"),
                _ => format!("connect {provider} first: no account is connected"),
            };
            return Err(CellError::new(ErrorCode::NotConnected, why));
        }
        let token = answer["access_token"]["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, "WorkOS answered no token"))?;
        let expires = answer["access_token"]["expires_at"].as_str().map(js_sys::Date::parse).filter(|t| t.is_finite()).map(|t| t as i64);
        let until = expires.map_or(now + TOKEN_HOLD_MAX_MS, |e| (e - TOKEN_MARGIN_MS).min(now + TOKEN_HOLD_MAX_MS));
        let mut tokens = self.tokens.borrow_mut();
        if tokens.len() >= TOKENS_MAX {
            tokens.clear();
        }
        tokens.insert(held, (token.to_string(), until));
        Ok(token.to_string())
    }

    /// Keeps the newest `max` rows of `table` (by expiry).
    fn trim(&self, table: &str, max: u64) -> CellResult<()> {
        assert!(matches!(table, "tickets" | "sessions"), "a table of this file's");
        self.exec(&format!("DELETE FROM {table} WHERE hash NOT IN (SELECT hash FROM {table} ORDER BY expires_at DESC LIMIT {max})"), vec![])
    }

    /// A keepalive closed: it counts only if its container is the running one.
    async fn keepalive_closed(&self, ws: &WebSocket) {
        let current = self.lifecycle().map(|l| format!("g{}", l.generation())).unwrap_or_default();
        if !self.state.get_tags(ws).contains(&current) {
            return;
        }
        if let Err(e) = self.drive(Event::Closed { socket: Socket::Keepalive }).await {
            console_error!("{}", json!({ "computer": "keepalive-closed", "error": e.message }));
        }
    }

    /// The guest's keepalive: accepted here, so it is the Computer DO's own
    /// activity (spike S3), and counted by the lifecycle while it is open.
    async fn keepalive(&self, req: &Request) -> CellResult<Response> {
        if req.headers().get("upgrade")?.is_none_or(|u| !u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("the keepalive is a WebSocket"));
        }
        let pair = WebSocketPair::new()?;
        // tagged with its container's start: a late close from an earlier one counts for nothing
        let generation = format!("g{}", self.lifecycle()?.generation());
        self.state.accept_websocket_with_tags(&pair.server, &["keepalive", &generation]);
        self.drive(Event::Opened { socket: Socket::Keepalive }).await?;
        Ok(Response::from_websocket(pair.client)?)
    }

    /// A request to one of its ports, from its owner (a session the router
    /// read from the cookie, or a signer it resolved).
    async fn port(&self, req: Request) -> CellResult<Response> {
        let owner = self.must(MetaKey::Owner)?;
        let who = match (req.headers().get(SESSION_HEADER)?, req.headers().get(SIGNER_HEADER)?) {
            (Some(session), _) => {
                let rows = self.rows("SELECT identity, expires_at FROM sessions WHERE hash = ?", vec![sha_hex(&session).into()])?;
                rows.first().filter(|r| r["expires_at"].as_i64().unwrap_or(0) >= js::now_ms()).and_then(|r| r["identity"].as_str().map(str::to_string))
            }
            (None, Some(signer)) => Some(signer),
            (None, None) => None,
        };
        if who.as_deref() != Some(owner.as_str()) {
            return Err(CellError::new(ErrorCode::Unauthenticated, "sign in to this computer: its owner opens it from the platform"));
        }
        let port: u16 = req.headers().get(PORT_HEADER)?.and_then(|p| p.parse().ok()).filter(|p| (1..=PORT_MAX).contains(p)).ok_or_else(|| CellError::invalid("name a port"))?;
        // a request to a sleeping computer's port wakes it, as a tab does
        let life = self.lifecycle()?;
        if !matches!(life.phase, Phase::Awake { .. }) {
            self.wake(Wake::Tab).await?;
        }
        let out = self.call("port", &[JsValue::from_f64(f64::from(port)), JsValue::from(req.inner())]).await?;
        let resp: worker_sys::web_sys::Response = out.dyn_into().map_err(|_| CellError::host("the port answered no Response"))?;
        Ok(Response::from(resp))
    }
}

/// The internal call `path` with `body` on computer `id`.
pub(crate) async fn ask(env: &Env, id: &str, path: &str, body: &Value) -> CellResult<Value> {
    crate::routed::ask_object(env, "COMPUTER", id, path, body).await
}

fn view_of(v: Value) -> CellResult<ComputerView> {
    serde_json::from_value(v).map_err(|e| CellError::host(format!("the computer's view: {e}")))
}

/// `/api/computers…` (the router has resolved the signer `who`, a person).
/// Computer `id`'s view, when `who` owns it (anyone else finds none).
async fn owned(env: &Env, who: &str, id: &str) -> CellResult<ComputerView> {
    if !valid_computer_id(id) {
        return Err(CellError::new(ErrorCode::NotFound, "no such computer"));
    }
    let v = view_of(ask(env, id, "computer/view", &json!({})).await?)?;
    if v.owner != who {
        return Err(CellError::new(ErrorCode::NotFound, "no such computer"));
    }
    Ok(v)
}

/// `{image}` from a body.
fn image_named(body: &[u8]) -> CellResult<String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
    v["image"].as_str().filter(|i| fragment_core::price::printable(i, 64)).map(str::to_string).ok_or_else(|| CellError::invalid("name an image"))
}

pub(crate) async fn route(env: &Env, who: &str, kind: IdentityKind, method: Method, rest: &[&str], body: &[u8]) -> CellResult<Response> {
    if kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "computers are people's: an agent runs on its owner's"));
    }
    match (method, rest) {
        (Method::Post, []) => {
            let id = fragment_core::computer::default_computer_of(who);
            json_response(&view_of(ask(env, &id, "computer/init", &json!({ "id": id, "owner": who })).await?)?)
        }
        (Method::Get, []) => {
            let id = fragment_core::computer::default_computer_of(who);
            let list = match ask(env, &id, "computer/view", &json!({})).await {
                Ok(v) => vec![view_of(v)?],
                Err(e) if e.code == ErrorCode::NotFound => vec![],
                Err(e) => return Err(e),
            };
            // the image a new computer gets: one pinned to another may update to it
            json_response(&json!({ "computers": list, "defaultImage": Config::from_env(env).computer_image }))
        }
        (Method::Get, [id]) => json_response(&owned(env, who, id).await?),
        (Method::Post, [id, "wake"]) => {
            owned(env, who, id).await?;
            json_response(&view_of(ask(env, id, "computer/wake", &json!({ "why": Wake::Owner })).await?)?)
        }
        (Method::Post, [id, "sleep"]) => {
            owned(env, who, id).await?;
            json_response(&view_of(ask(env, id, "computer/sleep", &json!({})).await?)?)
        }
        (Method::Put, [id, "image"]) => {
            owned(env, who, id).await?;
            json_response(&view_of(ask(env, id, "computer/pin", &json!({ "image": image_named(body)? })).await?)?)
        }
        (Method::Put, [id, "agents", fragment]) => {
            owned(env, who, id).await?;
            // the agent fragment takes the computer first (its owner must be the
            // computer's), and its key becomes the agent's identity
            let took = crate::fragment::ask(env, fragment, "computer/assign", &json!({ "computer": id, "owner": who })).await?;
            let identity = took["identity"].as_str().ok_or_else(|| CellError::host("the agent fragment named no identity"))?;
            let body = json!({ "fragment": fragment, "identity": identity, "owner": who });
            json_response(&view_of(ask(env, id, "computer/assign", &body).await?)?)
        }
        (Method::Put, [id, "agents", fragment, "connections"]) => {
            owned(env, who, id).await?;
            let v: Value = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            // a body that forgot the field is not a reset to every connection
            if v.get("connections").is_none() {
                return Err(CellError::invalid("name connections: a list of providers, or null for every connection its owner has"));
            }
            let b: AgentConnections = serde_json::from_value(v).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            if let Some(list) = &b.connections {
                if list.len() > swap::CREDENTIALS_MAX || list.iter().any(|c| !swap::valid_name(c)) {
                    return Err(CellError::invalid(format!("connections are at most {} provider names, or null", swap::CREDENTIALS_MAX)));
                }
            }
            let set = json!({ "fragment": fragment, "connections": b.connections });
            json_response(&view_of(ask(env, id, "computer/connections", &set).await?)?)
        }
        (Method::Delete, [id, "agents", fragment]) => {
            owned(env, who, id).await?;
            crate::fragment::ask(env, fragment, "computer/unassign", &json!({ "computer": id, "owner": who })).await?;
            json_response(&view_of(ask(env, id, "computer/unassign", &json!({ "fragment": fragment })).await?)?)
        }
        (Method::Post, [id, "ports", port, "ticket"]) => {
            let v = owned(env, who, id).await?;
            let port: u16 = port.parse().ok().filter(|p| (1..=PORT_MAX).contains(p)).ok_or_else(|| CellError::invalid("a port is 1-65535"))?;
            let t = ask(env, id, "computer/ticket", &json!({ "port": port, "identity": who })).await?;
            let ticket = t["ticket"].as_str().ok_or_else(|| CellError::host("the computer minted no ticket"))?;
            let url = format!("{}/__ticket?t={ticket}&next=/p/{port}/", v.origin);
            json_response(&PortTicket { url, expires_at: t["expiresAt"].as_i64().unwrap_or(0) })
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/computers/{}", m.as_ref(), rest.join("/")))),
    }
}

/// A request to a computer's own origin: `/__ticket` signs a browser in
/// (a ticket its owner minted becomes this origin's session cookie), and
/// `/p/<port>/…` is that port, for its owner. Its answers may be framed by
/// the platform's page alone (the shell's tab onto a port: decisions 11
/// and 41), never by a fragment's, which is one site with this origin.
pub(crate) async fn serve_host(req: Request, env: &Env, url: &Url, id: &str, signer: Option<String>) -> CellResult<Response> {
    let platform = Config::from_env(env).platform(url);
    let answered = match host_answer(req, env, url, id, signer).await {
        Ok(resp) => resp,
        Err(e) => e.response()?,
    };
    framed_by(answered, &platform)
}

/// `resp` with `frame-ancestors <platform>` added (an image's own policy
/// stays: a second one only narrows it). A socket's upgrade shows nothing,
/// and keeps the answer it had.
fn framed_by(resp: Response, platform: &str) -> CellResult<Response> {
    assert!(fragment_core::frames::is_origin(platform), "frame-ancestors names the platform's origin alone");
    if resp.status_code() == 101 {
        return Ok(resp);
    }
    let h = resp.headers().clone();
    h.append("content-security-policy", &format!("frame-ancestors {platform}"))?;
    Ok(resp.with_headers(h))
}

/// Which of a browser's cookies name its session on a computer's origin,
/// from the Fetch Metadata it sends, as on a fragment's (`crate::fetched`):
/// every fragment's page is one site with this origin, so a SameSite=Lax
/// cookie rides along on its images, fetches and frames, and the frame
/// cookie, partitioned under the platform's page, on those of any page
/// framed there. The session counts only on this origin's own page's
/// requests, a top-level navigation (the site cookie), and a frame's
/// navigation (the frame cookie, its answer shown only in the platform's
/// page: `framed_by`); a socket only from this origin's own page.
fn session_of(req: &Request, url: &Url) -> CellResult<Option<String>> {
    let fetched = crate::fetched(req)?;
    let socket = req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    // a socket has no CORS: one from any other page is refused before its cookies are read
    if socket && req.headers().get("origin")?.is_some_and(|o| o != url.origin().ascii_serialization()) {
        return Err(CellError::new(ErrorCode::Forbidden, "a computer's socket opens from its own page"));
    }
    let secure = url.scheme() == "https";
    let site = if fetched.site { crate::auth::cookie_of(req, SESSION_COOKIE, secure, "/")? } else { None };
    let frame = if fetched.frame { crate::auth::cookie_of(req, FRAME_COOKIE, secure, "/")? } else { None };
    Ok(if fetched.framed { frame.or(site) } else { site.or(frame) })
}

async fn host_answer(req: Request, env: &Env, url: &Url, id: &str, signer: Option<String>) -> CellResult<Response> {
    let path = url.path().to_string();
    let secure = url.scheme() == "https";
    if path == "/__ticket" {
        let q = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
        let ticket = q("t").ok_or_else(|| CellError::invalid("this link names no ticket"))?;
        let next = q("next").filter(|n| n.starts_with("/p/") && !n.contains("//")).unwrap_or_else(|| "/p/6080/".into());
        let s = ask(env, id, "computer/redeem", &json!({ "ticket": ticket })).await?;
        let session = s["session"].as_str().ok_or_else(|| CellError::host("the computer made no session"))?;
        let max_age_s = s["maxAgeS"].as_i64().unwrap_or(0);
        // In a frame (the platform's tab onto the port), the session is the
        // frame's: a partitioned cookie for the page around it, which a
        // browser that blocks third-party cookies keeps (CHIPS). The site
        // cookie is SameSite=Lax, which a cross-site frame never sends.
        let cookie = match crate::fetched(&req)?.framed {
            true => crate::auth::frame_cookie(FRAME_COOKIE, session, "/", max_age_s, secure),
            false => crate::auth::set_cookie(SESSION_COOKIE, session, "/", max_age_s, secure),
        };
        let headers = Headers::new();
        headers.set("location", &next)?;
        headers.set("set-cookie", &cookie)?;
        headers.set("cache-control", "no-store")?;
        return Ok(Response::empty()?.with_status(303).with_headers(headers));
    }
    let Some(rest) = path.strip_prefix("/p/") else {
        return Err(CellError::new(ErrorCode::NotFound, "a computer serves its ports at /p/<port>/"));
    };
    let (port, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let port: u16 = port.parse().map_err(|_| CellError::invalid("a port is a number"))?;
    let session = session_of(&req, url)?;
    let headers = Headers::new();
    for k in ["accept", "content-type", "range", "if-none-match", "upgrade", "sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"] {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    if headers.has("upgrade")? {
        headers.set("connection", "Upgrade")?;
    }
    headers.set(KIND_HEADER, "port")?;
    headers.set(PORT_HEADER, &port.to_string())?;
    if let Some(s) = session {
        headers.set(SESSION_HEADER, &s)?;
    }
    if let Some(s) = signer {
        headers.set(SIGNER_HEADER, &s)?;
    }
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers);
    if !matches!(req.method(), Method::Get | Method::Head) {
        init.with_body(req.inner().body().map(JsValue::from));
    }
    let inner = Request::new_with_init(&format!("http://container/{tail}{query}"), &init)?;
    Ok(env.durable_object("COMPUTER")?.get_by_name(id)?.fetch_with_request(inner).await?)
}

/// Every request a computer's guest sends to `api.fragment.internal`,
/// `model.fragment.internal`, or a host the swap catches (entry.mjs's
/// `ComputerEgress`, whose props name the computer and the route). A class with a static method, as
/// `InternalRoute` is: worker-build exports classes by name.
#[wasm_bindgen(wasm_bindgen = worker::wasm_bindgen)]
pub struct ComputerEgress;

#[wasm_bindgen]
impl ComputerEgress {
    pub async fn handle(request: worker_sys::web_sys::Request, env: Env, ctx: worker_sys::Context, computer: String, route: String) -> worker_sys::web_sys::Response {
        let req = Request::from(request);
        let ctx = Context::new(ctx);
        let (method, path) = (req.method().to_string(), req.path());
        let answered = match route.as_str() {
            "api" => egress_api(req, &env, &ctx, &computer).await,
            "model" => egress_model(req, &env, &ctx, &computer).await,
            "swap" => egress_swap(req, &env, &ctx, &computer).await,
            r => Err(CellError::new(ErrorCode::NotFound, format!("no egress route {r}"))),
        };
        // one line per refusal (lesson 14): the guest's requests never reach
        // wrangler's request log, being answered in-process
        if let Err(e) = &answered {
            console_log!("{}", json!({ "egress": route, "computer": computer, "method": method, "path": path, "error": e.code, "message": e.message }));
        } else if let Ok(r) = &answered {
            if r.status_code() >= 400 {
                console_log!("{}", json!({ "egress": route, "computer": computer, "method": method, "path": path, "status": r.status_code() }));
            }
        }
        let resp = answered.or_else(|e| e.response().map_err(CellError::from)).unwrap_or_else(|_| Response::error("egress failed", 500).expect("a plain response"));
        resp.into()
    }
}

/// The guest's request to the platform's API (`/api/…`) or a fragment's
/// own routes (`/f/<fragment>/…`), as the agent its `x-fragment-agent`
/// names: the agent fragment signs it (NIP-98) only when it runs on this
/// computer, and the router answers it as it would from the internet.
/// Without the header, only the computer's own routes answer.
async fn egress_api(mut req: Request, env: &Env, ctx: &Context, computer: &str) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let url = req.url()?;
    let path = url.path().to_string();
    match path.as_str() {
        "/api/computer" => return json_response(&ask(env, computer, "computer/guest", &json!({})).await?),
        "/api/computer/keepalive" => {
            let headers = Headers::new();
            headers.set(KIND_HEADER, "keepalive")?;
            for k in ["upgrade", "sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"] {
                if let Some(v) = req.headers().get(k)? {
                    headers.set(k, &v)?;
                }
            }
            headers.set("connection", "Upgrade")?;
            let mut init = RequestInit::new();
            init.with_method(Method::Get).with_headers(headers);
            let inner = Request::new_with_init("https://computer.internal/keepalive", &init)?;
            return Ok(env.durable_object("COMPUTER")?.get_by_name(computer)?.fetch_with_request(inner).await?);
        }
        _ => {}
    }
    let agent = req.headers().get(AGENT_HEADER)?.ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "name the agent this acts as (x-fragment-agent)"))?;
    if !fragment_proto::valid_fragment_name(&agent) {
        return Err(CellError::invalid("x-fragment-agent names an agent fragment (<label>.<username>)"));
    }
    let platform = cfg.platform_url.clone().ok_or_else(|| CellError::host("a computer's egress needs FRAGMENT_PLATFORM_URL"))?;
    let arrived = Url::parse(&platform).map_err(|e| CellError::host(format!("FRAGMENT_PLATFORM_URL: {e}")))?;
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let target = match path.strip_prefix("/f/") {
        Some(rest) => {
            let (name, tail) = rest.split_once('/').unwrap_or((rest, ""));
            if !fragment_proto::valid_fragment_name(name) {
                return Err(CellError::invalid("/f/<fragment>/… names a fragment"));
            }
            format!("{}/{tail}{query}", cfg.origin(&arrived, name))
        }
        None if path.starts_with("/api/") => format!("{platform}{path}{query}"),
        None => return Err(CellError::new(ErrorCode::NotFound, "the API is /api/… and a fragment's routes /f/<fragment>/…")),
    };
    let method = req.method();
    let socket = req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let body = match method {
        Method::Get | Method::Head => vec![],
        _ => crate::read_body(&mut req, EGRESS_BODY_MAX_BYTES).await?,
    };
    // a wake subscription is this computer's to ask for: the agent fragment
    // that signs for it also records which computer it wakes
    if method == Method::Post && path.ends_with("/subscriptions") {
        if let Ok(v) = serde_json::from_slice::<Value>(&body) {
            if v["wake"] == true {
                let fragment = target_fragment(&path).ok_or_else(|| CellError::invalid("a wake subscription is on a fragment"))?;
                let channel = v["channel"].as_str().ok_or_else(|| CellError::invalid("name a channel"))?;
                let who = crate::fragment::ask(env, &agent, "computer/identity", &json!({ "computer": computer })).await?;
                let identity = who["identity"].as_str().ok_or_else(|| CellError::host("the agent fragment named no identity"))?;
                let body = json!({ "computer": computer, "identity": identity, "channel": channel });
                return json_response(&crate::fragment::ask(env, &fragment, "computer/subscribe", &body).await?);
            }
        }
    }
    let payload = (!body.is_empty()).then(|| hex::encode(Sha256::digest(&body)));
    let signed = crate::fragment::ask(
        env,
        &agent,
        "computer/sign",
        &json!({ "computer": computer, "method": method.as_ref(), "url": target, "payload": payload }),
    )
    .await?;
    let authorization = signed["header"].as_str().ok_or_else(|| CellError::host("the agent fragment signed nothing"))?;
    let headers = Headers::new();
    for k in ["content-type", "accept", "if-none-match", "range"] {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    if socket {
        headers.set("upgrade", "websocket")?;
        headers.set("connection", "Upgrade")?;
        for k in ["sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"] {
            if let Some(v) = req.headers().get(k)? {
                headers.set(k, &v)?;
            }
        }
    }
    headers.set("authorization", authorization)?;
    if !body.is_empty() {
        // a body made from bytes names no length, and a blob's upload needs one
        headers.set("content-length", &body.len().to_string())?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if !body.is_empty() {
        init.with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let out = Request::new_with_init(&target, &init)?;
    crate::route(out, env, ctx).await
}

/// The guest's model call (docs/computers.md, Models): `POST
/// /v1/chat/completions`, OpenAI's shape, with `model` a tier and
/// `x-fragment-agent`. It is the platform's model route as that agent
/// (`models::route`, signed as egress_api signs), which bounds it, meters
/// it to the agent's owner, and refuses at zero credit. Its auth headers
/// are the guest's and go nowhere; any other path is 404, unmetered.
async fn egress_model(mut req: Request, env: &Env, ctx: &Context, computer: &str) -> CellResult<Response> {
    if req.method() != Method::Post || req.path() != "/v1/chat/completions" {
        return Err(CellError::new(ErrorCode::NotFound, "the model intercept answers POST /v1/chat/completions"));
    }
    if req.headers().get(AGENT_HEADER)?.is_none() {
        return Err(CellError::new(ErrorCode::Unauthenticated, "name the agent this call is for (x-fragment-agent): its owner pays for it"));
    }
    let headers = Headers::new();
    for k in [AGENT_HEADER, "content-type", "accept"] {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    let body = crate::read_body(&mut req, crate::models::MODEL_BODY_MAX_BYTES).await?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
    let api = Request::new_with_init("http://api.fragment.internal/api/models/v1/chat/completions", &init)?;
    egress_api(api, env, ctx, computer).await
}

/// Headers a hop answers for itself, never sent on.
const HOP_HEADERS: [&str; 9] = ["connection", "keep-alive", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "host"];

/// A guest's request to a connection's or an operator key's host
/// (decisions 22 and 37): each placeholder in its headers is swapped for
/// the credential when the request goes to one of that credential's own
/// hosts, for the agent `x-fragment-agent` names, and it goes on over
/// HTTPS. A request with no placeholder goes on as it is (decision 43).
/// Only headers carry placeholders; a body or a URL is sent as it came.
async fn egress_swap(mut req: Request, env: &Env, ctx: &Context, computer: &str) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let url = req.url()?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let agent = req.headers().get(AGENT_HEADER)?;
    let mut found: Vec<(String, String, Vec<Credential>)> = vec![];
    let mut wanted: Vec<Credential> = vec![];
    for (name, value) in req.headers().entries() {
        if HOP_HEADERS.contains(&name.as_str()) || name == AGENT_HEADER {
            continue;
        }
        let creds = swap::placeholders(&value).map_err(CellError::invalid)?;
        for c in &creds {
            let (map, what) = match c {
                Credential::Connection(p) => (&cfg.connections, p),
                Credential::Key(k) => (&cfg.operator_keys, k),
            };
            let hosts = map.get(what).ok_or_else(|| CellError::invalid(format!("{} names nothing on this deployment", c.placeholder())))?;
            if !hosts.contains(&host) {
                return Err(CellError::new(ErrorCode::Forbidden, format!("{} is for {}, not {host}", c.placeholder(), hosts.join(", "))));
            }
            if !wanted.contains(c) {
                wanted.push(c.clone());
            }
        }
        found.push((name, value, creds));
    }
    if wanted.len() > swap::PLACEHOLDERS_MAX {
        return Err(CellError::invalid(format!("at most {} placeholders a request", swap::PLACEHOLDERS_MAX)));
    }
    let mut resolved = BTreeMap::new();
    // whose ledger a key's call goes on, and as which agent
    let mut payer: Option<(String, String)> = None;
    if !wanted.is_empty() {
        let agent = agent.ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "name the agent this acts as (x-fragment-agent)"))?;
        if !fragment_proto::valid_fragment_name(&agent) {
            return Err(CellError::invalid("x-fragment-agent names an agent fragment (<label>.<username>)"));
        }
        for c in &wanted {
            let ask_body = match c {
                Credential::Connection(p) => json!({ "agent": agent, "connection": p }),
                Credential::Key(k) => json!({ "agent": agent, "key": k }),
            };
            let answer = ask(env, computer, "computer/credential", &ask_body).await?;
            let secret = answer["secret"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| CellError::host("the computer resolved no credential"))?;
            resolved.insert(c.clone(), secret.to_string());
            if let (Some(owner), Some(identity)) = (answer["owner"].as_str(), answer["identity"].as_str()) {
                payer = Some((owner.to_string(), identity.to_string()));
            }
        }
    }
    let headers = Headers::new();
    for (name, value, creds) in &found {
        headers.set(name, &if creds.is_empty() { value.clone() } else { swap::swapped(value, &resolved) })?;
    }
    let method = req.method();
    let body = match method {
        Method::Get | Method::Head => vec![],
        _ => crate::read_body(&mut req, EGRESS_BODY_MAX_BYTES).await?,
    };
    if !body.is_empty() {
        headers.set("content-length", &body.len().to_string())?;
    }
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let target = match &cfg.swap_upstream {
        Some(upstream) => {
            headers.set("x-fragment-upstream-host", &host)?;
            format!("{upstream}{}{query}", url.path())
        }
        None => format!("https://{host}{}{query}", url.path()),
    };
    let mut init = RequestInit::new();
    // a redirect is the guest's to follow: followed here, it would carry
    // the swapped credential to wherever it points
    init.with_method(method.clone()).with_headers(headers).with_redirect(RequestRedirect::Manual);
    if !body.is_empty() {
        init.with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let out = Fetch::Request(Request::new_with_init(&target, &init)?)
        .send()
        .await
        .map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("{host} did not answer: {e}")))?;
    // each key's call the provider answered is metered to the agent's owner
    // (decision 37), after the answer: a failed meter is logged, never retried
    // into the guest's call
    if let Some((owner, identity)) = payer.filter(|_| out.status_code() < 500) {
        for key in wanted.iter().filter_map(|c| match c {
            Credential::Key(k) => Some(k.clone()),
            Credential::Connection(_) => None,
        }) {
            let reference = format!("key:{computer}:{}", js::random_hex::<12>());
            let row = MeterRow {
                reference: reference.clone(),
                usage: Usage::Key { key, units: 1 },
                fragment: None,
                agent: Some(identity.clone()),
                computer: Some(computer.to_string()),
                at_ms: js::now_ms(),
            };
            let (env, owner) = (env.clone(), owner.clone());
            ctx.wait_until(async move {
                if let Err(e) = crate::ledger::ask(&env, &owner, &Meter { batch: reference, rows: vec![row] }).await {
                    console_error!("{}", json!({ "egress": "swap", "meter": e.message }));
                }
            });
        }
    }
    // one line per swap (lesson 14), naming the credentials, never their values
    if !wanted.is_empty() {
        let names: Vec<String> = wanted.iter().map(Credential::placeholder).collect();
        console_log!("{}", json!({ "egress": "swap", "computer": computer, "host": host, "method": method.as_ref(), "credentials": names, "status": out.status_code() }));
    }
    Ok(out)
}

/// The fragment a guest's `/f/<fragment>/…` or `/api/f/<fragment>/…` path names.
fn target_fragment(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/f/").or_else(|| path.strip_prefix("/api/f/"))?;
    let name = rest.split('/').next()?;
    fragment_proto::valid_fragment_name(name).then(|| name.to_string())
}
