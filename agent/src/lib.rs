//! Agents on celld (ROADMAP phase 5): a Durable Object per agent, with its
//! own key, its conversation in SQL, and goose's loop (`goose-agent`) as its
//! turn. Its tools are the operations of the fragments it belongs to, which
//! it calls through the platform's signed API with its own key: an agent
//! acts on a fragment exactly as its membership lets it.
//!
//! An agent is an identity in the platform's registry, owned by the person
//! who made it (docs/finite-integration.md). This script is co-hosted in
//! the platform's fleet and has no ingress of its own (docs/phase-6.md,
//! step 4): the platform's router authenticates each request, as it does
//! its own, and names the caller's identity in `x-agent-principal`, which
//! is all this script trusts. Making an agent makes its key here; the
//! router then registers it as its maker's in the same request. An agent's
//! name is `<label>.<username>`, its owner's username.
//!
//! The owner's API (through the platform; the router checks the signature):
//!   POST /api/agents                  {name, model?, instructions?} → {name, npub, model}
//!                                     (again, by its owner: the same answer, `replayed`)
//!   GET  /api/a/{name}                the conversation and the turn's state
//!   GET  /api/a/{name}/state?wait_ms= the turn's state alone, once the turn ends (or wait_ms, at most 25 s)
//!   POST /api/a/{name}/turns          {text}: start a turn, or steer the running one
//!   POST /api/a/{name}/stop           stop the running turn
//!   GET  /api/a/{name}/tools          the tools its memberships give it now
//!   POST /api/a/{name}/listen         {fragment, channel? ("chat"), reply? ("say")}: follow a channel (once: again is the same listen)
//!   PUT  /api/a/{name}/computer       {url, token, cwd? ("work")}: attach a computer (`fragment computer serve`)
//!   DELETE /api/a/{name}/computer     detach it
//!   POST /api/a/{name}/test           test controls (dev fleets: AGENT_TEST_HOOKS=allow)
//!
//! A listened-to channel's records arrive at `POST /api/a/{name}/inbox/{token}`
//! (the fragment's delivery; the token is the capability). A record from
//! someone else starts a turn (or steers the running one), and the turn's
//! last answer goes back through the fragment's reply operation.

mod computer;
mod fleet;
mod js;
mod keys;
mod store;
mod tools;
mod turn;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fragment_core::body::{LimitedBody, TooLarge};
use fragment_core::npub;
use fragment_proto::limits::AGENT_STATE_WAIT_MS_MAX;
use fragment_proto::{valid_channel_name, valid_fragment_name, valid_op_name, AgentState, Delivery, ErrorCode, IdentityView, TurnOutcome};
use futures::TryStreamExt;
use goose_provider_types::conversation::message::{Message, MessageContent};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use worker::wasm_bindgen;
use worker::{durable_object, event, DurableObject, Env, Headers, Method, Request, RequestInit, Response, State};

use crate::computer::Computer;
use crate::fleet::Fleet;
use crate::store::{kv_get, kv_set, kv_u64, last_answer, recent_messages};
use crate::turn::{Attached, Driver, Model, WATCHDOG_MS_DEFAULT, WATCHDOG_MS_MIN};

const PRINCIPAL_HEADER: &str = fragment_proto::routed::AGENT_PRINCIPAL;
/// A computer that connects out presents its connect token here.
const COMPUTER_TOKEN_HEADER: &str = "x-computer-token";
/// A computer's long poll waits this long, and a request it fetched but
/// never answered is handed out again after this.
const POLL_WAIT_MS: u64 = 25_000;
const RESEND_MS: i64 = 40_000;
/// A computer's answer (a screenshot inside) is at most this.
const ANSWER_BODY_MAX: usize = 6 * 1024 * 1024;
const MESSAGE_TEXT_MAX: usize = 16 * 1024;
const MODEL_MAX: usize = 128;
const INSTRUCTIONS_MAX: usize = 8 * 1024;
const TEST_HOLD_MS_MAX: u64 = 60_000;
const BODY_MAX: usize = 64 * 1024;
/// The channels one agent follows.
const LISTENS_MAX: usize = 16;
/// The platform's default model (ROADMAP decision 7).
const DEFAULT_MODEL: &str = "z-ai/glm-5.3-flash";
/// A computer's URL, and its token.
const COMPUTER_URL_MAX: usize = 1024;
const COMPUTER_TOKEN_MAX: usize = 256;
/// The newest rows of each list the owner's view shows (messages, steers,
/// tool runs, steps): the view grew with the agent's age.
const VIEW_ROWS_MAX: usize = fragment_core::history::WINDOW_MESSAGES_MAX;
/// How often a state read that waits looks at the turn again (this cell's
/// own SQL), and so how many looks one read takes at most.
const STATE_CHECK_MS: u64 = 250;
const STATE_CHECKS_MAX: u64 = AGENT_STATE_WAIT_MS_MAX / STATE_CHECK_MS + 1;
/// A record heard is remembered this long, past the longest a delivery is
/// redelivered (its outbox's 20 tries over about 2.5 hours, then the
/// queue's 5 retries at most an hour apart), then forgotten.
const HEARD_KEEP_MS: i64 = 24 * 3600 * 1000;

// ---------------------------------------------------------------- errors

struct Fail {
    code: ErrorCode,
    message: String,
}

impl Fail {
    fn new(code: ErrorCode, message: impl Into<String>) -> Fail {
        Fail { code, message: message.into() }
    }
    fn invalid(message: impl Into<String>) -> Fail {
        Fail::new(ErrorCode::InvalidRequest, message)
    }
    fn host(message: impl std::fmt::Display) -> Fail {
        Fail::new(ErrorCode::HostFailed, message.to_string())
    }
    fn response(self) -> worker::Result<Response> {
        Ok(Response::from_json(&json!({ "error": self.code, "message": self.message }))?.with_status(self.code.status()))
    }
}

impl From<worker::Error> for Fail {
    fn from(e: worker::Error) -> Fail {
        Fail::host(e)
    }
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Fail {
        Fail::host(e)
    }
}

type Answer<T> = Result<T, Fail>;

// ---------------------------------------------------------------- the router

/// Hands a request to an agent's cell, as `principal` (none for an inbox delivery).
async fn forward(env: &Env, name: &str, action: &str, method: Method, principal: Option<&str>, body: Vec<u8>) -> Answer<Response> {
    forward_with(env, name, action, method, principal.map(|p| (PRINCIPAL_HEADER, p)), body).await
}

