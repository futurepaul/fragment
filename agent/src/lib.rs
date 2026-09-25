//! Agents on celld (ROADMAP phase 5): a Durable Object per agent, with its
//! own key, its conversations in SQL, and goose's loop (`goose-agent`) as
//! its turn. Its tools are fragments' operations and the platform's verbs,
//! which it calls through the platform's signed API with its own key.
//!
//! An agent acts for whoever asked (ROADMAP decision 17): a turn records
//! who started it, and every call the turn makes names them (`for`, inside
//! the signed URL), so the platform acts with the lower of their role and
//! the agent's cap. The owner starts turns directly; anyone else, by a
//! message in a chat the agent follows. An anonymous visitor's message
//! starts nothing.
//!
//! One conversation per chat (store.rs): a turn belongs to one, reads only
//! it, and answers there. One turn runs at a time. A message from the
//! running turn's starter, in its conversation, steers it; any other waits
//! for a turn of its own, and a steer the turn ended before reading gets
//! one too. The driver that ends a turn starts the next one waiting in the
//! same step, so no message lands where nothing will read it.
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
//!   GET  /api/a/{name}                its conversations, and its turns' state
//!   GET  /api/a/{name}/state?wait_ms= the owner's own turn's state, once it ends (or wait_ms, at most 25 s)
//!   POST /api/a/{name}/turns          {text}: start a turn, steer the owner's running one, or wait for one
//!   POST /api/a/{name}/stop           stop the running turn
//!   GET  /api/a/{name}/tools          the tools the owner's turn has now
//!   POST /api/a/{name}/listen         {fragment, channel? ("chat"), reply? ("say")}: follow a channel (once: again is the same listen)
//!   PUT  /api/a/{name}/computer       {url, token, cwd? ("work")}: attach a computer (`fragment computer serve`)
//!   DELETE /api/a/{name}/computer     detach it
//!   POST /api/a/{name}/test           test controls (dev fleets: AGENT_TEST_HOOKS=allow)
//!
//! A listened-to channel's records arrive at `POST /api/a/{name}/inbox/{token}`
//! (the fragment's delivery; the token is the capability). A message from
//! someone else starts a turn in that chat's conversation (or steers or
//! waits, as above); a stop from the running turn's starter stops it. The
//! turn's last answer is posted to the chat's channel, naming its turn,
//! when the channel takes posts (the chat template's, docs/api.md), and
//! goes through the fragment's reply operation otherwise (chats made
//! before). Such a turn also posts its progress to the chat's `work`
//! channel (progress.rs).

mod computer;
mod fleet;
mod js;
mod keys;
mod model;
mod progress;
mod store;
mod tools;
mod turn;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use fragment_core::body::{LimitedBody, TooLarge};
use fragment_core::npub;
use fragment_core::work::{self, Said};
use fragment_proto::limits::AGENT_STATE_WAIT_MS_MAX;
use fragment_proto::{valid_channel_name, valid_fragment_name, valid_op_name, AgentState, Delivery, ErrorCode, FragmentList, IdentityView, TurnOutcome};
use futures::{StreamExt, TryStreamExt};
use goose_provider_types::conversation::message::{Message, MessageContent};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use worker::wasm_bindgen;
use worker::{durable_object, event, DurableObject, Env, Headers, Method, Request, RequestInit, Response, State};

use crate::computer::Computer;
use crate::fleet::Fleet;
use crate::progress::{Progress, Shape};
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
/// The channels one agent follows (the desktop's own limit on a person's
/// fragments).
const LISTENS_MAX: usize = 500;
/// Listens whose fragment the agent is no longer in, checked (and dropped
/// when it answers 404 or 403) as a new listen is made, at most.
const PRUNE_CHECKS_MAX: usize = 16;
const PRUNE_CHECKS_AT_ONCE: usize = 4;
/// Messages waiting while a turn runs (steers and turns of their own).
const WAITING_MAX: usize = 64;
/// The anonymous messages the owner's view keeps, newest.
const IGNORED_KEEP: i64 = 32;
/// Turns one driver runs back to back before it rests and leaves the next
/// to the alarm.
const TURNS_PER_DRIVER_MAX: usize = 256;
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
    /// A shorter deadline for each model call, so a test's slow model
    /// outlasts it in seconds (model.rs `DEADLINE_MS`).
    model_timeout_ms: Option<u64>,
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
/// chat's reply operation is not among its tools (tools.rs `listens`).
const TURN_NOTES: &str = "Your answer to a chat message is posted to that chat for you. You act for the person who \
     asked: you reach only what they may (platform__list_fragments lists it), and platform__operations and \
     platform__call reach a fragment you have no tools for.";

