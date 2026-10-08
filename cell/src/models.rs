//! The model route (docs/cloudflare-v1.md, decision 23; "Lessons from
//! cloudflare/agents" 7 and 8; spike S4): one OpenAI-shaped chat
//! completion on a tier's model, metered on its payer's ledger.
//!
//! Every call reserves its worst case on the payer's ledger first (the
//! request's bytes as tokens in, the tier's capped `max_tokens` out), under
//! a reference of its own (`aig:<hex>`), so a call the payer cannot cover
//! never reaches the model. Then it is made, bounded (`fragment_core::models`:
//! the tier's model, the capped `max_tokens`, GLM's `reasoning_effort`
//! clamped, none of the client's headers), and settled with its last,
//! cumulative usage (input less what was cached). A call that failed
//! before it used anything is released; one whose stream broke settles
//! without usage, at its worst case. Nothing of the request or its answer
//! is kept: only the usage, on the ledger.
//!
//! Two transports, one input. Production calls the Worker's `AI` binding
//! through the deployment's AI Gateway (`AI_GATEWAY_ID`), its logs off and
//! its metadata opaque ids (`opaque`). Dev and the e2e set
//! `FRAGMENT_AI_URL` instead, and the same input is POSTed to
//! `<url>/run/<model>`, its answer read the same way: a lower-rung fake at
//! the vendor boundary (crates/fakes, `workers_ai`), never product proof.
//! A job's image and decision steps call their models on the same
//! transport (`run`), and its text step reads its answer as it streams
//! (`streamed`).
//!
//! A streamed call is one call on its tier's model, waiting in its queue
//! when the model is busy, read to its first data line (its `first_ms`):
//! one that failed for now before that (a 429, a 5xx, a stream that broke
//! or ended) is made once more at once, under the same reservation, which
//! it never used (`streamed`). (A race of a second call on another model,
//! and a ladder of fallbacks for a busy model, were measured and removed:
//! docs/optchat.md, "Latency".)
//!
//! Who calls: an agent, `POST /api/models/v1/chat/completions` (`route`),
//! signed by the agent (its computer's model intercept signs it), `for`
//! naming whom it acts for and `fragment` the turn's fragment; `complete`
//! is the call itself. The payer is the agent's owner (decision 36).
//! A call names a tier, or `vision`: the deployment's vision model
//! (`FRAGMENT_VISION_MODEL`, config.rs), for a runtime's calls about an
//! image (its computer's screenshots), metered the same way.
//! A fragment someone else owns is asked whether it is still open under its
//! cap first (decision 26).
//!
//! Beside it, the decision route, `POST /api/models/v1/decide`
//! (`decide_route`): the same caller, payer and fragment, a body that is a
//! job's `ai.decide` input (fragment_core::decide, `clef-flash` unless it
//! names `clef`), and the same reserve, call and settle, its worst case
//! its input's bytes as tokens, its answer the step's `{answers, model,
//! usage}`.

use std::pin::Pin;

use fragment_core::decide;
use fragment_core::ledger::{Release, Reserve, Settle, Spend};
use fragment_core::models::{self as bounds, Bounded, Named, Stream};
use fragment_core::price::Usage;
use fragment_proto::{valid_fragment_name, ErrorCode, IdentityKind};
use futures_util::future::LocalBoxFuture;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::ledger::{self, FragmentOpen};
use crate::{js, read_body, routed};

/// A call's request (`fragment_core::models`): the largest image and its
/// prompt fit.
pub use fragment_core::models::MODEL_BODY_MAX_BYTES;
/// An unstreamed answer, or a refusal, read whole: at most `MAX_TOKENS`
/// of text and its JSON.
const ANSWER_MAX_BYTES: usize = 8 * 1024 * 1024;
/// A model's refusal, passed on: what a client reads of one (a message,
/// not a document).
const REFUSAL_MAX_CHARS: usize = 4096;

/// The fragment a call is spent in: its name and owner, and whether the
/// spender is that owner, or an agent of theirs acting for them (caps
/// stop everyone else's: decision 26).
pub struct InFragment<'a> {
    pub name: &'a str,
    pub owner: &'a str,
    pub by_owner: bool,
}