/// `forward`, with one header of the caller's (who, or a computer's token).
async fn forward_with(env: &Env, name: &str, action: &str, method: Method, header: Option<(&str, &str)>, body: Vec<u8>) -> Answer<Response> {
    let headers = Headers::new();
    if let Some((k, v)) = header {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if !body.is_empty() {
        init.with_body(Some(worker::js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let inner = Request::new_with_init(&format!("https://agent.internal/{action}"), &init)?;
    let stub = env.durable_object("AGENT")?.id_from_name(name)?.get_stub()?;
    Ok(stub.fetch_with_request(inner).await?)
}

/// A request body of at most `max` bytes, read as it arrives: a declared
/// length over it is refused unread, and a body without one (chunked) at
/// the chunk that crosses it, before anyone is authenticated.
async fn read_body(req: &mut Request, max: usize) -> Answer<Vec<u8>> {
    let too_large = |_: TooLarge| Fail::new(ErrorCode::TooLarge, format!("a request body is at most {max} bytes"));
    let declared: Option<usize> = req.headers().get("content-length")?.and_then(|l| l.parse().ok());
    let mut body = LimitedBody::new(max, declared).map_err(too_large)?;
    if req.inner().body().is_none() {
        return Ok(body.finish());
    }
    let mut stream = req.stream()?;
    // bounded: LimitedBody refuses the chunk that would cross `max`, and the read stops
    while let Some(chunk) = stream.try_next().await? {
        body.push(&chunk).map_err(too_large)?;
    }
    Ok(body.finish())
}

async fn route(mut req: Request, env: &Env) -> Answer<Response> {
    let url = fragment_nip98::arrived_url(req.url()?, req.headers().get("x-forwarded-proto")?.as_deref());
    let path = url.path().to_string();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if req.method() == Method::Get && segments == ["healthz"] {
        return Ok(Response::ok("ok")?);
    }
    // a fragment's delivery to a listening agent: unsigned, the token is the capability
    if let (Method::Post, ["api", "a", name, "inbox", token]) = (req.method(), segments.as_slice()) {
        if !valid_fragment_name(name) || token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Fail::new(ErrorCode::NotFound, "no such inbox"));
        }
        let (name, action) = (name.to_string(), format!("inbox/{token}"));
        let body = read_body(&mut req, BODY_MAX).await?;
        return forward(env, &name, &action, Method::Post, None, body).await;
    }
    // a computer that connects out: its connect token is the capability
    if let (Method::Post, ["api", "a", name, "computer", op @ ("poll" | "answer")]) = (req.method(), segments.as_slice()) {
        if !valid_fragment_name(name) {
            return Err(Fail::new(ErrorCode::NotFound, "no such agent"));
        }
        let token = req.headers().get(COMPUTER_TOKEN_HEADER)?.unwrap_or_default();
        let body = read_body(&mut req, ANSWER_BODY_MAX).await?;
        return forward_with(env, name, &format!("computer/{op}"), Method::Post, Some((COMPUTER_TOKEN_HEADER, &token)), body).await;
    }
    // the platform's router, the only way in, names who is calling
    let principal = req.headers().get(PRINCIPAL_HEADER)?.filter(|p| npub::is_identity(p)).ok_or_else(|| Fail::new(ErrorCode::Unauthenticated, "reach agents through the platform"))?;
    let (name, action) = match (req.method(), segments.as_slice()) {
        (Method::Post, ["api", "agents"]) => (None, "create".to_string()),
        (_, ["api", "a", name]) => (Some(name.to_string()), "view".to_string()),
        (_, ["api", "a", name, action]) => (Some(name.to_string()), action.to_string()),
        _ => return Err(Fail::new(ErrorCode::NotFound, format!("no route {} {path}", req.method().as_ref()))),
    };
    let body = read_body(&mut req, BODY_MAX).await?;
    let name = match name {
        Some(n) => n,
        None => serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["name"].as_str().map(str::to_string)).unwrap_or_default(),
    };
    if !valid_fragment_name(&name) {
        return Err(Fail::invalid("an agent's name is <label>.<username>"));
    }
    // the query rides along (a state read's wait_ms)
    let action = match url.query() {
        Some(query) => format!("{action}?{query}"),
        None => action,
    };
    forward(env, &name, &action, req.method(), Some(&principal), body).await
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: worker::Context) -> worker::Result<Response> {
    match route(req, &env).await {
        Ok(r) => Ok(r),
        Err(f) => f.response(),
    }
}

// ---------------------------------------------------------------- the agent

#[durable_object(alarm)]
pub struct Agent {
    state: State,
    env: Env,
    booted_at: u64,
    driving: Rc<Cell<bool>>,
    cancel: Rc<RefCell<Option<CancellationToken>>>,
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    model: Option<String>,
    instructions: Option<String>,
}

#[derive(Deserialize)]
struct TurnBody {
    text: String,
}

#[derive(Deserialize)]
struct ListenBody {
    fragment: String,
    channel: Option<String>,
    reply: Option<String>,
}

#[derive(Deserialize)]
struct ComputerBody {
    url: Option<String>,
    token: Option<String>,
    /// A computer that connects out: no URL, a connect token made here.
    #[serde(default)]
    connect: bool,
    cwd: Option<String>,
}

#[derive(Deserialize, Default)]
struct TestControls {
    hold_in_tool_ms: Option<u64>,
    hold_after_tool_ms: Option<u64>,
    watchdog_ms: Option<u64>,
    /// A smaller conversation window, so a test can outgrow it in a few turns.
    window_messages: Option<u64>,
    /// Fewer rows in the owner's view, so a test can outgrow it.
    view_rows: Option<u64>,
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

fn unanswered(why: computer::Unanswered) -> Fail {
    match why {
        computer::Unanswered::Refused => Fail::invalid("the computer refused this token (it is in the file `fragment computer serve --token-file` names)"),
        computer::Unanswered::Unreachable(e) => Fail::new(ErrorCode::UpstreamFailed, format!("the computer did not answer: {e}")),
    }
}

/// The model key a turn spends: the owner's org key, whose limit is their
/// month's allowance (the platform's `Ledger` mints it; OpenRouter stops
/// the owner there). Kept only for the turn, never stored.
async fn owners_key(fleet: &Fleet) -> anyhow::Result<String> {
    let (status, answer) = fleet.call(Method::Post, "/api/budget/key", None).await?;
    match (status, answer["key"].as_str()) {
        (200, Some(key)) if !key.is_empty() => Ok(key.to_string()),
        _ => Err(anyhow::anyhow!("no model key from the owner's budget ({status}): {}", fleet::message(&answer))),
    }
}

/// Told to the model on every turn, after the agent's instructions: those
/// are stored when the agent is made, so a line added to
/// `default_instructions` would never reach the agents made before it. The
/// chat's reply operation is not among its tools (tools.rs `answer_ops`).
const ANSWER_POSTED: &str = "Your answer to a chat message is posted to that chat for you.";

fn default_instructions(name: &str) -> String {
    let name = name.split('.').next().unwrap_or(name);
    format!(
        "You are {name}, an agent. Most of your tools are operations of the fragments you belong to: shared places such \
         as an app, a list, or a chat. The platform__ tools make new fragments for your owner and change their files: \
         when asked for an app, make one, read the todo template's files for the shape, write yours, deploy it, and say \
         where it is. Do what you are asked, one call at a time, and when the work is done answer in one short sentence."
    )
}

impl Agent {
    fn sql(&self) -> worker::SqlStorage {
        self.state.storage().sql()
    }

    /// The agent's identity and its owner's, as the registry holds them
    /// (learned once: neither ever changes).
    async fn registration(&self) -> Answer<(String, String)> {
        let sql = self.sql();
        if kv_get(&sql, "created_at")?.is_none() {
            return Err(Fail::new(ErrorCode::NotFound, "no such agent"));
        }
        if let (Some(id), Some(owner)) = (kv_get(&sql, "identity")?.filter(|v| !v.is_empty()), kv_get(&sql, "owner")?.filter(|v| !v.is_empty())) {
            return Ok((id, owner));
        }
        let (status, me) = self.fleet()?.call(Method::Get, "/api/identities/me", None).await.map_err(|e| Fail::new(ErrorCode::RegistryUnavailable, e.to_string()))?;
        match status {
            200 => {
                let me = IdentityView::deserialize(&me).map_err(|e| Fail::host(format!("the registry's answer for this agent: {e}")))?;
                let Some(owner) = me.owner else {
                    return Err(Fail::host("the registry named no owner for this agent"));
                };
                kv_set(&sql, "identity", &me.id)?;
                kv_set(&sql, "owner", &owner)?;
                Ok((me.id, owner))
            }
            401 => {
                let name = kv_get(&sql, "name")?.unwrap_or_default();
                Err(Fail::new(ErrorCode::InvalidRequest, format!("agent {name} is not registered yet: make it again (`fragment agent create {name}`) to finish")))
            }
            _ => Err(Fail::new(ErrorCode::RegistryUnavailable, format!("the platform answered {status}: {}", fleet::message(&me)))),
        }
    }

    /// Whether `principal` (the identity the router resolved) is the
    /// agent's owner.
    async fn require_owner(&self, principal: &str) -> Answer<()> {
        let (_, owner) = self.registration().await?;
        if principal != owner {
            return Err(Fail::new(ErrorCode::Forbidden, "only the agent's owner may do that"));
        }
        Ok(())
    }

    fn fleet(&self) -> Answer<Fleet> {
        Ok(Fleet { base: fleet::base(&self.env)?, signer: fleet::Signer { env: self.env.clone(), sql: self.sql() } })
    }

    /// The attached computer, its token opened by `KEYS` (sealed for this
    /// agent cell alone).
    async fn computer(&self) -> Answer<Option<Attached>> {
        open_computer(&self.env, &self.sql()).await.map_err(Fail::host)
    }

    /// Attaches a computer once it answers with this token: its tools join
    /// the agent's next turns.
    async fn attach(&self, body: ComputerBody) -> Answer<Value> {
        if body.connect {
            return self.attach_connecting(body.cwd);
        }
        let (Some(url), Some(token)) = (body.url, body.token) else { return Err(Fail::invalid("a computer has a url and a token, or connects out (connect: true)")) };
        let body = ComputerBody { url: Some(url.clone()), token: Some(token.clone()), connect: false, cwd: body.cwd };
        let url = url.trim().trim_end_matches('/').to_string();
        if url.len() > COMPUTER_URL_MAX {
            return Err(Fail::invalid(format!("a computer URL is at most {COMPUTER_URL_MAX} bytes")));
        }
        let local = var(&self.env, "FRAGMENT_EGRESS_LOCAL").as_deref() == Some("allow");
        fragment_core::egress::check(&url, local).map_err(|e| Fail::invalid(format!("url: {e}")))?;
        let token = body.token.unwrap_or_default();
        if token.len() < 16 || token.len() > COMPUTER_TOKEN_MAX {
            return Err(Fail::invalid(format!("a computer token is 16-{COMPUTER_TOKEN_MAX} bytes")));
        }
        let cwd = body.cwd.unwrap_or_else(|| "work".into());
        if !computer::valid_cwd(&cwd) {
            return Err(Fail::invalid(format!("cwd is 1-{} of [a-z0-9-]", computer::CWD_MAX)));
        }
        let c = Computer { url: url.clone(), token: token.clone(), tunnel: None };
        let manifest = c.check().await.map_err(unanswered)?;
        let tools: Vec<String> = computer::tools_of(&manifest)?.iter().map(|t| t.name.to_string()).collect();
        let sealed = keys::seal(&self.env, token.as_bytes()).await.map_err(Fail::host)?;
        let sql = self.sql();
        kv_set(&sql, "computer_url", &url)?;
        kv_set(&sql, "computer_token", sealed)?;
        kv_set(&sql, "computer_cwd", &cwd)?;
        Ok(json!({ "url": url, "cwd": cwd, "tools": tools }))
    }

    /// Attaches a computer that connects out: a new connect token (only its
    /// hash is kept), answered once, for `fragment computer connect`.
    fn attach_connecting(&self, cwd: Option<String>) -> Answer<Value> {
        let cwd = cwd.unwrap_or_else(|| "work".into());
        if !computer::valid_cwd(&cwd) {
            return Err(Fail::invalid(format!("cwd is 1-{} of [a-z0-9-]", computer::CWD_MAX)));
        }
        let token = hex::encode(js::random_bytes::<32>());
        let sql = self.sql();
        kv_set(&sql, "computer_url", computer::CONNECTS)?;
        kv_set(&sql, "computer_token", hex::encode(<sha2::Sha256 as sha2::Digest>::digest(token.as_bytes())))?;
        kv_set(&sql, "computer_cwd", &cwd)?;
        kv_set(&sql, "computer_seen_at", 0)?;
        sql.exec("DELETE FROM tunnel", None)?;
        let name = kv_get(&sql, "name")?.unwrap_or_default();
        let agent = format!("{}/api/a/{name}", var(&self.env, "AGENT_URL").unwrap_or_default().trim_end_matches('/'));
        Ok(json!({ "connect": true, "agent": agent, "token": token, "cwd": cwd }))
    }

    /// Whether `token` is the connect token of the computer attached here.
    fn require_computer(&self, token: &str) -> Answer<worker::SqlStorage> {
        let sql = self.sql();
        let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(token.as_bytes()));
        let stored = kv_get(&sql, "computer_token")?.unwrap_or_default();
        let same = hash.len() == stored.len() && hash.bytes().zip(stored.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
        if kv_get(&sql, "computer_url")?.as_deref() != Some(computer::CONNECTS) || !same {
            return Err(Fail::new(ErrorCode::Forbidden, "not this agent's computer (its connect token was replaced, or it was detached)"));
        }
        kv_set(&sql, "computer_seen_at", js::now_ms())?;
        Ok(sql)
    }

    /// `computer/poll`: the requests waiting for the computer that connects
    /// out, answered at once, or as they come for up to 25 s.
    async fn computer_poll(&self, token: &str) -> Answer<Value> {
        let deadline = js::now_ms() + POLL_WAIT_MS;
        loop {
            let sql = self.require_computer(token)?;
            let now = js::now_ms() as i64;
            let rows: Vec<Value> = sql
                .exec(
                    "SELECT rid, method, path, body FROM tunnel WHERE status IS NULL AND (sent_at IS NULL OR sent_at < ?) ORDER BY created_at LIMIT 16",
                    vec![(now - RESEND_MS).into()],
                )?
                .to_array()?;
            if !rows.is_empty() || js::now_ms() > deadline {
                for r in &rows {
                    sql.exec("UPDATE tunnel SET sent_at = ? WHERE rid = ?", vec![now.into(), r["rid"].as_str().unwrap_or("").into()])?;
                }
                let requests: Vec<Value> = rows
                    .iter()
                    .map(|r| json!({ "rid": r["rid"], "method": r["method"], "path": r["path"], "body": r["body"].as_str().and_then(|b| serde_json::from_str::<Value>(b).ok()) }))
                    .collect();
                return Ok(json!({ "requests": requests }));
            }
            worker::Delay::from(std::time::Duration::from_millis(200)).await;
        }
    }

    /// `computer/answer {rid, status, body}`: a request's answer.
    fn computer_answer(&self, token: &str, body: Value) -> Answer<Value> {
        let sql = self.require_computer(token)?;
        let rid = body["rid"].as_str().ok_or_else(|| Fail::invalid("rid is required"))?;
        let status = body["status"].as_i64().ok_or_else(|| Fail::invalid("status is required"))?;
        sql.exec("UPDATE tunnel SET status = ?, answer = ? WHERE rid = ? AND status IS NULL", vec![status.into(), body["body"].to_string().into(), rid.into()])?;
        Ok(json!({ "ok": true }))
    }

    fn detach(&self) -> Answer<Value> {
        let sql = self.sql();
        sql.exec("DELETE FROM tunnel", None)?;
        let attached = kv_get(&sql, "computer_url")?.is_some_and(|u| !u.is_empty());
        for k in ["computer_url", "computer_token", "computer_cwd"] {
            kv_set(&sql, k, "")?;
        }
        Ok(json!({ "detached": attached }))
    }

    async fn create(&self, principal: &str, body: CreateBody) -> Answer<Value> {
        let sql = self.sql();
        if kv_get(&sql, "created_at")?.is_some() {
            // its maker, again (the router registers it once more, which
            // answers the same identity): the same agent
            if kv_get(&sql, "creator")?.as_deref() != Some(principal) {
                return Err(Fail::new(ErrorCode::AlreadyExists, format!("agent {} already exists", body.name)));
            }
            let get = |k: &str| kv_get(&sql, k).map(|v| v.unwrap_or_default());
            return Ok(json!({ "name": get("name")?, "npub": get("npub")?, "model": get("model")?, "replayed": true }));
        }
        let model = body.model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
        if model.is_empty() || model.len() > MODEL_MAX {
            return Err(Fail::invalid(format!("a model is 1-{MODEL_MAX} bytes (an OpenRouter model id)")));
        }
        let instructions = body.instructions.unwrap_or_else(|| default_instructions(&body.name));
        if instructions.len() > INSTRUCTIONS_MAX {
            return Err(Fail::invalid(format!("instructions are at most {INSTRUCTIONS_MAX} bytes")));
        }
        // the key is made by KEYS: its secret never reaches this cell
        let (pubkey, sealed) = keys::nostr_keypair(&self.env).await.map_err(Fail::host)?;
        if kv_get(&sql, "created_at")?.is_some() {
            return Err(Fail::new(ErrorCode::AlreadyExists, format!("agent {} already exists", body.name)));
        }
        let agent_npub = npub::encode(&pubkey);
        for (k, v) in [
            ("name", body.name.as_str()),
            // who made it (an identity), until the registry names its owner
            ("creator", principal),
            ("npub", agent_npub.as_str()),
            ("secret", sealed.as_str()),
            ("model", model.as_str()),
            ("instructions", instructions.as_str()),
        ] {
            kv_set(&sql, k, v)?;
        }
        kv_set(&sql, "created_at", js::now_ms())?;
        Ok(json!({ "name": body.name, "npub": agent_npub, "model": model }))
    }

    fn start_driver(&self, reason: &str) -> Answer<bool> {
        if self.driving.get() {
            return Ok(false);
        }
        let sql = self.sql();
        let base = var(&self.env, "OPENROUTER_API_URL").unwrap_or_else(|| "https://openrouter.ai".into()).trim_end_matches('/').to_string();
        // the key is the owner's, asked for as the turn starts (below)
        let model = Model { base, key: String::new(), name: kv_get(&sql, "model")?.unwrap_or_else(|| DEFAULT_MODEL.into()) };
        let name = kv_get(&sql, "name")?.unwrap_or_default();
        let instructions = format!("{}\n\n{ANSWER_POSTED}", kv_get(&sql, "instructions")?.unwrap_or_else(|| default_instructions(&name)));
        let token = CancellationToken::new();
        if kv_u64(&sql, "cancel")? == 1 {
            token.cancel();
        }
        let fleet = self.fleet()?;
        let mut driver = Driver {
            storage: self.state.storage(),
            model,
            fleet: fleet.clone(),
            instructions,
            // opened as the turn starts (below)
            computer: None,
            cancel: token.clone(),
            id: format!("{}:{reason}", self.booted_at),
        };
        let env = self.env.clone();
        *self.cancel.borrow_mut() = Some(token);
        self.driving.set(true);
        let storage = self.state.storage();
        let driving = self.driving.clone();
        let cancel_slot = self.cancel.clone();
        self.state.wait_until(async move {
            let sql = storage.sql();
            let mut outcome = match (owners_key(&fleet).await, open_computer(&env, &sql).await) {
                (Ok(key), Ok(computer)) => {
                    driver.model.key = key;
                    driver.computer = computer;
                    turn::drive(driver).await
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            // a turn a channel started answers there
            if matches!(outcome, Ok(TurnOutcome::Idle)) {
                if let Err(error) = reply(&sql, &fleet).await {
                    outcome = Err(error);
                }
            }
            // a message that came in as the turn ended is read now: the turn
            // stays active and the alarm starts a driver for it
            let pending: Vec<Value> = sql.exec("SELECT seq FROM steer WHERE consumed = 0", None).and_then(|c| c.to_array()).unwrap_or_default();
            if matches!(outcome, Ok(TurnOutcome::Idle)) && !pending.is_empty() {
                let _ = kv_set(&sql, "rerun", 1);
                let _ = storage.set_alarm(std::time::Duration::from_millis(50)).await;
                driving.set(false);
                cancel_slot.borrow_mut().take();
                return;
            }
            let finish = || -> anyhow::Result<()> {
                match &outcome {
                    Ok(how) => kv_set(&sql, "outcome", how.as_str())?,
                    Err(error) => {
                        kv_set(&sql, "outcome", TurnOutcome::Error.as_str())?;
                        kv_set(&sql, "last_error", error.to_string())?;
                    }
                }
                kv_set(&sql, "active", 0)?;
                kv_set(&sql, "turn_ended_at", js::now_ms())
            };
            if let Err(error) = finish() {
                worker::console_error!("finishing the turn failed: {error}");
            }
            let _ = storage.delete_alarm().await;
            driving.set(false);
            cancel_slot.borrow_mut().take();
        });
        Ok(true)
    }

    fn turn(&self, body: TurnBody) -> Answer<Value> {
        self.begin(body.text, "")
    }

    /// Starts a turn with `text`, or steers the running one. `reply_to` is
    /// the listen (a token) whose fragment gets the turn's answer, if any.
    fn begin(&self, text: String, reply_to: &str) -> Answer<Value> {
        if text.is_empty() || text.len() > MESSAGE_TEXT_MAX {
            return Err(Fail::invalid(format!("text is 1-{MESSAGE_TEXT_MAX} bytes")));
        }
        let sql = self.sql();
        if kv_u64(&sql, "active")? == 1 {
            store::steer(&sql, &text)?;
            return Ok(json!({ "steered": true, "driving": self.driving.get() }));
        }
        kv_set(&sql, "reply_to", reply_to)?;
        // a new turn: images an earlier one kept and never showed are
        // dropped, and so are the earlier turns' steers the model read
        // (each is in the conversation as a message of its own)
        sql.exec("DELETE FROM shots", None)?;
        sql.exec("DELETE FROM steer WHERE consumed = 1", None)?;
        let mut kickoff = Message::user().with_text(text);
        kickoff.id = Some(format!("msg_{}", uuid::Uuid::new_v4()));
        store::append_message(&sql, &kickoff)?;
        kv_set(&sql, "active", 1)?;
        kv_set(&sql, "cancel", 0)?;
        kv_set(&sql, "last_error", "")?;
        kv_set(&sql, "turn_started_at", js::now_ms())?;
        kv_set(&sql, "outcome", TurnOutcome::Running.as_str())?;
        let started = self.start_driver("turn")?;
        Ok(json!({ "started": started }))
    }

    /// Follows a channel, once: listening again to the same fragment's
    /// channel (a retry, or a chat's alarm joining its owner's agent once
    /// more) reuses its token, so its inbox URL is the same and the fragment
    /// answers the one subscription it holds for that URL (or makes it
    /// again, if it was dropped). A new listen's row is written before the
    /// request, with no await between the read and the write, so a listen
    /// racing it finds the token too.
    async fn listen(&self, body: ListenBody) -> Answer<Value> {
        let channel = body.channel.unwrap_or_else(|| "chat".into());
        let reply = body.reply.unwrap_or_else(|| "say".into());
        if !valid_fragment_name(&body.fragment) || !valid_channel_name(&channel) || !valid_op_name(&reply) {
            return Err(Fail::invalid("a fragment name, a channel name, and a reply operation name"));
        }
        let base = var(&self.env, "AGENT_URL").ok_or_else(|| Fail::host("AGENT_URL is not set on this fleet"))?;
        let platform = self.fleet()?;
        let sql = self.sql();
        #[derive(Deserialize)]
        struct Listening {
            token: String,
        }
        let same: Vec<Listening> = sql
            .exec("SELECT token FROM listens WHERE fragment = ? AND channel = ? ORDER BY created_at LIMIT 1", vec![body.fragment.as_str().into(), channel.as_str().into()])?
            .to_array()?;
        let token = match same.into_iter().next() {
            Some(listening) => listening.token,
            None => {
                let listening: Vec<Value> = sql.exec("SELECT token FROM listens", None)?.to_array()?;
                if listening.len() >= LISTENS_MAX {
                    return Err(Fail::new(ErrorCode::RateLimited, format!("an agent follows at most {LISTENS_MAX} channels")));
                }
                let token = hex::encode(js::random_bytes::<16>());
                // no subscription yet (0) until the fragment answers
                sql.exec(
                    "INSERT INTO listens (token, fragment, channel, reply, sub, created_at) VALUES (?, ?, ?, ?, 0, ?)",
                    vec![token.as_str().into(), body.fragment.as_str().into(), channel.as_str().into(), reply.as_str().into(), (js::now_ms() as i64).into()],
                )?;
                token
            }
        };
        let name = kv_get(&sql, "name")?.unwrap_or_default();
        let url = format!("{}/api/a/{name}/inbox/{token}", base.trim_end_matches('/'));
        let path = format!("/api/f/{}/subscriptions", body.fragment);
        let (status, answer) = platform.call(Method::Post, &path, Some(&json!({ "channel": channel, "url": url }))).await?;
        if status != 200 {
            // a listen that never subscribed leaves nothing behind
            sql.exec("DELETE FROM listens WHERE token = ? AND sub = 0", vec![token.as_str().into()])?;
            let code = match status {
                401 | 403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                _ => ErrorCode::UpstreamFailed,
            };
            return Err(Fail::new(code, format!("{} answered {status}: {}", body.fragment, fleet::message(&answer))));
        }
        let sub = answer["id"].as_i64().ok_or_else(|| Fail::host("the fragment named no subscription"))?;
        // written again if a racing listen's refusal removed it meanwhile
        sql.exec(
            "INSERT INTO listens (token, fragment, channel, reply, sub, created_at) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (token) DO UPDATE SET reply = excluded.reply, sub = excluded.sub",
            vec![token.as_str().into(), body.fragment.as_str().into(), channel.as_str().into(), reply.as_str().into(), sub.into(), (js::now_ms() as i64).into()],
        )?;
        Ok(json!({ "fragment": body.fragment, "channel": channel, "reply": reply, "subscription": sub }))
    }

    /// A delivery from a followed channel: someone else's record starts a
    /// turn (or steers the running one); the agent's own, and a record heard
    /// before, are acknowledged and ignored.
    fn inbox(&self, token: &str, delivery: Delivery) -> Answer<Value> {
        let Delivery { kind: _, fragment: sent_from, channel: sent_on, record } = delivery;
        let sql = self.sql();
        let rows: Vec<Value> = sql.exec("SELECT fragment, channel FROM listens WHERE token = ?", vec![token.into()])?.to_array()?;
        let Some(listen) = rows.first() else {
            // unknown: the fragment drops the subscription
            return Err(Fail::new(ErrorCode::NotFound, "no such inbox"));
        };
        let (fragment, channel) = (listen["fragment"].as_str().unwrap_or(""), listen["channel"].as_str().unwrap_or(""));
        if sent_from != fragment || sent_on != channel || record.channel != channel {
            return Err(Fail::invalid("the delivery names another fragment or channel"));
        }
        // listening needed the registration, so the agent knows its identity
        let me = kv_get(&sql, "identity")?.unwrap_or_default();
        if !me.is_empty() && record.principal == me {
            return Ok(json!({ "ignored": "own" }));
        }
        // the record's place in its channel: decoding made sure it has one
        let key = format!("{fragment}/{channel}/{}", record.seq);
        let now = js::now_ms() as i64;
        let fresh: Vec<Value> = sql.exec("INSERT OR IGNORE INTO heard (key, at) VALUES (?, ?) RETURNING key", vec![key.as_str().into(), now.into()])?.to_array()?;
        if fresh.is_empty() {
            return Ok(json!({ "ignored": "heard" }));
        }
        // no delivery comes again after HEARD_KEEP_MS: older keys only grow the table
        sql.exec("DELETE FROM heard WHERE at < ?", vec![(now - HEARD_KEEP_MS).into()])?;
        // a chat's record says `{text}`; any other body is heard as its JSON
        #[derive(Deserialize)]
        struct Said {
            text: String,
        }
        let said = match serde_json::from_str::<Said>(record.body.get()) {
            Ok(s) => s.text,
            Err(_) => record.body.get().to_string(),
        };
        let who: String = record.principal.chars().take(12).collect();
        let mut text = format!("[{fragment} · {who}] {said}");
        if text.len() > MESSAGE_TEXT_MAX {
            text.truncate(text.floor_char_boundary(MESSAGE_TEXT_MAX));
        }
        self.begin(text, token)
    }

    fn stop(&self) -> Answer<Value> {
        let sql = self.sql();
        let active = kv_u64(&sql, "active")? == 1;
        if active {
            kv_set(&sql, "cancel", 1)?;
            kv_set(&sql, "stop_requested_at", js::now_ms())?;
            if let Some(token) = self.cancel.borrow().as_ref() {
                token.cancel();
            }
            if !self.driving.get() {
                self.start_driver("stop")?;
            }
        }
        Ok(json!({ "active": active, "driving": self.driving.get() }))
    }

    fn test(&self, body: TestControls) -> Answer<Value> {
        if var(&self.env, "AGENT_TEST_HOOKS").as_deref() != Some("allow") {
            return Err(Fail::new(ErrorCode::NotFound, "test controls are off on this fleet"));
        }
        let sql = self.sql();
        for (key, value) in [("test_hold_in_tool_ms", body.hold_in_tool_ms), ("test_hold_after_tool_ms", body.hold_after_tool_ms)] {
            let value = value.unwrap_or(0);
            if value > TEST_HOLD_MS_MAX {
                return Err(Fail::invalid(format!("{key} is at most {TEST_HOLD_MS_MAX}")));
            }
            kv_set(&sql, key, value)?;
        }
        let watchdog = body.watchdog_ms.unwrap_or(WATCHDOG_MS_DEFAULT);
        if watchdog < WATCHDOG_MS_MIN {
            return Err(Fail::invalid(format!("watchdog_ms is at least {WATCHDOG_MS_MIN}")));
        }
        kv_set(&sql, "watchdog_ms", watchdog)?;
        let window = body.window_messages.unwrap_or(0);
        if window == 1 || window > fragment_core::history::WINDOW_MESSAGES_MAX as u64 {
            return Err(Fail::invalid(format!("window_messages is 2-{} (0: the product's)", fragment_core::history::WINDOW_MESSAGES_MAX)));
        }
        kv_set(&sql, "test_window_messages", window)?;
        let rows = body.view_rows.unwrap_or(0);
        if rows == 1 || rows > VIEW_ROWS_MAX as u64 {
            return Err(Fail::invalid(format!("view_rows is 2-{VIEW_ROWS_MAX} (0: the product's)")));
        }
        kv_set(&sql, "test_view_rows", rows)?;
        Ok(json!({ "ok": true }))
    }

    /// The turn's state, as `state` answers it.
    fn turn_state(&self) -> Answer<AgentState> {
        let sql = self.sql();
        let outcome = match kv_get(&sql, "outcome")?.unwrap_or_default().as_str() {
            "" => None,
            stored => Some(TurnOutcome::parse(stored).ok_or_else(|| Fail::host(format!("corrupt state: outcome {stored:?}")))?),
        };
        let error = kv_get(&sql, "last_error")?.filter(|e| !e.is_empty());
        let answer = last_answer(&sql)?.map(|m| m.as_concat_text());
        Ok(AgentState { active: kv_u64(&sql, "active")? == 1, driving: self.driving.get(), outcome, error, answer })
    }

    /// `state?wait_ms=`: the turn's state once the turn is not active, or
    /// once `wait_ms` passed. It waits here, reading this cell's own SQL,
    /// so a client waiting out a turn asks once every 25 s instead of
    /// reading the whole view (signature, owner check, every list) twice a
    /// second.
    async fn state(&self, wait_ms: u64) -> Answer<AgentState> {
        assert!(wait_ms <= AGENT_STATE_WAIT_MS_MAX, "the door checks wait_ms");
        let deadline = js::now_ms() + wait_ms;
        // bounded: a look every STATE_CHECK_MS until the deadline
        for _ in 0..STATE_CHECKS_MAX {
            let state = self.turn_state()?;
            if !state.active || js::now_ms() >= deadline {
                return Ok(state);
            }
            worker::Delay::from(std::time::Duration::from_millis(STATE_CHECK_MS)).await;
        }
        self.turn_state()
    }

    fn view(&self) -> Answer<Value> {
        let sql = self.sql();
        let rows = match kv_u64(&sql, "test_view_rows")? {
            0 => VIEW_ROWS_MAX,
            n => (n as usize).min(VIEW_ROWS_MAX),
        };
        let messages: Vec<Value> = recent_messages(&sql, rows)?
            .iter()
            .map(|message| {
                let requests: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(MessageContent::as_tool_request)
                    .map(|r| json!({ "id": r.id, "name": r.tool_call.as_ref().map(|c| c.name.to_string()).unwrap_or_default() }))
                    .collect();
                json!({
                    "id": message.id,
                    "role": format!("{:?}", message.role).to_lowercase(),
                    "text": message.as_concat_text(),
                    "tool_requests": requests,
                    "tool_responses": message.get_tool_response_ids(),
                    "steer": message.metadata.steer,
                })
            })
            .collect();
        // the newest rows of a table, oldest first
        let table = |columns: &str, table: &str| -> Answer<Vec<Value>> {
            let query = format!("SELECT {columns} FROM (SELECT * FROM {table} ORDER BY seq DESC LIMIT ?) ORDER BY seq");
            Ok(sql.exec(&query, vec![(rows as i64).into()])?.to_array::<Value>()?)
        };
        let get = |k: &str| kv_get(&sql, k).map(|v| v.unwrap_or_default());
        Ok(json!({
            "name": get("name")?,
            "id": get("identity")?,
            "owner": get("owner")?,
            "npub": get("npub")?,
            "model": get("model")?,
            "active": kv_u64(&sql, "active")? == 1,
            "driving": self.driving.get(),
            "outcome": get("outcome")?,
            "error": get("last_error")?,
            "tokens": { "input": kv_u64(&sql, "tokens_in")?, "output": kv_u64(&sql, "tokens_out")? },
            "computer": match get("computer_url")? {
                url if url.is_empty() => Value::Null,
                url if url == computer::CONNECTS => json!({ "connect": true, "cwd": get("computer_cwd")?, "seenAt": kv_u64(&sql, "computer_seen_at")? }),
                url => json!({ "url": url, "cwd": get("computer_cwd")? }),
            },
            "watchdogRestarts": kv_u64(&sql, "watchdog_restarts")?,
            "messages": messages,
            "steer": table("seq, text, consumed", "steer")?,
            "toolRuns": table("tool_call_id, tool, at, driver", "tool_runs")?,
            "steps": table("step, effects, ms, at, driver", "steps")?,
        }))
    }

    async fn handle(&self, mut req: Request) -> Answer<Response> {
        let principal = req.headers().get(PRINCIPAL_HEADER)?.unwrap_or_default();
        let action = req.path().trim_start_matches('/').to_string();
        let url = req.url()?;
        let parse = |bytes: &[u8]| -> Answer<Value> {
            if bytes.is_empty() {
                return Ok(json!({}));
            }
            serde_json::from_slice(bytes).map_err(|e| Fail::invalid(format!("body: {e}")))
        };
        let computer_token = req.headers().get(COMPUTER_TOKEN_HEADER)?.unwrap_or_default();
        let bytes = req.bytes().await?;
        if let Some(token) = action.strip_prefix("inbox/") {
            // decoded whole at the door, once, from its bytes: a delivery
            // without its record's seq is refused, never keyed `…/null`. (A
            // record's raw body would decode from a parsed Value too; what
            // cannot hold one is serde's buffer for an internally tagged
            // enum, which is why proto's Delivery is a struct.)
            let delivery: Delivery = serde_json::from_slice(&bytes).map_err(|e| Fail::invalid(format!("delivery: {e}")))?;
            return Ok(Response::from_json(&self.inbox(token, delivery)?)?);
        }
        let body = parse(&bytes)?;
        let from = |v: Value| -> Answer<Value> { Ok(v) };
        match action.as_str() {
            "computer/poll" => return Ok(Response::from_json(&self.computer_poll(&computer_token).await?)?),
            "computer/answer" => return Ok(Response::from_json(&self.computer_answer(&computer_token, body)?)?),
            _ => {}
        }
        let answer = match (req.method(), action.as_str()) {
            (Method::Post, "create") => self.create(&principal, serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?).await,
            (method, action) => {
                if principal.is_empty() {
                    return Err(Fail::host("the router named no principal"));
                }
                self.require_owner(&principal).await?;
                match (method.clone(), action) {
                    (Method::Get, "view") => self.view(),
                    (Method::Get, "state") => from(serde_json::to_value(self.state(wait_ms(&url)?).await?).map_err(|e| Fail::host(e.to_string()))?),
                    (Method::Post, "turns") => self.turn(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    (Method::Post, "stop") => self.stop(),
                    (Method::Post, "listen") => self.listen(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?).await,
                    (Method::Get, "tools") => {
                        let mut names = tools::list(self.fleet()?, &self.sql()).await?;
                        if let Some(attached) = self.computer().await? {
                            let manifest = attached.computer.check().await.map_err(unanswered)?;
                            names.extend(computer::tools_of(&manifest)?.iter().map(|t| t.name.to_string()));
                        }
                        from(json!({ "tools": names }))
                    }
                    (Method::Put, "computer") => self.attach(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?).await,
                    (Method::Delete, "computer") => self.detach(),
                    (Method::Post, "test") => self.test(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    _ => Err(Fail::new(ErrorCode::NotFound, format!("no route {} {action}", method.as_ref()))),
                }
            }
        }?;
        Ok(Response::from_json(&answer)?)
    }
}

/// A state read's `wait_ms` (0 when absent): at most AGENT_STATE_WAIT_MS_MAX.
fn wait_ms(url: &worker::Url) -> Answer<u64> {
    let Some((_, given)) = url.query_pairs().find(|(k, _)| k == "wait_ms") else { return Ok(0) };
    match given.parse::<u64>() {
        Ok(ms) if ms <= AGENT_STATE_WAIT_MS_MAX => Ok(ms),
        _ => Err(Fail::invalid(format!("wait_ms is 0-{AGENT_STATE_WAIT_MS_MAX}"))),
    }
}

/// The attached computer, its token opened by `KEYS` (sealed for this
/// agent cell alone).
async fn open_computer(env: &Env, sql: &worker::SqlStorage) -> anyhow::Result<Option<Attached>> {
    let Some(url) = kv_get(sql, "computer_url")?.filter(|u| !u.is_empty()) else { return Ok(None) };
    if url == computer::CONNECTS {
        // one that has not asked lately is not there: the turn goes on without it
        if js::now_ms() > kv_u64(sql, "computer_seen_at")? + computer::SEEN_WITHIN_MS {
            return Ok(None);
        }
        let cwd = kv_get(sql, "computer_cwd")?.unwrap_or_else(|| "work".into());
        return Ok(Some(Attached { computer: Computer { url, token: String::new(), tunnel: Some(sql.clone()) }, cwd }));
    }
    let sealed = kv_get(sql, "computer_token")?.ok_or_else(|| anyhow::anyhow!("the computer has no token"))?;
    let npub = kv_get(sql, "npub")?.unwrap_or_default();
    let opened = keys::open(env, &sealed, &format!("{npub}/computer")).await.map_err(|e| anyhow::anyhow!("the computer's token: {e}"))?;
    if let Some(fresh) = opened.resealed {
        kv_set(sql, "computer_token", fresh)?;
    }
    let token = String::from_utf8(opened.plaintext).map_err(|_| anyhow::anyhow!("the computer's token is not text"))?;
    let cwd = kv_get(sql, "computer_cwd")?.unwrap_or_else(|| "work".into());
    Ok(Some(Attached { computer: Computer { url, token, tunnel: None }, cwd }))
}

/// Posts a channel-started turn's last answer to its fragment, through the
/// listen's reply operation, once (the id comes from the message).
async fn reply(sql: &worker::SqlStorage, fleet: &Fleet) -> anyhow::Result<()> {
    let Some(token) = kv_get(sql, "reply_to")?.filter(|t| !t.is_empty()) else { return Ok(()) };
    let rows: Vec<Value> = sql.exec("SELECT fragment, reply FROM listens WHERE token = ?", vec![token.as_str().into()]).and_then(|c| c.to_array()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some(listen) = rows.first() else { return Ok(()) };
    // a turn that ended without text has nothing to say
    let Some(answer) = last_answer(sql)? else { return Ok(()) };
    let id = fragment_core::tools::reply_id(answer.id.as_deref().unwrap_or(""));
    let fragment = listen["fragment"].as_str().unwrap_or("");
    let mut text = answer.as_concat_text();
    // the turn's images (a screenshot) land in the chat's files, shown with the answer
    let shots: Vec<Value> = sql.exec("SELECT seq, mime, data FROM shots ORDER BY seq", None).and_then(|c| c.to_array()).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !shots.is_empty() {
        let paths: Vec<String> = shots
            .iter()
            .map(|s| format!("shots/{id}-{}.{}", s["seq"], if s["mime"] == "image/jpeg" { "jpg" } else { "png" }))
            .collect();
        let files: Vec<Value> = shots.iter().zip(&paths).map(|(s, p)| json!({ "path": p, "base64": s["data"] })).collect();
        let (status, body) = fleet.call(Method::Post, &format!("/api/f/{fragment}/files"), Some(&json!({ "files": files, "message": "screenshots", "key": format!("{id}-shots") }))).await?;
        if status == 200 {
            for p in &paths {
                text.push_str(&format!("\n\n![screenshot](__file?path={p})"));
            }
        } else {
            text.push_str(&format!("\n\n(the screenshot could not be shown here: {})", fleet::message(&body)));
        }
    }
    let path = format!("/api/f/{fragment}/ops/{}", listen["reply"].as_str().unwrap_or(""));
    let (status, body) = fleet.call(Method::Post, &path, Some(&json!({ "id": id, "input": { "text": text } }))).await?;
    if status != 200 {
        anyhow::bail!("posting the answer to {path}: {status} {}", fleet::message(&body));
    }
    sql.exec("DELETE FROM shots", None).map_err(|e| anyhow::anyhow!("{e}"))?;
    kv_set(sql, "reply_to", "")
}

impl DurableObject for Agent {
    fn new(state: State, env: Env) -> Self {
        if let Err(error) = state.storage().sql().exec(store::SCHEMA, None) {
            worker::console_error!("schema: {error}");
        }
        Self { state, env, booted_at: js::now_ms(), driving: Rc::new(Cell::new(false)), cancel: Rc::new(RefCell::new(None)) }
    }

    async fn fetch(&self, req: Request) -> worker::Result<Response> {
        match self.handle(req).await {
            Ok(r) => Ok(r),
            Err(f) => f.response(),
        }
    }

    async fn alarm(&self) -> worker::Result<Response> {
        let sql = self.sql();
        let active = kv_u64(&sql, "active").map_err(|e| worker::Error::RustError(e.to_string()))? == 1;
        if active {
            // The watchdog: a driver that died with its node is replaced
            // here and resumes from the last applied step. A live driver
            // gets the wake re-armed, so one always stays pending.
            let rerun = kv_u64(&sql, "rerun").unwrap_or(0) == 1;
            match self.start_driver(if rerun { "rerun" } else { "alarm" }) {
                Ok(true) if rerun => {
                    let _ = kv_set(&sql, "rerun", 0);
                }
                Ok(true) => {
                    let restarts = kv_u64(&sql, "watchdog_restarts").unwrap_or(0);
                    let _ = kv_set(&sql, "watchdog_restarts", restarts + 1);
                }
                Ok(false) => turn::arm_watchdog(&self.state.storage(), &sql).await.map_err(|e| worker::Error::RustError(e.to_string()))?,
                Err(f) => {
                    let _ = kv_set(&sql, "outcome", TurnOutcome::Error.as_str());
                    let _ = kv_set(&sql, "last_error", &f.message);
                    let _ = kv_set(&sql, "active", 0);
                }
            }
        }
        Response::ok("ok")
    }
}
