//! The `Fragment` supervisor: one Durable Object per fragment. It owns the
//! fragment's identity, members, visibility, secrets, file-plane pins, and
//! code record, and it answers every call into the app. The router has
//! already verified the caller; it passes the principal, the fragment's
//! name, and the URL the request arrived on in headers only it sets.
//!
//! Routes (inner paths; the router maps the public ones onto them):
//!
//!   POST   /create                        create (the signer owns it)
//!   DELETE /delete                        delete (owner)
//!   GET    /api/status                    viewer
//!   GET    /api/manifest                  viewer: fragment.json at main
//!   GET    /api/members                   viewer
//!   PUT    /api/members/<npub>            owner
//!   DELETE /api/members/<npub>            owner, or the member themselves
//!   POST   /api/invites  GET /api/invites  DELETE /api/invites/<id>   owner
//!   POST   /api/join                      any signed principal with a token
//!   PUT    /api/visibility                owner
//!   POST   /api/rotate                    owner
//!   PUT    /api/secrets/<KEY>  GET /api/secrets  DELETE /api/secrets/<KEY>   editor
//!   GET    /api/storage-token             editor
//!   POST   /api/refresh                   editor
//!   POST   /api/webhook                   code.storage (HMAC)
//!   GET    /api/files  /api/file?path=  /api/file/stat?path=   viewer
//!   GET    /api/events?since=             viewer
//!   POST   /api/ops/<operation>           the operation's role
//!   *      /serve/<path>                  the site, `__tree`, `__file`, `__op`, `__watch`

use std::cell::RefCell;

