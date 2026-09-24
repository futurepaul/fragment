//! The `Fragment` supervisor: one Durable Object per fragment. It owns the
//! fragment's identity, members, visibility, secrets, file-plane pins, and
//! code record, and it answers every call into the app. The router has
//! already verified the caller and resolved their identity; it passes the
//! identity (with the key it signed with, its kind, and an agent's owner),
//! the fragment's name, and the URL the request arrived on in headers only
//! it sets.
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

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;

use fragment_core::access::{self, Decision, Purpose, Standing};
use fragment_core::npub;
use fragment_proto::{
    limits, valid_fragment_name, CodeStatus, Counts, CreateFragment, Created, ErrorCode, FragmentStatus, IdentityKind, OpDecl, Pins, Role,
    Urls, Visibility,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::*;

use crate::config::Config;
use crate::cs::Cs;
use crate::error::{CellError, CellResult};
use crate::js;

/// The caller's identity (`id:…`), the key it signed with, the identity's
/// kind, and an agent's owner.
pub const PRINCIPAL_HEADER: &str = "x-fragment-principal";
pub const KEY_HEADER: &str = "x-fragment-key";
pub const KIND_HEADER: &str = "x-fragment-kind";
pub const OWNER_HEADER: &str = "x-fragment-owner";
pub const NAME_HEADER: &str = "x-fragment-name";
pub const URL_HEADER: &str = "x-fragment-url";
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
  body TEXT NOT NULL, op TEXT, idx INTEGER, PRIMARY KEY (channel, seq));
CREATE UNIQUE INDEX IF NOT EXISTS records_effect ON records (op, idx) WHERE op IS NOT NULL;
CREATE TABLE IF NOT EXISTS tree (
  ref TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL, mode TEXT NOT NULL, last_commit TEXT NOT NULL,
  PRIMARY KEY (ref, path));
CREATE TABLE IF NOT EXISTS deliveries (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS code (
  id INTEGER PRIMARY KEY CHECK (id = 1), sha TEXT NOT NULL, loader_id TEXT NOT NULL, source TEXT NOT NULL,
  operations TEXT NOT NULL, cpu_ms INTEGER NOT NULL, installed_at INTEGER NOT NULL,
  channels TEXT NOT NULL DEFAULT '{}', modules TEXT NOT NULL DEFAULT '{}', triggers TEXT NOT NULL DEFAULT '[]',
  notify TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL, via TEXT NOT NULL, trigger TEXT, principal TEXT NOT NULL,
  role TEXT NOT NULL, depth INTEGER NOT NULL, call_id TEXT, input_sha TEXT, input TEXT NOT NULL, status TEXT NOT NULL,
  attempt INTEGER NOT NULL, output TEXT, error TEXT, created_at INTEGER NOT NULL, launched_at INTEGER, finished_at INTEGER);
CREATE UNIQUE INDEX IF NOT EXISTS runs_call ON runs (principal, call_id) WHERE call_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS runs_status ON runs (status, op);
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
    pub(crate) cfg: Config,
    /// Serializes pin refreshes: two refreshes racing could leave the older head pinned.
    pub(crate) plane: futures_util::lock::Mutex<()>,
    pub(crate) rate: RefCell<fragment_core::ratelimit::Rate>,
    /// Whether this activation has swept its pending mutations.
    pub(crate) swept: Cell<bool>,
    /// The ledger ids a call or a sweep is settling now (channels.rs).
    pub(crate) settling: RefCell<BTreeMap<String, Arc<futures_util::lock::Mutex<()>>>>,
}