/// One model call: who pays, which agent makes it, where, on which tier
/// (or the vision model), the client's OpenAI-shaped body, and whether it
/// streams.
pub struct ModelCall<'a> {
    /// The person whose ledger pays: the agent's owner.
    pub payer: &'a str,
    /// The agent making the call (its identity), when one does.
    pub agent: Option<&'a str>,
    pub fragment: Option<InFragment<'a>>,
    pub named: Named,
    pub body: Value,
    pub stream: bool,
}

/// Work a call leaves to finish after its answer has started (a streamed
/// answer's settle): the router's `Context`, or a Durable Object's `State`.
pub trait Background {
    fn later(&self, work: LocalBoxFuture<'static, ()>);
}

impl Background for Context {
    fn later(&self, work: LocalBoxFuture<'static, ()>) {
        self.wait_until(work);
    }
}

impl Background for State {
    fn later(&self, work: LocalBoxFuture<'static, ()>) {
        self.wait_until(work);
    }
}

/// The opaque id the AI Gateway's metadata carries for an identity: the
/// first 16 hex of its SHA-256, never a name or an email (spike S4: the
/// gateway keeps a metadata row per call even with its logs off).
pub fn opaque(identity: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(identity.as_bytes());
    hex::encode(&digest[..8])
}

fn refused(why: bounds::Refusal) -> CellError {
    CellError::invalid(why.message())
}

/// A call's hold on its payer's ledger, until it settles or is released.
struct Held {
    env: Env,
    payer: String,
    reference: String,
    model: &'static str,
    /// What the call named, for its log lines: a tier, `vision`, or `decide`.
    named: &'static str,
    /// When it was held, just before the call was made: its log line says
    /// how long the call took.
    t0: i64,
}

impl Held {
    /// The call's end, charged once: its usage, or its worst case when it
    /// reported none (or the stream broke). A settle that does not land is
    /// left held, and the ledger's sweep charges it at its worst case after
    /// six hours: the money path fails closed.
    async fn settle(&self, usage: Option<Usage>, log_id: Option<String>) {
        self.settle_timed(usage, log_id, None).await;
    }

    /// `settle`, its log line saying when the answer's first data line came
    /// (`first_ms`, a streamed answer's) and how long the call took.
    async fn settle_timed(&self, usage: Option<Usage>, log_id: Option<String>, first_ms: Option<i64>) {
        let ms = js::now_ms() - self.t0;
        // its tokens (in, cached, out), for the latency they explain
        let tokens = match &usage {
            Some(Usage::Tokens { model, input, cached_input, output, .. }) => json!([model, input, cached_input, output]),
            _ => Value::Null,
        };
        let settle = Settle { reference: self.reference.clone(), usage };
        match ledger::retried(&self.env, &self.payer, &settle).await {
            // one line per event: `wrangler tail` drops lines (lesson 14)
            Ok(settled) => console_log!("{}", json!({ "event": "model.settled", "ref": self.reference, "logId": log_id, "tier": self.named, "model": self.model, "charge": settled.charge, "basis": settled.basis, "first_ms": first_ms, "ms": ms, "tokens": tokens })),
            Err(e) => console_error!("{}", json!({ "event": "model.settle-failed", "ref": self.reference, "logId": log_id, "message": e.message })),
        }
    }

