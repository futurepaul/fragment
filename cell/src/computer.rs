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
//!   and the keepalive socket that holds it awake. Its own view lists, per
//!   agent, the credentials the agent may use, each a placeholder that
//!   names the agent (`swap::TagKey`). A request to a provider's host has
//!   its placeholders swapped for the credentials (`egress_swap`,
//!   decisions 22 and 37), which this cell resolves: for the agent the tag
//!   names, among those that run here now (any of its owner's providers,
//!   unless its owner narrowed them: decision 44); a connection's token
//!   from WorkOS Pipes, asked for each request; an operator
//!   key, metered; an own key its owner gave, sealed here. Each call a
//!   provider answered is counted by agent and month (`uses`).
//! - **Its agents' new fragments** (`computer/joined`, from a fragment an
//!   agent of its was added to): the agent's own fragment posts `joined`
//!   on its `tasks`, and the fragment that added it then wakes the
//!   computer (`Wake::Joined`; docs/computers.md).
//! - **A wipe of its owner** (docs/api.md, Operators: `computer/wipe`)
//!   marks it wiped before anything else, destroys its container, deletes
//!   every save from R2 (each record, then whatever else is under its saves'
//!   prefix) and empties its record, the snapshot's id with it (Cloudflare
//!   deletes no snapshot: forgotten, it is never restored, and expires in
//!   30 days). From the mark on it starts, saves and writes nothing, and
//!   answers as a computer never made: an event already under way (a save,
//!   a start) fails at its next write.

use std::cell::RefCell;
use std::collections::BTreeMap;

use fragment_core::catalog::{self, Kind};
use fragment_core::computer::{Action, Event, Lifecycle, Phase, Plan as Restore, Rules, Save, SaveNote, Saves, Socket, Step, Wake, HOLD_WAIT_MS};
use fragment_core::ledger::{Meter, MeterRow, Month, Release, Reserve, Settle, Spend};
use fragment_core::price::Usage;
use fragment_core::swap::{self, Placeholder, Plan};
use fragment_proto::computer::{valid_computer_id, valid_port_path, AgentConnections, AgentCredential, ComputerAgent, ComputerPhase, ComputerUses, ComputerView, PortTicket, PortTicketAsk, ProviderState, ProviderUse, RestoreSource, PORT_PATH_MAX_BYTES};
use fragment_proto::{ErrorCode, IdentityKind};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::wasm_bindgen::{self, prelude::*, JsCast};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::{body_json, json_response};
use crate::js;

mod own_models;

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