impl DurableObject for FragmentCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        let sql = state.storage().sql();
        sql.exec(SCHEMA, None).expect("the Fragment schema applies");
        // a code table made before channels, applib, and triggers (phase 2 slices B and C)
        let cols: Vec<Value> = sql.exec("PRAGMA table_info(code)", None).and_then(|c| c.to_array()).unwrap_or_default();
        for (col, decl) in [
            ("channels", "channels TEXT NOT NULL DEFAULT '{}'"),
            ("modules", "modules TEXT NOT NULL DEFAULT '{}'"),
            ("triggers", "triggers TEXT NOT NULL DEFAULT '[]'"),
            ("notify", "notify TEXT NOT NULL DEFAULT '[]'"),
        ] {
            if !cols.iter().any(|c| c["name"] == col) {
                sql.exec(&format!("ALTER TABLE code ADD COLUMN {decl}"), None).expect("the code table migrates");
            }
        }
        let cols: Vec<Value> = sql.exec("PRAGMA table_info(members)", None).and_then(|c| c.to_array()).unwrap_or_default();
        for col in ["kind", "owner"] {
            if !cols.iter().any(|c| c["name"] == col) {
                sql.exec(&format!("ALTER TABLE members ADD COLUMN {col} TEXT"), None).expect("the members table migrates");
            }
        }
        // after the migration: a members table from before phase 4 has no
        // owner column until it runs (the index in SCHEMA broke those cells)
        sql.exec("CREATE INDEX IF NOT EXISTS members_owner ON members (owner) WHERE owner IS NOT NULL", None).expect("the members index applies");
        let paused_by_migration = crate::jobs::migrate_trigger_state(&sql, js::now_ms());
        let cfg = Config::from_env(&env);
        let rate = fragment_core::ratelimit::Rate::new(limits::PUBLIC_CALLS_PER_MIN, limits::PUBLIC_CALLS_PER_MIN_FRAGMENT);
        let cell = FragmentCell { state, raw, env, cfg, plane: futures_util::lock::Mutex::new(()), rate: RefCell::new(rate), swept: Cell::new(false), settling: RefCell::default() };
        if !paused_by_migration.is_empty() {
            let summary = format!("the stored pause list did not parse; paused every triggered operation: {}", paused_by_migration.join(", "));
            cell.event("op.paused", &summary, json!({ "ops": paused_by_migration, "by": "migration" }));
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
            if let Err(e) = self.live_message(&ws, &s) {
                ws.send_with_str(json!({ "type": "error", "message": e.message }).to_string())?;
            }
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

/// Who is calling, as the router established it.
pub struct Caller {
    /// The signer's identity (`id:…`), when the request was signed.
    pub principal: Option<String>,
    /// The key it signed with (64 hex).
    pub key: Option<String>,
    pub kind: Option<IdentityKind>,
    /// An agent's owner.
    pub owner: Option<String>,
    /// The URL the request arrived on (canonical URLs and cookies derive from it).
    pub url: url::Url,
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

    pub(crate) fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.sql().exec(q, binds)?;
        Ok(())
    }

    pub(crate) fn meta(&self, key: &str) -> CellResult<Option<String>> {
        let rows = self.rows("SELECT value FROM meta WHERE key = ?", vec![key.into()])?;
        Ok(rows.first().map(|r| r["value"].as_str().expect("meta.value is TEXT").to_string()))
    }

    pub(crate) fn set_meta(&self, key: &str, value: &str) -> CellResult<()> {
        self.exec("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![key.into(), value.into()])
    }

    pub(crate) fn del_meta(&self, key: &str) -> CellResult<()> {
        self.exec("DELETE FROM meta WHERE key = ?", vec![key.into()])
    }

    /// A meta value every created fragment has.
    pub(crate) fn must(&self, key: &str) -> CellResult<String> {
        self.meta(key)?.ok_or_else(|| CellError::host(format!("a created fragment has no {key}")))
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
        match self.meta("created_at")? {
            Some(_) => self.must("name"),
            None => Err(CellError::new(ErrorCode::NotFound, "no such fragment")),
        }
    }

    pub(crate) fn visibility(&self) -> CellResult<Visibility> {
        let v = self.must("visibility")?;
        Visibility::parse(&v).ok_or_else(|| CellError::host(format!("stored visibility {v:?}")))
    }

    pub(crate) fn member_role(&self, principal: &str) -> CellResult<Option<Role>> {
        let rows = self.rows("SELECT role FROM members WHERE principal = ?", vec![principal.into()])?;
        Ok(rows.first().and_then(|r| r["role"].as_str()).and_then(Role::parse))
    }

    /// What the caller brings: their membership, or an agent of theirs that
    /// is a member (they read what it reads).
    fn standing(&self, caller: &Caller, link: bool) -> CellResult<Standing> {
        let member = match &caller.principal {
            Some(p) => self.member_role(p)?,
            None => None,
        };
        let owns_member_agent = match (&caller.principal, member) {
            (Some(p), None) => !self.rows("SELECT principal FROM members WHERE owner = ? LIMIT 1", vec![p.as_str().into()])?.is_empty(),
            _ => false,
        };
        Ok(Standing { member, owns_member_agent, link, signed: caller.principal.is_some() })
    }

    /// Whether the caller sees the fragment as themselves (a member, or an
    /// agent's owner), not by its link or its visibility: their sockets are
    /// tagged `p:<id>` and close when that standing is revoked.
    pub(crate) fn has_standing(&self, caller: &Caller) -> CellResult<bool> {
        let s = self.standing(caller, false)?;
        Ok(s.member.is_some() || s.owns_member_agent)
    }

    /// The caller's role for reading, or the refusal: `needs` is the weakest
    /// role that may read.
    pub(crate) fn require(&self, caller: &Caller, link: bool, needs: Role) -> CellResult<Role> {
        self.decide(caller, link, needs, Purpose::Read)
    }

    /// The caller's role for acting (an operation): an agent's owner reads
    /// through it and never acts through it.
    pub(crate) fn require_to_act(&self, caller: &Caller, link: bool, needs: Role) -> CellResult<Role> {
        self.decide(caller, link, needs, Purpose::Act)
    }

    fn decide(&self, caller: &Caller, link: bool, needs: Role, purpose: Purpose) -> CellResult<Role> {
        self.name()?;
        let standing = self.standing(caller, link)?;
        let visibility = self.visibility()?;
        let why = match (needs, visibility) {
            _ if standing.owns_member_agent && purpose == Purpose::Act => {
                "you read this fragment through your agent's membership; acting here needs your own".to_string()
            }
            (Role::Public, Visibility::Link) => "this fragment is shared by link: open it with its share link (?view=)".to_string(),
            (Role::Public, Visibility::Members) => "this fragment is for its members only".to_string(),
            _ => format!("this needs the {} role", needs.as_str()),
        };
        match access::decide(visibility, standing, purpose, needs) {
            Decision::Allow(role) => Ok(role),
            Decision::Unauthenticated => Err(CellError::new(ErrorCode::Unauthenticated, format!("{why}; sign in or sign the request"))),
            Decision::Forbidden => Err(CellError::new(ErrorCode::Forbidden, why)),
        }
    }

    /// The signer's identity.
    pub(crate) fn caller_id<'a>(&self, caller: &'a Caller) -> CellResult<&'a str> {
        caller.principal.as_deref().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "sign the request"))
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        // Only the router reaches a Durable Object's fetch; it sets these.
        let principal = req.headers().get(PRINCIPAL_HEADER)?;
        let key = req.headers().get(KEY_HEADER)?;
        let kind = req.headers().get(KIND_HEADER)?.and_then(|k| IdentityKind::parse(&k));
        let owner = req.headers().get(OWNER_HEADER)?;
        let arrived =
            req.headers().get(URL_HEADER)?.and_then(|u| url::Url::parse(&u).ok()).ok_or_else(|| CellError::host("no URL from the router"))?;
        let routed_name = req.headers().get(NAME_HEADER)?.ok_or_else(|| CellError::host("no fragment name from the router"))?;
        if principal.is_some() && kind.is_none() {
            return Err(CellError::host("the router named an identity without its kind"));
        }
        let caller = Caller { principal, key, kind, owner, url: arrived };
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
            let bytes = req.bytes().await?;
            let body: Value = serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            // A Workflow from a deleted fragment's earlier life stops.
            if body["incarnation"].as_str() != self.meta("created_at")?.as_deref() {
                return json_response(&json!({ "stop": true }));
            }
            let answer = match step {
                "advance" => self.job_advance(body, bytes.len()).await?,
                "effect" => self.job_effect(body).await?,
                "finish" => self.job_finish(body)?,
                _ => return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
            };
            return json_response(&answer);
        }
        if path == "/deliver/report" {
            // Only the delivery consumer sets the header; the router never passes it.
            if req.headers().get(crate::deliveries::REPORT_HEADER)?.is_none() {
                return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}")));
            }
            let body: Value = body_json(&mut req).await?;
            return json_response(&self.delivery_report(&body)?);
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
        if let Some(rest) = path.strip_prefix("/serve/") {
            let rest = rest.to_string();
            return self.serve(req, &caller, &routed_name, &rest).await;
        }
        let url = req.url()?;
        let query = |k: &str| url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
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
        let claimed_at: i64 = self.meta("claimed_at")?.and_then(|s| s.parse().ok()).unwrap_or(0);
        if self.meta("created_at")?.is_some() || js::now_ms() - claimed_at < CLAIM_TTL_MS {
            return Err(CellError::new(ErrorCode::AlreadyExists, format!("fragment {} already exists", body.name)));
        }
        self.set_meta("name", &body.name)?;
        self.set_meta("claimed_at", &js::now_ms().to_string())?;
        // the fragment's own key is made by KEYS; its secret stays sealed there
        let made = async {
            let repo_name = fragment_proto::repo_name(&body.name).ok_or_else(|| CellError::invalid("a fragment's name is <label>.<username>"))?;
            let repo = Cs::new(cs_cfg, &self.env).ensure_repo(&repo_name).await?;
            let (pubkey, sealed) = crate::keys::nostr_keypair(&self.env).await?;
            Ok::<_, CellError>((repo, pubkey, sealed))
        };
        let (repo, fragment_pub, sealed) = match made.await {
            Ok(r) => r,
            Err(e) => {
                self.del_meta("name")?;
                self.del_meta("claimed_at")?;
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
            ("owner", owner.as_str()),
            ("npub", fragment_npub.as_str()),
            ("fragment_secret", sealed.as_str()),
            ("visibility", visibility.as_str()),
            ("view_token", view_token.as_str()),
            ("inbox_token", inbox_token.as_str()),
            ("webhook_secret", webhook_secret.as_str()),
            ("repo", repo.as_str()),
            ("index_version", "0"),
            ("poll_at", poll_at.as_str()),
            // written last: a fragment exists once it has created_at
            ("created_at", created_at.as_str()),
        ] {
            self.set_meta(k, v)?;
        }
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind, owner) VALUES (?, 'owner', ?, ?, ?, ?)",
            vec![
                owner.as_str().into(),
                owner.as_str().into(),
                SqlStorageValue::Integer(now),
                caller.kind.map_or(SqlStorageValue::Null, |k| k.as_str().into()),
                caller.owner.as_deref().map_or(SqlStorageValue::Null, |o| o.into()),
            ],
        )?;
        self.index_change(&owner, Some(Role::Owner))?;
        if let Some(t) = &body.template {
            self.set_meta("template_pending", t)?;
        }
        self.event("create", &format!("fragment {} created by {owner} (repo {repo})", body.name), json!({ "repo": repo, "key": caller.key.as_deref().map(npub::display) }));
        self.flush_index().await;
        self.certificate().await?;
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
        let rows = self.rows("SELECT sha, operations FROM code WHERE id = 1", vec![])?;
        let (sha, operations) = match rows.first() {
            Some(r) => (
                r["sha"].as_str().map(str::to_string),
                serde_json::from_str::<std::collections::BTreeMap<String, OpDecl>>(r["operations"].as_str().unwrap_or("{}")).unwrap_or_default(),
            ),
            None => (None, Default::default()),
        };
        Ok(CodeStatus { sha, operations, error: self.meta("code_error")? })
    }

    fn status(&self, caller: &Caller) -> CellResult<Response> {
        let role = self.require(caller, false, Role::Viewer)?;
        let name = self.name()?;
        json_response(&FragmentStatus {
            npub: self.must("npub")?,
            owner: npub::display(&self.must("owner")?),
            role,
            visibility: self.visibility()?,
            repo: self.must("repo")?,
            pins: Pins { main: self.meta("pin_main")?, live: self.meta("pin_live")? },
            counts: Counts {
                files: self.count("SELECT COUNT(*) AS n FROM tree WHERE ref = 'main'")?,
                events: self.count("SELECT COUNT(*) AS n FROM records WHERE channel = 'events'")?,
                members: self.count("SELECT COUNT(*) AS n FROM members")?,
            },
            code: self.code_status()?,
            view_token: Some(self.must("view_token")?),
            inbox_token: if role >= Role::Editor { Some(self.must("inbox_token")?) } else { None },
            urls: Urls { canonical: self.cfg.canonical(&caller.url, &name) },
            blob_min_bytes: Some(fragment_core::blob::BLOB_MIN_BYTES as u64),
            name,
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
        let events: Vec<Value> = self
            .read_channel("events", after, limit)?
            .into_iter()
            .map(|r| json!({ "id": r.seq, "at": r.at, "kind": r.kind, "summary": r.body["summary"], "data": r.body["data"] }))
            .collect();
        json_response(&json!({ "events": events }))
    }

    /// The alarm runs the index and delivery outboxes, due schedules,
    /// queued runs, and the poll backstop (which also checks running runs),
    /// then re-arms.
    async fn on_alarm(&self) -> CellResult<()> {
        if self.meta("created_at")?.is_none() {
            return Ok(());
        }
        self.flush_index().await;
        if self.meta("cert_pending")?.is_some() {
            self.certificate().await?;
        }
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
        let poll_at: i64 = self.meta("poll_at")?.and_then(|s| s.parse().ok()).unwrap_or(0);
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
            self.set_meta("poll_at", &(js::now_ms() + self.cfg.poll_interval_ms).to_string())?;
        }
        self.schedule().await
    }

    /// Asks for the certificate of this fragment's host (decision 16: one
    /// per host, from Fly), once; a failure is retried at the next alarm.
    async fn certificate(&self) -> CellResult<()> {
        let name = self.name()?;
        let outcome = crate::keys::fly_certificate(&self.env, &name).await;
        let asked = match &outcome {
            Ok(None) => {
                self.event("certificate.skipped", "this node asks no one for certificates (no Fly)", json!({}));
                true
            }
            // Fly answers a host it already has with an error that says so
            Ok(Some((status, body))) if (200..300).contains(status) || body.to_string().contains("already") => {
                self.event("certificate.requested", &format!("asked Fly for {name}'s certificate"), json!({ "status": status }));
                true
            }
            Ok(Some((status, body))) => {
                self.event("certificate.failed", &format!("Fly answered {status}: {body}"), json!({ "status": status }));
                false
            }
            Err(e) => {
                self.event("certificate.failed", &e.message, json!({ "code": e.code }));
                false
            }
        };
        if asked { self.del_meta("cert_pending") } else { self.set_meta("cert_pending", "1") }
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
        if self.meta("created_at")?.is_none() {
            return Ok(());
        }
        let poll_at: i64 = self.meta("poll_at")?.and_then(|s| s.parse().ok()).unwrap_or_else(|| js::now_ms() + self.cfg.poll_interval_ms);
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