    /// The call failed before it used anything: its reservation goes back.
    async fn release(&self, why: &str) {
        match ledger::retried(&self.env, &self.payer, &Release { reference: self.reference.clone() }).await {
            Ok(_) => console_log!("{}", json!({ "event": "model.released", "ref": self.reference, "why": why })),
            Err(e) => console_error!("{}", json!({ "event": "model.release-failed", "ref": self.reference, "why": why, "message": e.message })),
        }
    }
}

/// Holds a call's worst case on its payer's ledger, under a reference of
/// its own (`aig:<hex>`), as an agent's turn. Whose cap applies: the
/// payer's own fragment's, on its ledger; another owner's, asked of
/// theirs first, unless the spender is that owner's (decision 26).
async fn hold(env: &Env, payer: &str, agent: Option<&str>, fragment: Option<&InFragment<'_>>, model: &'static str, named: &'static str, worst: Usage) -> CellResult<Held> {
    let (fragment, capped) = match fragment {
        Some(f) if f.owner == payer => (Some(f.name.to_string()), !f.by_owner),
        Some(f) => {
            if !f.by_owner {
                ledger::retried(env, f.owner, &FragmentOpen { fragment: f.name.to_string() }).await?;
            }
            (None, false)
        }
        None => (None, false),
    };
    let reference = format!("aig:{}", js::random_hex::<16>());
    let reserve = Reserve { reference, spend: Spend::AgentTurn, worst, fragment, agent: agent.map(str::to_string), capped };
    ledger::hold(env, payer, &reserve).await?;
    Ok(Held { env: env.clone(), payer: payer.to_string(), reference: reserve.reference, model, named, t0: js::now_ms() })
}

/// One model call, metered (the module's doc): its answer as the client
/// reads it, OpenAI's shape. A streamed answer is settled after it ends,
/// by `after`, whether or not the client read it to its end. A tier's call
/// runs on the payer's choice for their agents (`hands`): another tier, or
/// their own provider (providers/mod.rs), unless its spender is someone
/// else in the payer's fragment.
pub async fn complete(env: &Env, mut call: ModelCall<'_>, after: &dyn Background) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let body_bytes = serde_json::to_vec(&call.body).expect("a JSON value serializes").len();
    if body_bytes > MODEL_BODY_MAX_BYTES {
        return Err(CellError::too_large("a model call", body_bytes, MODEL_BODY_MAX_BYTES));
    }
    if let Named::Tier(named) = call.named {
        let own_spend = call.fragment.as_ref().is_none_or(|f| f.owner != call.payer || f.by_owner);
        match crate::providers::chosen(env, call.payer, fragment_core::providers::Role::Hands, named, own_spend).await? {
            crate::providers::Chosen::Fragment(tier) => call.named = Named::Tier(tier),
            crate::providers::Chosen::Own(own) => return crate::providers::complete(env, call, own, after).await,
        }
    }
    let model = bounds::capped(call.named, cfg.vision_model.as_str()).map_err(refused)?;
    let bounded = bounds::bound(model, call.body, call.stream).map_err(refused)?;
    let named = call.named.as_str();
    let held = hold(env, call.payer, call.agent, call.fragment.as_ref(), bounded.model, named, bounded.worst(body_bytes)).await?;
    let meta = Metadata::of(call.payer, call.agent);
    if bounded.stream {
        let s = streamed(env, &bounded, &meta).await;
        return answer_streamed(held, s, after);
    }
    let mut upstream = match transport(env, cfg, bounded.model, &bounded.input, &meta, None).await {
        Ok(r) => r,
        Err(e) => {
            held.release("the model was not reached").await;
            return Err(e);
        }
    };
    let status = upstream.status_code();
    let log_id = upstream.headers().get("cf-aig-log-id")?;
    if status != 200 {
        // a refusal used nothing: the vendor's answer, as it came
        let text = bounded_text(&mut upstream).await;
        held.release(&format!("the model answered {status}")).await;
        return refusal_response(status, text);
    }
    let bytes = match read_whole(&mut upstream).await {
        Ok(b) => b,
        Err(e) => {
            // what it used is unknown: its worst case
            held.settle(None, log_id).await;
            return Err(e);
        }
    };
    let usage = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| bounds::usage_of(bounded.model, &v["usage"]));
    held.settle(usage, log_id).await;
    let mut resp = Response::from_bytes(bytes)?;
    resp.headers_mut().set("content-type", "application/json")?;
    Ok(resp)
}

/// A refusal passed on as the vendor gave it.
fn refusal_response(status: u16, text: String) -> CellResult<Response> {
    let mut resp = Response::ok(text)?.with_status(status);
    resp.headers_mut().set("content-type", "application/json")?;
    Ok(resp)
}

