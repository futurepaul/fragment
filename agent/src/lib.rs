//! Agents on celld (ROADMAP phase 5): a Durable Object per agent, with its
//! own key, its conversation in SQL, and goose's loop (`goose-agent`) as its
//! turn. Its tools are the operations of the fragments it belongs to, which
//! it calls through the platform's signed API with its own key: an agent
//! acts on a fragment exactly as its membership lets it.
//!
//! The owner's API (NIP-98, the owner's key):
//!   POST /api/agents                  {name, model?, instructions?} → {name, npub, owner, model}
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
mod store;
mod tools;
mod turn;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fragment_core::{npub, secrets};
use fragment_nip98::Keys;
use fragment_proto::{limits, valid_channel_name, valid_fragment_name, valid_op_name, ErrorCode};
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
    let header = req.headers().get("authorization")?;
    let now_s = (js::now_ms() / 1000) as i64;
    let principal = fragment_nip98::verify(header.as_deref(), req.method().as_ref(), url.as_str(), &body, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| Fail::new(ErrorCode::Unauthenticated, e.to_string()))?;
    let name = match name {
        Some(n) => n,
        None => serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["name"].as_str().map(str::to_string)).unwrap_or_default(),
    };
    if !valid_fragment_name(&name) {
        return Err(Fail::invalid("an agent name must match ^[a-z0-9][a-z0-9-]{0,62}$"));
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

fn host_secrets(env: &Env) -> Answer<Vec<String>> {
    let all: Vec<String> = ["FRAGMENT_HOST_SECRET", "FRAGMENT_HOST_SECRET_PREVIOUS"].iter().filter_map(|n| var(env, n)).collect();
    match all.first() {
        Some(s) if s.len() >= secrets::HOST_SECRET_MIN_BYTES => Ok(all),
        _ => Err(Fail::host("FRAGMENT_HOST_SECRET is not set on this fleet (at least 32 bytes)")),
    }
}

fn unanswered(why: computer::Unanswered) -> Fail {
    match why {
        computer::Unanswered::Refused => Fail::invalid("the computer refused this token (it is in the file `fragment computer serve --token-file` names)"),
        computer::Unanswered::Unreachable(e) => Fail::new(ErrorCode::UpstreamFailed, format!("the computer did not answer: {e}")),
    }
}

fn default_instructions(name: &str) -> String {
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

    fn require_owner(&self, principal: &str) -> Answer<()> {
        match kv_get(&self.sql(), "owner")? {
            None => Err(Fail::new(ErrorCode::NotFound, "no such agent")),
            Some(owner) if owner == principal => Ok(()),
            Some(_) => Err(Fail::new(ErrorCode::Forbidden, "only the agent's owner may do that")),
        }
    }

    fn keys(&self) -> Answer<Arc<Keys>> {
        let sql = self.sql();
        let sealed = kv_get(&sql, "secret")?.ok_or_else(|| Fail::host("the agent has no key"))?;
        let npub = kv_get(&sql, "npub")?.ok_or_else(|| Fail::host("the agent has no npub"))?;
        let hosts = host_secrets(&self.env)?;
        let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
        let opened = secrets::open(&hosts, &npub, &sealed).map_err(|e| Fail::host(format!("the agent's key: {e}")))?;
        let secret = String::from_utf8(opened.plaintext).map_err(|_| Fail::host("the agent's key is not text"))?;
        Keys::from_secret_hex(&secret).map(Arc::new).ok_or_else(|| Fail::host("the agent's key is not a secp256k1 key"))
    }

    fn fleet(&self) -> Answer<Fleet> {
        Ok(Fleet { base: fleet::base(&self.env)?, keys: self.keys()? })
    }

    /// The attached computer, its token opened (sealed under the fleet's
    /// host secret, salted with the agent's npub).
    fn computer(&self) -> Answer<Option<Attached>> {
        let sql = self.sql();
        let Some(url) = kv_get(&sql, "computer_url")?.filter(|u| !u.is_empty()) else { return Ok(None) };
        let sealed = kv_get(&sql, "computer_token")?.ok_or_else(|| Fail::host("the computer has no token"))?;
        let npub = kv_get(&sql, "npub")?.unwrap_or_default();
        let hosts = host_secrets(&self.env)?;
        let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
        let opened = secrets::open(&hosts, &format!("{npub}/computer"), &sealed).map_err(|e| Fail::host(format!("the computer's token: {e}")))?;
        let token = String::from_utf8(opened.plaintext).map_err(|_| Fail::host("the computer's token is not text"))?;
        let cwd = kv_get(&sql, "computer_cwd")?.unwrap_or_else(|| "work".into());
        Ok(Some(Attached { computer: Computer { url, token }, cwd }))
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
        let sql = self.sql();
        let npub = kv_get(&sql, "npub")?.unwrap_or_default();
        let hosts = host_secrets(&self.env)?;
        let sealed = secrets::seal(&hosts[0], &format!("{npub}/computer"), body.token.as_bytes(), js::random_bytes()).map_err(Fail::host)?;
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

    fn create(&self, principal: &str, body: CreateBody) -> Answer<Value> {
        let sql = self.sql();
        if kv_get(&sql, "created_at")?.is_some() {
            return Err(Fail::new(ErrorCode::AlreadyExists, format!("agent {} already exists", body.name)));
        }
        let model = body.model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
        if model.is_empty() || model.len() > MODEL_MAX {
            return Err(Fail::invalid(format!("a model is 1-{MODEL_MAX} bytes (an OpenRouter model id)")));
        }
        let instructions = body.instructions.unwrap_or_else(|| default_instructions(&body.name));
        if instructions.len() > INSTRUCTIONS_MAX {
            return Err(Fail::invalid(format!("instructions are at most {INSTRUCTIONS_MAX} bytes")));
        }
        let hosts = host_secrets(&self.env)?;
        let (secret, pubkey) = loop {
            let secret = hex::encode(js::random_bytes::<32>());
            if let Some(pubkey) = fragment_nip98::pubkey_of_secret(&secret) {
                break (secret, pubkey);
            }
        };
        let agent_npub = npub::encode(&pubkey);
        let sealed = secrets::seal(&hosts[0], &agent_npub, secret.as_bytes(), js::random_bytes()).map_err(Fail::host)?;
        for (k, v) in [
            ("name", body.name.as_str()),
            ("owner", principal),
            ("npub", agent_npub.as_str()),
            ("secret", sealed.as_str()),
            ("model", model.as_str()),
            ("instructions", instructions.as_str()),
        ] {
            kv_set(&sql, k, v)?;
        }
        kv_set(&sql, "created_at", js::now_ms())?;
        Ok(json!({ "name": body.name, "npub": agent_npub, "owner": npub::encode(principal), "model": model }))
    }

    fn start_driver(&self, reason: &str) -> Answer<bool> {
        if self.driving.get() {
            return Ok(false);
        }
        let sql = self.sql();
        let key = var(&self.env, "OPENROUTER_API_KEY").ok_or_else(|| Fail::host("OPENROUTER_API_KEY is not set on this fleet"))?;
        let base = var(&self.env, "OPENROUTER_API_URL").unwrap_or_else(|| "https://openrouter.ai".into()).trim_end_matches('/').to_string();
        let model = Model { base, key, name: kv_get(&sql, "model")?.unwrap_or_else(|| DEFAULT_MODEL.into()) };
        let name = kv_get(&sql, "name")?.unwrap_or_default();
        let instructions = kv_get(&sql, "instructions")?.unwrap_or_else(|| default_instructions(&name));
        let token = CancellationToken::new();
        if kv_u64(&sql, "cancel")? == 1 {
            token.cancel();
        }
        let fleet = self.fleet()?;
        let driver = Driver {
            storage: self.state.storage(),
            model,
            fleet: fleet.clone(),
            instructions,
            computer: self.computer()?,
            cancel: token.clone(),
            id: format!("{}:{reason}", self.booted_at),
        };
        *self.cancel.borrow_mut() = Some(token);
        self.driving.set(true);
        let storage = self.state.storage();
        let driving = self.driving.clone();
        let cancel_slot = self.cancel.clone();
        self.state.wait_until(async move {
            let mut outcome = turn::drive(driver).await;
            let sql = storage.sql();
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
        let me = kv_get(&sql, "npub")?.unwrap_or_default();
        let me_hex = npub::parse(&me).unwrap_or_default();
        let from = record["principal"].as_str().unwrap_or("");
        if from == me || from == me_hex {
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
            (Method::Post, "create") => self.create(&principal, serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
            (method, action) => {
                if principal.is_empty() {
                    return Err(Fail::host("the router named no principal"));
                }
                self.require_owner(&principal)?;
                match (method.clone(), action) {
                    (Method::Get, "view") => self.view(),
                    (Method::Post, "turns") => self.turn(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    (Method::Post, "stop") => self.stop(),
                    (Method::Post, "listen") => self.listen(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?).await,
                    (Method::Get, "tools") => {
                        let mut names = tools::list(self.fleet()?).await?;
                        if let Some(attached) = self.computer()? {
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
