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
//! A job's image step calls its model on the same transport (`run`).
//!
//! Who calls: an agent, `POST /api/models/v1/chat/completions` (`route`),
//! signed by the agent (its computer's model intercept signs it), `for`
//! naming whom it acts for and `fragment` the turn's fragment; `complete`
//! is the call itself. The payer is the agent's owner (decision 36).
//! A call names a tier, or `vision`: the deployment's vision model
//! (`FRAGMENT_VISION_MODEL`, config.rs), for a runtime's calls about an
//! image (Hermes' screenshots), metered the same way.
//! A fragment someone else owns is asked whether it is still open under its
//! cap first (decision 26).
//!
//! Transcription (decision 9: a voice memo is one the agent transcribes
//! itself): `POST /api/models/v1/audio/transcriptions`, OpenAI's multipart
//! shape, `model` the route's `whisper` (`transcription_route`), runs
//! Workers AI's Whisper on the same transport, reserved at its audio's
//! bytes and settled at the length Whisper heard, in neurons
//! (`fragment_core::transcribe`). It names no fragment: its agent's owner
//! pays, under no fragment's cap, as a call through the model intercept
//! does.

use std::pin::Pin;

use fragment_core::ledger::{Release, Reserve, Settle, Spend};
use fragment_core::models::{self as bounds, Bounded, Named, Stream};
use fragment_core::price::Usage;
use fragment_core::{multipart, transcribe};
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

/// A call's request (`fragment_core::models`): Hermes' shrunk screenshot
/// and its prompt fit.
pub use fragment_core::models::MODEL_BODY_MAX_BYTES;
/// An unstreamed answer, or a refusal, read whole: at most `MAX_TOKENS`
/// of text and its JSON.
const ANSWER_MAX_BYTES: usize = 8 * 1024 * 1024;

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
    /// What the call named: a tier, `vision` or `whisper`.
    name: &'static str,
}

impl Held {
    /// The call's end, charged once: its usage, or its worst case when it
    /// reported none (or the stream broke). A settle that does not land is
    /// left held, and the ledger's sweep charges it at its worst case after
    /// six hours: the money path fails closed.
    async fn settle(&self, usage: Option<Usage>, log_id: Option<String>) {
        let settle = Settle { reference: self.reference.clone(), usage };
        match ledger::retried(&self.env, &self.payer, &settle).await {
            // one line per event: `wrangler tail` drops lines (lesson 14)
            Ok(settled) => console_log!("{}", json!({ "event": "model.settled", "ref": self.reference, "logId": log_id, "tier": self.name, "model": self.model, "charge": settled.charge, "basis": settled.basis })),
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

/// One model call, metered (the module's doc): its answer as the client
/// reads it, OpenAI's shape. A streamed answer is settled after it ends,
/// by `after`, whether or not the client read it to its end.
pub async fn complete(env: &Env, call: ModelCall<'_>, after: &dyn Background) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let body_bytes = serde_json::to_vec(&call.body).expect("a JSON value serializes").len();
    if body_bytes > MODEL_BODY_MAX_BYTES {
        return Err(CellError::too_large("a model call", body_bytes, MODEL_BODY_MAX_BYTES));
    }
    let model = bounds::capped(call.named, cfg.vision_model.as_str()).map_err(refused)?;
    let bounded = bounds::bound(model, call.body, call.stream).map_err(refused)?;
    // whose cap applies: the payer's own fragment's on its ledger; another
    // owner's, asked of theirs, unless the spender is that owner's
    let (fragment, capped) = match &call.fragment {
        Some(f) if f.owner == call.payer => (Some(f.name.to_string()), !f.by_owner),
        Some(f) => {
            if !f.by_owner {
                ledger::retried(env, f.owner, &FragmentOpen { fragment: f.name.to_string() }).await?;
            }
            (None, false)
        }
        None => (None, false),
    };
    let reserve = Reserve {
        reference: format!("aig:{}", js::random_hex::<16>()),
        spend: Spend::AgentTurn,
        worst: bounded.worst(body_bytes),
        fragment,
        agent: call.agent.map(str::to_string),
        capped,
    };
    ledger::hold(env, call.payer, &reserve).await?;
    let held = Held { env: env.clone(), payer: call.payer.to_string(), reference: reserve.reference, model: bounded.model, name: call.named.as_str() };
    let meta = Metadata { user_id: opaque(call.payer), agent_id: call.agent.map(opaque) };
    let mut upstream = match transport(env, cfg, bounded.model, &bounded.input, &meta).await {
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
        let mut resp = Response::ok(text)?.with_status(status);
        resp.headers_mut().set("content-type", "application/json")?;
        return Ok(resp);
    }
    if !bounded.stream {
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
        return Ok(resp);
    }
    let ResponseBody::Stream(source) = upstream.body() else {
        held.settle(None, log_id).await;
        return Err(CellError::new(ErrorCode::UpstreamFailed, "the model's stream had no body"));
    };
    // one branch for the client, one read to its end for the meter: a
    // client that goes away mid-answer does not leave the call unsettled
    let (client, meter) = js::tee(source)?;
    let model = bounded.model;
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
        held.settle(usage, log_id).await;
    }));
    let normalized = Normalized { source: Box::pin(ByteStream::from(client)), stream: Stream::default(), done: false };
    let mut resp = Response::from_stream(normalized)?;
    resp.headers_mut().set("content-type", "text/event-stream")?;
    resp.headers_mut().set("cache-control", "no-cache")?;
    Ok(resp)
}

