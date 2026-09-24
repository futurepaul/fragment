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
//!   POST /api/a/{name}/turns          {text}: start a turn, or steer the running one
//!   POST /api/a/{name}/stop           stop the running turn
//!   GET  /api/a/{name}/tools          the tools its memberships give it now
//!   POST /api/a/{name}/listen         {fragment, channel? ("chat"), reply? ("say")}: follow a channel
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

use fragment_core::npub;
use fragment_proto::{valid_channel_name, valid_fragment_name, valid_op_name, ErrorCode};
use goose_provider_types::conversation::message::{Message, MessageContent};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use worker::wasm_bindgen;
use worker::{durable_object, event, DurableObject, Env, Headers, Method, Request, RequestInit, Response, State, Url};

use crate::computer::Computer;
use crate::fleet::Fleet;
use crate::store::{kv_get, kv_set, kv_u64, load_messages};
use crate::turn::{Attached, Driver, Model, WATCHDOG_MS_DEFAULT, WATCHDOG_MS_MIN};

const PRINCIPAL_HEADER: &str = "x-agent-principal";
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

/// The URL a request arrived on as its client named it (a proxy that ends
/// TLS forwards plain HTTP and says so in `x-forwarded-proto`).
fn arrived_url(req: &Request) -> Answer<Url> {
    let mut url = req.url()?;
    if url.scheme() == "http" && req.headers().get("x-forwarded-proto")?.as_deref() == Some("https") {
        url.set_scheme("https").map_err(|_| Fail::host("could not name the https URL"))?;
    }
    Ok(url)
}

/// Hands a request to an agent's cell, as `principal` (none for an inbox delivery).
async fn forward(env: &Env, name: &str, action: &str, method: Method, principal: Option<&str>, body: Vec<u8>) -> Answer<Response> {
    let headers = Headers::new();
    if let Some(p) = principal {
        headers.set(PRINCIPAL_HEADER, p)?;
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

async fn route(mut req: Request, env: &Env) -> Answer<Response> {
    let url = arrived_url(&req)?;
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
        let body = req.bytes().await?;
        if body.len() > BODY_MAX {
            return Err(Fail::new(ErrorCode::TooLarge, format!("a delivery is at most {BODY_MAX} bytes")));
        }
        return forward(env, &name, &action, Method::Post, None, body).await;
    }
    // the platform's router, the only way in, names who is calling
    let principal = req.headers().get(PRINCIPAL_HEADER)?.filter(|p| npub::is_identity(p)).ok_or_else(|| Fail::new(ErrorCode::Unauthenticated, "reach agents through the platform"))?;
    let (name, action) = match (req.method(), segments.as_slice()) {
        (Method::Post, ["api", "agents"]) => (None, "create".to_string()),
        (_, ["api", "a", name]) => (Some(name.to_string()), "view".to_string()),
        (_, ["api", "a", name, action]) => (Some(name.to_string()), action.to_string()),
        _ => return Err(Fail::new(ErrorCode::NotFound, format!("no route {} {path}", req.method().as_ref()))),
    };
    let body = req.bytes().await?;
    if body.len() > BODY_MAX {
        return Err(Fail::new(ErrorCode::TooLarge, format!("a request body is at most {BODY_MAX} bytes")));
    }
    let name = match name {
        Some(n) => n,
        None => serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["name"].as_str().map(str::to_string)).unwrap_or_default(),
    };
    if !valid_fragment_name(&name) {
        return Err(Fail::invalid("an agent's name is <label>.<username>"));
    }
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
    url: String,
    token: String,
    cwd: Option<String>,
}