/// A streamed route call's answer: the client's branch, and the meter's
/// read to its end by `after`, which settles the call's hold from its
/// usage. A call that failed releases its hold (it used nothing) and
/// passes the refusal on.
fn answer_streamed(held: Held, s: Streamed, after: &dyn Background) -> CellResult<Response> {
    let first_ms = s.first_ms;
    let begun = match s.opened {
        Ok(b) => b,
        Err(Failed::Unanswered(e)) => {
            after.later(Box::pin(async move { held.release("the model was not reached").await }));
            return Err(e);
        }
        Err(Failed::Refused { status, body }) => {
            after.later(Box::pin(async move { held.release(&format!("the model answered {status}")).await }));
            return refusal_response(status, String::from_utf8_lossy(&body).chars().take(REFUSAL_MAX_CHARS).collect());
        }
    };
    let log_id = begun.log_id.clone();
    let model = held.model;
    let whole = Response::from_stream(begun.bytes())?;
    let ResponseBody::Stream(source) = whole.body() else { unreachable!("an answer made from a stream is one") };
    // one branch for the client, one read to its end for the meter: a
    // client that goes away mid-answer does not leave the call unsettled
    let (client, meter) = js::tee(source)?;
    after.later(Box::pin(async move {
        let mut stream = Stream::default();
        let mut bytes = ByteStream::from(meter);
        let mut broke = false;
        // bounded by the model's answer: at most the tier's max_tokens
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(c) => stream.push(&c, None),
                Err(_) => {
                    broke = true;
                    break;
                }
            }
        }
        stream.finish(None);
        let usage = if broke { None } else { stream.usage().and_then(|u| bounds::usage_of(model, u)) };
        held.settle_timed(usage, log_id, Some(first_ms)).await;
    }));
    let normalized = Normalized { source: Box::pin(ByteStream::from(client)), stream: Stream::default(), done: false };
    let mut resp = Response::from_stream(normalized)?;
    resp.headers_mut().set("content-type", "text/event-stream")?;
    resp.headers_mut().set("cache-control", "no-cache")?;
    Ok(resp)
}

/// An answer that began: its gateway log id, what was read of it up to its
/// first data line (`head`), the rest of it, and the fetch's controller
/// (aborting it ends the connection: one the gateway holds open after
/// `[DONE]`).
pub(crate) struct Begun {
    pub log_id: Option<String>,
    pub head: Vec<u8>,
    pub rest: ByteStream,
    pub abort: AbortController,
}

impl Begun {
    /// Its bytes from the first: the head, then the rest.
    fn bytes(self) -> impl futures_util::Stream<Item = worker::Result<Vec<u8>>> {
        futures_util::stream::once(std::future::ready(Ok(self.head))).chain(self.rest)
    }
}

/// Why a streamed call failed before it began.
pub(crate) enum Failed {
    /// The model's refusal: its status and body (read whole).
    Refused { status: u16, body: Vec<u8> },
    /// No answer: the model was not reached, or its stream broke or ended
    /// before its first data line.
    Unanswered(CellError),
}

impl Failed {
    /// Whether the same call made again may answer: a 429, a 5xx, or no
    /// answer at all.
    fn passing(&self) -> bool {
        matches!(self, Failed::Refused { status: 429 | 500..=599, .. } | Failed::Unanswered(_))
    }
}

/// A streamed call, opened: the answer that began, or why it failed; when
/// its first data line came (ms from its first call made); and how many
/// calls it took.
pub(crate) struct Streamed {
    pub opened: std::result::Result<Begun, Failed>,
    pub first_ms: i64,
    pub calls: u32,
}

/// The calls one streamed call makes at most: the second is made at once
/// when the first failed for now before it began, having used nothing (a
/// retry within its step; no race, no other model: docs/optchat.md,
/// "Latency").
const STREAM_CALLS_MAX: u32 = 2;

/// A streamed call (`bounded`, on its own model, waiting in its queue
/// when busy), read to its first data line, and made once more at once
/// when it failed for now before that (`STREAM_CALLS_MAX`). Unmetered:
/// its caller holds one reservation for it, which a call that failed
/// before it began never used.
pub(crate) async fn streamed(env: &Env, bounded: &Bounded, meta: &Metadata) -> Streamed {
    assert!(bounded.stream, "a streamed call asks for a stream");
    let cfg = Config::from_env(env);
    let t0 = js::now_ms();
    let mut calls = 0;
    // bounded by STREAM_CALLS_MAX
    loop {
        calls += 1;
        let opened = open(env, cfg, bounded, meta).await;
        match opened {
            Err(f) if f.passing() && calls < STREAM_CALLS_MAX => continue,
            opened => return Streamed { opened, first_ms: js::now_ms() - t0, calls },
        }
    }
}