use fragment_core::access::{self, Decision};
use fragment_core::{npub, secrets};
use fragment_proto::{
    limits, valid_fragment_name, CodeStatus, Counts, CreateFragment, Created, ErrorCode, FragmentStatus, OpDecl, Pins, Role, Urls,
    Visibility,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::*;

use crate::config::Config;
use crate::cs::Cs;
use crate::error::{CellError, CellResult};
use crate::js;

pub const PRINCIPAL_HEADER: &str = "x-fragment-principal";
pub const NAME_HEADER: &str = "x-fragment-name";
pub const URL_HEADER: &str = "x-fragment-url";
/// How long a create in progress holds its name.
const CLAIM_TTL_MS: i64 = 120_000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS members (
  principal TEXT PRIMARY KEY, role TEXT NOT NULL, added_by TEXT NOT NULL, added_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS invites (
  id TEXT PRIMARY KEY, token_sha TEXT NOT NULL UNIQUE, role TEXT NOT NULL, uses_left INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, created_by TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS index_outbox (
  principal TEXT PRIMARY KEY, role TEXT, version INTEGER NOT NULL, attempts INTEGER NOT NULL, next_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS secrets (
  name TEXT PRIMARY KEY, sealed TEXT NOT NULL, set_by TEXT NOT NULL, set_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, kind TEXT NOT NULL, summary TEXT NOT NULL, data TEXT);
CREATE TABLE IF NOT EXISTS tree (
  ref TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL, mode TEXT NOT NULL, last_commit TEXT NOT NULL,
  PRIMARY KEY (ref, path));
CREATE TABLE IF NOT EXISTS deliveries (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS code (
  id INTEGER PRIMARY KEY CHECK (id = 1), sha TEXT NOT NULL, loader_id TEXT NOT NULL, source TEXT NOT NULL,
  operations TEXT NOT NULL, cpu_ms INTEGER NOT NULL, installed_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ops (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, principal TEXT NOT NULL, input_sha TEXT NOT NULL, result TEXT NOT NULL,
  at INTEGER NOT NULL);
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
}

impl DurableObject for FragmentCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        state.storage().sql().exec(SCHEMA, None).expect("the Fragment schema applies");
        let cfg = Config::from_env(&env);
        FragmentCell { state, raw, env, cfg, plane: futures_util::lock::Mutex::new(()), rate: RefCell::new(fragment_core::ratelimit::Rate::new(limits::PUBLIC_CALLS_PER_MIN, limits::PUBLIC_CALLS_PER_MIN_FRAGMENT)) }
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
            let _ = self.schedule().await;
        }
        Response::ok("")
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
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
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

/// Who is calling, as the router established it.
pub struct Caller {
    /// A verified key (64 hex), when the request was signed.
    pub principal: Option<String>,
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

    /// Appends to the audit trail (the `events` channel from slice C).
    pub(crate) fn event(&self, kind: &str, summary: &str, data: Value) {
        let summary: String = summary.chars().take(500).collect();
        let data = if data.is_null() { SqlStorageValue::Null } else { data.to_string().into() };
        let _ = self.exec("INSERT INTO events (at, kind, summary, data) VALUES (?, ?, ?, ?)", vec![SqlStorageValue::Integer(js::now_ms()), kind.into(), summary.into(), data]);
        let _ = self.exec("DELETE FROM events WHERE id <= (SELECT COALESCE(MAX(id), 0) - ? FROM events)", vec![SqlStorageValue::Integer(limits::EVENTS_KEPT)]);
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

    /// The caller's role, or the refusal: `needs` is the weakest role that may act.
    pub(crate) fn require(&self, caller: &Caller, link: bool, needs: Role) -> CellResult<Role> {
        self.name()?;
        let member = match &caller.principal {
            Some(p) => self.member_role(p)?,
            None => None,
        };
        let visibility = self.visibility()?;
        let why = match (needs, visibility) {
            (Role::Public, Visibility::Link) => "this fragment is shared by link: open it with its share link (?view=)".to_string(),
            (Role::Public, Visibility::Members) => "this fragment is for its members only".to_string(),
            _ => format!("this needs the {} role", needs.as_str()),
        };
        match access::decide(visibility, member, link, caller.principal.is_some(), needs) {
            Decision::Allow(role) => Ok(role),
            Decision::Unauthenticated => Err(CellError::new(ErrorCode::Unauthenticated, format!("{why}; sign in or sign the request"))),
            Decision::Forbidden => Err(CellError::new(ErrorCode::Forbidden, why)),
        }
    }

    pub(crate) fn caller_hex<'a>(&self, caller: &'a Caller) -> CellResult<&'a str> {
        caller.principal.as_deref().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "sign the request"))
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        // Only the router reaches a Durable Object's fetch; it sets these.
        let principal = req.headers().get(PRINCIPAL_HEADER)?;
        let arrived =
            req.headers().get(URL_HEADER)?.and_then(|u| url::Url::parse(&u).ok()).ok_or_else(|| CellError::host("no URL from the router"))?;
        let routed_name = req.headers().get(NAME_HEADER)?.ok_or_else(|| CellError::host("no fragment name from the router"))?;
        let caller = Caller { principal, url: arrived };
        let path = req.path();
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
                self.put_secret(&caller, key, bytes)
            }
            (Method::Get, ["api", "secrets"]) => self.list_secrets(&caller),
            (Method::Delete, ["api", "secrets", key]) => self.delete_secret(&caller, key),
            (Method::Get, ["api", "storage-token"]) => self.storage_token(&caller),
            (Method::Post, ["api", "refresh"]) => self.refresh(&caller).await,
            (Method::Post, ["api", "webhook"]) => {
                let event = req.headers().get("x-pierre-event")?.unwrap_or_default();
                let signature = req.headers().get("x-pierre-signature")?.unwrap_or_default();
                let body = req.bytes().await?;
                self.webhook(&event, &signature, &body).await
            }
            (Method::Get, ["api", "files"]) => self.files(&caller).await,
            (Method::Get, ["api", "file"]) => self.file(&caller, &query("path").unwrap_or_default()).await,
            (Method::Get, ["api", "file", "stat"]) => self.stat(&caller, &query("path").unwrap_or_default()).await,
            (Method::Get, ["api", "events"]) => self.events(&caller, query("since").and_then(|s| s.parse().ok()).unwrap_or(0)),
            (Method::Post, ["api", "ops", op]) => {
                let body = body_json(&mut req).await?;
                let op = op.to_string();
                self.api_op(&caller, &op, body).await
            }
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {path}", req.method().as_ref()))),
        }
    }

    async fn create(&self, caller: &Caller, body: CreateFragment) -> CellResult<Response> {
        let owner = self.caller_hex(caller)?.to_string();
        if !valid_fragment_name(&body.name) {
            return Err(CellError::invalid("a fragment name must match ^[a-z0-9][a-z0-9-]{0,62}$"));
        }
        let fragment_pub = fragment_nip98::pubkey_of_secret(&body.fragment_secret).ok_or_else(|| {
            CellError::invalid("fragmentSecret must be the fragment's secp256k1 secret key as 64 hex characters, generated by the client")
        })?;
        let host_secrets = self.cfg.host_secrets()?;
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
        let repo = match Cs::new(cs_cfg).ensure_repo(&body.name).await {
            Ok(r) => r,
            Err(e) => {
                self.del_meta("name")?;
                self.del_meta("claimed_at")?;
                return Err(e);
            }
        };
        let now = js::now_ms();
        let fragment_npub = npub::encode(&fragment_pub);
        let sealed = secrets::seal(host_secrets[0], &fragment_npub, body.fragment_secret.as_bytes(), js::random_bytes())
            .map_err(|e| CellError::host(e.to_string()))?;
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
            "INSERT INTO members (principal, role, added_by, added_at) VALUES (?, 'owner', ?, ?)",
            vec![owner.as_str().into(), owner.as_str().into(), SqlStorageValue::Integer(now)],
        )?;
        self.index_change(&owner, Some(Role::Owner))?;
        self.event("create", &format!("fragment {} created by {} (repo {repo})", body.name, npub::encode(&owner)), json!({ "repo": repo }));
        self.flush_index().await;
        self.schedule().await?;
        json_response(&Created {
            name: body.name.clone(),
            npub: fragment_npub,
            owner: npub::encode(&owner),
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
            owner: npub::encode(&self.must("owner")?),
            role,
            visibility: self.visibility()?,
            repo: self.must("repo")?,
            pins: Pins { main: self.meta("pin_main")?, live: self.meta("pin_live")? },
            counts: Counts {
                files: self.count("SELECT COUNT(*) AS n FROM tree WHERE ref = 'main'")?,
                events: self.count("SELECT COUNT(*) AS n FROM events")?,
                members: self.count("SELECT COUNT(*) AS n FROM members")?,
            },
            code: self.code_status()?,
            view_token: Some(self.must("view_token")?),
            inbox_token: if role >= Role::Editor { Some(self.must("inbox_token")?) } else { None },
            urls: Urls { canonical: self.cfg.canonical(&caller.url, &name) },
            name,
        })
    }

    fn events(&self, caller: &Caller, since: i64) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let rows = self.rows(
            "SELECT id, at, kind, summary, data FROM events WHERE id > ? ORDER BY id LIMIT ?",
            vec![SqlStorageValue::Integer(since), SqlStorageValue::Integer(limits::EVENTS_PAGE as i64)],
        )?;
        let events: Vec<Value> = rows
            .into_iter()
            .map(|mut r| {
                r["data"] = r["data"].as_str().and_then(|d| serde_json::from_str(d).ok()).unwrap_or(Value::Null);
                r
            })
            .collect();
        json_response(&json!({ "events": events }))
    }

    /// The alarm runs the index outbox and the poll backstop, then re-arms.
    async fn on_alarm(&self) -> CellResult<()> {
        if self.meta("created_at")?.is_none() {
            return Ok(());
        }
        self.flush_index().await;
        let poll_at: i64 = self.meta("poll_at")?.and_then(|s| s.parse().ok()).unwrap_or(0);
        if poll_at <= js::now_ms() {
            self.poll().await;
            self.set_meta("poll_at", &(js::now_ms() + self.cfg.poll_interval_ms).to_string())?;
        }
        self.schedule().await
    }

    /// Arms the alarm for the earliest due work.
    pub(crate) async fn schedule(&self) -> CellResult<()> {
        if self.meta("created_at")?.is_none() {
            return Ok(());
        }
        let poll_at: i64 = self.meta("poll_at")?.and_then(|s| s.parse().ok()).unwrap_or_else(|| js::now_ms() + self.cfg.poll_interval_ms);
        let outbox = self.rows("SELECT MIN(next_at) AS at FROM index_outbox", vec![])?.first().and_then(|r| r["at"].as_i64());
        let at = outbox.map_or(poll_at, |o| o.min(poll_at)).max(js::now_ms() + 50);
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