/// One bounded call, unmetered: its status, its answer (read whole), and
/// the gateway's log id. Its caller meters it: a job's text step keeps
/// what it bought before it settles (ai.rs).
pub(crate) async fn call(env: &Env, bounded: &Bounded, payer: &str, agent: Option<&str>) -> CellResult<(u16, Vec<u8>, Option<String>)> {
    assert!(!bounded.stream, "an unmetered call is read whole");
    run(env, bounded.model, &bounded.input, payer, agent).await
}

/// One call of a catalog model with its input, on the same transport,
/// unmetered and read whole (`call`; a job's image step: ai.rs, whose
/// caller bounds the input and meters it).
pub(crate) async fn run(env: &Env, model: &str, input: &Value, payer: &str, agent: Option<&str>) -> CellResult<(u16, Vec<u8>, Option<String>)> {
    let meta = Metadata { user_id: opaque(payer), agent_id: agent.map(opaque) };
    let mut resp = transport(env, Config::from_env(env), model, input, &meta).await?;
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
struct Metadata {
    user_id: String,
    agent_id: Option<String>,
}

/// Makes a bounded call: `input` to `model`, through the AI binding and
/// the deployment's gateway, or, in dev and the e2e, to the fake at
/// `FRAGMENT_AI_URL`.
async fn transport(env: &Env, cfg: &Config, model: &str, input: &Value, meta: &Metadata) -> CellResult<Response> {
    let mut metadata = json!({ "user_id": meta.user_id });
    if let Some(agent) = &meta.agent_id {
        metadata["agent_id"] = json!(agent);
    }
    // GLM caches a prefix per session: an agent's calls share theirs (lesson 8)
    let headers = match &meta.agent_id {
        Some(agent) => json!({ "x-session-affinity": agent }),
        None => json!({}),
    };
    if let Some(url) = &cfg.ai_url {
        // the lower rung: the binding's input, to a fake at the vendor boundary
        let h = Headers::new();
        h.set("content-type", "application/json")?;
        h.set("x-fragment-ai-metadata", &metadata.to_string())?;
        if let Some(agent) = &meta.agent_id {
            h.set("x-session-affinity", agent)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(h).with_body(Some(input.to_string().into()));
        let req = Request::new_with_init(&format!("{url}/run/{model}"), &init)?;
        return Fetch::Request(req).send().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("the model did not answer: {e}")));
    }
    let Some(gateway) = &cfg.ai_gateway_id else {
        return Err(CellError::host("this deployment has no model route: set AI_GATEWAY_ID (its AI Gateway)"));
    };
    let options = json!({ "gateway": { "id": gateway, "metadata": metadata, "collectLog": false }, "extraHeaders": headers });
    js::ai_run(env.as_ref(), model, input, &options).await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("the model did not answer: {}", e.message)))
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
    const CHARS: usize = 4096;
    let (bytes, _) = crate::cs::read_answer(resp, 4 * CHARS).await.unwrap_or_default();
    String::from_utf8_lossy(&bytes).chars().take(CHARS).collect()
}

/// The answer a `whose` asks a fragment for: its owner, and whether the
/// agent naming it is in it.
#[derive(Deserialize)]
pub(crate) struct Whose {
    pub owner: String,
    pub member: bool,
}