/// One call, read until its first data line.
async fn open(env: &Env, cfg: &Config, bounded: &Bounded, meta: &Metadata) -> std::result::Result<Begun, Failed> {
    let abort = AbortController::default();
    let mut resp = transport(env, cfg, bounded.model, &bounded.input, meta, Some(&abort.signal())).await.map_err(Failed::Unanswered)?;
    let status = resp.status_code();
    let log_id = resp.headers().get("cf-aig-log-id").ok().flatten();
    if status != 200 {
        let body = read_whole(&mut resp).await.unwrap_or_default();
        return Err(Failed::Refused { status, body });
    }
    let unanswered = |why: String| Failed::Unanswered(CellError::new(ErrorCode::UpstreamFailed, why));
    let mut rest = resp.stream().map_err(|e| unanswered(format!("the model's answer had no stream: {e}")))?;
    let mut head = Vec::new();
    // bounded by bounds::HEAD_MAX_BYTES: `began` is true past it
    loop {
        match rest.next().await {
            Some(Ok(chunk)) => {
                head.extend_from_slice(&chunk);
                if bounds::began(&head) {
                    return Ok(Begun { log_id, head, rest, abort });
                }
            }
            Some(Err(e)) => return Err(unanswered(format!("the model's stream broke before it began: {e}"))),
            None => return Err(unanswered("the model's stream ended before it began".into())),
        }
    }
}

/// One call of a catalog model with its input, on the same transport,
/// unmetered and read whole (`call`; a job's image and decision steps:
/// ai.rs, whose caller bounds the input and meters it).
pub(crate) async fn run(env: &Env, model: &str, input: &Value, payer: &str, agent: Option<&str>) -> CellResult<(u16, Vec<u8>, Option<String>)> {
    let meta = Metadata::of(payer, agent);
    let mut resp = transport(env, Config::from_env(env), model, input, &meta, None).await?;
    let status = resp.status_code();
    let log_id = resp.headers().get("cf-aig-log-id")?;
    let bytes = read_whole(&mut resp).await?;
    Ok((status, bytes, log_id))
}

/// The client's branch of a streamed answer, in OpenAI's shape
/// (`fragment_core::models::Stream`).
struct Normalized {
    source: Pin<Box<ByteStream>>,
    stream: Stream,
    done: bool,
}

impl Metadata {
    /// A call's, for its payer and the agent making it.
    pub(crate) fn of(payer: &str, agent: Option<&str>) -> Metadata {
        let agent_id = agent.map(opaque);
        Metadata { user_id: opaque(payer), affinity: agent_id.clone(), agent_id }
    }
}

impl futures_util::Stream for Normalized {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        let this = &mut *self;
        // bounded by the source: each pass takes one chunk of it, and a
        // chunk that completes no line yields nothing and asks again
        loop {
            if this.done {
                return Poll::Ready(None);
            }
            match futures_util::ready!(this.source.as_mut().poll_next(cx)) {

                Some(Ok(chunk)) => {
                    let mut out = Vec::new();
                    this.stream.push(&chunk, Some(&mut out));
                    if !out.is_empty() {
                        return Poll::Ready(Some(Ok(out)));
                    }
                }
                Some(Err(e)) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                None => {
                    this.done = true;
                    let mut out = Vec::new();
                    this.stream.finish(Some(&mut out));
                    return Poll::Ready((!out.is_empty()).then_some(Ok(out)));
                }
            }
        }
    }
}

/// The AI Gateway's metadata for a call: opaque ids only.
#[derive(Clone)]
pub(crate) struct Metadata {
    user_id: String,
    agent_id: Option<String>,
    /// GLM's prefix-cache session (`x-session-affinity`): the agent's own.
    affinity: Option<String>,
}