/// How to build an app, told on every turn with the notes above (about 400
/// tokens): without it, an agent asked for an app read other fragments'
/// files for a dozen calls to learn the format, then wrote the whole page
/// in one reply that outlasted the node's fetch (2026-09-25). Every turn,
/// not only those that look like building: "make the numbers bigger" is
/// building too, and a guess that missed costs far more than the guide.
const BUILD_GUIDE: &str = "How to build an app (a fragment):
- A fragment is a place with its own address. Visitors see site/index.html (and the other files under site/), served \
as written: a page of HTML, CSS, and JavaScript needs nothing else. Most apps (a countdown, a calculator, a landing \
page) are that one file.
- Only a page that must keep shared data or update live needs operations: fragment.json declares them (keep its name \
and meta; add \"operations\": {\"add\": {\"kind\": \"mutation\", \"role\": \"public\", \"input\": <a JSON Schema>}}, kind \
query or mutation), app.mjs runs them (export class App extends DurableObject, from \"cloudflare:workers\", with one method \
per operation taking (input, call), its data in this.ctx.storage.sql), and the page calls them: import * as fragment from \
\"./__fragment.js\", then fragment.call(\"add\", {...}) or fragment.live(\"list\", {}, onResult).
- Templates: blank (one page: the usual start), todo (a live list, the example of operations), inbox, chat. Make a \
fragment with platform__create_fragment, or use the one the person names (<label>.<their username>).
- Then platform__write_file for each file, platform__deploy (nothing is live until you deploy), and answer with its URL.
- Do not read other fragments or templates to learn this format: it is all here. Read a fragment's files only to change \
what is in them.
- Be quick: write a whole first version at once, deploy it, then improve it.
- A reply is cut off after about 4,000 tokens. One file per call, each under 150 lines (6 KB); put the rest of a longer \
file in more calls to platform__append_file. What a cut-off call wrote is kept, and you are told where it stops.";

fn default_instructions(name: &str) -> String {
    let name = name.split('.').next().unwrap_or(name);
    format!(
        "You are {name}, an agent. Most of your tools are operations of the fragments you belong to: shared places such \
         as an app, a list, or a chat. The platform__ tools make new fragments for your owner and change their files. \
         Do what you are asked, one call at a time, and when the work is done answer in one short sentence."
    )
}

/// The default before the build guide: it told the model to read the todo
/// template's files for the shape, which the guide says not to do. An agent
/// made with it (stored at creation) gets today's default instead.
fn default_instructions_before_the_guide(name: &str) -> String {
    let name = name.split('.').next().unwrap_or(name);
    format!(
        "You are {name}, an agent. Most of your tools are operations of the fragments you belong to: shared places such \
         as an app, a list, or a chat. The platform__ tools make new fragments for your owner and change their files: \
         when asked for an app, make one, read the todo template's files for the shape, write yours, deploy it, and say \
         where it is. Do what you are asked, one call at a time, and when the work is done answer in one short sentence."
    )
}

/// What a turn tells the model: the agent's instructions, then the notes
/// and the build guide every turn gets.
fn turn_instructions(stored: Option<String>, name: &str) -> String {
    let own = match stored {
        Some(s) if s != default_instructions_before_the_guide(name) => s,
        _ => default_instructions(name),
    };
    format!("{own}\n\n{TURN_NOTES}\n\n{BUILD_GUIDE}")
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
        Ok(Fleet { base: fleet::base(&self.env)?, signer: fleet::Signer { env: self.env.clone(), sql: self.sql() }, acting_for: None })
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

    /// Starts a driver for the active turn, unless one is running: it runs
    /// that turn, answers it, and then each turn waiting, back to back.
    fn start_driver(&self, reason: &str) -> Answer<bool> {
        if self.driving.get() {
            return Ok(false);
        }
        let sql = self.sql();
        let base = var(&self.env, "OPENROUTER_API_URL").unwrap_or_else(|| "https://openrouter.ai".into()).trim_end_matches('/').to_string();
        let name = kv_get(&sql, "name")?.unwrap_or_default();
        let setup = Setup {
            base,
            model: kv_get(&sql, "model")?.unwrap_or_else(|| DEFAULT_MODEL.into()),
            instructions: turn_instructions(kv_get(&sql, "instructions")?, &name),
            deadline_ms: match kv_u64(&sql, "test_model_timeout_ms")? {
                0 => model::DEADLINE_MS,
                ms => ms,
            },
            fleet: self.fleet()?,
            env: self.env.clone(),
            storage: Rc::new(self.state.storage()),
            id: format!("{}:{reason}", self.booted_at),
        };
        // a stop that came before this driver is honored
        let mut token = CancellationToken::new();
        if kv_u64(&sql, "cancel")? == 1 {
            token.cancel();
        }
        *self.cancel.borrow_mut() = Some(token.clone());
        self.driving.set(true);
        let driving = self.driving.clone();
        let cancel_slot = self.cancel.clone();
        self.state.wait_until(async move {
            let sql = setup.storage.sql();
            let mut resting = true;
            // bounded: TURNS_PER_DRIVER_MAX turns, then the alarm starts the next driver
            for pass in 0..TURNS_PER_DRIVER_MAX {
                let outcome = run_turn(&setup, token.clone()).await;
                // No await from here to the end of the pass: a message that
                // lands meanwhile finds either the turn active (and steers
                // it or waits: both are read below) or the agent at rest
                // with no driver (and starts one).
                if let Err(error) = finish_turn(&sql, &outcome) {
                    worker::console_error!("finishing the turn failed: {error}");
                }
                match next_turn(&sql) {
                    Ok(true) if pass + 1 < TURNS_PER_DRIVER_MAX => {
                        token = CancellationToken::new();
                        *cancel_slot.borrow_mut() = Some(token.clone());
                    }
                    Ok(true) => resting = false,
                    Ok(false) => break,
                    Err(error) => {
                        worker::console_error!("starting the next turn failed: {error}");
                        break;
                    }
                }
            }
            cancel_slot.borrow_mut().take();
            driving.set(false);
            // Issued before anything else runs: a driver started after it
            // arms its own watchdog after this lands.
            let _ = match resting {
                true => setup.storage.delete_alarm().await,
                false => setup.storage.set_alarm(std::time::Duration::from_millis(50)).await,
            };
        });
        Ok(true)
    }

    /// The owner's own message (`POST turns`).
    fn turn(&self, owner: &str, body: TurnBody) -> Answer<Value> {
        self.begin(store::DIRECT, owner, body.text)
    }

    /// A message for `conv` from `asker`: it starts a turn when none runs,
    /// steers the running one when that is `asker`'s in `conv`, and waits
    /// for a turn of its own otherwise.
    fn begin(&self, conv: &str, asker: &str, text: String) -> Answer<Value> {
        if text.is_empty() || text.len() > MESSAGE_TEXT_MAX {
            return Err(Fail::invalid(format!("text is 1-{MESSAGE_TEXT_MAX} bytes")));
        }
        assert!(npub::is_identity(asker), "a turn's asker is an identity");
        let sql = self.sql();
        if kv_u64(&sql, "active")? == 1 {
            if waiting(&sql)? >= WAITING_MAX {
                return Err(Fail::new(ErrorCode::RateLimited, format!("at most {WAITING_MAX} messages wait for the agent; try again when it is done")));
            }
            let running = (kv_get(&sql, "turn_conv")?.unwrap_or_default(), kv_get(&sql, "turn_asker")?.unwrap_or_default());
            if running.0 == conv && running.1 == asker {
                store::steer(&sql, &text)?;
                return Ok(json!({ "steered": true, "driving": self.driving.get() }));
            }
            wait(&sql, conv, asker, &text)?;
            return Ok(json!({ "queued": true, "driving": self.driving.get() }));
        }
        // at rest: the oldest message waiting goes first (a driver drains
        // them before it rests, so there is none unless one died between)
        wait(&sql, conv, asker, &text)?;
        next_turn(&sql)?;
        let started = self.start_driver("turn")?;
        Ok(json!({ "started": started }))
    }

    /// Follows a channel, once: listening again to the same fragment's
    /// channel (a retry, or a chat's alarm joining its owner's agent once
    /// more) reuses its token, so its inbox URL is the same and the fragment
    /// answers the one subscription it holds for that URL (or makes it
    /// again, if it was dropped). A new listen first drops the listens of
    /// fragments the agent is no longer in (`prune_listens`). Its row is
    /// written before the request, with no await between the read and the
    /// write, so a listen racing it finds the token too.
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
        let same = |sql: &worker::SqlStorage| -> Answer<Option<String>> {
            let rows: Vec<Listening> = sql
                .exec("SELECT token FROM listens WHERE fragment = ? AND channel = ? ORDER BY created_at LIMIT 1", vec![body.fragment.as_str().into(), channel.as_str().into()])?
                .to_array()?;
            Ok(rows.into_iter().next().map(|l| l.token))
        };
        if same(&sql)?.is_none() {
            // housekeeping: a prune that fails leaves the listens as they were
            if let Err(e) = self.prune_listens().await {
                worker::console_warn!("pruning listens: {}", e.message);
            }
        }
        let token = match same(&sql)? {
            Some(token) => token,
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
            // a listen that never subscribed leaves nothing behind, nor one
            // on a fragment that is gone (404) or that it is not in (403)
            sql.exec("DELETE FROM listens WHERE token = ? AND (sub = 0 OR ?)", vec![token.as_str().into(), (matches!(status, 403 | 404) as i64).into()])?;
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

    /// Drops the listens of fragments the agent is no longer in. Those its
    /// memberships leave out (its `Principal` index, which can trail a
    /// join) are asked, at most PRUNE_CHECKS_MAX of them: one that answers
    /// 404 (it is gone) or 403 (the agent is not in it) loses its listens.
    /// Answers how many fragments did.
    async fn prune_listens(&self) -> Answer<usize> {
        let fleet = self.fleet()?;
        let mine: FragmentList = fleet.get_as("/api/fragments").await?;
        let member: HashSet<String> = mine.fragments.into_iter().map(|f| f.name).collect();
        #[derive(Deserialize)]
        struct Row {
            fragment: String,
        }
        let rows: Vec<Row> = self.sql().exec("SELECT DISTINCT fragment FROM listens ORDER BY fragment", None)?.to_array()?;
        let unlisted: Vec<String> = rows.into_iter().map(|r| r.fragment).filter(|f| !member.contains(f)).take(PRUNE_CHECKS_MAX).collect();
        let gone: Vec<String> = futures::stream::iter(unlisted)
            .map(|fragment| {
                let fleet = fleet.clone();
                async move {
                    let status = fleet.call(Method::Get, &format!("/api/f/{fragment}/status"), None).await.map(|(status, _)| status);
                    matches!(status, Ok(403 | 404)).then_some(fragment)
                }
            })
            .buffer_unordered(PRUNE_CHECKS_AT_ONCE)
            .filter_map(|gone| async move { gone })
            .collect()
            .await;
        let sql = self.sql();
        for fragment in &gone {
            sql.exec("DELETE FROM listens WHERE fragment = ?", vec![fragment.as_str().into()])?;
        }
        Ok(gone.len())
    }

    /// A delivery from a followed channel: someone else's message starts a
    /// turn in that chat's conversation (or steers, or waits: `begin`). The
    /// agent's own, and a record heard before, are acknowledged and
    /// ignored; so is a message from someone not signed in (an anonymous
    /// visitor holding a chat's link), which the owner's view notes. A
    /// record of another kind is never a message: a stop (`{kind: "stop",
    /// turn}`, the chat page's Stop) stops the running turn when it comes
    /// from that turn's starter, in its chat; any other is ignored.
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
        let signed_in = npub::is_identity(&record.principal);
        let said = work::said(record.body.get());
        // a message that cannot wait now is refused before it is heard, so
        // its redelivery is heard
        let message = matches!(said, Said::Message(_));
        if message && signed_in && kv_u64(&sql, "active")? == 1 && waiting(&sql)? >= WAITING_MAX {
            return Err(Fail::new(ErrorCode::RateLimited, format!("at most {WAITING_MAX} messages wait for the agent")));
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
        let said = match said {
            Said::Message(text) => text,
            Said::Stop { turn } => return self.stop_from(&store::conv_of(fragment, channel), &record.principal, turn.as_deref()),
            Said::Other => return Ok(json!({ "ignored": "kind" })),
        };
        if !signed_in {
            sql.exec(
                "INSERT INTO ignored (fragment, channel, principal, at) VALUES (?, ?, ?, ?)",
                vec![fragment.into(), channel.into(), record.principal.as_str().into(), now.into()],
            )?;
            sql.exec("DELETE FROM ignored WHERE seq <= (SELECT MAX(seq) FROM ignored) - ?", vec![IGNORED_KEEP.into()])?;
            return Ok(json!({ "ignored": "anonymous" }));
        }
        // a chat's message says `{text}`; any other body is heard as its JSON
        let who: String = record.principal.chars().take(12).collect();
        let mut text = format!("[{fragment} · {who}] {said}");
        if text.len() > MESSAGE_TEXT_MAX {
            text.truncate(text.floor_char_boundary(MESSAGE_TEXT_MAX));
        }
        self.begin(&store::conv_of(fragment, channel), &record.principal, text)
    }

    /// A stop posted in a chat: it stops the running turn only when that
    /// turn is this chat's, `principal` started it (ROADMAP decision 17: the
    /// asker's turn is theirs), and it names that turn (or none).
    fn stop_from(&self, conv: &str, principal: &str, turn: Option<&str>) -> Answer<Value> {
        let sql = self.sql();
        let running = kv_u64(&sql, "active")? == 1
            && kv_get(&sql, "turn_conv")?.as_deref() == Some(conv)
            && kv_get(&sql, "turn_asker")?.as_deref() == Some(principal)
            && turn.is_none_or(|t| kv_get(&sql, "turn_id").ok().flatten().as_deref() == Some(t));
        if !running {
            return Ok(json!({ "ignored": "stop" }));
        }
        self.stop()
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
        let deadline = body.model_timeout_ms.unwrap_or(0);
        if deadline != 0 && !(model::DEADLINE_MS_MIN..=model::DEADLINE_MS).contains(&deadline) {
            return Err(Fail::invalid(format!("model_timeout_ms is {}-{} (0: the product's)", model::DEADLINE_MS_MIN, model::DEADLINE_MS)));
        }
        kv_set(&sql, "test_model_timeout_ms", deadline)?;
        Ok(json!({ "ok": true }))
    }

    /// The owner's own turn's state, as `state` answers it: active while
    /// one runs or waits (behind a chat's), and its conversation's outcome
    /// and answer.
    fn turn_state(&self) -> Answer<AgentState> {
        let sql = self.sql();
        let parse = |stored: &str| TurnOutcome::parse(stored).ok_or_else(|| Fail::host(format!("corrupt state: outcome {stored:?}")));
        let running = kv_u64(&sql, "active")? == 1 && kv_get(&sql, "turn_conv")?.is_none_or(|c| c == store::DIRECT);
        let queued: Vec<Value> = sql.exec("SELECT seq FROM pending WHERE conv = ? LIMIT 1", vec![store::DIRECT.into()])?.to_array()?;
        #[derive(Deserialize)]
        struct Ended {
            outcome: String,
            error: String,
        }
        let ended: Vec<Ended> = sql.exec("SELECT outcome, error FROM convs WHERE conv = ?", vec![store::DIRECT.into()])?.to_array()?;
        let (outcome, error) = match ended.into_iter().next() {
            _ if running || !queued.is_empty() => (Some(TurnOutcome::Running), None),
            Some(ended) => (Some(parse(&ended.outcome)?), Some(ended.error)),
            // an agent whose turns ended before conversations were kept apart
            None => match kv_get(&sql, "outcome")?.unwrap_or_default().as_str() {
                "" => (None, None),
                stored => (Some(parse(stored)?), kv_get(&sql, "last_error")?),
            },
        };
        let answer = last_answer(&sql, store::DIRECT)?.map(|m| m.as_concat_text());
        let error = error.filter(|e| !e.is_empty());
        Ok(AgentState { active: running || !queued.is_empty(), driving: self.driving.get(), outcome, error, answer })
    }

    /// `state?wait_ms=`: the owner's turn's state once it is not active, or
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
            .map(|(message, conv)| {
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
                    "conversation": conv,
                })
            })
            .collect();
        // the newest rows of a table, oldest first
        let table = |columns: &str, table: &str| -> Answer<Vec<Value>> {
            let query = format!("SELECT {columns} FROM (SELECT * FROM {table} ORDER BY seq DESC LIMIT ?) ORDER BY seq");
            Ok(sql.exec(&query, vec![(rows as i64).into()])?.to_array::<Value>()?)
        };
        let get = |k: &str| kv_get(&sql, k).map(|v| v.unwrap_or_default());
        let newest = |query: &str| -> Answer<Vec<Value>> { Ok(sql.exec(query, vec![(rows as i64).into()])?.to_array::<Value>()?) };
        let listens: Vec<Value> = sql.exec("SELECT COUNT(*) AS n FROM listens", None)?.to_array()?;
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
            "conversation": get("turn_conv")?,
            "asker": get("turn_asker")?,
            "waiting": table("conv AS conversation, asker, at", "pending")?,
            "conversations": newest("SELECT conv AS conversation, outcome, error, asker, at FROM convs ORDER BY at DESC LIMIT ?")?,
            "listens": {
                "count": listens.first().map_or(json!(0), |r| r["n"].clone()),
                "newest": newest("SELECT fragment, channel, created_at AS at FROM listens ORDER BY created_at DESC LIMIT ?")?,
            },
            "ignored": table("fragment, channel, principal, at", "ignored")?,
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
                    (Method::Post, "turns") => self.turn(&principal, serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?),
                    (Method::Post, "stop") => self.stop(),
                    (Method::Post, "listen") => self.listen(serde_json::from_value(body).map_err(|e| Fail::invalid(format!("body: {e}")))?).await,
                    (Method::Get, "tools") => {
                        // what the owner's own turn has
                        let mut names = tools::list(self.fleet()?.acting_for(&principal), &self.sql()).await?;
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

/// What every turn one driver runs shares.
struct Setup {
    /// The model service's base URL.
    base: String,
    /// The model's name.
    model: String,
    instructions: String,
    /// One model call's deadline.
    deadline_ms: u64,
    /// The agent's own; each turn's tools act for its asker through it.
    fleet: Fleet,
    env: Env,
    storage: Rc<worker::Storage>,
    /// The driver's name in the steps it records.
    id: String,
}

/// Runs the active turn to its end, and answers it in its chat. A turn a
/// chat started posts its progress to the chat's `work` channel, when the
/// chat declares one: its start, its tool calls, and its end, whatever the
/// outcome (progress.rs).
async fn run_turn(setup: &Setup, cancel: CancellationToken) -> anyhow::Result<TurnOutcome> {
    let sql = setup.storage.sql();
    let conv = kv_get(&sql, "turn_conv")?.filter(|c| !c.is_empty()).unwrap_or_else(|| store::DIRECT.to_string());
    let asker = kv_get(&sql, "turn_asker")?.unwrap_or_default();
    if !npub::is_identity(&asker) {
        anyhow::bail!("this turn began before turns recorded who asked for them; send the message again");
    }
    // what the chat declares, read once as the turn starts (again at its
    // answer, if this read fails)
    let shape = match store::chat_of(&conv) {
        None => None,
        Some((fragment, channel)) => match progress::shape(&setup.fleet, fragment, channel).await {
            Ok(shape) => Some(shape),
            Err(e) => {
                worker::console_warn!("reading {fragment}'s channels: {e:#}");
                None
            }
        },
    };
    // a turn begun before turns had ids (a deploy mid-turn) posts no progress
    let turn = kv_get(&sql, "turn_id")?.filter(|t| !t.is_empty());
    let progress = match (store::chat_of(&conv), shape, &turn) {
        (Some((fragment, _)), Some(Shape { work: true, .. }), Some(turn)) => {
            Some(Rc::new(Progress { fleet: setup.fleet.clone(), fragment: fragment.to_string(), turn: turn.clone(), sql: sql.clone() }))
        }
        _ => None,
    };
    if let Some(p) = &progress {
        p.start(&asker).await;
    }
    let outcome = drive_and_answer(setup, cancel, &conv, asker, shape, turn.as_deref(), progress.clone()).await;
    if let Some(p) = &progress {
        p.end(&outcome).await;
    }
    outcome
}

/// The turn itself, and its answer in the chat that started it. A turn
/// that fails is never silent: it says so where its answer would go (and
/// in its conversation, so the owner's view and the next turn see it).
async fn drive_and_answer(
    setup: &Setup,
    cancel: CancellationToken,
    conv: &str,
    asker: String,
    shape: Option<Shape>,
    turn: Option<&str>,
    progress: Option<Rc<Progress>>,
) -> anyhow::Result<TurnOutcome> {
    let sql = setup.storage.sql();
    let outcome = drive_turn(setup, cancel, conv, asker, progress).await;
    match outcome {
        // a turn a chat started answers there
        Ok(TurnOutcome::Idle) => {
            reply(&sql, &setup.fleet, conv, shape, turn).await?;
            Ok(TurnOutcome::Idle)
        }
        Ok(other) => Ok(other),
        Err(error) => {
            if let Err(e) = say_failed(&sql, &setup.fleet, conv, shape, turn, &error).await {
                worker::console_warn!("saying the turn failed in {conv}: {e:#}");
            }
            Err(error)
        }
    }
}

async fn drive_turn(setup: &Setup, cancel: CancellationToken, conv: &str, asker: String, progress: Option<Rc<Progress>>) -> anyhow::Result<TurnOutcome> {
    let sql = setup.storage.sql();
    // the owner is learned before any turn can start (`registration`)
    let owner_turn = kv_get(&sql, "owner")?.is_some_and(|owner| owner == asker);
    let key = owners_key(&setup.fleet).await?;
    // the computer is its owner's: it joins its owner's turns only
    let computer = if owner_turn { open_computer(&setup.env, &sql).await? } else { None };
    let driver = Driver {
        storage: setup.storage.clone(),
        model: Model { base: setup.base.clone(), key, name: setup.model.clone(), deadline_ms: setup.deadline_ms },
        fleet: setup.fleet.clone(),
        instructions: setup.instructions.clone(),
        computer,
        cancel,
        id: setup.id.clone(),
        conv: conv.to_string(),
        asker,
        owner_turn,
        progress,
    };
    turn::drive(driver).await
}

/// What a failed turn says, as its answer would: why, and what to do.
fn failed_text(error: &anyhow::Error) -> String {
    let why = fragment_core::work::cut(&error.to_string(), fragment_core::work::ERROR_MAX_CHARS);
    format!("I couldn't finish: {why}. Ask me to try again.")
}

/// Says a failed turn's reason where its answer would go: stored as the
/// agent's message in the turn's conversation, then posted to its chat as
/// an answer is (once: its id comes from the message's). A turn whose
/// conversation already ends with the model's text (a replay after this
/// ran) posts that.
async fn say_failed(sql: &worker::SqlStorage, fleet: &Fleet, conv: &str, shape: Option<Shape>, turn: Option<&str>, error: &anyhow::Error) -> anyhow::Result<()> {
    if last_answer(sql, conv)?.is_none() {
        let mut said = Message::assistant().with_text(failed_text(error));
        said.id = Some(format!("msg_{}", uuid::Uuid::new_v4()));
        store::append_message(sql, conv, &said)?;
    }
    reply(sql, fleet, conv, shape, turn).await
}

/// Records the active turn's end (its conversation's too). The messages
/// its starter sent that it ended before reading wait for a turn of their
/// own, unless it was stopped. No await: the caller starts the next turn
/// in the same step.
fn finish_turn(sql: &worker::SqlStorage, outcome: &anyhow::Result<TurnOutcome>) -> anyhow::Result<()> {
    let (how, error) = match outcome {
        Ok(how) => (*how, String::new()),
        Err(error) => (TurnOutcome::Error, error.to_string()),
    };
    let conv = kv_get(sql, "turn_conv")?.unwrap_or_default();
    let asker = kv_get(sql, "turn_asker")?.unwrap_or_default();
    let now = js::now_ms() as i64;
    kv_set(sql, "outcome", how.as_str())?;
    kv_set(sql, "last_error", &error)?;
    kv_set(sql, "active", 0)?;
    kv_set(sql, "turn_ended_at", now)?;
    sql.exec(
        "INSERT INTO convs (conv, outcome, error, asker, at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (conv) DO UPDATE SET outcome = excluded.outcome, error = excluded.error, asker = excluded.asker, at = excluded.at",
        vec![conv.as_str().into(), how.as_str().into(), error.as_str().into(), asker.as_str().into(), now.into()],
    )?;
    if how != TurnOutcome::Stopped && npub::is_identity(&asker) {
        sql.exec(
            "INSERT INTO pending (conv, asker, text, at) SELECT ?, ?, text, ? FROM steer WHERE consumed = 0 ORDER BY seq",
            vec![conv.as_str().into(), asker.as_str().into(), now.into()],
        )?;
    }
    sql.exec("DELETE FROM steer WHERE consumed = 0", None)?;
    Ok(())
}

/// Starts the oldest message waiting as a turn of its own; `false` when
/// none waits. No await.
fn next_turn(sql: &worker::SqlStorage) -> anyhow::Result<bool> {
    #[derive(Deserialize)]
    struct Waiting {
        seq: i64,
        conv: String,
        asker: String,
        text: String,
    }
    let rows: Vec<Waiting> = sql.exec("SELECT seq, conv, asker, text FROM pending ORDER BY seq LIMIT 1", None)?.to_array()?;
    let Some(next) = rows.into_iter().next() else { return Ok(false) };
    sql.exec("DELETE FROM pending WHERE seq = ?", vec![next.seq.into()])?;
    // the steers an earlier turn's model read are in its conversation, and
    // the images it kept and never showed are dropped
    sql.exec("DELETE FROM steer", None)?;
    sql.exec("DELETE FROM shots", None)?;
    let id = format!("msg_{}", uuid::Uuid::new_v4());
    let mut kickoff = Message::user().with_text(next.text);
    kickoff.id = Some(id.clone());
    store::append_message(sql, &next.conv, &kickoff)?;
    // the turn's id in its chat's records, where its messages start, and
    // how many of its calls its progress has posted (progress.rs)
    kv_set(sql, "turn_id", fragment_core::work::turn_id(&id))?;
    kv_set(sql, "turn_seq", store::message_seq(sql, &id)?)?;
    kv_set(sql, "work_posted", 0)?;
    kv_set(sql, "turn_conv", &next.conv)?;
    kv_set(sql, "turn_asker", &next.asker)?;
    kv_set(sql, "active", 1)?;
    kv_set(sql, "cancel", 0)?;
    kv_set(sql, "last_error", "")?;
    kv_set(sql, "turn_started_at", js::now_ms())?;
    kv_set(sql, "outcome", TurnOutcome::Running.as_str())?;
    Ok(true)
}

/// Puts a message in line for a turn of its own.
fn wait(sql: &worker::SqlStorage, conv: &str, asker: &str, text: &str) -> anyhow::Result<()> {
    sql.exec("INSERT INTO pending (conv, asker, text, at) VALUES (?, ?, ?, ?)", vec![conv.into(), asker.into(), text.into(), (js::now_ms() as i64).into()])?;
    Ok(())
}

/// The messages waiting while a turn runs: its steers not read yet, and
/// those in line for turns of their own.
fn waiting(sql: &worker::SqlStorage) -> anyhow::Result<usize> {
    #[derive(Deserialize)]
    struct Count {
        n: i64,
    }
    let rows: Vec<Count> =
        sql.exec("SELECT (SELECT COUNT(*) FROM pending) + (SELECT COUNT(*) FROM steer WHERE consumed = 0) AS n", None)?.to_array()?;
    Ok(rows.first().map_or(0, |c| c.n.max(0) as usize))
}

/// Posts a chat turn's last answer to its fragment, once (the id comes
/// from the message): to the chat's channel, naming its turn, when the
/// channel takes posts; else through the listen's reply operation, as
/// chats made before postable channels answer. The owner's own
/// conversation has nowhere to post. A chat that is gone (404), or that no
/// longer has the agent (403), drops its listen.
async fn reply(sql: &worker::SqlStorage, fleet: &Fleet, conv: &str, shape: Option<Shape>, turn: Option<&str>) -> anyhow::Result<()> {
    let Some((fragment, channel)) = store::chat_of(conv) else { return Ok(()) };
    #[derive(Deserialize)]
    struct Listen {
        reply: String,
    }
    let rows: Vec<Listen> = sql.exec("SELECT reply FROM listens WHERE fragment = ? AND channel = ?", vec![fragment.into(), channel.into()])?.to_array()?;
    // a listen dropped since: nowhere to answer
    let Some(listen) = rows.into_iter().next() else { return Ok(()) };
    // a turn that ended without text has nothing to say
    let Some(answer) = last_answer(sql, conv)? else { return Ok(()) };
    let id = fragment_core::tools::reply_id(answer.id.as_deref().unwrap_or(""));
    let mut text = answer.as_concat_text();
    // the turn's images (a screenshot) land in the chat's files, shown with the answer
    let shots: Vec<Value> = sql.exec("SELECT seq, mime, data FROM shots ORDER BY seq", None)?.to_array()?;
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
    let shape = match shape {
        Some(shape) => shape,
        None => progress::shape(fleet, fragment, channel).await?,
    };
    let (path, body) = match shape.posts {
        true => (format!("/api/f/{fragment}/channels/{channel}"), json!({ "id": id, "body": fragment_core::work::answer(&text, turn) })),
        false => (format!("/api/f/{fragment}/ops/{}", listen.reply), json!({ "id": id, "input": { "text": text } })),
    };
    let (status, body) = fleet.call(Method::Post, &path, Some(&body)).await?;
    if matches!(status, 403 | 404) {
        sql.exec("DELETE FROM listens WHERE fragment = ?", vec![fragment.into()])?;
    }
    if status != 200 {
        anyhow::bail!("posting the answer to {path}: {status} {}", fleet::message(&body));
    }
    sql.exec("DELETE FROM shots", None)?;
    Ok(())
}

impl DurableObject for Agent {
    fn new(state: State, env: Env) -> Self {
        let sql = state.storage().sql();
        if let Err(error) = sql.exec(store::SCHEMA, None) {
            worker::console_error!("schema: {error}");
        }
        if let Err(error) = store::migrate(&sql) {
            worker::console_error!("migrating the schema: {error}");
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
        let failed = |e: anyhow::Error| worker::Error::RustError(e.to_string());
        let active = kv_u64(&sql, "active").map_err(failed)? == 1;
        if active {
            // The watchdog: a driver that died with its node is replaced
            // here and resumes from the last applied step. A live driver
            // gets the wake re-armed, so one always stays pending.
            match self.start_driver("alarm") {
                Ok(true) => {
                    let restarts = kv_u64(&sql, "watchdog_restarts").unwrap_or(0);
                    let _ = kv_set(&sql, "watchdog_restarts", restarts + 1);
                }
                Ok(false) => turn::arm_watchdog(&self.state.storage(), &sql).await.map_err(failed)?,
                Err(f) => {
                    let _ = kv_set(&sql, "outcome", TurnOutcome::Error.as_str());
                    let _ = kv_set(&sql, "last_error", &f.message);
                    let _ = kv_set(&sql, "active", 0);
                }
            }
        } else if next_turn(&sql).map_err(failed)? {
            // a message waiting with no driver (one rested after its most
            // turns, or died between them)
            if let Err(f) = self.start_driver("waiting") {
                worker::console_error!("starting a waiting turn: {}", f.message);
            }
        }
        Response::ok("ok")
    }
}
