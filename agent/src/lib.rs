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
//!   POST /api/a/{name}/test           test controls (dev fleets: AGENT_TEST_HOOKS=allow)

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
use fragment_proto::{limits, valid_fragment_name, ErrorCode};
use goose_provider_types::conversation::message::{Message, MessageContent};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use worker::wasm_bindgen;
use worker::{durable_object, event, DurableObject, Env, Headers, Method, Request, RequestInit, Response, State, Url};

use crate::fleet::Fleet;
use crate::store::{kv_get, kv_set, kv_u64, load_messages};
use crate::turn::{Driver, Model, WATCHDOG_MS_DEFAULT, WATCHDOG_MS_MIN};

const PRINCIPAL_HEADER: &str = "x-agent-principal";
const MESSAGE_TEXT_MAX: usize = 16 * 1024;
const MODEL_MAX: usize = 128;
const INSTRUCTIONS_MAX: usize = 8 * 1024;
const TEST_HOLD_MS_MAX: u64 = 60_000;
const BODY_MAX: usize = 64 * 1024;
/// The platform's default model (ROADMAP decision 7).
const DEFAULT_MODEL: &str = "z-ai/glm-5.3-flash";

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

async fn route(mut req: Request, env: &Env) -> Answer<Response> {
    let url = arrived_url(&req)?;
    let path = url.path().to_string();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if req.method() == Method::Get && segments == ["healthz"] {
        return Ok(Response::ok("ok")?);
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
    let headers = Headers::new();
    headers.set(PRINCIPAL_HEADER, &principal)?;
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers);
    if !body.is_empty() {
        init.with_body(Some(worker::js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let inner = Request::new_with_init(&format!("https://agent.internal/{action}"), &init)?;
    let stub = env.durable_object("AGENT")?.id_from_name(&name)?.get_stub()?;
    Ok(stub.fetch_with_request(inner).await?)
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
        let driver = Driver {
            storage: self.state.storage(),
            model,
            fleet: self.fleet()?,
            instructions,
            cancel: token.clone(),
            id: format!("{}:{reason}", self.booted_at),
        };
        *self.cancel.borrow_mut() = Some(token);
        self.driving.set(true);
        let storage = self.state.storage();
        let driving = self.driving.clone();
        let cancel_slot = self.cancel.clone();
        self.state.wait_until(async move {
            let outcome = turn::drive(driver).await;
            let sql = storage.sql();
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
        if body.text.is_empty() || body.text.len() > MESSAGE_TEXT_MAX {
            return Err(Fail::invalid(format!("text is 1-{MESSAGE_TEXT_MAX} bytes")));
        }
        let sql = self.sql();
        if kv_u64(&sql, "active")? == 1 {
            store::steer(&sql, &body.text)?;
            return Ok(json!({ "steered": true, "driving": self.driving.get() }));
        }
        let mut kickoff = Message::user().with_text(body.text);
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
            "watchdogRestarts": kv_u64(&sql, "watchdog_restarts")?,
            "messages": messages,
            "steer": table("SELECT seq, text, consumed FROM steer ORDER BY seq")?,
            "toolRuns": table("SELECT tool_call_id, tool, at, driver FROM tool_runs ORDER BY seq")?,
            "steps": table("SELECT step, effects, ms, at, driver FROM steps ORDER BY seq")?,
        }))
    }

    async fn handle(&self, mut req: Request) -> Answer<Response> {
        let principal = req.headers().get(PRINCIPAL_HEADER)?.ok_or_else(|| Fail::host("the router named no principal"))?;
        let action = req.path().trim_start_matches('/').to_string();
        let parse = |bytes: &[u8]| -> Answer<Value> {
            if bytes.is_empty() {
                return Ok(json!({}));
            }
            serde_json::from_slice(bytes).map_err(|e| Fail::invalid(format!("body: {e}")))
        };
        let body = parse(&req.bytes().await?)?;
        let from = |v: Value| -> Answer<Value> { Ok(v) };
        let answer = match (req.method(), action.as_str()) {
            (Method::Post, "create") => self.create(&principal, serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
            (method, action) => {
                self.require_owner(&principal)?;
                match (method.clone(), action) {
                    (Method::Get, "view") => self.view(),
                    (Method::Post, "turns") => self.turn(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    (Method::Post, "stop") => self.stop(),
                    (Method::Get, "tools") => from(json!({ "tools": tools::list(self.fleet()?).await? })),
                    (Method::Post, "test") => self.test(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    _ => Err(Fail::new(ErrorCode::NotFound, format!("no route {} {action}", method.as_ref()))),
                }
            }
        }?;
        Ok(Response::from_json(&answer)?)
    }
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
            match self.start_driver("alarm") {
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