/// `POST /api/models/v1/chat/completions[?fragment=<name>]`: an agent's
/// model call, signed by the agent (`for` names whom it acts for), its
/// `model` a tier or `vision`. Its owner pays; `fragment` (one the agent
/// is in) is where the turn is, for that fragment's cap.
pub(crate) async fn route(mut req: Request, env: &Env, url: &Url, after: &dyn Background) -> CellResult<Response> {
    let body = read_body(&mut req, MODEL_BODY_MAX_BYTES).await?;
    let agent = crate::signer_for(env, &req, url, &body).await?;
    if agent.kind != IdentityKind::Agent {
        return Err(CellError::new(ErrorCode::Forbidden, "the model route is an agent's: its owner pays for its calls"));
    }
    let owner = agent.owner.clone().ok_or_else(|| CellError::host("an agent without an owner"))?;
    let mut fragments = url.query_pairs().filter(|(k, _)| k == "fragment").map(|(_, v)| v.into_owned());
    let fragment = fragments.next();
    if fragments.next().is_some() {
        return Err(CellError::invalid("`fragment` is named once"));
    }
    let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
    let named = match v.get("model") {
        None | Some(Value::Null) => bounds::route_named(None),
        Some(Value::String(m)) => bounds::route_named(Some(m)),
        Some(_) => Err(bounds::Refusal::UnknownTier),
    };
    let named = named.map_err(refused)?;
    let stream = v.get("stream") == Some(&Value::Bool(true));
    let placed = match &fragment {
        None => None,
        Some(name) => {
            if !valid_fragment_name(name) {
                return Err(CellError::invalid("`fragment` is a fragment's name, <label>.<username>"));
            }
            let ask = routed::internal_request("meter/whose", &json!({ "principal": agent.id }).to_string())?;
            let mut answer = env.durable_object("FRAGMENT")?.get_by_name(name)?.fetch_with_request(ask).await?;
            if answer.status_code() != 200 {
                return Err(CellError::new(ErrorCode::NotFound, format!("no fragment {name}")));
            }
            let whose: Whose = answer.json().await?;
            if !whose.member {
                return Err(CellError::new(ErrorCode::Forbidden, format!("a model call names a fragment its agent is in, and it is not in {name}")));
            }
            Some(whose)
        }
    };
    // the owner's own: the agent acting as itself, or for its owner, in its owner's fragment
    let for_owner = agent.acting_for.as_deref().is_none_or(|asker| asker == owner);
    let call = ModelCall {
        payer: &owner,
        agent: Some(&agent.id),
        fragment: match (&fragment, &placed) {
            (Some(name), Some(whose)) => Some(InFragment { name, owner: &whose.owner, by_owner: whose.owner == owner && for_owner }),
            _ => None,
        },
        named,
        body: v,
        stream,
    };
    complete(env, call, after).await
}

/// `POST /api/models/v1/audio/transcriptions`: an agent's transcription
/// (the module's doc), signed by the agent; OpenAI's multipart shape, its
/// `model` `whisper`. Its owner pays. The audio is refused past
/// `transcribe::AUDIO_MAX_BYTES` (413) before anything is reserved or sent.
pub(crate) async fn transcription_route(mut req: Request, env: &Env, url: &Url) -> CellResult<Response> {
    let body = read_body(&mut req, transcribe::BODY_MAX_BYTES).await?;
    let agent = crate::signer_for(env, &req, url, &body).await?;
    if agent.kind != IdentityKind::Agent {
        return Err(CellError::new(ErrorCode::Forbidden, "the model route is an agent's: its owner pays for its calls"));
    }
    let owner = agent.owner.clone().ok_or_else(|| CellError::host("an agent without an owner"))?;
    let content_type = req.headers().get("content-type")?.unwrap_or_default();
    let malformed = |m: multipart::Malformed| CellError::invalid(m.message());
    let boundary = multipart::boundary(&content_type).map_err(malformed)?;
    let parts = multipart::parts(&body, &boundary).map_err(malformed)?;
    let bounded = match transcribe::bound(&parts) {
        Ok(b) => b,
        Err(transcribe::Refusal::TooLarge(n)) => return Err(CellError::too_large("the audio", n, transcribe::AUDIO_MAX_BYTES)),
        Err(why) => return Err(CellError::invalid(why.message())),
    };
    drop(parts);
    drop(body);
    let reserve = Reserve {
        reference: format!("aig:{}", js::random_hex::<16>()),
        spend: Spend::AgentTurn,
        worst: bounded.worst(),
        fragment: None,
        agent: Some(agent.id.clone()),
        capped: false,
    };
    ledger::hold(env, &owner, &reserve).await?;
    let held = Held { env: env.clone(), payer: owner.clone(), reference: reserve.reference, model: transcribe::TRANSCRIBE_MODEL, name: transcribe::WHISPER };
    let meta = Metadata { user_id: opaque(&owner), agent_id: Some(opaque(&agent.id)) };
    let mut upstream = match transport(env, Config::from_env(env), transcribe::TRANSCRIBE_MODEL, &bounded.input, &meta).await {
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
        let mut resp = Response::ok(text)?.with_status(status);
        resp.headers_mut().set("content-type", "application/json")?;
        return Ok(resp);
    }
    let answer = match read_whole(&mut upstream).await {
        Ok(b) => b,
        Err(e) => {
            // what it used is unknown: its worst case
            held.settle(None, log_id).await;
            return Err(e);
        }
    };
    let whisper: Value = serde_json::from_slice(&answer).unwrap_or(Value::Null);
    held.settle(transcribe::usage_of(&whisper), log_id).await;
    let Some((kind, out)) = transcribe::answer(bounded.format, &whisper) else {
        return Err(CellError::new(ErrorCode::UpstreamFailed, "the model's transcription carried no text"));
    };
    let mut resp = Response::from_bytes(out)?;
    resp.headers_mut().set("content-type", kind)?;
    Ok(resp)
}
