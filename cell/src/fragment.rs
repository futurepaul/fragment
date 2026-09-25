//! The `Fragment` supervisor: one Durable Object per fragment. It owns the
//! fragment's identity, members, visibility, secrets, file-plane pins, and
//! code record, and it answers every call into the app. The router has
//! already verified the caller; it hands over a `Routed` (routed.rs: the
//! fragment's name, the URL the request arrived on, how its site was
//! addressed, and who is asking) in headers only it sets, decoded once
//! here into a `Caller`. On the control API the router resolved who is
//! asking; on the site it did not, and the fragment resolves them only
//! where its answer depends on it (`identified`, `reader`). Calls from
//! inside the platform (the internal routes below) are answered before
//! that decode: they carry no caller.
//!
//! Routes (inner paths; the router maps the public ones onto them):
//!
//!   POST   /create                        create (the signer owns it)
//!   DELETE /delete                        delete (owner)
//!   GET    /api/status                    viewer
//!   GET    /api/manifest                  viewer: fragment.json at main
//!   GET    /api/members                   viewer
//!   PUT    /api/members/<id|npub>         owner (a key names the identity holding it)
//!   DELETE /api/members/<id|npub>         owner, or the member themselves
//!   POST   /api/invites  GET /api/invites  DELETE /api/invites/<id>   owner
//!   POST   /api/join                      any signed principal with a token
//!   PUT    /api/visibility                owner
//!   POST   /api/rotate                    owner
//!   PUT    /api/secrets/<KEY>  GET /api/secrets  DELETE /api/secrets/<KEY>   editor
//!   GET    /api/storage-token             editor
//!   POST   /api/refresh                   editor
//!   POST   /api/webhook                   code.storage (HMAC)
//!   GET    /api/files  /api/file?path=  /api/file/stat?path=   viewer
//!   PUT    /api/blobs/<sha256>            editor (the body, streamed and hashed)
//!   GET|HEAD /api/blobs/<sha256>          viewer
//!   GET    /api/events?since=|tail=       viewer
//!   POST   /api/ops/<operation>           the operation's role (a job answers its run)
//!   GET    /api/runs?status=&op=  /api/runs/<id>   viewer
//!   POST   /api/replay  POST /api/pause   editor
//!   GET    /api/triggers                  viewer
//!   POST   /api/inbox                     the inbox token (no signature)
//!   *      /serve/<path>                  the site, `__tree`, `__file`, `__op`, `__watch`
//!   POST   /job/advance|effect|finish     a run's Workflow (jobs.rs); never routed from outside
//!   POST   /cap/files/read|list|stat      the app facet's `Files` capability (files.rs); never routed from outside
//!   POST   /deliver/report                the delivery consumer (deliveries.rs); never routed from outside
//!   POST   /test/keys  /test/fragment     the router's `/api/test/*`, on fleets with test hooks only (ops.rs)

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;

use fragment_core::access::{self, Decision, Purpose, Standing};
use fragment_core::npub;
use fragment_proto::{
    limits, valid_fragment_name, CodeStatus, Counts, CreateFragment, Created, ErrorCode, FragmentStatus, IdentityKind, Pins, Role, Urls,
    Visibility,
};
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::*;

use crate::config::Config;
use crate::cs::Cs;
use crate::error::{CellError, CellResult};
use crate::js;
use crate::routed::{Credential, Mode, Routed, Signed};

/// How long a create in progress holds its name.
const CLAIM_TTL_MS: i64 = 120_000;
/// The alarm fires no sooner than this after it is armed …
const ALARM_SOON_MS: i64 = 50;
/// … or than this after it failed.
const ALARM_RETRY_MS: i64 = 30_000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS members (
  principal TEXT PRIMARY KEY, role TEXT NOT NULL, added_by TEXT NOT NULL, added_at INTEGER NOT NULL,
  kind TEXT, owner TEXT);