#[derive(Deserialize, Default)]
struct TestControls {
    hold_in_tool_ms: Option<u64>,
    hold_after_tool_ms: Option<u64>,
    watchdog_ms: Option<u64>,
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

fn default_instructions(name: &str) -> String {
    let name = name.split('.').next().unwrap_or(name);
    format!(
        "You are {name}, an agent. Each of your tools is an operation of a fragment you belong to: a shared place such as \
         an app, a list, or a chat. Use them to do what you are asked, one call at a time, and when the work is done \
         answer in one short sentence."
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
                let (Some(id), Some(owner)) = (me["id"].as_str(), me["owner"].as_str()) else {
                    return Err(Fail::host("the registry answered no id or owner for this agent"));
                };
                kv_set(&sql, "identity", id)?;
                kv_set(&sql, "owner", owner)?;
                Ok((id.to_string(), owner.to_string()))
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
        let url = body.url.trim().trim_end_matches('/').to_string();
        if url.len() > COMPUTER_URL_MAX {
            return Err(Fail::invalid(format!("a computer URL is at most {COMPUTER_URL_MAX} bytes")));
        }
        let local = var(&self.env, "FRAGMENT_EGRESS_LOCAL").as_deref() == Some("allow");
        fragment_core::egress::check(&url, local).map_err(|e| Fail::invalid(format!("url: {e}")))?;
        if body.token.len() < 16 || body.token.len() > COMPUTER_TOKEN_MAX {
            return Err(Fail::invalid(format!("a computer token is 16-{COMPUTER_TOKEN_MAX} bytes")));
        }
        let cwd = body.cwd.unwrap_or_else(|| "work".into());
        if !computer::valid_cwd(&cwd) {
            return Err(Fail::invalid(format!("cwd is 1-{} of [a-z0-9-]", computer::CWD_MAX)));
        }
        let c = Computer { url: url.clone(), token: body.token.clone() };
        let manifest = c.check().await.map_err(unanswered)?;
        let tools: Vec<String> = computer::tools_of(&manifest)?.iter().map(|t| t.name.to_string()).collect();
        let sealed = keys::seal(&self.env, body.token.as_bytes()).await.map_err(Fail::host)?;
        let sql = self.sql();
        kv_set(&sql, "computer_url", &url)?;
        kv_set(&sql, "computer_token", sealed)?;
        kv_set(&sql, "computer_cwd", &cwd)?;
        Ok(json!({ "url": url, "cwd": cwd, "tools": tools }))
    }

    fn detach(&self) -> Answer<Value> {
        let sql = self.sql();
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
        let instructions = kv_get(&sql, "instructions")?.unwrap_or_else(|| default_instructions(&name));
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
            if matches!(outcome, Ok("idle")) {
                if let Err(error) = reply(&sql, &fleet).await {
                    outcome = Err(error);
                }
            }
            // a message that came in as the turn ended is read now: the turn
            // stays active and the alarm starts a driver for it
            let pending: Vec<Value> = sql.exec("SELECT seq FROM steer WHERE consumed = 0", None).and_then(|c| c.to_array()).unwrap_or_default();
            if matches!(outcome, Ok("idle")) && !pending.is_empty() {
                let _ = kv_set(&sql, "rerun", 1);
                let _ = storage.set_alarm(std::time::Duration::from_millis(50)).await;
                driving.set(false);
                cancel_slot.borrow_mut().take();
                return;
            }
            let finish = || -> anyhow::Result<()> {
                match &outcome {
                    Ok(how) => kv_set(&sql, "outcome", how)?,
                    Err(error) => {
                        kv_set(&sql, "outcome", "error")?;
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
        let mut kickoff = Message::user().with_text(text);
        kickoff.id = Some(format!("msg_{}", uuid::Uuid::new_v4()));
        store::append_message(&sql, &kickoff)?;
        kv_set(&sql, "active", 1)?;
        kv_set(&sql, "cancel", 0)?;
        kv_set(&sql, "last_error", "")?;
        kv_set(&sql, "turn_started_at", js::now_ms())?;
        kv_set(&sql, "outcome", "running")?;
        let started = self.start_driver("turn")?;
        Ok(json!({ "started": started }))
    }

    async fn listen(&self, body: ListenBody) -> Answer<Value> {
        let channel = body.channel.unwrap_or_else(|| "chat".into());
        let reply = body.reply.unwrap_or_else(|| "say".into());
        if !valid_fragment_name(&body.fragment) || !valid_channel_name(&channel) || !valid_op_name(&reply) {
            return Err(Fail::invalid("a fragment name, a channel name, and a reply operation name"));
        }
        let sql = self.sql();
        let listening: Vec<Value> = sql.exec("SELECT token FROM listens", None)?.to_array()?;
        if listening.len() >= LISTENS_MAX {
            return Err(Fail::new(ErrorCode::RateLimited, format!("an agent follows at most {LISTENS_MAX} channels")));
        }
        let base = var(&self.env, "AGENT_URL").ok_or_else(|| Fail::host("AGENT_URL is not set on this fleet"))?;
        let name = kv_get(&sql, "name")?.unwrap_or_default();
        let token = hex::encode(js::random_bytes::<16>());
        let url = format!("{}/api/a/{name}/inbox/{token}", base.trim_end_matches('/'));
        let path = format!("/api/f/{}/subscriptions", body.fragment);
        let (status, answer) = self.fleet()?.call(Method::Post, &path, Some(&json!({ "channel": channel, "url": url }))).await?;
        if status != 200 {
            let code = match status {
                401 | 403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                _ => ErrorCode::UpstreamFailed,
            };
            return Err(Fail::new(code, format!("{} answered {status}: {}", body.fragment, fleet::message(&answer))));
        }
        let sub = answer["id"].as_i64().ok_or_else(|| Fail::host("the fragment named no subscription"))?;
        sql.exec(
            "INSERT INTO listens (token, fragment, channel, reply, sub, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            vec![token.as_str().into(), body.fragment.as_str().into(), channel.as_str().into(), reply.as_str().into(), sub.into(), (js::now_ms() as i64).into()],
        )?;
        Ok(json!({ "fragment": body.fragment, "channel": channel, "reply": reply, "subscription": sub }))
    }

    /// A delivery from a followed channel: someone else's record starts a
    /// turn (or steers the running one); the agent's own, and a record heard
    /// before, are acknowledged and ignored.
    fn inbox(&self, token: &str, body: Value) -> Answer<Value> {
        let sql = self.sql();
        let rows: Vec<Value> = sql.exec("SELECT fragment, channel FROM listens WHERE token = ?", vec![token.into()])?.to_array()?;
        let Some(listen) = rows.first() else {
            // unknown: the fragment drops the subscription
            return Err(Fail::new(ErrorCode::NotFound, "no such inbox"));
        };
        let (fragment, channel) = (listen["fragment"].as_str().unwrap_or(""), listen["channel"].as_str().unwrap_or(""));
        if body["fragment"] != fragment || body["channel"] != channel {
            return Err(Fail::invalid("the delivery names another fragment or channel"));
        }
        let record = &body["record"];
        // listening needed the registration, so the agent knows its identity
        let me = kv_get(&sql, "identity")?.unwrap_or_default();
        let from = record["principal"].as_str().unwrap_or("");
        if !me.is_empty() && from == me {
            return Ok(json!({ "ignored": "own" }));
        }
        let key = format!("{fragment}/{channel}/{}", record["seq"]);
        let fresh: Vec<Value> = sql.exec("INSERT OR IGNORE INTO heard (key, at) VALUES (?, ?) RETURNING key", vec![key.as_str().into(), (js::now_ms() as i64).into()])?.to_array()?;
        if fresh.is_empty() {
            return Ok(json!({ "ignored": "heard" }));
        }
        let said = match record["body"]["text"].as_str() {
            Some(t) => t.to_string(),
            None => record["body"].to_string(),
        };
        let who: String = from.chars().take(12).collect();
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
        Ok(json!({ "ok": true }))
    }

    fn view(&self) -> Answer<Value> {
        let sql = self.sql();
        let messages: Vec<Value> = load_messages(&sql)?
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
        let table = |query: &str| -> Answer<Vec<Value>> { Ok(sql.exec(query, None)?.to_array::<Value>()?) };
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
                url => json!({ "url": url, "cwd": get("computer_cwd")? }),
            },
            "watchdogRestarts": kv_u64(&sql, "watchdog_restarts")?,
            "messages": messages,
            "steer": table("SELECT seq, text, consumed FROM steer ORDER BY seq")?,
            "toolRuns": table("SELECT tool_call_id, tool, at, driver FROM tool_runs ORDER BY seq")?,
            "steps": table("SELECT step, effects, ms, at, driver FROM steps ORDER BY seq")?,
        }))
    }