/// Makes a bounded call: `input` to `model`, through the AI binding and
/// the deployment's gateway, or, in dev and the e2e, to the fake at
/// `FRAGMENT_AI_URL`; `signal` aborts it (a stream read to its end, whose
/// connection the gateway may hold open after it).
async fn transport(env: &Env, cfg: &Config, model: &str, input: &Value, meta: &Metadata, signal: Option<&AbortSignal>) -> CellResult<Response> {
    let mut metadata = json!({ "user_id": meta.user_id });
    if let Some(agent) = &meta.agent_id {
        metadata["agent_id"] = json!(agent);
    }
    // GLM caches a prefix per session: an agent's calls share theirs (lesson 8)
    let headers = match &meta.affinity {
        Some(agent) => json!({ "x-session-affinity": agent }),
        None => json!({}),
    };
    if let Some(url) = &cfg.ai_url {
        // the lower rung: the binding's input, to a fake at the vendor boundary
        let h = Headers::new();
        h.set("content-type", "application/json")?;
        h.set("x-fragment-ai-metadata", &metadata.to_string())?;
        if let Some(agent) = &meta.affinity {
            h.set("x-session-affinity", agent)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(h).with_body(Some(input.to_string().into()));
        let req = Request::new_with_init(&format!("{url}/run/{model}"), &init)?;
        let sent = match signal {
            Some(signal) => Fetch::Request(req).send_with_signal(signal).await,
            None => Fetch::Request(req).send().await,
        };
        return sent.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("the model did not answer: {e}")));
    }
    let Some(gateway) = &cfg.ai_gateway_id else {
        return Err(CellError::host("this deployment has no model route: set AI_GATEWAY_ID (its AI Gateway)"));
    };
    let options = json!({ "gateway": { "id": gateway, "metadata": metadata, "collectLog": false }, "extraHeaders": headers });
    js::ai_run(env.as_ref(), model, input, &options, signal).await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("the model did not answer: {}", e.message)))
}

/// An answer's body, at most `ANSWER_MAX_BYTES` (read no further).
async fn read_whole(resp: &mut Response) -> CellResult<Vec<u8>> {
    match crate::cs::read_answer(resp, ANSWER_MAX_BYTES).await? {
        (_, true) => Err(CellError::new(ErrorCode::UpstreamFailed, format!("the model's answer is over {ANSWER_MAX_BYTES} bytes"))),
        (bytes, false) => Ok(bytes),
    }
}

/// A refusal's body as text, cut to what a client reads (and read no further).
async fn bounded_text(resp: &mut Response) -> String {
    let (bytes, _) = crate::cs::read_answer(resp, 4 * REFUSAL_MAX_CHARS).await.unwrap_or_default();
    String::from_utf8_lossy(&bytes).chars().take(REFUSAL_MAX_CHARS).collect()
}

/// The answer a `whose` asks a fragment for: its owner, and whether the
/// agent naming it is in it.
#[derive(Deserialize)]
pub(crate) struct Whose {
    pub owner: String,
    pub member: bool,
}

/// Who makes a call on an agent's routes (`route`, `decide_route`): the
/// signer, an agent; its owner, who pays; and the `fragment` it names.
struct Caller {
    agent: String,
    owner: String,
    /// The fragment named, one the agent is in: its name, its owner, and
    /// whether that owner spends there (the agent as itself, or for its
    /// owner, in its owner's fragment).
    fragment: Option<(String, String, bool)>,
}

impl Caller {
    fn in_fragment(&self) -> Option<InFragment<'_>> {
        self.fragment.as_ref().map(|(name, owner, by_owner)| InFragment { name, owner, by_owner: *by_owner })
    }
}

/// The caller of an agent's route, checked: signed by an agent (`for`
/// names whom it acts for), naming at most one `fragment`, one it is in.
async fn caller(env: &Env, req: &Request, url: &Url, body: &[u8]) -> CellResult<Caller> {
    let agent = crate::signer_for(env, req, url, body).await?;
    if agent.kind != IdentityKind::Agent {
        return Err(CellError::new(ErrorCode::Forbidden, "the model route is an agent's: its owner pays for its calls"));
    }
    let owner = agent.owner.clone().ok_or_else(|| CellError::host("an agent without an owner"))?;
    let mut fragments = url.query_pairs().filter(|(k, _)| k == "fragment").map(|(_, v)| v.into_owned());
    let named = fragments.next();
    if fragments.next().is_some() {
        return Err(CellError::invalid("`fragment` is named once"));
    }
    let Some(name) = named else { return Ok(Caller { agent: agent.id.clone(), owner, fragment: None }) };
    if !valid_fragment_name(&name) {
        return Err(CellError::invalid("`fragment` is a fragment's name, <label>.<username>"));
    }
    let ask = routed::internal_request("meter/whose", &json!({ "principal": agent.id }).to_string())?;
    let mut answer = env.durable_object("FRAGMENT")?.get_by_name(&name)?.fetch_with_request(ask).await?;
    if answer.status_code() != 200 {
        return Err(CellError::new(ErrorCode::NotFound, format!("no fragment {name}")));
    }
    let whose: Whose = answer.json().await?;
    if !whose.member {
        return Err(CellError::new(ErrorCode::Forbidden, format!("a model call names a fragment its agent is in, and it is not in {name}")));
    }
    // the owner's own: the agent acting as itself, or for its owner, in its owner's fragment
    let for_owner = agent.acting_for.as_deref().is_none_or(|asker| asker == owner);
    let by_owner = whose.owner == owner && for_owner;
    Ok(Caller { agent: agent.id.clone(), owner, fragment: Some((name, whose.owner, by_owner)) })
}