CREATE TABLE IF NOT EXISTS invites (
  id TEXT PRIMARY KEY, token_sha TEXT NOT NULL UNIQUE, role TEXT NOT NULL, uses_left INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, created_by TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS index_outbox (
  principal TEXT PRIMARY KEY, role TEXT, version INTEGER NOT NULL, attempts INTEGER NOT NULL, next_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS secrets (
  name TEXT PRIMARY KEY, sealed TEXT NOT NULL, set_by TEXT NOT NULL, set_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS records (
  channel TEXT NOT NULL, seq INTEGER NOT NULL, at INTEGER NOT NULL, principal TEXT NOT NULL, kind TEXT NOT NULL,
  body TEXT NOT NULL, op TEXT, idx INTEGER, outboxed INTEGER NOT NULL DEFAULT 1, PRIMARY KEY (channel, seq));
CREATE UNIQUE INDEX IF NOT EXISTS records_effect ON records (op, idx) WHERE op IS NOT NULL;
CREATE TABLE IF NOT EXISTS tree (
  ref TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL, mode TEXT NOT NULL, last_commit TEXT NOT NULL,
  PRIMARY KEY (ref, path));
CREATE TABLE IF NOT EXISTS deliveries (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS code (
  id INTEGER PRIMARY KEY CHECK (id = 1), sha TEXT NOT NULL, loader_id TEXT NOT NULL, source TEXT NOT NULL,
  cpu_ms INTEGER NOT NULL, installed_at INTEGER NOT NULL,
  modules TEXT NOT NULL DEFAULT '{}', notify TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS code_ops (op TEXT PRIMARY KEY, kind TEXT NOT NULL, role TEXT NOT NULL, input TEXT);
CREATE TABLE IF NOT EXISTS code_channels (channel TEXT PRIMARY KEY, read TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS code_triggers (idx INTEGER PRIMARY KEY, kind TEXT NOT NULL, target TEXT NOT NULL, run TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS code_triggers_on ON code_triggers (kind, target);
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL, via TEXT NOT NULL, trigger TEXT, principal TEXT NOT NULL,
  role TEXT NOT NULL, depth INTEGER NOT NULL, call_id TEXT, input_sha TEXT, input TEXT NOT NULL, status TEXT NOT NULL,
  attempt INTEGER NOT NULL, output TEXT, error TEXT, created_at INTEGER NOT NULL, launched_at INTEGER, finished_at INTEGER);
CREATE UNIQUE INDEX IF NOT EXISTS runs_call ON runs (principal, call_id) WHERE call_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS runs_status ON runs (status, op);
CREATE TABLE IF NOT EXISTS steps (
  run INTEGER NOT NULL, attempt INTEGER NOT NULL, idx INTEGER NOT NULL, kind TEXT NOT NULL, value TEXT, error TEXT,
  CHECK ((value IS NULL) <> (error IS NULL)), PRIMARY KEY (run, attempt, idx));
CREATE TABLE IF NOT EXISTS schedules (idx INTEGER PRIMARY KEY, op TEXT NOT NULL, cron TEXT NOT NULL, next_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS paused_ops (op TEXT PRIMARY KEY, by TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS op_breakers (op TEXT PRIMARY KEY, reset_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS file_commits (key TEXT PRIMARY KEY, sha TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS own_commits (sha TEXT PRIMARY KEY, depth INTEGER NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS blobs (sha TEXT PRIMARY KEY, size INTEGER NOT NULL, uploaded_at INTEGER NOT NULL, seen_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pointers (
  ref TEXT NOT NULL, path TEXT NOT NULL, sha TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY (ref, path));
CREATE TABLE IF NOT EXISTS subs (
  id INTEGER PRIMARY KEY AUTOINCREMENT, principal TEXT NOT NULL, channel TEXT NOT NULL, url TEXT NOT NULL,
  created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS push_subs (
  id INTEGER PRIMARY KEY AUTOINCREMENT, endpoint TEXT NOT NULL UNIQUE, p256dh TEXT NOT NULL, auth TEXT NOT NULL,
  who TEXT NOT NULL, principal TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sent (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pending (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, ledger_id TEXT NOT NULL UNIQUE, op TEXT NOT NULL, principal TEXT NOT NULL,
  depth INTEGER NOT NULL, tries INTEGER NOT NULL, next_at INTEGER NOT NULL, at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS pending_due ON pending (next_at);
CREATE TABLE IF NOT EXISTS delivery_outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL CHECK (kind IN ('record', 'push', 'notify')),
  sub INTEGER, channel TEXT, seq INTEGER, who TEXT, url TEXT, body TEXT,
  after_sub INTEGER NOT NULL DEFAULT 0, upto_sub INTEGER NOT NULL DEFAULT 0,
  attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS delivery_outbox_due ON delivery_outbox (next_at);
CREATE INDEX IF NOT EXISTS delivery_outbox_waiting ON delivery_outbox (id) WHERE attempts > 0;
CREATE TABLE IF NOT EXISTS spend (ref TEXT PRIMARY KEY, run INTEGER NOT NULL, micros INTEGER NOT NULL, at INTEGER NOT NULL, video TEXT);
CREATE INDEX IF NOT EXISTS spend_run ON spend (run);
";

#[durable_object]
pub struct FragmentCell {
    pub(crate) state: State,
    pub(crate) raw: JsValue,
    pub(crate) env: Env,
    /// The isolate's settings (config.rs: built once per isolate).
    pub(crate) cfg: &'static Config,
    /// Serializes pin refreshes: two refreshes racing could leave the older head pinned.
    pub(crate) plane: futures_util::lock::Mutex<()>,
    pub(crate) rate: RefCell<fragment_core::ratelimit::Rate>,
    /// Whether this activation has swept its pending mutations.
    pub(crate) swept: Cell<bool>,
    /// The ledger ids a call or a sweep is settling now (channels.rs).
    pub(crate) settling: RefCell<BTreeMap<String, Arc<futures_util::lock::Mutex<()>>>>,
    /// How the app facet starts (ops.rs `app_loader`).
    pub(crate) app: js::AppLoader,
    /// What this activation knows of its live sockets (live.rs).
    pub(crate) live: RefCell<crate::live::LiveMemory>,
}

impl DurableObject for FragmentCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        let sql = state.storage().sql();
        sql.exec(SCHEMA, None).expect("the Fragment schema applies");
        // before the trigger state's migration, which reads the installed triggers
        let code_migrated = crate::plane::migrate_code(&sql);
        let cols: Vec<Value> = sql.exec("PRAGMA table_info(members)", None).and_then(|c| c.to_array()).unwrap_or_default();
        for col in ["kind", "owner"] {
            if !cols.iter().any(|c| c["name"] == col) {
                sql.exec(&format!("ALTER TABLE members ADD COLUMN {col} TEXT"), None).expect("the members table migrates");
            }
        }
        // a records table from before the delivery outbox: its records were
        // delivered by the code that wrote them, so they count as outboxed
        let cols: Vec<Value> = sql.exec("PRAGMA table_info(records)", None).and_then(|c| c.to_array()).unwrap_or_default();
        if !cols.iter().any(|c| c["name"] == "outboxed") {
            sql.exec("ALTER TABLE records ADD COLUMN outboxed INTEGER NOT NULL DEFAULT 1", None).expect("the records table migrates");
        }
        // after the migration: a members table from before phase 4 has no
        // owner column until it runs (the index in SCHEMA broke those cells)
        sql.exec("CREATE INDEX IF NOT EXISTS members_owner ON members (owner) WHERE owner IS NOT NULL", None).expect("the members index applies");
        let paused_by_migration = crate::jobs::migrate_trigger_state(&sql, js::now_ms());
        let cfg = Config::from_env(&env);
        let rate = fragment_core::ratelimit::Rate::new(limits::PUBLIC_CALLS_PER_MIN, limits::PUBLIC_CALLS_PER_MIN_FRAGMENT);
        let app = crate::ops::app_loader(&raw, env.as_ref(), sql.clone());
        let cell = FragmentCell {
            state,
            raw,
            env,
            cfg,
            plane: futures_util::lock::Mutex::new(()),
            rate: RefCell::new(rate),
            swept: Cell::new(false),
            settling: RefCell::default(),
            app,
            live: RefCell::default(),
        };
        if !paused_by_migration.is_empty() {
            let summary = format!("the stored pause list did not parse; paused every triggered operation: {}", paused_by_migration.join(", "));
            cell.event("op.paused", &summary, json!({ "ops": paused_by_migration, "by": "migration" }));
        }
        if let Some(why) = code_migrated {
            cell.event("code.dropped", &format!("the installed code did not move into its tables ({why}); live installs again at the next refresh"), Value::Null);
        }
        cell
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(resp) => Ok(resp),
            Err(e) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        if let Err(e) = self.on_alarm().await {
            self.event("alarm.failed", &e.message, json!({ "code": e.code }));
            // Whatever is due, a failing alarm tries again no sooner than
            // this, so a lasting failure never spins.
            let _ = self.arm(None, ALARM_RETRY_MS).await;
        }
        Response::ok("")
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let WebSocketIncomingMessage::String(s) = message else { return Ok(()) };
        if self.state.get_tags(&ws).iter().any(|t| t == "live") {
            self.live_message(&ws, &s);
        } else if serde_json::from_str::<Value>(&s).is_ok_and(|v| v["type"] == "ping") {
            ws.send_with_str(r#"{"type":"pong"}"#)?;
        }
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, code: usize, reason: String, _clean: bool) -> Result<()> {
        if self.state.get_tags(&ws).iter().any(|t| t == "live") {
            self.live_closed(&ws);
        }
        let code = if code == 1005 || code == 1006 { 1000 } else { code as u16 };
        let _ = ws.close(Some(code), Some(reason));
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

/// An `events` record's body as `event` writes it.
#[derive(serde::Deserialize)]
struct EventBody<'a> {
    #[serde(borrow)]
    summary: &'a RawValue,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(serde::Serialize)]
struct EventView<'a> {
    id: i64,
    at: i64,
    kind: &'a str,
    summary: &'a RawValue,
    data: &'a RawValue,
}

#[derive(serde::Serialize)]
struct Events<'a> {
    events: Vec<EventView<'a>>,
}

/// Who is calling, as the router established it (`Routed`, less the name).
#[derive(Clone)]
pub struct Caller {
    /// Who signed, or whose session this is (`None`: anonymous, or not
    /// resolved yet): an identity always with its kind, and the key when
    /// one signed.
    pub signed: Option<Signed>,
    /// A site request's signer or session, not resolved yet (`signed` is
    /// `None` meanwhile). Until `identified` resolves it the caller reads
    /// as anonymous, which never sees more than who they are would.
    pub unresolved: Option<Credential>,
    /// The URL the request arrived on: canonical URLs, cookies, and the
    /// query string derive from it.
    pub url: url::Url,
    /// How a site request addressed the fragment (`None` off the site).
    pub mode: Option<Mode>,
}

impl Caller {
    /// The caller's identity (`id:…`), when someone signed or has a session.
    pub fn principal(&self) -> Option<&str> {
        self.signed.as_ref().map(|s| s.id.as_str())
    }

    /// The key it signed with (64 hex).
    pub fn key(&self) -> Option<&str> {
        self.signed.as_ref().and_then(|s| s.key.as_deref())
    }

    pub fn kind(&self) -> Option<IdentityKind> {
        self.signed.as_ref().map(|s| s.kind)
    }

    /// An agent's owner.
    pub fn owner(&self) -> Option<&str> {
        self.signed.as_ref().and_then(|s| s.owner.as_deref())
    }
}

/// The fragment's `meta` rows, one variant per key: every read and write
/// names its row here, so each key is spelled once (`key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetaKey {
    /// `<label>.<username>`, written when a create claims it.
    Name,
    /// When a create in progress claimed the name (the claim expires).
    ClaimedAt,
    /// When the fragment was created, its incarnation: a fragment exists
    /// once it has this row, written last.
    CreatedAt,
    /// The identity that created it.
    Owner,
    /// The fragment's own key.
    Npub,
    /// Its secret key, sealed by `KEYS`.
    FragmentSecret,
    Visibility,
    /// The share link's token.
    ViewToken,
    InboxToken,
    WebhookSecret,
    /// Its code.storage repo.
    Repo,
    /// The latest members index change (members.rs).
    IndexVersion,
    /// When the poll backstop runs next.
    PollAt,
    /// A template still to commit (publish.rs).
    TemplatePending,
    /// An owner's agent still to join (publish.rs).
    AgentPending,
    /// A certificate still to ask for.
    /// The commits the cell pins (plane.rs).
    PinMain,
    PinLive,
    /// When a request first asked code.storage for the pins (plane.rs
    /// `ensure_pins`); after it, moves arrive by webhook, refresh, or poll.
    PinsCheckedAt,
    /// The commit main's manifest was read from.
    MainReadAt,
    /// The commit live's code was installed from.
    LiveReadAt,
    /// `fragment.json` at main, as JSON text.
    ManifestMain,
    /// The live manifest's `meta`, as JSON text: a page's Open Graph tags.
    MetaLive,
    /// The live manifest's `capabilities`, as a JSON list.
    CapabilitiesLive,
    /// Why live's code was not installed.
    CodeError,
    /// When the blob collection runs next.
    BlobsGcAt,
    /// The fragment's VAPID key, sealed (push.rs).
    Vapid,
    /// Test fleets only: a shorter ledger window (`/test/fragment ledger`).
    TestLedgerMs,
    /// Test fleets only: how many more queue sends fail (`fail-deliveries`).
    TestFailDeliveries,
    /// Test fleets only: how many more outbox writes fail (`fail-outbox`).
    TestFailOutbox,
    /// Test fleets only: how many more trigger steps fail (`fail-triggers`).
    TestFailTriggers,
    /// Test fleets only: how many more step answers are lost on their way
    /// back to the Workflow, after the step was performed (`drop-effects`).
    TestDropEffects,
}

impl MetaKey {
    pub(crate) const fn key(self) -> &'static str {
        match self {
            MetaKey::Name => "name",
            MetaKey::ClaimedAt => "claimed_at",
            MetaKey::CreatedAt => "created_at",
            MetaKey::Owner => "owner",
            MetaKey::Npub => "npub",
            MetaKey::FragmentSecret => "fragment_secret",
            MetaKey::Visibility => "visibility",
            MetaKey::ViewToken => "view_token",
            MetaKey::InboxToken => "inbox_token",
            MetaKey::WebhookSecret => "webhook_secret",
            MetaKey::Repo => "repo",
            MetaKey::IndexVersion => "index_version",
            MetaKey::PollAt => "poll_at",
            MetaKey::TemplatePending => "template_pending",
            MetaKey::AgentPending => "agent_pending",
            MetaKey::PinMain => "pin_main",
            MetaKey::PinLive => "pin_live",
            MetaKey::PinsCheckedAt => "pins_checked_at",
            MetaKey::MainReadAt => "main_read_at",
            MetaKey::LiveReadAt => "live_read_at",
            MetaKey::ManifestMain => "manifest_main",
            MetaKey::MetaLive => "meta_live",
            MetaKey::CapabilitiesLive => "capabilities_live",
            MetaKey::CodeError => "code_error",
            MetaKey::BlobsGcAt => "blobs_gc_at",
            MetaKey::Vapid => "vapid",
            MetaKey::TestLedgerMs => "test_ledger_ms",
            MetaKey::TestFailDeliveries => "test_fail_deliveries",
            MetaKey::TestFailOutbox => "test_fail_outbox",
            MetaKey::TestFailTriggers => "test_fail_triggers",
            MetaKey::TestDropEffects => "test_drop_effects",
        }
    }

    /// The pin of `which`, one of `plane::REFS`.
    pub(crate) fn pin(which: &str) -> MetaKey {
        match which {
            "main" => MetaKey::PinMain,
            "live" => MetaKey::PinLive,
            other => panic!("{other} is not a pinned ref"),
        }
    }

    /// The commit what follows `which` was read from (plane.rs `follow`).
    pub(crate) fn read_at(which: &str) -> MetaKey {
        match which {
            "main" => MetaKey::MainReadAt,
            "live" => MetaKey::LiveReadAt,
            other => panic!("{other} is not a pinned ref"),
        }
    }
}

/// A created fragment's facts as one request reads them (`facts`): the
/// `meta` rows its routes consult, in one statement. It is that request's
/// snapshot and is never kept, so nothing invalidates it: a handler that
/// writes one of these rows and reads it back in the same request
/// (create, rotate, visibility) reads `meta` directly, and `ensure_pins`
/// updates the pins it moves.
pub(crate) struct Facts {
    pub name: String,
    pub visibility: Visibility,
    pub view_token: String,
    pub repo: String,
    pub pin_main: Option<String>,
    pub pin_live: Option<String>,
    /// Whether a request has asked code.storage for the pins yet.
    pub pins_checked: bool,
}

impl Facts {
    /// The pin of `which`, one of `plane::REFS`.
    pub(crate) fn pin(&self, which: &str) -> Option<&str> {
        match which {
            "main" => self.pin_main.as_deref(),
            "live" => self.pin_live.as_deref(),
            other => panic!("{other} is not a pinned ref"),
        }
    }

    pub(crate) fn set_pin(&mut self, which: &str, pin: Option<String>) {
        match which {
            "main" => self.pin_main = pin,
            "live" => self.pin_live = pin,
            other => panic!("{other} is not a pinned ref"),
        }
    }
}

pub(crate) fn missing(key: MetaKey) -> CellError {
    CellError::host(format!("a created fragment has no {}", key.key()))
}

fn not_created() -> CellError {
    CellError::new(ErrorCode::NotFound, "no such fragment")
}

/// Whether a standing is the caller's own (a membership, or an agent
/// member's owner), not a share link or a visibility floor.
pub(crate) fn as_themselves(standing: Standing) -> bool {
    standing.member.is_some() || standing.owns_member_agent
}

/// The caller's role for `purpose`, or the refusal and why: pure, over the
/// visibility and standing a request read once. `needs` is the weakest
/// role that may do it.
pub(crate) fn decide(visibility: Visibility, standing: Standing, purpose: Purpose, needs: Role) -> CellResult<Role> {
    let why = || match (needs, visibility) {
        _ if standing.owns_member_agent && purpose == Purpose::Act => "you read this fragment through your agent's membership; acting here needs your own".to_string(),
        (Role::Public, Visibility::Link) => "this fragment is shared by link: open it with its share link (?view=)".to_string(),
        (Role::Public, Visibility::Members) => "this fragment is for its members only".to_string(),
        _ => format!("this needs the {} role", needs.as_str()),
    };
    match access::decide(visibility, standing, purpose, needs) {
        Decision::Allow(role) => Ok(role),
        Decision::Unauthenticated => Err(CellError::new(ErrorCode::Unauthenticated, format!("{}; sign in or sign the request", why()))),
        Decision::Forbidden => Err(CellError::new(ErrorCode::Forbidden, why())),
    }
}

pub(crate) async fn body_json<T: DeserializeOwned>(req: &mut Request) -> CellResult<T> {
    let bytes = req.bytes().await?;
    serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))
}

pub(crate) fn json_response<T: serde::Serialize>(v: &T) -> CellResult<Response> {
    Ok(Response::from_json(v)?.with_headers(no_store()))
}

pub(crate) fn no_store() -> Headers {
    let h = Headers::new();
    h.set("content-type", "application/json").expect("static header");
    h.set("cache-control", "no-store").expect("static header");
    h
}

pub(crate) fn decode_segment(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default()
}

impl FragmentCell {
    pub(crate) fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    pub(crate) fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.sql().exec(q, binds)?.to_array::<Value>()?)
    }

    /// Rows decoded into `T`: a row that does not fit is a host fault.
    pub(crate) fn typed<T: DeserializeOwned>(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<T>> {
        Ok(self.sql().exec(q, binds)?.to_array::<T>()?)
    }

    pub(crate) fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.sql().exec(q, binds)?;
        Ok(())
    }

    pub(crate) fn meta(&self, key: MetaKey) -> CellResult<Option<String>> {
        let [value] = self.metas([key])?;
        Ok(value)
    }

    /// Several `meta` rows in one statement, in the order `keys` names them.
    pub(crate) fn metas<const N: usize>(&self, keys: [MetaKey; N]) -> CellResult<[Option<String>; N]> {
        #[derive(serde::Deserialize)]
        struct Row {
            key: String,
            value: String,
        }
        let marks = vec!["?"; N].join(", ");
        let rows: Vec<Row> = self.typed(&format!("SELECT key, value FROM meta WHERE key IN ({marks})"), keys.iter().map(|k| k.key().into()).collect())?;
        assert!(rows.len() <= N, "meta.key is the primary key");
        let mut out: [Option<String>; N] = std::array::from_fn(|_| None);
        for row in rows {
            let i = keys.iter().position(|k| k.key() == row.key).expect("the IN list answers only the keys it names");
            out[i] = Some(row.value);
        }
        Ok(out)
    }

    pub(crate) fn set_meta(&self, key: MetaKey, value: &str) -> CellResult<()> {
        self.exec("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![key.key().into(), value.into()])
    }

    pub(crate) fn del_meta(&self, key: MetaKey) -> CellResult<()> {
        self.exec("DELETE FROM meta WHERE key = ?", vec![key.key().into()])
    }

    /// A meta value every created fragment has.
    pub(crate) fn must(&self, key: MetaKey) -> CellResult<String> {
        self.meta(key)?.ok_or_else(|| missing(key))
    }

    pub(crate) fn count(&self, q: &str) -> CellResult<u64> {
        Ok(self.rows(q, vec![])?.first().and_then(|r| r["n"].as_u64()).unwrap_or(0))
    }

    /// Appends to the audit trail: the `events` channel, written by the platform.
    pub(crate) fn event(&self, kind: &str, summary: &str, data: Value) {
        let summary: String = summary.chars().take(500).collect();
        let _ = self.append("events", "platform", kind, &json!({ "summary": summary, "data": data }), None);
    }

    /// The fragment's name, or 404 when it was never created (or was deleted).
    pub(crate) fn name(&self) -> CellResult<String> {
        match self.metas([MetaKey::CreatedAt, MetaKey::Name])? {
            [Some(_), Some(name)] => Ok(name),
            [Some(_), None] => Err(missing(MetaKey::Name)),
            [None, _] => Err(not_created()),
        }
    }

    /// The fragment's facts for one request (`Facts`), or 404 when it was
    /// never created (or was deleted): one statement.
    pub(crate) fn facts(&self) -> CellResult<Facts> {
        let [created_at, name, visibility, view_token, repo, pin_main, pin_live, pins_checked_at] = self.metas([
            MetaKey::CreatedAt,
            MetaKey::Name,
            MetaKey::Visibility,
            MetaKey::ViewToken,
            MetaKey::Repo,
            MetaKey::PinMain,
            MetaKey::PinLive,
            MetaKey::PinsCheckedAt,
        ])?;
        if created_at.is_none() {
            return Err(not_created());
        }
        let visibility = visibility.ok_or_else(|| missing(MetaKey::Visibility))?;
        Ok(Facts {
            name: name.ok_or_else(|| missing(MetaKey::Name))?,
            visibility: Visibility::parse(&visibility).ok_or_else(|| CellError::host(format!("stored visibility {visibility:?}")))?,
            view_token: view_token.ok_or_else(|| missing(MetaKey::ViewToken))?,
            repo: repo.ok_or_else(|| missing(MetaKey::Repo))?,
            pin_main,
            pin_live,
            pins_checked: pins_checked_at.is_some(),
        })
    }

    pub(crate) fn visibility(&self) -> CellResult<Visibility> {
        let v = self.must(MetaKey::Visibility)?;
        Visibility::parse(&v).ok_or_else(|| CellError::host(format!("stored visibility {v:?}")))
    }

    pub(crate) fn member_role(&self, principal: &str) -> CellResult<Option<Role>> {
        let rows = self.rows("SELECT role FROM members WHERE principal = ?", vec![principal.into()])?;
        Ok(rows.first().and_then(|r| r["role"].as_str()).and_then(Role::parse))
    }

    /// What the caller brings, in one statement: their membership, or an
    /// agent of theirs that is a member (they read what it reads). A
    /// request reads it once and decides with it (`decide`) as often as
    /// it asks.
    pub(crate) fn standing(&self, caller: &Caller, link: bool) -> CellResult<Standing> {
        let Some(principal) = caller.principal() else {
            return Ok(Standing { member: None, owns_member_agent: false, link, signed: false });
        };
        #[derive(serde::Deserialize)]
        struct Row {
            role: Option<String>,
            agent: i64,
        }
        let rows: Vec<Row> = self.typed(
            "SELECT (SELECT role FROM members WHERE principal = ?) AS role, EXISTS (SELECT 1 FROM members WHERE owner = ?) AS agent",
            vec![principal.into(), principal.into()],
        )?;
        let row = rows.into_iter().next().expect("a SELECT without FROM answers one row");
        let member = row.role.as_deref().and_then(Role::parse);
        Ok(Standing { member, owns_member_agent: member.is_none() && row.agent != 0, link, signed: true })
    }

    /// Whether the caller sees the fragment as themselves (a member, or an
    /// agent's owner), not by its link or its visibility: their sockets are
    /// tagged `p:<id>` and close when that standing is revoked.
    pub(crate) fn has_standing(&self, caller: &Caller) -> CellResult<bool> {
        Ok(as_themselves(self.standing(caller, false)?))
    }

    /// The caller's role for reading, or the refusal: `needs` is the weakest
    /// role that may read.
    pub(crate) fn require(&self, caller: &Caller, link: bool, needs: Role) -> CellResult<Role> {
        self.admit(&self.facts()?, caller, link, needs)
    }

    /// `require` over facts the request already read. A route that decides
    /// more than once reads its standing itself and calls `decide`.
    pub(crate) fn admit(&self, facts: &Facts, caller: &Caller, link: bool, needs: Role) -> CellResult<Role> {
        decide(facts.visibility, self.standing(caller, link)?, Purpose::Read, needs)
    }

    /// The caller with who they are resolved: a site request's credential,
    /// asked of the registry here, once (a stale session is nobody; a key
    /// no one holds is 401; a registry that cannot answer is 503). Any
    /// other caller as it came.
    pub(crate) async fn identified<'a>(&self, caller: &'a Caller, name: &str) -> CellResult<Cow<'a, Caller>> {
        let Some(credential) = &caller.unresolved else { return Ok(Cow::Borrowed(caller)) };
        let signed = credential.clone().resolve(&self.env, name).await?;
        Ok(Cow::Owned(Caller { signed, unresolved: None, url: caller.url.clone(), mode: caller.mode }))
    }

    /// The caller of a read that answers alike for everyone who may see
    /// the fragment (a page, a file): who they are is resolved only when
    /// the anonymous standing may not read, since no one sees less than
    /// that (`access::effective_role` only adds to it). A refusal is the
    /// resolved caller's (401 or 403).
    pub(crate) async fn reader<'a>(&self, facts: &Facts, caller: &'a Caller, link: bool) -> CellResult<Cow<'a, Caller>> {
        if caller.unresolved.is_some() && self.admit(facts, caller, link, Role::Public).is_ok() {
            return Ok(Cow::Borrowed(caller));
        }
        let caller = self.identified(caller, &facts.name).await?;
        self.admit(facts, &caller, link, Role::Public)?;
        Ok(caller)
    }

    /// The signer's identity.
    pub(crate) fn caller_id<'a>(&self, caller: &'a Caller) -> CellResult<&'a str> {
        caller.principal().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "sign the request"))
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        let path = req.path();
        if path == "/test/keys" && self.cfg.test_hooks {
            // this cell's own use of KEYS: the e2e checks that what one
            // fragment seals another cannot open (the host attests which)
            let body: Value = body_json(&mut req).await?;
            let answer = match (body["op"].as_str(), body["plaintext"].as_str(), body["sealed"].as_str()) {
                (Some("seal"), Some(text), _) => json!({ "sealed": crate::keys::seal(&self.env, text.as_bytes()).await? }),
                (Some("open"), _, Some(sealed)) => json!({ "plaintext": String::from_utf8_lossy(&crate::keys::open(&self.env, sealed, "").await?.plaintext) }),
                _ => return Err(CellError::invalid("op is seal {plaintext} or open {sealed}")),
            };
            return json_response(&answer);
        }
        if path == "/test/fragment" && self.cfg.test_hooks {
            let body: Value = body_json(&mut req).await?;
            return json_response(&self.test_fragment(&body)?);
        }
        if let Some(step) = path.strip_prefix("/job/") {
            // Only this script's Workflow sets the header; the router never passes it.
            if req.headers().get(crate::jobs::JOB_HEADER)?.is_none() {
                return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}")));
            }
            let step = step.to_string();
            let bytes = req.bytes().await?;
            return json_response(&self.job_callback(&step, &bytes).await?);
        }
        if path == "/deliver/report" {
            // Only the delivery consumer sets the header; the router never passes it.
            if req.headers().get(crate::deliveries::REPORT_HEADER)?.is_none() {
                return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}")));
            }
            let report = body_json(&mut req).await?;
            return json_response(&self.delivery_report(&report)?);
        }
        if let Some(op) = path.strip_prefix("/cap/files/") {
            // Only the `Files` capability sets the header; the router never passes it.
            if req.headers().get(crate::files::CAP_HEADER)?.as_deref() != Some("files") {
                return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}")));
            }
            let op = op.to_string();
            let body: Value = body_json(&mut req).await?;
            return self.cap_files(&op, &body).await;
        }
        // Every route below is the router's: decoded once, from headers only it sets.
        let Routed { name: routed_name, url, mode, signed, credential } = Routed::from_headers(req.headers())?;
        let caller = Caller { signed, unresolved: credential, url, mode };
        if let Some(rest) = path.strip_prefix("/serve/") {
            let rest = rest.to_string();
            return self.serve(req, &caller, &routed_name, &rest).await;
        }
        // the router resolves who is asking on every route but the site's
        if caller.unresolved.is_some() {
            return Err(CellError::host(format!("the router left the caller of {path} unresolved")));
        }
        let query = |k: &str| caller.url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
        let segments: Vec<String> = path.trim_start_matches('/').split('/').map(decode_segment).collect();
        let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
        match (req.method(), segs.as_slice()) {
            (Method::Post, ["create"]) => {
                let body: CreateFragment = body_json(&mut req).await?;
                if body.name != routed_name {
                    return Err(CellError::host("the router addressed a different fragment than the body names"));
                }
                self.create(&caller, body).await
            }
            (Method::Delete, ["delete"]) => self.delete(&caller).await,
            (Method::Get, ["api", "status"]) => self.status(&caller),
            (Method::Get, ["api", "manifest"]) => self.manifest(&caller),
            (Method::Get, ["api", "members"]) => self.members(&caller),
            (Method::Put, ["api", "members", who]) => {
                let body = body_json(&mut req).await?;
                self.set_member(&caller, who, body).await
            }
            (Method::Delete, ["api", "members", who]) => self.remove_member(&caller, who).await,
            (Method::Post, ["api", "invites"]) => {
                let body = body_json(&mut req).await?;
                self.create_invite(&caller, body)
            }
            (Method::Get, ["api", "invites"]) => self.invites(&caller),
            (Method::Delete, ["api", "invites", id]) => self.revoke_invite(&caller, id),
            (Method::Post, ["api", "join"]) => {
                let body = body_json(&mut req).await?;
                self.join(&caller, body).await
            }
            (Method::Put, ["api", "visibility"]) => {
                let body = body_json(&mut req).await?;
                self.set_visibility(&caller, body)
            }
            (Method::Post, ["api", "rotate"]) => {
                let bytes = req.bytes().await?;
                let body: Value =
                    if bytes.is_empty() { json!({}) } else { serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))? };
                self.rotate(&caller, body)
            }
            (Method::Put, ["api", "secrets", key]) => {
                let bytes = req.bytes().await?;
                self.put_secret(&caller, key, bytes).await
            }
            (Method::Get, ["api", "secrets"]) => self.list_secrets(&caller),
            (Method::Delete, ["api", "secrets", key]) => self.delete_secret(&caller, key),
            (Method::Get, ["api", "storage-token"]) => self.storage_token(&caller).await,
            (Method::Post, ["api", "refresh"]) => self.refresh(&caller).await,
            (Method::Post, ["api", "webhook"]) => {
                let event = req.headers().get("x-pierre-event")?.unwrap_or_default();
                let signature = req.headers().get("x-pierre-signature")?.unwrap_or_default();
                let body = req.bytes().await?;
                self.webhook(&event, &signature, &body).await
            }
            (Method::Get, ["api", "files"]) => self.files(&caller).await,
            (Method::Post, ["api", "files"]) => {
                let body = body_json(&mut req).await?;
                self.write_files_api(&caller, body).await
            }
            (Method::Post, ["api", "deploy"]) => {
                let bytes = req.bytes().await?;
                let body: Value =
                    if bytes.is_empty() { json!({}) } else { serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))? };
                self.deploy_api(&caller, body).await
            }
            (Method::Get, ["api", "file"]) => self.file(&caller, &query("path").unwrap_or_default()).await,
            (Method::Get, ["api", "file", "stat"]) => self.stat(&caller, &query("path").unwrap_or_default()).await,
            (Method::Get, ["api", "events"]) => self.events(&caller, query("since").and_then(|s| s.parse().ok()).unwrap_or(0), query("tail")),
            (Method::Get, ["api", "channels"]) => self.channels(&caller),
            (Method::Get, ["api", "channels", channel]) => {
                let after = query("after").and_then(|s| s.parse().ok()).unwrap_or(0);
                let limit = query("limit").and_then(|s| s.parse().ok()).unwrap_or(limits::CHANNEL_PAGE);
                self.channel(&caller, channel, after, limit)
            }
            (Method::Post, ["api", "ops", op]) => {
                let body = body_json(&mut req).await?;
                let op = op.to_string();
                self.api_op(&caller, &op, body).await
            }
            (Method::Put, ["api", "blobs", sha]) => {
                let sha = sha.to_string();
                self.put_blob(&caller, &sha, &req).await
            }
            (method @ (Method::Get | Method::Head), ["api", "blobs", sha]) => {
                let range = req.headers().get("range")?;
                self.get_blob(&caller, sha, method == Method::Head, range.as_deref()).await
            }
            (Method::Get, ["api", "runs"]) => {
                let limit = query("limit").and_then(|s| s.parse().ok()).unwrap_or(30);
                self.runs_api(&caller, query("status"), query("op"), limit)
            }
            (Method::Get, ["api", "runs", id]) => self.run_api(&caller, id),
            (Method::Post, ["api", "replay"]) => {
                let body = body_json(&mut req).await?;
                self.replay(&caller, body).await
            }
            (Method::Post, ["api", "pause"]) => {
                let body = body_json(&mut req).await?;
                self.pause(&caller, body)
            }
            (Method::Get, ["api", "triggers"]) => self.triggers_api(&caller),
            (Method::Post, ["api", "subscriptions"]) => {
                let body: Value = body_json(&mut req).await?;
                self.subscribe(&caller, &body)
            }
            (Method::Get, ["api", "subscriptions"]) => self.subscriptions(&caller),
            (Method::Delete, ["api", "subscriptions", id]) => self.unsubscribe(&caller, id),
            (Method::Post, ["api", "inbox"]) => {
                let token = match req.headers().get("x-fragment-inbox-token")? {
                    Some(t) => t,
                    None => query("t").unwrap_or_default(),
                };
                let hops = req.headers().get(crate::jobs::HOPS_HEADER)?.and_then(|h| h.parse().ok()).unwrap_or(0);
                let body = req.bytes().await?;
                self.inbox(&token, hops, &body).await
            }
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {path}", req.method().as_ref()))),
        }
    }

    async fn create(&self, caller: &Caller, body: CreateFragment) -> CellResult<Response> {
        let owner = self.caller_id(caller)?.to_string();
        if !valid_fragment_name(&body.name) {
            return Err(CellError::invalid("a fragment name must match ^[a-z0-9][a-z0-9-]{0,62}$"));
        }
        if let Some(t) = body.template.as_deref().filter(|t| crate::publish::template(t).is_none()) {
            let names: Vec<&str> = crate::publish::TEMPLATES.iter().map(|(n, _)| *n).collect();
            return Err(CellError::invalid(format!("no template {t:?}; the templates are {}", names.join(", "))));
        }
        let cs_cfg = self.cfg.codestorage()?;
        // Claim the name before the first await: a concurrent create for the
        // same name reaches this same object and must see it taken.
        // A claim left by a create that crashed mid-flight expires.
        let [created_at, claimed_at] = self.metas([MetaKey::CreatedAt, MetaKey::ClaimedAt])?;
        let claimed_at: i64 = claimed_at.and_then(|s| s.parse().ok()).unwrap_or(0);
        if created_at.is_some() || js::now_ms() - claimed_at < CLAIM_TTL_MS {
            return Err(CellError::new(ErrorCode::AlreadyExists, format!("fragment {} already exists", body.name)));
        }
        self.set_meta(MetaKey::Name, &body.name)?;
        self.set_meta(MetaKey::ClaimedAt, &js::now_ms().to_string())?;
        // the fragment's own key is made by KEYS; its secret stays sealed there
        let made = async {
            let repo_name = fragment_proto::flat_name(&body.name).ok_or_else(|| CellError::invalid("a fragment's name is <label>.<username>"))?;
            let repo = Cs::new(cs_cfg, &self.env).ensure_repo(&repo_name).await?;
            let (pubkey, sealed) = crate::keys::nostr_keypair(&self.env).await?;
            Ok::<_, CellError>((repo, pubkey, sealed))
        };
        let (repo, fragment_pub, sealed) = match made.await {
            Ok(r) => r,
            Err(e) => {
                self.del_meta(MetaKey::Name)?;
                self.del_meta(MetaKey::ClaimedAt)?;
                return Err(e);
            }
        };
        let now = js::now_ms();
        let fragment_npub = npub::encode(&fragment_pub);
        let visibility = body.visibility.unwrap_or(Visibility::Link);
        let (view_token, inbox_token, webhook_secret) = (js::random_hex::<12>(), js::random_hex::<16>(), js::random_hex::<16>());
        let poll_at = (now + self.cfg.poll_interval_ms).to_string();
        let created_at = now.to_string();
        for (k, v) in [
            (MetaKey::Owner, owner.as_str()),
            (MetaKey::Npub, fragment_npub.as_str()),
            (MetaKey::FragmentSecret, sealed.as_str()),
            (MetaKey::Visibility, visibility.as_str()),
            (MetaKey::ViewToken, view_token.as_str()),
            (MetaKey::InboxToken, inbox_token.as_str()),
            (MetaKey::WebhookSecret, webhook_secret.as_str()),
            (MetaKey::Repo, repo.as_str()),
            (MetaKey::IndexVersion, "0"),
            (MetaKey::PollAt, poll_at.as_str()),
            // written last: a fragment exists once it has created_at
            (MetaKey::CreatedAt, created_at.as_str()),
        ] {
            self.set_meta(k, v)?;
        }
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind, owner) VALUES (?, 'owner', ?, ?, ?, ?)",
            vec![
                owner.as_str().into(),
                owner.as_str().into(),
                SqlStorageValue::Integer(now),
                caller.kind().map_or(SqlStorageValue::Null, |k| k.as_str().into()),
                caller.owner().map_or(SqlStorageValue::Null, |o| o.into()),
            ],
        )?;
        self.index_change(&owner, Some(Role::Owner))?;
        if let Some(t) = &body.template {
            self.set_meta(MetaKey::TemplatePending, t)?;
        }
        self.event("create", &format!("fragment {} created by {owner} (repo {repo})", body.name), json!({ "repo": repo, "key": caller.key().map(npub::display) }));
        self.flush_index().await;
        // a template that did not land, or a chat's agent that did not
        // join, is retried by the alarm
        if let Err(e) = self.seed().await {
            self.event("template.failed", &e.message, json!({ "code": e.code }));
        } else if let Err(e) = self.join_owners_agent().await {
            self.event("agent.join-failed", &e.message, json!({ "code": e.code }));
        }
        self.schedule().await?;
        json_response(&Created {
            name: body.name.clone(),
            npub: fragment_npub,
            owner: owner.clone(),
            visibility,
            view_token,
            inbox_token,
            webhook_secret,
            repo,
            canonical: self.cfg.canonical(&caller.url, &body.name),
        })
    }

    async fn delete(&self, caller: &Caller) -> CellResult<Response> {
        let name = self.name()?;
        self.require(caller, false, Role::Owner)?;
        let members: Vec<String> =
            self.rows("SELECT principal FROM members", vec![])?.iter().filter_map(|r| r["principal"].as_str().map(str::to_string)).collect();
        for m in &members {
            self.index_change(m, None)?;
        }
        self.flush_index().await;
        for ws in self.state.get_websockets() {
            let _ = ws.close(Some(4004), Some("the fragment was deleted"));
        }
        js::delete_app_facet(&self.raw)?;
        self.delete_blobs().await?;
        self.state.storage().delete_all().await?;
        self.sql().exec(SCHEMA, None)?;
        json_response(&json!({ "ok": true, "deleted": name }))
    }

    pub(crate) fn code_status(&self) -> CellResult<CodeStatus> {
        let rows = self.rows("SELECT sha FROM code WHERE id = 1", vec![])?;
        let sha = rows.first().map(|r| r["sha"].as_str().expect("code.sha is TEXT").to_string());
        Ok(CodeStatus { sha, operations: self.operations()?, error: self.meta(MetaKey::CodeError)? })
    }

    fn status(&self, caller: &Caller) -> CellResult<Response> {
        let facts = self.facts()?;
        let role = decide(facts.visibility, self.standing(caller, false)?, Purpose::Read, Role::Viewer)?;
        let [npub, owner, inbox_token] = self.metas([MetaKey::Npub, MetaKey::Owner, MetaKey::InboxToken])?;
        json_response(&FragmentStatus {
            npub: npub.ok_or_else(|| missing(MetaKey::Npub))?,
            owner: npub::display(&owner.ok_or_else(|| missing(MetaKey::Owner))?),
            role,
            visibility: facts.visibility,
            repo: facts.repo,
            pins: Pins { main: facts.pin_main, live: facts.pin_live },
            counts: Counts {
                files: self.count("SELECT COUNT(*) AS n FROM tree WHERE ref = 'main'")?,
                events: self.count("SELECT COUNT(*) AS n FROM records WHERE channel = 'events'")?,
                members: self.count("SELECT COUNT(*) AS n FROM members")?,
            },
            code: self.code_status()?,
            view_token: Some(facts.view_token),
            inbox_token: if role >= Role::Editor { Some(inbox_token.ok_or_else(|| missing(MetaKey::InboxToken))?) } else { None },
            urls: Urls { canonical: self.cfg.canonical(&caller.url, &facts.name) },
            blob_min_bytes: Some(fragment_core::blob::BLOB_MIN_BYTES as u64),
            name: facts.name,
        })
    }

    /// The `events` channel in the shape `fragment events` reads: the page
    /// after `since`, or with `tail` the newest `tail` events (oldest first
    /// either way). Without `tail`, a log longer than one page never
    /// showed its recent events to `fragment events --tail`.
    fn events(&self, caller: &Caller, since: i64, tail: Option<String>) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let (after, limit) = match tail {
            None => (since, limits::EVENTS_PAGE),
            Some(t) => {
                let n = t.parse::<usize>().ok().filter(|n| (1..=limits::EVENTS_PAGE).contains(n));
                let Some(n) = n else { return Err(CellError::invalid(format!("tail is 1-{}", limits::EVENTS_PAGE))) };
                if since != 0 {
                    return Err(CellError::invalid("since or tail, not both"));
                }
                let oldest = self.rows(
                    "SELECT MIN(seq) AS seq FROM (SELECT seq FROM records WHERE channel = 'events' ORDER BY seq DESC LIMIT ?)",
                    vec![SqlStorageValue::Integer(n as i64)],
                )?;
                (oldest.first().and_then(|r| r["seq"].as_i64()).map_or(0, |seq| seq - 1), n)
            }
        };
        let records = self.read_channel("events", after, limit)?;
        let mut events = Vec::with_capacity(records.len());
        for r in &records {
            // the platform's own body, `{summary, data}`, taken apart without building it
            let body: EventBody = serde_json::from_str(r.body.get()).map_err(|e| CellError::host(format!("event {}: {e}", r.seq)))?;
            events.push(EventView { id: r.seq, at: r.at, kind: &r.kind, summary: body.summary, data: body.data });
        }
        json_response(&Events { events })
    }

    /// The alarm runs the index and delivery outboxes, due schedules,
    /// queued runs, and the poll backstop (which also checks running runs),
    /// then re-arms.
    async fn on_alarm(&self) -> CellResult<()> {
        if self.meta(MetaKey::CreatedAt)?.is_none() {
            return Ok(());
        }
        self.flush_index().await;
        if let Err(e) = self.seed().await {
            self.event("template.failed", &e.message, json!({ "code": e.code }));
        } else if let Err(e) = self.join_owners_agent().await {
            self.event("agent.join-failed", &e.message, json!({ "code": e.code }));
        }
        self.drain_deliveries().await;
        // Settles pending mutations that are due; one that fails waits for
        // its own next try and never fails the alarm.
        self.sweep_due().await?;
        self.fire_cron()?;
        self.launch_queued().await;
        let poll_at: i64 = self.meta(MetaKey::PollAt)?.and_then(|s| s.parse().ok()).unwrap_or(0);
        if poll_at <= js::now_ms() {
            self.trim_audit()?;
            self.trim_runs()?;
            self.trim_writes()?;
            self.poll().await;
            if let Err(e) = self.collect_blobs().await {
                self.event("blobs.collect-failed", &e.message, json!({ "code": e.code }));
            }
            self.reconcile_runs().await;
            self.release_held_videos().await;
            self.launch_queued().await;
            self.set_meta(MetaKey::PollAt, &(js::now_ms() + self.cfg.poll_interval_ms).to_string())?;
        }
        self.schedule().await
    }

    /// Arms the alarm for the earliest due work.
    pub(crate) async fn schedule(&self) -> CellResult<()> {
        self.arm(None, ALARM_SOON_MS).await
    }

    /// Arms the alarm for the earliest due work, or `at` if that is sooner.
    pub(crate) async fn schedule_by(&self, at: i64) -> CellResult<()> {
        self.arm(Some(at), ALARM_SOON_MS).await
    }

    /// Arms the alarm for the earliest due work (or `also`), but no sooner
    /// than `min_ms` from now.
    async fn arm(&self, also: Option<i64>, min_ms: i64) -> CellResult<()> {
        let [created_at, poll_at] = self.metas([MetaKey::CreatedAt, MetaKey::PollAt])?;
        if created_at.is_none() {
            return Ok(());
        }
        let poll_at: i64 = poll_at.and_then(|s| s.parse().ok()).unwrap_or_else(|| js::now_ms() + self.cfg.poll_interval_ms);
        let outbox = self.rows("SELECT MIN(next_at) AS at FROM index_outbox", vec![])?.first().and_then(|r| r["at"].as_i64());
        let due = [outbox, self.runs_due_at()?, self.pending_due_at()?, self.outbox_due_at()?, also];
        let at = due.into_iter().flatten().fold(poll_at, i64::min).max(js::now_ms() + min_ms);
        self.state.storage().set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(at as f64)))).await?;
        Ok(())
    }

    /// Closes the change feeds of one principal (a revoked member) or of
    /// every share-link holder (`view`).
    pub(crate) fn close_sockets(&self, tag: &str, reason: &str) {
        for ws in self.state.get_websockets_with_tag(tag) {
            let _ = ws.close(Some(4003), Some(reason));
        }
    }
}