/// `agents.connections` is JSON: `null`, every provider the owner has
/// (decision 44), or a list that narrows it. An agent's row names it as
/// it is made (`computer/assign`), so the column's default is never read.
/// `uses` counts each agent's calls to each provider a month (`Month`'s
/// index) and what they were charged; `own_keys` holds the owner's own
/// keys, sealed for this object. `model_choices`, `chatgpt` (one row: the
/// owner's Sign in with ChatGPT, its tokens sealed) and `model_uses` are
/// their own models' (own_models.rs).
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS agents (fragment TEXT PRIMARY KEY, identity TEXT NOT NULL, owner TEXT NOT NULL, added_at INTEGER NOT NULL, connections TEXT NOT NULL DEFAULT 'null');
CREATE TABLE IF NOT EXISTS awake (from_ms INTEGER PRIMARY KEY, to_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tickets (hash TEXT PRIMARY KEY, port INTEGER NOT NULL, identity TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (hash TEXT PRIMARY KEY, identity TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS uses (month INTEGER NOT NULL, provider TEXT NOT NULL, agent TEXT NOT NULL, calls INTEGER NOT NULL, micros INTEGER NOT NULL, PRIMARY KEY (month, provider, agent));
CREATE TABLE IF NOT EXISTS own_keys (provider TEXT PRIMARY KEY, sealed TEXT NOT NULL, set_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS model_choices (role TEXT PRIMARY KEY, provider TEXT NOT NULL, model TEXT NOT NULL, set_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS chatgpt (one INTEGER PRIMARY KEY CHECK (one = 1), client_id TEXT NOT NULL, sealed TEXT, expires_at INTEGER NOT NULL, scopes TEXT NOT NULL, email TEXT, state TEXT NOT NULL, refreshing_until INTEGER NOT NULL, set_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS model_uses (month INTEGER NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL, role TEXT NOT NULL, calls INTEGER NOT NULL, input INTEGER NOT NULL, cached INTEGER NOT NULL, cache_write INTEGER NOT NULL, output INTEGER NOT NULL, PRIMARY KEY (month, provider, model, role));
";
/// The one row a wiped computer keeps: when it was wiped. Apart from
/// `SCHEMA`, whose tables the wipe drops.
const WIPED_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wiped (at INTEGER NOT NULL);";
/// R2 pages (up to 1000 keys each) one `computer/wipe` deletes under its
/// saves' prefix; the rest are the next call's.
const WIPE_PAGES_PER_CALL: usize = 10;
/// The Durable Object class a sealed own key names (`keys::scope`).
const SEAL_CLASS: &str = "Computer";
/// Months of uses kept (this one and the twelve before it).
const USES_MONTHS_KEPT: u32 = 13;
/// Use rows one month holds at most (providers × agents, with room for
/// agents that came and went).
const USES_ROWS_MAX: u64 = 4 * (catalog::PROVIDERS_MAX as u64) * AGENTS_MAX;
/// How long a connection's state, as Pipes said it, is believed: the
/// guest reads its credentials every few seconds, and asking WorkOS that
/// often is not ours to do. The person's own read of their connections
/// tells it at once (`computer/own-keys`), and so does each swap (what
/// Pipes answered for its token).
const STATES_TTL_MS: i64 = 60_000;
/// After WorkOS did not answer, its states are asked again no sooner.
const STATES_RETRY_MS: i64 = 10_000;

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
/// The markers the DO leaves in the container (docs/computers.md, "What
/// every image carries"): the restore gate's, and a save's hold, which the
/// guest reads as "claim no turn, and say `held` once nothing is claimed
/// and what you keep is copied" (P2 of docs/explorations/pi-durable.md).
/// A hold clears the last answer before it asks, so only an answer to it
/// counts. Each needs only `sh` (and its `test`), `mkdir`, `touch`, `rm`
/// and `cat`, which every image carries.
const RESTORED_MARK: &str = "mkdir -p /run/computer && touch /run/computer/restored";
const HOLD_MARK: &str = "rm -f /run/computer/held /run/computer/unheld && mkdir -p /run/computer /data/work && touch /run/computer/hold";
const HOLD_UNMARK: &str = "rm -f /run/computer/hold /run/computer/held /run/computer/unheld";
const HELD_TEST: &str = "test -e /run/computer/held";
const HELD_READ: &str = "cat /run/computer/held";
/// A guest's word on a hold it has not answered (docs/computers.md, "The
/// hold"), read only once the hold's wait ran out, as logged.
const UNHELD_READ: &str = "cat /run/computer/unheld 2>/dev/null || true";
/// The read of it answers within this, or the hold's line says so.
const UNHELD_EXEC_MS: i64 = 5_000;
/// The image's check of what a start restored (docs/computers.md, "Data and
/// the restore gate"), when it carries one: run after the restore and
/// before the gate opens. Its exit `CHECK_UNUSABLE` says the save is
/// unusable; any other but 0 says the check itself failed.
const CHECK: &str = "test ! -x /usr/local/bin/computer-check || exec /usr/local/bin/computer-check";
const CHECK_EXEC_MS: i64 = 120_000;
const CHECK_UNUSABLE: i64 = 3;
/// What a save saves (docs/computers.md, "Data and the restore gate"; step 2
/// of docs/durable-computers.md): `/data/work`, what the guest's tools
/// write, and the rest of `/data`, the guest's own state, each a record of
/// its own and restored together, the first first (a restore replaces its
/// directory whole, so `/data`'s record goes in before the work's).
/// `/data`'s record leaves the work out (`WORK_LEFT_OUT`), and what a held
/// guest names; the work's record leaves nothing out.
const SAVED_DIRS: [&str; 2] = ["/data", "/data/work"];
const WORK_LEFT_OUT: &str = "/work/";
/// The lever's failed saves at most at once (`fail-saves`).
const FAIL_SAVES_MAX: u32 = 100;
/// A marker's exec answers within this, or it failed: a sleep goes on
/// without its hold, a start fails.
const MARK_EXEC_MS: i64 = 30_000;
/// The largest request body the egress signs and hands on (it is hashed
/// for NIP-98 first, so it is read whole).
const EGRESS_BODY_MAX_BYTES: usize = 32 * 1024 * 1024;
/// A port number the platform proxies to.
const PORT_MAX: u16 = 65_535;
/// Awake intervals one flush sends (one is made every five minutes awake).
const METER_ROWS_MAX: u64 = 64;

#[derive(Clone, Copy)]
enum MetaKey {
    Id,
    Owner,
    Image,
    CreatedAt,
    Lifecycle,
    /// What the DO knows of its saves (`fragment_core::computer::Saves`):
    /// the newest saves of `/data` with their `DirectoryBackup` records
    /// (the authority on what a wake restores, handed back to restore and
    /// to delete them), the snapshot that caches the current one, what each
    /// start restored, its rollbacks, and the records let go of until their
    /// deletes worked. The one save kept before several
    /// were (the old `backup` key) is not read: a hard cut.
    Saves,
    /// Why the last wake was refused.
    Note,
    /// What its saves' failures say (`SaveNote`): a sleep that kept its
    /// container, or slept unsaved; gone once a save works.
    SaveNote,
    /// The test lever's saves still to fail (`fail-saves`).
    FailSaves,
    /// The owner's WorkOS user, once the registry named it
    /// (`workos_user`): `{owner, issuer, subject}`.
    WorkosUser,
    /// This host's id for Sign in with ChatGPT (`ext_agent_host_id`):
    /// made once, kept through sign-outs (own_models.rs).
    ChatgptHost,
}

impl MetaKey {
    fn key(self) -> &'static str {
        match self {
            MetaKey::Id => "id",
            MetaKey::Owner => "owner",
            MetaKey::Image => "image",
            MetaKey::CreatedAt => "created_at",
            MetaKey::Lifecycle => "lifecycle",
            MetaKey::Saves => "saves",
            MetaKey::Note => "note",
            MetaKey::SaveNote => "save_note",
            MetaKey::FailSaves => "fail_saves",
            MetaKey::WorkosUser => "workos_user",
            MetaKey::ChatgptHost => "chatgpt_host",
        }
    }
}

/// What a start restores, as it was chosen at its beginning.
struct Planned {
    plan: Restore,
    /// The pinned image's name, and its reference now (what the start runs).
    image: String,
    reference: Option<String>,
    /// The save it restores (its records), when there is one.
    save: Option<Save>,
}

/// Why a start did not come up.
enum Unstarted {
    /// The container, the runtime or the platform failed: it may pass.
    Failed(CellError),
    /// The save it restored never will (its archive is gone or altered).
    Unusable(String),
}

impl From<CellError> for Unstarted {
    fn from(e: CellError) -> Unstarted {
        Unstarted::Failed(e)
    }
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
    /// The owner's connections' states as Pipes last said them, and until
    /// when they are believed (`STATES_TTL_MS`). In memory only. Only what
    /// the guest's view lists: a token is asked of Pipes for each swap.
    states: RefCell<Option<(BTreeMap<String, ProviderState>, i64)>>,
    /// Its owner was wiped (`wiped`'s row, read as it starts, or set by
    /// the wipe): no write lands, and it answers as no computer.
    wiped: std::cell::Cell<bool>,
}

impl DurableObject for ComputerCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        let sql = state.storage().sql();
        sql.exec(SCHEMA, None).expect("the Computer schema applies");
        sql.exec(WIPED_SCHEMA, None).expect("the wiped row's schema applies");
        let marks: Vec<Value> = sql.exec("SELECT COUNT(*) AS n FROM wiped", None).and_then(|c| c.to_array()).expect("the wiped row reads");
        let wiped = marks.first().and_then(|r| r["n"].as_i64()).expect("COUNT answers a row") > 0;
        let cfg = Config::from_env(&env);
        ComputerCell { state, raw, cfg, env, adopted: std::cell::Cell::new(false), states: RefCell::default(), wiped: std::cell::Cell::new(wiped) }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(r) => Ok(r),
            Err(e) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        if !self.wiped.get() && self.meta(MetaKey::Id).ok().flatten().is_some() {
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

/// A wiped computer's answer: as no computer.
fn wiped_computer() -> CellError {
    CellError::new(ErrorCode::NotFound, "no such computer (its owner was wiped)")
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
    /// `None`: every provider the owner has (decision 44).
    connections: Option<Vec<String>>,
}

/// What an agent may have swapped in, as `agents.connections` keeps it.
fn stored_connections(text: &str, fragment: &str) -> CellResult<Option<Vec<String>>> {
    serde_json::from_str(text).map_err(|e| CellError::host(format!("{fragment}'s stored connections {text:?}: {e}")))
}

/// Whether an agent narrowed to `narrowed` may use `provider`.
fn may_use(narrowed: &Option<Vec<String>>, provider: &str) -> bool {
    narrowed.as_ref().is_none_or(|list| list.iter().any(|p| p == provider))
}

/// `computer/credentials`' body: the placeholders a request to `host`
/// carries, each its provider and tag (the egress planned them).
#[derive(Deserialize)]
struct CredentialsAsk {
    host: String,
    placeholders: Vec<Asked>,
}

#[derive(Deserialize)]
struct Asked {
    provider: String,
    tag: String,
}

/// `computer/used`'s body: an agent's calls a provider answered, at `at`,
/// each with what it was charged.
#[derive(Deserialize)]
struct UsedAsk {
    agent: String,
    at: i64,
    uses: Vec<Used>,
}

#[derive(Deserialize)]
struct Used {
    provider: String,
    micros: i64,
}

/// `computer/uses`' body: a month (`YYYY-MM`), or this one.
#[derive(Deserialize)]
struct UsesAsk {
    month: Option<String>,
}

/// `computer/own-key`'s body: the owner's own key for `provider`, or none.
#[derive(Deserialize)]
struct OwnKeyAsk {
    provider: String,
    key: Option<String>,
}

/// `computer/own-keys`' body: the owner's connections' states, as their
/// own read of them just found (kept for the guest's next read).
#[derive(Deserialize)]
struct OwnKeysAsk {
    #[serde(default)]
    connections: Option<BTreeMap<String, ProviderState>>,
}

/// `computer/joined`'s body: the agent `identity` was added to `fragment`,
/// its membership made at `at` (which keys the notice: runs_on.rs).
#[derive(Deserialize)]
struct JoinedAsk {
    identity: String,
    fragment: String,
    at: i64,
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

/// `computer/test`'s body (a test fleet's: `POST /api/test/computer`):
/// `times` is `fail-saves`', `on` is `always-on`'s.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestLever {
    op: String,
    #[serde(default)]
    times: Option<u32>,
    #[serde(default)]
    on: Option<bool>,
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

    /// Every write the computer makes (its meta, saves, lifecycle, agents,
    /// meters): none once it is wiped, so nothing under way when its wipe
    /// began (a save, a start's report) writes it again.
    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        if self.wiped.get() {
            return Err(wiped_computer());
        }
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

    fn lifecycle(&self) -> CellResult<Lifecycle> {
        match self.meta(MetaKey::Lifecycle)? {
            Some(text) => serde_json::from_str(&text).map_err(|e| CellError::host(format!("the stored lifecycle: {e}"))),
            None => Ok(Lifecycle::new()),
        }
    }

    fn saves(&self) -> CellResult<Saves> {
        match self.meta(MetaKey::Saves)? {
            Some(text) => serde_json::from_str(&text).map_err(|e| CellError::host(format!("the stored saves: {e}"))),
            None => Ok(Saves::default()),
        }
    }

    /// One fact about its saves, applied to them as stored. `f` is not
    /// async, so nothing runs between the read and the write: a handler
    /// beside this one applies its own fact before or after, never between.
    fn update_saves<T>(&self, f: impl FnOnce(&mut Saves) -> T) -> CellResult<T> {
        let mut saves = self.saves()?;
        let out = f(&mut saves);
        let text = serde_json::to_string(&saves).map_err(|e| CellError::host(e.to_string()))?;
        self.set_meta(MetaKey::Saves, &text)?;
        Ok(out)
    }

    fn delete_meta(&self, k: MetaKey) -> CellResult<()> {
        self.exec("DELETE FROM meta WHERE key = ?", vec![k.key().into()])
    }

    /// The deployment's rules for its lifecycle.
    fn rules(&self) -> Rules {
        Rules { unsaved_max_ms: self.cfg.computer_unsaved_max_ms }
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
        if let Some(exited) = self.take_over().await? {
            // the event that brought this isolate up applies after the exit
            queue.push(exited);
        }
        let mut refused = None;
        let rules = self.rules();
        // bounded: each event's actions report at most one event each, and a
        // chain of them ends at an action that reports none (a meter, an
        // unhold), or at a start or a sleep's end
        while let Some(event) = queue.pop() {
            let mut life = self.lifecycle()?;
            let logged = json!(event);
            let step = life.apply_with(event, js::now_ms(), &rules);
            // one line per event (lesson 14): what happened, and what it led to
            let mut line = json!({ "computer": self.meta(MetaKey::Id)?, "event": logged, "phase": life.phase, "actions": step.actions });
            if let Some(ended) = step.ended {
                line["ended"] = json!(ended);
            }
            if let SaveNote::Says(note) = &step.note {
                line["note"] = json!(note);
            }
            console_log!("{line}");
            self.set_meta(MetaKey::Lifecycle, &serde_json::to_string(&life).map_err(|e| CellError::host(e.to_string()))?)?;
            // in the same write as the lifecycle (nothing awaits between), and
            // before its actions: the start it may make reads how this life ended
            if let Some(ended) = step.ended {
                self.update_saves(|s| s.ended(ended))?;
            }
            match &step.note {
                SaveNote::Same => {}
                SaveNote::Says(note) => self.set_meta(MetaKey::SaveNote, note)?,
                SaveNote::Clear => self.delete_meta(MetaKey::SaveNote)?,
            }
            self.alarm_at(step.alarm_ms).await?;
            refused = refused.or(step.refused.clone());
            for next in self.perform(step).await? {
                queue.push(next);
            }
        }
        Ok(refused)
    }

    /// This isolate's first look, once (`adopt`): the exit to apply when the
    /// container it should find is gone.
    async fn take_over(&self) -> CellResult<Option<Event>> {
        if self.adopted.replace(true) {
            return Ok(None);
        }
        self.adopt().await
    }

    /// A new isolate's first look (lesson 6): a container its lifecycle says
    /// is running is taken over (watched again, its intercepts and idle
    /// stop armed again, never destroyed: `/data` may be unsaved), or, gone,
    /// reported as exited.
    async fn adopt(&self) -> CellResult<Option<Event>> {
        let Some(generation) = self.lifecycle()?.running() else { return Ok(None) };
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
                Action::Sleep { generation } => reported.push(self.hold(generation, true).await),
                Action::Hold { generation } => reported.push(self.hold(generation, false).await),
                Action::Save { generation, held, seq } => reported.push(self.save(generation, held, seq).await),
                Action::Unhold { generation } => self.unhold(generation).await,
                Action::Stop { generation, saved } => reported.push(self.stop(generation, saved).await),
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
                usage: Usage::Awake { instance: fragment_core::price::INSTANCE.into(), ms: (to - from) as u64 },
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

    /// The hosts the swap intercepts: every provider's in the catalog.
    fn swap_hosts(&self) -> JsValue {
        js::to_js(&json!(self.cfg.providers.hosts()))
    }

    /// The environment every image may rely on (docs/computers.md).
    fn guest_env(&self, id: &str, image: &str, restoring: bool) -> Value {
        let mut env = json!({
            "FRAGMENT_COMPUTER": id,
            "FRAGMENT_API": "http://api.fragment.internal",
            "FRAGMENT_MODEL": "http://model.fragment.internal",
            "FRAGMENT_IMAGE": image,
        });
        if restoring {
            env["RESTORE_PENDING"] = json!("1");
        }
        env
    }

    /// One start: from the snapshot when it caches the current save for the
    /// pinned image, else the image (restoring the save, behind the restore
    /// gate; docs/computers.md, "Saves and what a wake restores"). Any
    /// failure destroys what started. A start from the snapshot that fails
    /// forgets the snapshot and reports `SnapshotFailed`, which the lifecycle
    /// answers with a start from the image and the save in the same wake, no
    /// strike against the computer (F7). A start whose save will never
    /// restore (its archive gone or altered) marks it unusable and reports
    /// `RestoreFailed`, which starts it again at once from the save before
    /// it, when there is one (F5). A start that comes up records what it
    /// restored, and logs it (P7).
    async fn start(&self, generation: u64) -> Event {
        let planned = match self.plan(generation).await {
            Ok(p) => p,
            Err(e) => return Event::StartFailed { generation, why: e.message },
        };
        let from = planned.plan.source();
        let save_id = planned.save.as_ref().map(|s| s.id.clone());
        match self.try_start(generation, &planned).await {
            Ok(()) => {
                let came_up = |s: &mut Saves| s.came_up(generation, from, save_id.as_deref(), planned.reference.as_deref(), js::now_ms()).map(|r| (r, s.rollbacks()));
                match self.update_saves(came_up) {
                    // one line per start that came up (lesson 14): what it restored
                    Ok(Some((restored, rollbacks))) => console_log!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "restored": restored, "rollbacks": rollbacks })),
                    Ok(None) => {}
                    Err(e) => console_error!("{}", json!({ "computer": "restored", "generation": generation, "error": e.message })),
                }
                Event::Ready { generation }
            }
            Err(Unstarted::Unusable(why)) => {
                let id = save_id.expect("only a start from a save finds it unusable");
                let older = self.update_saves(|s| s.unusable(&id)).unwrap_or(false);
                console_log!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "save": "unusable", "id": id, "generation": generation, "why": why, "older": older }));
                let _ = self.call("destroy", &[JsValue::from_f64(generation as f64), "its save would not restore".into()]).await;
                Event::RestoreFailed { generation, why, older }
            }
            Err(Unstarted::Failed(e)) if from == RestoreSource::Snapshot => {
                // forgotten before the destroy: the exit that reports is the
                // start's own failure (`exit_of`), and any start after it
                // restores the save
                let forgot = self.update_saves(|s| s.snapshot_failed());
                console_log!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "snapshot": "failed", "generation": generation, "why": e.message, "forgot": forgot.ok().flatten() }));
                let _ = self.call("destroy", &[JsValue::from_f64(generation as f64), "the snapshot did not start".into()]).await;
                Event::SnapshotFailed { generation, why: e.message }
            }
            Err(Unstarted::Failed(e)) => {
                let _ = self.call("destroy", &[JsValue::from_f64(generation as f64), "the start failed".into()]).await;
                Event::StartFailed { generation, why: e.message }
            }
        }
    }

    /// What the start `generation` restores, recorded as the start under
    /// way. Nothing awaits between reading the saves and recording it.
    async fn plan(&self, generation: u64) -> CellResult<Planned> {
        let image = self.must(MetaKey::Image)?;
        let reference = self.call("imageRef", &[image.as_str().into()]).await?.as_string();
        let snapshots = reference.as_deref().filter(|_| self.cfg.computer_snapshots);
        let (plan, save) = self.update_saves(|s| {
            let plan = s.plan(snapshots);
            s.starting(generation, plan.source());
            let save = s.current().cloned().filter(|_| plan != Restore::Nothing);
            (plan, save)
        })?;
        assert_eq!(plan == Restore::Nothing, save.is_none(), "a start restores the save there is");
        Ok(Planned { plan, image, reference, save })
    }

    async fn try_start(&self, generation: u64, planned: &Planned) -> Result<(), Unstarted> {
        let id = self.must(MetaKey::Id)?;
        let g = JsValue::from_f64(generation as f64);
        // lesson 6: a container a new isolate found running is never ours to
        // reuse half-known, nor is an earlier start's that is not gone yet:
        // it goes before this one starts (`0`: whatever runs)
        if js::invoke(&self.host()?, "running", &[]).await?.as_bool() == Some(true) {
            self.call("destroy", &[JsValue::from_f64(0.0), "a new start".into()]).await?;
        }
        let env = js::to_js(&self.guest_env(&id, &planned.image, planned.plan == Restore::Backup));
        let snapshot_js = match &planned.plan {
            Restore::Snapshot { id } => JsValue::from_str(id),
            Restore::Backup | Restore::Nothing => JsValue::NULL,
        };
        // the size its awake time is priced at (decision 13's, by default)
        let size = fragment_core::price::instance_size(fragment_core::price::INSTANCE).map_err(CellError::host)?;
        let size = serde_json::to_value(&size).map_err(|e| CellError::host(format!("an instance size: {e}")))?;
        // a wipe that began while this start awaited starts nothing: the
        // check and the start are one turn (the host's start is synchronous)
        if self.wiped.get() {
            return Err(wiped_computer().into());
        }
        self.call("start", &[g.clone(), planned.image.as_str().into(), snapshot_js, env, js::to_js(&size)]).await?;
        let armed = self.call("arm", &[g.clone(), id.as_str().into(), JsValue::from_f64(RUNTIME_IDLE_MS as f64), self.swap_hosts()]).await?;
        if armed.as_bool() != Some(true) {
            return Err(CellError::host(format!("start {generation} was superseded before it was armed")).into());
        }
        match &planned.plan {
            Restore::Backup => {
                self.exec_ready(&g).await?;
                let save = planned.save.as_ref().expect("a start from the save has its records");
                // a directory's record before any inside it, as it was taken
                for record in &save.records {
                    let answer = js::from_js(&self.call("restore", &[g.clone(), js::to_js(record)]).await?).map_err(CellError::host)?;
                    if let Some(why) = answer["unusable"].as_str() {
                        return Err(Unstarted::Unusable(format!("save {}'s {}: {why}", save.number, record["dir"].as_str().unwrap_or("?"))));
                    }
                }
                // the image's check, behind the gate: nothing reads /data yet
                let argv = js::to_js(&json!(["sh", "-c", CHECK]));
                let out = js::from_js(&self.call("exec", &[g.clone(), argv, JsValue::from_f64(CHECK_EXEC_MS as f64)]).await?).map_err(CellError::host)?;
                let said = out["output"].as_str().unwrap_or("").trim().chars().rev().take(300).collect::<String>().chars().rev().collect::<String>();
                match out["exitCode"].as_i64() {
                    Some(0) => {}
                    Some(CHECK_UNUSABLE) => return Err(Unstarted::Unusable(format!("save {}'s check: {said}", save.number))),
                    code => return Err(CellError::host(format!("the image's check of save {} exited {code:?}: {said}", save.number)).into()),
                }
                self.mark(&g, RESTORED_MARK).await.map_err(|e| CellError::host(format!("opening the restore gate: {}", e.message)))?;
            }
            Restore::Snapshot { .. } => {
                // its sleep held the guest before it took the snapshot: a
                // container started from it must not wake held
                self.exec_ready(&g).await?;
                self.mark(&g, HOLD_UNMARK).await.map_err(|e| CellError::host(format!("letting go of the snapshot's hold: {}", e.message)))?;
            }
            Restore::Nothing => {}
        }
        Ok(())
    }

    /// Waits for the container of start `g` to take an exec.
    async fn exec_ready(&self, g: &JsValue) -> CellResult<()> {
        let up = self.call("execReady", &[g.clone(), JsValue::from_f64(EXEC_READY_MS as f64)]).await?;
        if up.as_bool() != Some(true) {
            return Err(CellError::host(format!("the container took no exec within {}s", EXEC_READY_MS / 1000)));
        }
        Ok(())
    }

    /// Runs one of the DO's markers (`script`, through `sh -c`) in the
    /// container of start `g`.
    async fn mark(&self, g: &JsValue, script: &str) -> CellResult<()> {
        let argv = js::to_js(&json!(["sh", "-c", script]));
        let out = js::from_js(&self.call("exec", &[g.clone(), argv, JsValue::from_f64(MARK_EXEC_MS as f64)]).await?).map_err(CellError::host)?;
        if out["exitCode"] != 0 {
            return Err(CellError::host(format!("`{script}` answered {}: {}", out["exitCode"], out["output"])));
        }
        Ok(())
    }

    /// An exit of start `generation`'s container, as the lifecycle should
    /// hear it: one from the snapshot that stopped before it came up is the
    /// start's own failure (the snapshot is forgotten), never a strike.
    fn exit_of(&self, generation: u64) -> CellResult<Event> {
        let starting = matches!(self.lifecycle()?.phase, Phase::Starting { generation: g, .. } if g == generation);
        if starting && self.saves()?.start_from(generation) == Some(RestoreSource::Snapshot) {
            let forgot = self.update_saves(|s| s.snapshot_failed())?;
            console_log!("{}", json!({ "computer": self.meta(MetaKey::Id)?, "snapshot": "stopped", "generation": generation, "forgot": forgot }));
            return Ok(Event::SnapshotFailed { generation, why: "its container stopped as it started from the snapshot".into() });
        }
        Ok(Event::Exited { generation })
    }

    /// Whether a container runs here now.
    async fn container_running(&self) -> bool {
        self.call("running", &[]).await.is_ok_and(|r| r.as_bool() == Some(true))
    }

    /// A save's hold (docs/computers.md, "The hold"): the DO touches
    /// `/run/computer/hold` and waits `HOLD_WAIT_MS` for the guest's
    /// `/run/computer/held` (no claim in flight, and what it keeps copied).
    /// An image that never answers is saved anyway, recorded as not held. A
    /// sleep (`sleeping`) whose container is gone already is over; an awake
    /// save of one fails.
    async fn hold(&self, generation: u64, sleeping: bool) -> Event {
        let g = JsValue::from_f64(generation as f64);
        let id = self.meta(MetaKey::Id).ok().flatten().unwrap_or_default();
        if !self.container_running().await {
            if sleeping {
                let _ = self.call("destroy", &[g, "asleep".into()]).await;
                return Event::Asleep { generation };
            }
            return Event::SaveFailed { generation, seq: 0, why: "its container is not running".into() };
        }
        let t0 = js::now_ms();
        if let Err(e) = self.mark(&g, HOLD_MARK).await {
            // a guest the DO cannot mark is saved as it is
            console_error!("{}", json!({ "computer": id, "hold": "failed", "generation": generation, "error": e.message }));
            return Event::Held { generation, held: false };
        }
        let argv = js::to_js(&json!(["sh", "-c", HELD_TEST]));
        // `{ok, tries, last}`: whether it answered, the looks it took, and the last one's answer
        let polled: Value = match self.call("execUntil", &[g.clone(), argv, JsValue::from_f64(HOLD_WAIT_MS as f64)]).await {
            Ok(r) => js::from_js(&r).unwrap_or(Value::Null),
            Err(e) => json!({ "ok": false, "last": { "error": e.message } }),
        };
        let held = polled["ok"] == true;
        // one line per hold; one unanswered says what the guest said of it
        let unheld = if held { None } else { Some(self.unheld(&g).await) };
        console_log!("{}", json!({ "computer": id, "hold": generation, "held": held, "sleep": sleeping, "ms": js::now_ms() - t0, "tries": polled["tries"], "last": polled["last"], "unheld": unheld }));
        Event::Held { generation, held }
    }

    /// What the guest of start `g` says of a hold it has not answered (its
    /// `/run/computer/unheld`, `fragment_core::computer::unheld`): `null`
    /// when it says nothing, as an image need not.
    async fn unheld(&self, g: &JsValue) -> Value {
        let argv = js::to_js(&json!(["sh", "-c", UNHELD_READ]));
        match self.call("exec", &[g.clone(), argv, JsValue::from_f64(UNHELD_EXEC_MS as f64)]).await.and_then(|o| js::from_js(&o).map_err(CellError::host)) {
            Ok(out) => json!(fragment_core::computer::unheld(out["output"].as_str().unwrap_or_default())),
            Err(e) => json!(format!("(unread: {})", e.message)),
        }
    }

    /// Saves `/data` as save `seq` of start `generation` (`held`: its guest
    /// answered the hold), and keeps it: the newest of the saves kept, the
    /// oldest past `SAVES_KEPT` let go of (`forget`). Any record of it
    /// already taken when a later one fails is let go of too. The test
    /// lever's failed saves (`fail-saves`) fail here first.
    async fn save(&self, generation: u64, held: bool, seq: u64) -> Event {
        let g = JsValue::from_f64(generation as f64);
        let id = self.meta(MetaKey::Id).ok().flatten().unwrap_or_default();
        let t0 = js::now_ms();
        if let Some(n) = self.meta(MetaKey::FailSaves).ok().flatten().and_then(|n| n.parse::<u32>().ok()).filter(|n| *n > 0) {
            let left = if n > 1 { self.set_meta(MetaKey::FailSaves, &(n - 1).to_string()) } else { self.delete_meta(MetaKey::FailSaves) };
            if let Err(e) = left {
                console_error!("{}", json!({ "computer": id, "lever": "fail-saves", "error": e.message }));
            }
            console_log!("{}", json!({ "computer": id, "save": seq, "generation": generation, "failed": "the test lever failed it", "left": n - 1 }));
            return Event::SaveFailed { generation, seq, why: "the test lever failed it".into() };
        }
        // what a held guest copied to names the save keeps, it leaves out
        let left_out = if held { self.left_out(&g).await } else { vec![] };
        let mut records: Vec<Value> = Vec::with_capacity(SAVED_DIRS.len());
        for dir in SAVED_DIRS {
            // the guest's own state leaves its work out (a record of its
            // own) and what it copied; its work is saved as it is
            let exclude = match dir {
                "/data" => std::iter::once(WORK_LEFT_OUT.to_string()).chain(left_out.iter().cloned()).collect::<Vec<_>>(),
                _ => vec![],
            };
            let exclude = js::to_js(&json!(exclude));
            match self.call("backup", &[g.clone(), dir.into(), exclude]).await.and_then(|r| js::from_js(&r).map_err(CellError::host)) {
                Ok(record) => records.push(record),
                Err(e) => {
                    console_log!("{}", json!({ "computer": id, "save": seq, "generation": generation, "dir": dir, "failed": e.message, "ms": js::now_ms() - t0 }));
                    match self.update_saves(|s| s.let_go(std::mem::take(&mut records))) {
                        Ok(lost) => self.forget(lost).await,
                        Err(kept) => console_error!("{}", json!({ "computer": id, "save": seq, "unforgotten": kept.message })),
                    }
                    return Event::SaveFailed { generation, seq, why: e.message };
                }
            }
        }
        let bytes: u64 = records.iter().filter_map(|r| r["size"].as_u64()).sum();
        match self.keep_save(generation, records, held).await {
            Ok(save) => {
                // one line per save (lesson 14): which, how big, how long
                console_log!("{}", json!({ "computer": id, "saved": save.number, "id": save.id, "generation": generation, "held": held, "leftOut": left_out, "bytes": bytes, "ms": js::now_ms() - t0 }));
                Event::Saved { generation, seq }
            }
            Err(e) => {
                console_error!("{}", json!({ "computer": id, "save": seq, "generation": generation, "unkept": e.message }));
                Event::SaveFailed { generation, seq, why: e.message }
            }
        }
    }

    /// What the held guest of start `g` names as left out of its save (its
    /// answer's lines: `fragment_core::computer::left_out`). An answer the
    /// DO cannot read, or refuses, leaves nothing out: the guest is saved
    /// whole, and the refusal logged.
    async fn left_out(&self, g: &JsValue) -> Vec<String> {
        let argv = js::to_js(&json!(["sh", "-c", HELD_READ]));
        // one byte past the bound: an answer that long is refused, never cut
        let keep = JsValue::from_f64((fragment_core::computer::HELD_ANSWER_MAX_BYTES + 1) as f64);
        let read = self.call("exec", &[g.clone(), argv, JsValue::from_f64(MARK_EXEC_MS as f64), keep]).await.and_then(|o| js::from_js(&o).map_err(CellError::host));
        let answer = match read {
            Ok(out) if out["exitCode"] == 0 => out["output"].as_str().unwrap_or_default().to_string(),
            Ok(out) => {
                console_error!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "held": "unread", "exit": out["exitCode"] }));
                return vec![];
            }
            Err(e) => {
                console_error!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "held": "unread", "error": e.message }));
                return vec![];
            }
        };
        fragment_core::computer::left_out(&answer).unwrap_or_else(|why| {
            console_error!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "held": "refused", "why": why }));
            vec![]
        })
    }

    /// A save worked: it is the newest save, and the ones it pushed out of
    /// the newest `SAVES_KEPT` are let go of (`forget`). Answers it.
    async fn keep_save(&self, generation: u64, records: Vec<Value>, held: bool) -> CellResult<Save> {
        let at = js::now_ms();
        let lost = self.update_saves(|s| s.saved(generation, records, at, held))?;
        let saves = self.saves()?;
        let save = saves.all().first().cloned().ok_or_else(|| CellError::host("no save after one was kept"))?;
        assert_eq!((save.generation, save.at_ms), (generation, at), "the save kept is the one just taken");
        self.forget(lost).await;
        Ok(save)
    }

    /// Deletes each record let go of (`Saves::forgetting`), and forgets it
    /// once its delete worked: one that failed stays, for the next save's
    /// pass (a delete of what is gone already works). `lost`: records
    /// pushed out unforgotten, whose archives stay in R2, logged.
    async fn forget(&self, lost: Vec<Value>) {
        let id = self.meta(MetaKey::Id).ok().flatten();
        for record in &lost {
            console_error!("{}", json!({ "computer": id, "unforgotten": record["id"] }));
        }
        let pending = self.saves().map(|s| s.forgetting().to_vec()).unwrap_or_default();
        // bounded: at most FORGETTING_MAX
        for record in pending {
            let rid = record["id"].as_str().unwrap_or_default().to_string();
            let done = match self.call("forget", &[js::to_js(&record)]).await {
                Ok(_) => self.update_saves(|s| s.forgot(&rid)),
                Err(e) => Err(e),
            };
            if let Err(e) = done {
                console_error!("{}", json!({ "computer": id, "forget": rid, "error": e.message }));
            }
        }
    }

    /// Lets go of the guest's hold: an awake save's end, or a sleep that
    /// kept its container. A container that is gone has nothing to let go.
    async fn unhold(&self, generation: u64) {
        if !self.container_running().await {
            return;
        }
        let g = JsValue::from_f64(generation as f64);
        if let Err(e) = self.mark(&g, HOLD_UNMARK).await {
            console_error!("{}", json!({ "computer": self.meta(MetaKey::Id).ok().flatten(), "unhold": "failed", "generation": generation, "error": e.message }));
        }
    }

    /// A sleep's end (docs/computers.md): its snapshot (only after its save
    /// worked, since a snapshot is only ever a cache of a save, and only of
    /// that save), the signal, five seconds for the guest, the destroy. A
    /// snapshot that fails forgets nothing: the one kept is still its own
    /// save's.
    async fn stop(&self, generation: u64, saved: bool) -> Event {
        let g = JsValue::from_f64(generation as f64);
        let id = self.meta(MetaKey::Id).ok().flatten().unwrap_or_default();
        if self.container_running().await {
            let newest = self.saves().ok().and_then(|s| s.all().first().cloned()).filter(|s| s.generation == generation);
            if let (true, true, Some(save)) = (saved, self.cfg.computer_snapshots, newest) {
                self.snapshot(generation, &save.id).await;
            }
            let _ = self.call("signal", &[g.clone(), JsValue::from_f64(15.0)]).await;
            let _ = self.call("exited", &[JsValue::from_f64(EXIT_WAIT_MS as f64)]).await;
        }
        let gone = self.call("destroy", &[g.clone(), "asleep".into()]).await.is_ok_and(|r| r.as_bool() == Some(true));
        // still this sleep's: no exit of it has been heard, nor a start after it
        let ours = matches!(self.lifecycle().map(|l| l.phase), Ok(Phase::Sleeping { generation: s, .. }) if s == generation);
        if !gone && ours && self.container_running().await {
            // a container that outlives its sleep would claim no turn again:
            // the hold is the sleep's alone
            if let Err(e) = self.mark(&g, HOLD_UNMARK).await {
                console_error!("{}", json!({ "computer": id, "unhold": "failed", "generation": generation, "error": e.message }));
            }
        }
        Event::Asleep { generation }
    }

    /// A sleep's snapshot, taken after its save `save`: kept as that save's
    /// cache, for the image its start ran (not the one pinned now: a pin
    /// while it ran is the next start's).
    async fn snapshot(&self, generation: u64, save: &str) {
        let g = JsValue::from_f64(generation as f64);
        let id = self.meta(MetaKey::Id).ok().flatten().unwrap_or_default();
        let name = format!("{}-{generation}", id.replace(':', "-"));
        let taken = self.call("snapshot", &[g, name.into()]).await.map(|s| s.as_string());
        let reference = self.saves().ok().and_then(|s| s.image_of(generation).map(str::to_string));
        match (taken, reference) {
            (Ok(Some(snapshot)), Some(reference)) => match self.update_saves(|s| s.snapshotted(save, &snapshot, &reference)) {
                Ok(true) => {}
                Ok(false) => console_log!("{}", json!({ "computer": id, "snapshot": "unkept", "generation": generation, "why": "its save is no longer the current one" })),
                Err(e) => console_error!("{}", json!({ "computer": id, "snapshot": "unkept", "generation": generation, "error": e.message })),
            },
            (Ok(_), _) => console_error!("{}", json!({ "computer": id, "snapshot": "unkept", "generation": generation, "why": "no snapshot id, or the image its start ran is unknown here" })),
            // a wake without it restores the save; the one kept is still its own save's
            (Err(e), _) => console_log!("{}", json!({ "computer": id, "snapshot": "failed", "generation": generation, "error": e.message })),
        }
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
                credentials: vec![],
            });
        }
        Ok(agents)
    }

    /// The guest's own view (`GET /api/computer`): its agents, each with
    /// the credentials it may use now (a connection its owner connected, an
    /// operator key the deployment holds, an own key its owner gave; none
    /// its owner narrowed it from), each a placeholder naming the agent.
    async fn guest_view(&self) -> CellResult<ComputerView> {
        let mut view = self.view()?;
        let catalog = &self.cfg.providers;
        view.credential_env = catalog.env_names();
        if catalog.is_empty() || view.agents.is_empty() {
            return Ok(view);
        }
        let keys = crate::keys::tag_keys(&self.env).await?;
        let states = self.connection_states(&view.owner).await;
        let own = self.own_key_names()?;
        for agent in &mut view.agents {
            for p in catalog.providers() {
                if !may_use(&agent.connections, &p.name) {
                    continue;
                }
                let available = match p.kind {
                    Kind::Connection => states.get(&p.name) == Some(&ProviderState::Connected),
                    Kind::Operator => crate::keys::holds_operator_key(&self.env, &p.name),
                    Kind::Own => own.contains(&p.name),
                };
                if !available {
                    continue;
                }
                let tag = keys[0].tag(&view.computer, &agent.fragment, &p.name);
                agent.credentials.push(AgentCredential {
                    provider: p.name.clone(),
                    kind: p.kind,
                    env: p.env.clone(),
                    placeholder: Placeholder::new(p.kind, &p.name, &tag).text(),
                    hosts: p.hosts.clone(),
                });
            }
        }
        Ok(view)
    }

    /// The owner's connections' states: as Pipes last said them while
    /// that is believed, else asked again (no token minted). WorkOS not
    /// answering is logged and reads as nothing connected, asked again
    /// shortly.
    async fn connection_states(&self, owner: &str) -> BTreeMap<String, ProviderState> {
        let now = js::now_ms();
        if let Some((states, until)) = self.states.borrow().as_ref() {
            if *until > now {
                return states.clone();
            }
        }
        let (states, until) = match crate::connections::connection_states(&self.env, self.cfg, self.workos_user(owner)).await {
            Ok(s) => (s, now + STATES_TTL_MS),
            Err(e) => {
                console_error!("{}", json!({ "computer": "connection-states", "error": e.message }));
                (BTreeMap::new(), now + STATES_RETRY_MS)
            }
        };
        *self.states.borrow_mut() = Some((states.clone(), until));
        states
    }

    /// The WorkOS user the owner signed in as, whose connections Pipes
    /// keeps: asked of the registry until it names one, then kept here for
    /// that issuer. A fact, not a cache: the first subject a person signed
    /// in as with an issuer is theirs for good, since the registry never
    /// relinks or forgets a sign-in (registry/signin.rs `person_for`).
    async fn workos_user(&self, owner: &str) -> CellResult<Option<String>> {
        let issuer = crate::keys::workos(&self.env, self.cfg).await?.issuer();
        let kept: Option<Value> = self.meta(MetaKey::WorkosUser)?.and_then(|t| serde_json::from_str(&t).ok());
        if let Some(subject) = kept.as_ref().filter(|k| k["owner"] == owner && k["issuer"] == issuer.as_str()).and_then(|k| k["subject"].as_str()) {
            return Ok(Some(subject.to_string()));
        }
        let subject = crate::ask_registry(&self.env, &crate::registry::calls::SubjectOf { identity: owner.into(), issuer: issuer.clone() }).await?.subject;
        if let Some(s) = &subject {
            self.set_meta(MetaKey::WorkosUser, &json!({ "owner": owner, "issuer": issuer, "subject": s }).to_string())?;
        }
        Ok(subject)
    }

    /// One connection's state, as a swap just learned it from Pipes.
    fn note_state(&self, provider: &str, state: ProviderState) {
        if let Some((states, _)) = self.states.borrow_mut().as_mut() {
            states.insert(provider.to_string(), state);
        }
    }

    /// The providers the owner gave an own key for.
    fn own_key_names(&self) -> CellResult<Vec<String>> {
        let rows = self.rows("SELECT provider FROM own_keys ORDER BY provider", vec![])?;
        Ok(rows.iter().filter_map(|r| r["provider"].as_str().map(str::to_string)).collect())
    }

    /// The owner's own key for `provider`, opened.
    async fn own_key(&self, provider: &str) -> CellResult<Option<String>> {
        let rows = self.rows("SELECT sealed FROM own_keys WHERE provider = ?", vec![provider.into()])?;
        let Some(sealed) = rows.first().and_then(|r| r["sealed"].as_str()).map(str::to_string) else { return Ok(None) };
        let scope = crate::keys::scope(SEAL_CLASS, &self.state);
        let opened = crate::keys::open(&self.env, &scope, &sealed).await?;
        if let Some(resealed) = opened.resealed {
            // only over the value opened: the owner may have set another
            // while the store was read
            self.exec("UPDATE own_keys SET sealed = ? WHERE provider = ? AND sealed = ?", vec![resealed.into(), provider.into(), sealed.into()])?;
        }
        let key = String::from_utf8(opened.plaintext).map_err(|_| CellError::host("an own key that is not text"))?;
        Ok(Some(key))
    }

    /// A month's uses: each agent's calls to each provider and their charges.
    fn uses(&self, month: Month) -> CellResult<ComputerUses> {
        let rows = self.rows("SELECT provider, agent, calls, micros FROM uses WHERE month = ? ORDER BY provider, agent", vec![i64::from(month.0).into()])?;
        let uses = rows
            .iter()
            .map(|r| ProviderUse {
                provider: r["provider"].as_str().unwrap_or_default().to_string(),
                agent: r["agent"].as_str().unwrap_or_default().to_string(),
                calls: r["calls"].as_u64().unwrap_or(0),
                micros: r["micros"].as_i64().unwrap_or(0),
            })
            .collect();
        Ok(ComputerUses { computer: self.must(MetaKey::Id)?, month: month.label(), uses, models: self.model_uses(month)? })
    }

    /// Counts an agent's calls a provider answered, in `at`'s month, and
    /// forgets months past `USES_MONTHS_KEPT`. A month already holding
    /// `USES_ROWS_MAX` rows takes no new one (its calls are logged).
    fn count_uses(&self, b: &UsedAsk) -> CellResult<()> {
        if b.at <= 0 || b.uses.len() > swap::PLACEHOLDERS_MAX || !fragment_proto::valid_fragment_name(&b.agent) {
            return Err(CellError::invalid("a use names an agent, when, and at most a request's providers"));
        }
        let month = Month::of(b.at);
        for u in &b.uses {
            if self.cfg.providers.get(&u.provider).is_none() || u.micros < 0 {
                return Err(CellError::invalid(format!("no provider {:?} to count, or a charge below zero", u.provider)));
            }
            let m = i64::from(month.0);
            let held = self.rows("SELECT COUNT(*) AS n FROM uses WHERE month = ?", vec![m.into()])?;
            let exists = !self.rows("SELECT 1 FROM uses WHERE month = ? AND provider = ? AND agent = ?", vec![m.into(), u.provider.as_str().into(), b.agent.as_str().into()])?.is_empty();
            if !exists && held.first().and_then(|r| r["n"].as_u64()).unwrap_or(0) >= USES_ROWS_MAX {
                console_error!("{}", json!({ "computer": "uses-full", "month": month.label(), "provider": u.provider, "agent": b.agent }));
                continue;
            }
            self.exec(
                "INSERT INTO uses (month, provider, agent, calls, micros) VALUES (?, ?, ?, 1, ?) ON CONFLICT (month, provider, agent) DO UPDATE SET calls = calls + 1, micros = micros + excluded.micros",
                vec![m.into(), u.provider.as_str().into(), b.agent.as_str().into(), u.micros.into()],
            )?;
        }
        let oldest = i64::from(month.0.saturating_sub(USES_MONTHS_KEPT - 1));
        self.exec("DELETE FROM uses WHERE month < ?", vec![oldest.into()])
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
        let origin = self.cfg.computer_origin(&id).ok_or_else(|| CellError::host(format!("{id} is no computer's id")))?;
        let saves = self.saves()?;
        // why it won't wake; else what its saves' failures say (a sleep that
        // kept its container, or slept unsaved); else why a wake was refused
        let why = match why {
            Some(why) => Some(why),
            None => match self.meta(MetaKey::SaveNote)? {
                Some(note) => Some(note),
                None => self.meta(MetaKey::Note)?,
            },
        };
        Ok(ComputerView {
            computer: id,
            owner: self.must(MetaKey::Owner)?,
            image: self.must(MetaKey::Image)?,
            phase,
            why,
            agents: self.agents()?,
            origin,
            credential_env: vec![],
            restored: saves.restored().cloned(),
            rollbacks: saves.rollbacks(),
            saves: saves.views(),
        })
    }

    /// Where its saves are in R2: entry.mjs's `DirectoryBackup` prefix,
    /// named by this object's id.
    fn backups_prefix(&self) -> String {
        format!("computers/{}/backups/", self.state.id())
    }

    /// Its owner's wipe (docs/api.md, Operators): marked wiped first (from
    /// here no write lands and it answers as no computer), its alarm and
    /// its container gone, each save's record deleted (the delete a save's
    /// own `forget` makes), then whatever else is under its saves' prefix
    /// (an upload its destroy cut short), at most `WIPE_PAGES_PER_CALL`
    /// pages a call; once none is left, its tables dropped and made again
    /// empty, the snapshot's id with them. Answers `{destroyed, records,
    /// objects, more}`: `more`, its prefix holds more (call again). Each
    /// part is idempotent: a wipe cut anywhere is done by the next call.
    async fn wipe(&self) -> CellResult<Value> {
        let id = self.meta(MetaKey::Id)?;
        if !self.wiped.get() {
            self.sql().exec("INSERT INTO wiped (at) VALUES (?)", vec![js::now_ms().into()])?;
            self.wiped.set(true);
            console_log!("{}", json!({ "computer": id, "wipe": "marked" }));
        }
        self.state.storage().delete_alarm().await?;
        // whatever start runs it (`0`), and waited out
        let destroyed = self.call("destroy", &[JsValue::from_f64(0.0), "its owner was wiped".into()]).await?.as_bool() == Some(true);
        if !destroyed {
            return Err(CellError::host("its container is still running: the wipe goes on once it is gone"));
        }
        let records = self.saves()?.every_record();
        // bounded: a computer's records are (Saves::every_record)
        for record in &records {
            self.call("forget", &[js::to_js(record)]).await?;
        }
        let (objects, more) = js::blob_delete_under(&self.env, &self.backups_prefix(), WIPE_PAGES_PER_CALL).await?;
        if !more {
            // one step, no await: its record emptied, the mark kept
            let sql = self.sql();
            for table in fragment_core::ddl::tables(SCHEMA) {
                sql.exec(&format!("DROP TABLE IF EXISTS {table}"), None)?;
            }
            sql.exec(SCHEMA, None)?;
            assert!(self.meta(MetaKey::Id)?.is_none() && self.saves()?.every_record().is_empty(), "a wiped computer keeps no record");
        }
        console_log!("{}", json!({ "computer": id, "wipe": if more { "partly" } else { "done" }, "records": records.len(), "objects": objects }));
        Ok(json!({ "destroyed": destroyed, "records": records.len(), "objects": objects, "more": more }))
    }

    /// What a wipe finds of it (docs/api.md, Operators): whether it is
    /// wiped, made, its phase, its saves, how many objects its saves' prefix
    /// holds (one page: `more` past it), and whether it keeps a snapshot.
    async fn wipe_view(&self) -> CellResult<Value> {
        let made = self.meta(MetaKey::Id)?.is_some();
        let phase = match made {
            true => Some(self.view()?.phase),
            false => None,
        };
        let saves = self.saves()?;
        let (keys, more) = js::blob_list(&self.env, &self.backups_prefix()).await?;
        Ok(json!({ "wiped": self.wiped.get(), "made": made, "phase": phase, "saves": saves.all().len(), "backups": keys.len(), "more": more, "snapshot": saves.has_snapshot() }))
    }

    /// A test fleet's lever on this computer (`POST /api/test/computer`,
    /// docs/api.md): `kill` sends SIGKILL to the guest's PID 1, so its
    /// container exits as a crash does and its real exit is reported;
    /// `saves` answers what it keeps of its saves and what its last start
    /// restored; `fail-saves {times}` fails its next saves; `always-on {on}`
    /// is its owner's plan changing (the plan itself does not reach a
    /// computer yet).
    async fn lever(&self, b: &TestLever) -> CellResult<Value> {
        assert!(self.cfg.test_hooks, "only a test fleet pulls a computer's levers");
        let id = self.must(MetaKey::Id)?;
        match b.op.as_str() {
            "fail-saves" => {
                let times = b.times.filter(|t| (1..=FAIL_SAVES_MAX).contains(t)).ok_or_else(|| CellError::invalid(format!("fail-saves names its times, 1 to {FAIL_SAVES_MAX}")))?;
                self.set_meta(MetaKey::FailSaves, &times.to_string())?;
                console_log!("{}", json!({ "computer": id, "lever": "fail-saves", "times": times }));
                Ok(json!({ "computer": id, "failSaves": times }))
            }
            "always-on" => {
                let on = b.on.ok_or_else(|| CellError::invalid("always-on names on: true or false"))?;
                self.drive(Event::AlwaysOn { on }).await?;
                console_log!("{}", json!({ "computer": id, "lever": "always-on", "on": on }));
                Ok(json!({ "computer": id, "alwaysOn": on, "view": self.view()? }))
            }
            "kill" => {
                // the container this isolate signals is the one it watches
                if let Some(exited) = self.take_over().await? {
                    self.drive(exited).await?;
                }
                let Some(generation) = self.lifecycle()?.running() else {
                    return Err(CellError::invalid("it is not running: wake it first"));
                };
                let sent = self.call("signal", &[JsValue::from_f64(generation as f64), JsValue::from_f64(9.0)]).await?;
                if sent.as_bool() != Some(true) {
                    return Err(CellError::host(format!("start {generation}'s container is not running here")));
                }
                console_log!("{}", json!({ "computer": id, "lever": "kill", "generation": generation }));
                Ok(json!({ "computer": id, "killed": generation }))
            }
            "saves" => {
                let life = self.lifecycle()?;
                let mut v = serde_json::to_value(self.saves()?).map_err(|e| CellError::host(e.to_string()))?;
                v["computer"] = json!(id);
                v["generation"] = json!(life.generation());
                v["saving"] = json!(life.saving());
                v["unsavedSince"] = json!(life.unsaved_since_ms());
                v["failSaves"] = json!(self.meta(MetaKey::FailSaves)?.and_then(|n| n.parse::<u32>().ok()).unwrap_or(0));
                Ok(v)
            }
            op => Err(CellError::invalid(format!("no computer lever {op:?}: kill, saves, fail-saves or always-on"))),
        }
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        let path = req.path();
        // a wiped computer answers as none, its wipe's own calls aside
        if self.wiped.get() && !matches!(path.as_str(), "/computer/wipe" | "/computer/wipe-view") {
            return Err(wiped_computer());
        }
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
            "computer/wipe" => json_response(&self.wipe().await?),
            "computer/wipe-view" => json_response(&self.wipe_view().await?),
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
                    if let Some(c) = list.iter().find(|c| self.cfg.providers.get(c).is_none()) {
                        let offered: Vec<&str> = self.cfg.providers.providers().iter().map(|p| p.name.as_str()).collect();
                        return Err(CellError::invalid(format!("no provider {c:?} on this deployment (its providers: {offered:?})")));
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
            "computer/credentials" => {
                let b: CredentialsAsk = body_json(&mut req).await?;
                json_response(&self.credentials(b).await?)
            }
            "computer/used" => {
                let b: UsedAsk = body_json(&mut req).await?;
                self.must(MetaKey::Id)?;
                self.count_uses(&b)?;
                json_response(&json!({ "ok": true }))
            }
            "computer/uses" => {
                let b: UsesAsk = body_json(&mut req).await?;
                let month = match b.month.as_deref() {
                    Some(label) => Month::parse(label).ok_or_else(|| CellError::invalid("a month is YYYY-MM"))?,
                    None => Month::of(js::now_ms()),
                };
                json_response(&self.uses(month)?)
            }
            "computer/own-key" => {
                let b: OwnKeyAsk = body_json(&mut req).await?;
                self.must(MetaKey::Id)?;
                if self.cfg.providers.get(&b.provider).is_none_or(|p| p.kind != Kind::Own) {
                    return Err(CellError::invalid(format!("{:?} is no own key's provider here", b.provider)));
                }
                match b.key {
                    Some(key) => {
                        if !crate::connections::own_key_ok(&key) {
                            return Err(CellError::invalid("a key is a printable token"));
                        }
                        let sealed = crate::keys::seal(&self.env, &crate::keys::scope(SEAL_CLASS, &self.state), key.as_bytes()).await?;
                        self.exec(
                            "INSERT INTO own_keys (provider, sealed, set_at) VALUES (?, ?, ?) ON CONFLICT (provider) DO UPDATE SET sealed = excluded.sealed, set_at = excluded.set_at",
                            vec![b.provider.as_str().into(), sealed.into(), js::now_ms().into()],
                        )?;
                    }
                    None => self.exec("DELETE FROM own_keys WHERE provider = ?", vec![b.provider.as_str().into()])?,
                }
                json_response(&json!({ "providers": self.own_key_names()? }))
            }
            p @ ("computer/models" | "computer/model-choice" | "computer/model-call" | "computer/model-credential" | "computer/model-used" | "computer/chatgpt") => {
                let p = p.to_string();
                self.own_models(&p, &mut req).await
            }
            "computer/own-keys" => {
                let b: OwnKeysAsk = body_json(&mut req).await?;
                self.must(MetaKey::Id)?;
                // the owner just read their connections: believed as fresh
                if let Some(states) = b.connections {
                    *self.states.borrow_mut() = Some((states, js::now_ms() + STATES_TTL_MS));
                }
                json_response(&json!({ "providers": self.own_key_names()? }))
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
            "computer/guest" => json_response(&self.guest_view().await?),
            "computer/exited" => {
                let b: Exited = body_json(&mut req).await?;
                let event = self.exit_of(b.generation)?;
                self.drive(event).await?;
                json_response(&json!({ "ok": true }))
            }
            "computer/test" if self.cfg.test_hooks => {
                let b: TestLever = body_json(&mut req).await?;
                json_response(&self.lever(&b).await?)
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

    /// The secrets to swap in for a request's placeholders (`b`, a request
    /// to `b.host`), when their tags name one agent that runs here now and
    /// that agent may use each provider: `{agent, identity, owner, secrets:
    /// [{provider, tag, secret}]}`, so the egress holds a key's call on the
    /// agent's owner's ledger and counts each call as the agent's. A tag that names
    /// no agent of this computer (forged, another computer's, or an agent
    /// removed since) is refused. An agent may use every provider its owner
    /// has unless its owner narrowed it to a list (decision 44: a person's
    /// agents are not fenced from each other, so the list is a role's
    /// specialization, not a wall).
    async fn credentials(&self, b: CredentialsAsk) -> CellResult<Value> {
        if b.placeholders.is_empty() || b.placeholders.len() > swap::PLACEHOLDERS_MAX {
            return Err(CellError::invalid(format!("ask for 1 to {} placeholders", swap::PLACEHOLDERS_MAX)));
        }
        let id = self.must(MetaKey::Id)?;
        let rows = self.rows("SELECT fragment, owner, identity, connections FROM agents ORDER BY added_at", vec![])?;
        let fragments: Vec<String> = rows.iter().filter_map(|r| r["fragment"].as_str().map(str::to_string)).collect();
        let keys = crate::keys::tag_keys(&self.env).await?;
        let mut agent: Option<&str> = None;
        for p in &b.placeholders {
            let provider = self.cfg.providers.get(&p.provider).ok_or_else(|| CellError::invalid(format!("no provider {:?} on this deployment", p.provider)))?;
            if !provider.hosts.contains(&b.host) {
                return Err(CellError::new(ErrorCode::Forbidden, format!("{} is for {}, not {}", provider.name, provider.hosts.join(", "), b.host)));
            }
            let named = swap::agent_of(&keys, &id, &fragments, &p.provider, &p.tag).ok_or_else(|| {
                CellError::new(
                    ErrorCode::Forbidden,
                    format!("this {} placeholder names no agent on this computer: forged, another computer's, or an agent removed from it (read GET /api/computer again)", p.provider),
                )
            })?;
            if agent.is_some_and(|a| a != named) {
                return Err(CellError::invalid("one request's placeholders name one agent: send another's in a request of its own"));
            }
            agent = Some(named);
        }
        let agent = agent.expect("at least one placeholder, each naming an agent");
        let row = rows.iter().find(|r| r["fragment"] == agent).expect("the agent named is one of the rows read");
        let owner = row["owner"].as_str().unwrap_or_default().to_string();
        let identity = row["identity"].as_str().unwrap_or_default().to_string();
        let narrowed = stored_connections(row["connections"].as_str().unwrap_or_default(), agent)?;
        let mut secrets = vec![];
        for p in &b.placeholders {
            let provider = self.cfg.providers.get(&p.provider).expect("checked above");
            if !may_use(&narrowed, &provider.name) {
                let list = narrowed.as_deref().unwrap_or_default();
                return Err(CellError::new(
                    ErrorCode::Forbidden,
                    format!(
                        "{agent} may not use {}: its owner narrowed it to {} on their computer (null lets it use every provider its owner has)",
                        provider.name,
                        if list.is_empty() { "none".to_string() } else { list.join(", ") }
                    ),
                ));
            }
            let secret = match provider.kind {
                Kind::Connection => self.connection_token(&owner, &provider.name).await?,
                // its call is held on its owner's ledger before it is made (`hold_keys`)
                Kind::Operator => crate::keys::operator_key(&self.env, &provider.name).await?,
                Kind::Own => self.own_key(&provider.name).await?.ok_or_else(|| CellError::new(ErrorCode::NotConnected, format!("give your own {} key first (PUT /api/connections/{}/key)", provider.name, provider.name)))?,
            };
            if secret.is_empty() || !secret.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err(CellError::new(ErrorCode::UpstreamFailed, format!("{}'s credential is not a printable token", provider.name)));
            }
            secrets.push(json!({ "provider": provider.name, "tag": p.tag, "secret": secret }));
        }
        Ok(json!({ "agent": agent, "owner": owner, "identity": identity, "secrets": secrets }))
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

    /// `owner`'s token for `provider`: WorkOS Pipes', for the WorkOS user
    /// `owner` signed in as, asked for each swap (Pipes holds and refreshes
    /// it), so a connection disconnected or revoked stops at the next
    /// request. What Pipes answers is the connection's state too, as the
    /// guest's view lists it (`note_state`).
    async fn connection_token(&self, owner: &str, provider: &str) -> CellResult<String> {
        let user = self.workos_user(owner).await?.ok_or_else(|| CellError::new(ErrorCode::NotConnected, format!("{owner} has no WorkOS account to connect {provider} with")))?;
        let (status, answer) = crate::keys::pipes_token(&self.env, &self.cfg.workos()?.api, provider, &user).await?;
        if status != 200 {
            let why = answer["message"].as_str().unwrap_or("no reason given");
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS refused {provider}'s token ({status}): {why}")));
        }
        if answer["active"] != true {
            // the guest's next read lists the connection no more
            let (state, why) = match answer["error"].as_str() {
                Some("needs_reauthorization") => (ProviderState::NeedsReauthorization, format!("connect {provider} again: its account needs authorizing again")),
                _ => (ProviderState::NotConnected, format!("connect {provider} first: no account is connected")),
            };
            self.note_state(provider, state);
            return Err(CellError::new(ErrorCode::NotConnected, why));
        }
        let token = answer["access_token"]["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, "WorkOS answered no token"))?;
        self.note_state(provider, ProviderState::Connected);
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
            // a body that forgot the field is not a reset to every provider
            if v.get("connections").is_none() {
                return Err(CellError::invalid("name connections: a list of providers, or null for every provider its owner has"));
            }
            let b: AgentConnections = serde_json::from_value(v).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            if let Some(list) = &b.connections {
                if list.len() > catalog::PROVIDERS_MAX || list.iter().any(|c| !catalog::valid_name(c)) {
                    return Err(CellError::invalid(format!("connections are at most {} provider names, or null", catalog::PROVIDERS_MAX)));
                }
            }
            let set = json!({ "fragment": fragment, "connections": b.connections });
            json_response(&view_of(ask(env, id, "computer/connections", &set).await?)?)
        }
        (Method::Get, [id, "uses"]) => {
            owned(env, who, id).await?;
            json_response(&ask(env, id, "computer/uses", &json!({ "month": null })).await?)
        }
        (Method::Get, [id, "uses", month]) => {
            owned(env, who, id).await?;
            if Month::parse(month).is_none() {
                return Err(CellError::invalid("a month is YYYY-MM"));
            }
            json_response(&ask(env, id, "computer/uses", &json!({ "month": month })).await?)
        }
        (Method::Delete, [id, "agents", fragment]) => {
            owned(env, who, id).await?;
            crate::fragment::ask(env, fragment, "computer/unassign", &json!({ "computer": id, "owner": who })).await?;
            json_response(&view_of(ask(env, id, "computer/unassign", &json!({ "fragment": fragment })).await?)?)
        }
        (Method::Post, [id, "ports", port, "ticket"]) => {
            let v = owned(env, who, id).await?;
            let port: u16 = port.parse().ok().filter(|p| (1..=PORT_MAX).contains(p)).ok_or_else(|| CellError::invalid("a port is 1-65535"))?;
            // where on the port it lands: a path and query the image reads
            // (an agent's screen: `/?agent=<agent>`), carried, never read
            let asked: PortTicketAsk = if body.iter().all(u8::is_ascii_whitespace) { PortTicketAsk::default() } else { serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))? };
            let path = asked.path.unwrap_or_else(|| "/".into());
            if !valid_port_path(&path) {
                return Err(CellError::invalid(format!("a ticket's path is a path on its port: `/`, then at most {PORT_PATH_MAX_BYTES} visible characters, no `//`, `#`, `\\`, `.` or `..`")));
            }
            let t = ask(env, id, "computer/ticket", &json!({ "port": port, "identity": who })).await?;
            let ticket = t["ticket"].as_str().ok_or_else(|| CellError::host("the computer minted no ticket"))?;
            let next: String = url::form_urlencoded::byte_serialize(format!("/p/{port}{path}").as_bytes()).collect();
            let url = format!("{}/__ticket?t={ticket}&next={next}", v.origin);
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
    let platform = Config::from_env(env).platform();
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
        // `/p/<port>` and a path on it, as the ticket's minting checked it
        let landing = |n: &str| n.strip_prefix("/p/").and_then(|r| r.split_once('/')).is_some_and(|(port, rest)| port.parse::<u16>().is_ok() && valid_port_path(&format!("/{rest}")));
        let next = q("next").filter(|n| landing(n)).unwrap_or_else(|| "/p/6080/".into());
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
    let platform = cfg.platform();
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
/// `x-fragment-agent`, or `POST /v1/decide`, a Clef decision. It is the
/// platform's model route as that agent (`models::route` or
/// `models::decide_route`, signed as egress_api signs), which bounds it,
/// meters it to the agent's owner, and refuses at zero credit. Its auth
/// headers are the guest's and go nowhere; any other path is 404,
/// unmetered.
async fn egress_model(mut req: Request, env: &Env, ctx: &Context, computer: &str) -> CellResult<Response> {
    let api = match (req.method(), req.path().as_str()) {
        (Method::Post, "/v1/chat/completions") => "http://api.fragment.internal/api/models/v1/chat/completions",
        (Method::Post, "/v1/decide") => "http://api.fragment.internal/api/models/v1/decide",
        _ => return Err(CellError::new(ErrorCode::NotFound, "the model intercept answers POST /v1/chat/completions and POST /v1/decide")),
    };
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
    egress_api(Request::new_with_init(api, &init)?, env, ctx, computer).await
}

/// Headers a hop answers for itself, never sent on.
const HOP_HEADERS: [&str; 9] = ["connection", "keep-alive", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "host"];

/// A guest's request to a provider's host (decisions 22 and 37; Paul,
/// 2026-10-04): each placeholder it carries, in a header, the query or
/// basic auth (`swap::Plan`), is swapped for its credential when the
/// request goes to one of its provider's own hosts, in a place its catalog
/// row names, for the agent its tag names, and the request goes on over
/// HTTPS. A request with no placeholder goes on as it is (decision 43).
/// A body is sent as it came. The platform's own headers (`x-fragment-…`)
/// go to no provider. An operator key's call is held on the agent's
/// owner's ledger before it is made (`hold_keys`, awaited: no hold, no
/// call). After the answer is handed back, the hold is settled if the
/// provider answered, or released (`end_holds`), and each call it answered
/// is counted as the agent's (`computer/used`). Both run in `wait_until`:
/// a hold that never ends is charged by the ledger's sweep, so deferring
/// the settle loses nothing.
async fn egress_swap(mut req: Request, env: &Env, ctx: &Context, computer: &str) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let url = req.url()?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let headers_in: Vec<(String, String)> = req.headers().entries().filter(|(name, _)| !HOP_HEADERS.contains(&name.as_str()) && !name.starts_with("x-fragment-")).collect();
    let plan = Plan::of(&headers_in, url.query(), url.path(), &cfg.providers, &host).map_err(|e| CellError::new(e.code(), e.to_string()))?;
    let method = req.method();
    let body = match method {
        Method::Get | Method::Head => vec![],
        _ => crate::read_body(&mut req, EGRESS_BODY_MAX_BYTES).await?,
    };
    let mut secrets = BTreeMap::new();
    // whose ledger a key's call goes on, and as which agent
    let mut payer: Option<(String, String, String)> = None;
    if !plan.is_empty() {
        let asked: Vec<Value> = plan.wanted.iter().map(|p| json!({ "provider": p.provider, "tag": p.tag })).collect();
        let answer = ask(env, computer, "computer/credentials", &json!({ "host": host, "placeholders": asked })).await?;
        for s in answer["secrets"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let (provider, tag, secret) = (s["provider"].as_str().unwrap_or_default(), s["tag"].as_str().unwrap_or_default(), s["secret"].as_str().unwrap_or_default());
            if let Some(p) = plan.wanted.iter().find(|w| w.provider == provider && w.tag == tag) {
                secrets.insert(p.clone(), secret.to_string());
            }
        }
        if secrets.len() != plan.wanted.len() || secrets.values().any(String::is_empty) {
            return Err(CellError::host("the computer resolved fewer credentials than were asked for"));
        }
        let field = |k: &str| answer[k].as_str().map(str::to_string).ok_or_else(|| CellError::host(format!("the computer named no {k}")));
        payer = Some((field("agent")?, field("owner")?, field("identity")?));
    }
    let out_parts = plan.apply(&headers_in, url.query(), &cfg.providers, &secrets);
    let headers = Headers::new();
    for (name, value) in &out_parts.headers {
        headers.set(name, value)?;
    }
    if !body.is_empty() {
        headers.set("content-length", &body.len().to_string())?;
    }
    let query = out_parts.query.map(|q| format!("?{q}")).unwrap_or_default();
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
    let out = Request::new_with_init(&target, &init)?;
    // held last, so every hold reaches `end_holds`
    let holds = match &payer {
        Some((_, owner, identity)) => hold_keys(env, cfg, computer, owner, identity, &plan).await?,
        None => vec![],
    };
    let sent = Fetch::Request(out).send().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("{host} did not answer: {e}")));
    let status = sent.as_ref().ok().map(Response::status_code);
    let answered = status.is_some_and(|s| s < 500);
    // after the answer, never in its way: a hold that does not end is the
    // ledger's to charge (its sweep), so nothing here goes unaccounted
    if let Some((agent, owner, _)) = payer {
        // each call the provider answered is the agent's (decision 37), a key's at its price
        let uses: Vec<Value> = plan.wanted.iter().map(|p| json!({ "provider": p.provider, "micros": holds.iter().find(|h| h.provider == p.provider).map_or(0, |h| h.amount) })).collect();
        let (env, computer) = (env.clone(), computer.to_string());
        ctx.wait_until(async move {
            end_holds(&env, &owner, &holds, answered).await;
            if !answered {
                return;
            }
            if let Err(e) = ask(&env, &computer, "computer/used", &json!({ "agent": agent, "at": js::now_ms(), "uses": uses })).await {
                console_error!("{}", json!({ "egress": "swap", "used": e.message }));
            }
        });
    }
    // one line per swap (lesson 14), naming the providers, never a tag or a value
    if !plan.is_empty() {
        let names: Vec<String> = plan.wanted.iter().map(Placeholder::named).collect();
        console_log!("{}", json!({ "egress": "swap", "computer": computer, "host": host, "method": method.as_ref(), "credentials": names, "status": status }));
    }
    sent
}

/// An operator key's call held on its owner's ledger (`hold_keys`).
struct KeyHold {
    provider: String,
    reference: String,
    amount: i64,
}

/// Holds each operator key's call in `plan` on `owner`'s ledger before it
/// is made, as the agent `identity`: one call at its price, under
/// `key:<computer>:<12 hex>`, as a model call holds its worst case
/// (docs/ledger.md). A key is the operator's money: a ledger that refuses,
/// or does not answer, refuses the call, and what it held already goes
/// back.
async fn hold_keys(env: &Env, cfg: &Config, computer: &str, owner: &str, identity: &str, plan: &Plan) -> CellResult<Vec<KeyHold>> {
    let mut holds: Vec<KeyHold> = vec![];
    // bounded: at most swap::PLACEHOLDERS_MAX
    for p in plan.wanted.iter().filter(|p| cfg.providers.get(&p.provider).is_some_and(|row| row.kind == Kind::Operator)) {
        let reserve = Reserve {
            reference: format!("key:{computer}:{}", js::random_hex::<12>()),
            spend: Spend::AgentTurn,
            worst: Usage::Key { key: p.provider.clone(), units: 1 },
            fragment: None,
            agent: Some(identity.to_string()),
            capped: false,
        };
        match crate::ledger::hold(env, owner, &reserve).await {
            Ok(amount) => holds.push(KeyHold { provider: p.provider.clone(), reference: reserve.reference, amount }),
            Err(e) => {
                end_holds(env, owner, &holds, false).await;
                return Err(CellError::new(e.code, format!("{}'s call is not made: {}", p.provider, e.message)));
            }
        }
    }
    Ok(holds)
}

/// Ends each hold: settled at its one call when its provider answered
/// (under 500), else released (the call was not made, or not answered).
/// One that does not land stays held, which the ledger's sweep charges at
/// its amount.
async fn end_holds(env: &Env, owner: &str, holds: &[KeyHold], answered: bool) {
    for h in holds {
        let ended = match answered {
            true => crate::ledger::retried(env, owner, &Settle { reference: h.reference.clone(), usage: Some(Usage::Key { key: h.provider.clone(), units: 1 }) }).await.map(|_| ()),
            false => crate::ledger::retried(env, owner, &Release { reference: h.reference.clone() }).await.map(|_| ()),
        };
        if let Err(e) = ended {
            console_error!("{}", json!({ "egress": "swap", "hold": h.reference, "answered": answered, "error": e.message }));
        }
    }
}

/// The fragment a guest's `/f/<fragment>/…` or `/api/f/<fragment>/…` path names.
fn target_fragment(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/f/").or_else(|| path.strip_prefix("/api/f/"))?;
    let name = rest.split('/').next()?;
    fragment_proto::valid_fragment_name(name).then(|| name.to_string())
}