/// `POST /api/models/v1/chat/completions[?fragment=<name>]`: an agent's
/// model call, signed by the agent (`for` names whom it acts for), its
/// `model` a tier or `vision`. Its owner pays; `fragment` (one the agent
/// is in) is where the turn is, for that fragment's cap.
pub(crate) async fn route(mut req: Request, env: &Env, url: &Url, after: &dyn Background) -> CellResult<Response> {
    let body = read_body(&mut req, MODEL_BODY_MAX_BYTES).await?;
    let caller = caller(env, &req, url, &body).await?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
    let named = match v.get("model") {
        None | Some(Value::Null) => bounds::route_named(None),
        Some(Value::String(m)) => bounds::route_named(Some(m)),
        Some(_) => Err(bounds::Refusal::UnknownTier),
    };
    let named = named.map_err(refused)?;
    let stream = v.get("stream") == Some(&Value::Bool(true));
    let call = ModelCall { payer: &caller.owner, agent: Some(&caller.agent), fragment: caller.in_fragment(), named, body: v, stream };
    complete(env, call, after).await
}

/// `POST /api/models/v1/decide[?fragment=<name>]`: an agent's decision,
/// Clef on the model route's transport (the module's doc). Its caller and
/// payer are `route`'s; its body is a job's `ai.decide` input, `model`
/// `clef-flash` unless it names `clef` (`decide::route_body`), checked as
/// a step is (`decide::decide_call`) before anything is reserved. Its
/// answer is the step's, `{answers, model, usage}`. A call the model
/// refuses is released and its refusal passed through; an answer that
/// does not answer every question is charged its reservation and refused.
pub(crate) async fn decide_route(mut req: Request, env: &Env, url: &Url) -> CellResult<Response> {
    let body = read_body(&mut req, MODEL_BODY_MAX_BYTES).await?;
    let caller = caller(env, &req, url, &body).await?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
    let step = decide::route_body(v).map_err(|why| CellError::invalid(why.message()))?;
    let call = decide::decide_call(&step).map_err(|why| CellError::invalid(why.message()))?;
    let input_bytes = call.input.to_string().len();
    if input_bytes > MODEL_BODY_MAX_BYTES {
        return Err(CellError::too_large("a model call", input_bytes, MODEL_BODY_MAX_BYTES));
    }
    let held = hold(env, &caller.owner, Some(&caller.agent), caller.in_fragment().as_ref(), call.model, "decide", call.worst(input_bytes)).await?;
    let (status, bytes, log_id) = match run(env, call.model, &call.input, &caller.owner, Some(&caller.agent)).await {
        Ok(answered) => answered,
        Err(e) => {
            held.release("the model was not reached").await;
            return Err(e);
        }
    };
    if status != 200 {
        // a refusal used nothing: the vendor's answer, as it came
        held.release(&format!("the model answered {status}")).await;
        let text: String = String::from_utf8_lossy(&bytes).chars().take(REFUSAL_MAX_CHARS).collect();
        let mut resp = Response::ok(text)?.with_status(status);
        resp.headers_mut().set("content-type", "application/json")?;
        return Ok(resp);
    }
    let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let answers = match decide::answers_of(&step, &answer) {
        Ok(answers) => answers,
        Err(fault) => {
            // it answered, so it was paid for: charged its reservation, then refused
            held.settle(None, log_id).await;
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("{}: its call is charged at its worst case", fault.message())));
        }
    };
    held.settle(call.usage(&answer), log_id).await;
    let used = answer.get("usage").or_else(|| answer["result"].get("usage")).cloned().unwrap_or(Value::Null);
    Ok(Response::from_json(&json!({ "answers": answers, "model": call.model, "usage": used }))?)
}