    async fn handle(&self, mut req: Request) -> Answer<Response> {
        let principal = req.headers().get(PRINCIPAL_HEADER)?.unwrap_or_default();
        let action = req.path().trim_start_matches('/').to_string();
        let parse = |bytes: &[u8]| -> Answer<Value> {
            if bytes.is_empty() {
                return Ok(json!({}));
            }
            serde_json::from_slice(bytes).map_err(|e| Fail::invalid(format!("body: {e}")))
        };
        let body = parse(&req.bytes().await?)?;
        let from = |v: Value| -> Answer<Value> { Ok(v) };
        if let Some(token) = action.strip_prefix("inbox/") {
            return Ok(Response::from_json(&self.inbox(token, body)?)?);
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
                    (Method::Post, "turns") => self.turn(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    (Method::Post, "stop") => self.stop(),
                    (Method::Post, "listen") => self.listen(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?).await,
                    (Method::Get, "tools") => {
                        let mut names = tools::list(self.fleet()?).await?;
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

/// The attached computer, its token opened by `KEYS` (sealed for this
/// agent cell alone).
async fn open_computer(env: &Env, sql: &worker::SqlStorage) -> anyhow::Result<Option<Attached>> {
    let Some(url) = kv_get(sql, "computer_url")?.filter(|u| !u.is_empty()) else { return Ok(None) };
    let sealed = kv_get(sql, "computer_token")?.ok_or_else(|| anyhow::anyhow!("the computer has no token"))?;
    let npub = kv_get(sql, "npub")?.unwrap_or_default();
    let opened = keys::open(env, &sealed, &format!("{npub}/computer")).await.map_err(|e| anyhow::anyhow!("the computer's token: {e}"))?;
    if let Some(fresh) = opened.resealed {
        kv_set(sql, "computer_token", fresh)?;
    }
    let token = String::from_utf8(opened.plaintext).map_err(|_| anyhow::anyhow!("the computer's token is not text"))?;
    let cwd = kv_get(sql, "computer_cwd")?.unwrap_or_else(|| "work".into());
    Ok(Some(Attached { computer: Computer { url, token }, cwd }))
}

/// Posts a channel-started turn's last answer to its fragment, through the
/// listen's reply operation, once (the id comes from the message).
async fn reply(sql: &worker::SqlStorage, fleet: &Fleet) -> anyhow::Result<()> {
    let Some(token) = kv_get(sql, "reply_to")?.filter(|t| !t.is_empty()) else { return Ok(()) };
    let rows: Vec<Value> = sql.exec("SELECT fragment, reply FROM listens WHERE token = ?", vec![token.as_str().into()]).and_then(|c| c.to_array()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some(listen) = rows.first() else { return Ok(()) };
    let answer = load_messages(sql)?
        .into_iter()
        .rev()
        .find(|m| m.role == rmcp::model::Role::Assistant && !m.as_concat_text().trim().is_empty());
    let Some(answer) = answer else { return Ok(()) };
    let id = fragment_core::tools::reply_id(answer.id.as_deref().unwrap_or(""));
    let path = format!("/api/f/{}/ops/{}", listen["fragment"].as_str().unwrap_or(""), listen["reply"].as_str().unwrap_or(""));
    let (status, body) = fleet.call(Method::Post, &path, Some(&json!({ "id": id, "input": { "text": answer.as_concat_text() } }))).await?;
    if status != 200 {
        anyhow::bail!("posting the answer to {path}: {status} {}", fleet::message(&body));
    }
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
                    let _ = kv_set(&sql, "outcome", "error");
                    let _ = kv_set(&sql, "last_error", &f.message);
                    let _ = kv_set(&sql, "active", 0);
                }
            }
        }
        Response::ok("ok")
    }
}
