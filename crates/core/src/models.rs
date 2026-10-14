//! The model route's bounds (docs/cloudflare-v1.md, decisions 23 and 61;
//! "Lessons from cloudflare/agents" 7 and 8; spike S4). Pure:
//! cell/src/models.rs makes the calls and meters them on the payer's ledger.
//!
//! A call names a tier, never a model: the tier picks the model and caps
//! what one call may write. The route takes two names besides the tiers
//! (`Named`): `vision`, the deployment's vision model, for an agent
//! runtime's calls about an image (Hermes' auxiliary vision, which reads
//! its `computer_use` screenshots); and `fallback`, the deployment's
//! fallback model, for a runtime's own switch when its model stalls
//! (Hermes' `fallback_providers`, after its stale detector gives up on a
//! call that streams nothing). Neither is a tier: an agent, a job's step or
//! a manifest names none but the tiers.
//!
//! Each of those four names runs where the deployment says (`Models`, its
//! config's `models`): a Workers AI model through its AI Gateway, or a
//! model on an OpenAI-compatible API, its key a Secrets Store secret
//! (`Upstream`). Unnamed, each is today's Workers AI model; Finite's own
//! deployment runs its tiers on Finite Private (Paul, 2026-10-14; decision
//! 61). What the platform sends is the client's OpenAI-shaped chat
//! completion with only these changes (`bound`): no `model` (the tier's is
//! the call's), `max_tokens` at most the tier's cap, `reasoning_effort`
//! clamped to `low` or `high` (GLM takes a missing or unknown one as `max`,
//! the dearest; DeepSeek V4 Flash takes both, as does Finite Private's
//! limiter, which fills in `high` only when none is sent), and usage asked
//! for when it streams. Nothing else of the client's reaches the vendor: no
//! header, no key.
//!
//! A call that its model fails before answering anything (it is not
//! reached, or it answers 429 or a 5xx) is made once more, the same, on the
//! deployment's fallback model (`Tries`; Paul, 2026-10-08: "we def need
//! fallback models"). Not a race: the second call is made only after the
//! first has failed. An answer that began is final, and the fallback's
//! failure is the call's.
//!
//! What a call may cost is reserved before it is made (`worst`); what it
//! cost is read from its answer's usage (`usage_of`), priced as the model
//! that answered it. A streamed answer
//! carries usage on every chunk, each a per-chunk delta, then a line of its
//! own with the whole call's (spike S4): only that last, cumulative one is
//! metered, and the client reads OpenAI's shape, usage once on a last chunk
//! with no choices (`Stream`). An OpenAI-compatible API streams that shape
//! already, and is metered from its last chunk.

use fragment_proto::Tier;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::price::{PriceBook, Usage};

/// GLM-5.3 Flash and DeepSeek V4 Flash, as Workers AI's catalog names
/// them: each id once, each role below naming its model.
pub const GLM_FLASH: &str = "@cf/zai-org/glm-5.3-flash";
pub const DEEPSEEK_FLASH: &str = "@cf/deepseek-ai/deepseek-v4-flash-0731";
/// Finite Private's GLM-5.3 Flash, as its API names it: Finite's own
/// GLM-5.3 Flash (`zai-org/GLM-5.3-Flash` on SGLang, 393,216 tokens of
/// context) on Tinfoil, behind finite-mono's `finite-private-limiter` at
/// `https://finite-private.finite.containers.tinfoil.dev/v1` (finite-mono
/// `infra/tinfoil/confidential-finite-private`, read 2026-10-14). It calls
/// tools and streams; its images are not qualified (decision 61). Also its
/// row in the price book.
pub const FINITE_PRIVATE_FLASH: &str = "glm-5-3-flash";
/// The tiers' models unless a deployment names others (decision 23). The
/// cheap tier, every agent's default, is GLM-5.3 Flash, DeepSeek V4 Flash
/// its fallback (`FALLBACK_MODEL_DEFAULT`; Paul, 2026-10-09: "GLM default,
/// DeepSeek fallback", after DeepSeek, the default for a few hours, was
/// short of capacity on Workers AI: first bytes of 11 to 61 s, a 429
/// "Capacity temporarily exceeded", a 160 s hang ending in a 500, while
/// GLM's were 3 to 5 s). Both take the route's requests as they are (tools,
/// `reasoning_effort` `low` and `high`, `max_tokens` up to `MAX_TOKENS`;
/// checked 2026-10-09).
pub const CHEAP_MODEL: &str = GLM_FLASH;
pub const MEDIUM_MODEL: &str = "@cf/zai-org/glm-5.3";
/// The route's name for the deployment's vision model.
pub const VISION: &str = "vision";
/// The route's name for the deployment's fallback model.
pub const FALLBACK: &str = "fallback";
/// The vision model unless the deployment names another (Paul,
/// 2026-10-05): GLM-5.3 Flash, "Vision: Yes" in Workers AI's catalog
/// (developers.cloudflare.com/workers-ai/models/glm-5.3-flash/, read
/// 2026-10-05), already priced, and the cheap tier's. Neither DeepSeek V4
/// Flash nor GLM-5.3, the medium tier's, takes images. DeepSeek Flash's
/// vision build (`deepseek-flash`) is only on DeepSeek's own API, and
/// reaching it would take our own key (decision 23: no BYOK).
pub const VISION_MODEL_DEFAULT: &str = GLM_FLASH;
/// The fallback model unless the deployment names another (Paul,
/// 2026-10-08 and 10-09): DeepSeek V4 Flash, on Workers AI, a model of its
/// own beside the cheap tier's GLM (its own replicas, and its own rate
/// limit, 50 calls a minute per account on Unified Billing). It reads no
/// images, so the route's `vision` has no fallback.
pub const FALLBACK_MODEL_DEFAULT: &str = DEEPSEEK_FLASH;
/// The route's four names, as a deployment's `models` names them, each
/// where one of its models runs (`Models`).
pub const SLOTS: [&str; 4] = ["cheap", "medium", FALLBACK, VISION];
/// The largest image Hermes sends for its vision call after a size
/// refusal: it shrinks one to this many bytes of base64 data URL and tries
/// again once (its `tools/vision_tools.py`, `_RESIZE_TARGET_BYTES`, which
/// its config does not set).
pub const IMAGE_DATA_URL_MAX_BYTES: usize = 5 * 1024 * 1024;
/// A call's request: an image of `IMAGE_DATA_URL_MAX_BYTES` and a MiB for
/// the rest of it (Hermes' prompt about a screenshot carries the screen's
/// element list), so the call Hermes retries after a 413 fits. GLM's
/// million-token window is about 4 MB of text; an agent's window sends a
/// few hundred KiB. Finite Private's limiter takes at most 2 MiB a request
/// (its 413 past that is the call's): a text tier's call there is that
/// size at most, and `vision` stays on Workers AI (decision 61).
pub const MODEL_BODY_MAX_BYTES: usize = IMAGE_DATA_URL_MAX_BYTES + 1024 * 1024;
const _: () = assert!(MODEL_BODY_MAX_BYTES > IMAGE_DATA_URL_MAX_BYTES && MODEL_BODY_MAX_BYTES < 8 * 1024 * 1024, "the cap fits Hermes' shrunk image, and stays near it");
/// The most one call may write, reasoning included: a job's text step may
/// ask for up to this.
/// It bounds the worst case each call reserves: GLM-5.3's is $0.11 of
/// output at list price, before its input.
pub const MAX_TOKENS: u32 = 16_384;
/// The reasoning efforts GLM takes that are not its dearest: anything else
/// asked for is `low` (GLM maps `none`, `minimal`, `medium` and `xhigh` to
/// `max`, its default, so passing them on would buy the most expensive).
pub const EFFORTS: [&str; 2] = ["low", "high"];
/// One line of a streamed answer: Workers AI's chunks are a few hundred
/// bytes. A longer line is passed through unread, and the call is metered
/// at its reservation (its usage cannot be trusted).
pub const SSE_LINE_MAX: usize = 1024 * 1024;

/// Where one of the route's models runs (a deployment's `models`, each
/// slot one of these, `provider` its kind). Its `model` is also its row in
/// the price book: a call is priced as the model that answered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum Upstream {
    /// A Workers AI model by its catalog id, through the deployment's AI
    /// Gateway (the Worker's AI binding; dev and the e2e, the fake at
    /// `FRAGMENT_AI_URL`).
    WorkersAi { model: String },
    /// A model on an OpenAI-compatible API (Finite Private's):
    /// `POST <base_url>/chat/completions`, its `model` the id there, with
    /// the key the deployment binds for the slot naming it
    /// (`secrets_store::model_key`) as a bearer token.
    Openai { base_url: String, model: String },
}

impl Upstream {
    /// Its model's id, and its row in the price book.
    pub fn model(&self) -> &str {
        match self {
            Upstream::WorkersAi { model } | Upstream::Openai { model, .. } => model,
        }
    }
}

fn workers_ai(model: &str) -> Upstream {
    Upstream::WorkersAi { model: model.to_string() }
}

/// The route's models, one for each of its names (`SLOTS`): the deployment's
/// `models` (`FRAGMENT_MODELS`), each slot it does not name on Workers AI as
/// before (`Models::defaults`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Models {
    pub cheap: Upstream,
    pub medium: Upstream,
    pub fallback: Upstream,
    pub vision: Upstream,
}

/// The slots a deployment names; the rest are the defaults'.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Slots {
    cheap: Option<Upstream>,
    medium: Option<Upstream>,
    fallback: Option<Upstream>,
    vision: Option<Upstream>,
}

impl Models {
    /// Every slot on Workers AI: GLM-5.3 Flash cheap, GLM-5.3 medium,
    /// DeepSeek V4 Flash the fallback, GLM-5.3 Flash vision.
    pub fn defaults() -> Models {
        Models { cheap: workers_ai(CHEAP_MODEL), medium: workers_ai(MEDIUM_MODEL), fallback: workers_ai(FALLBACK_MODEL_DEFAULT), vision: workers_ai(VISION_MODEL_DEFAULT) }
    }

    /// A deployment's models: `named` (`FRAGMENT_MODELS`: an object of the
    /// slots it names, each an `Upstream`) over the defaults, checked.
    /// Refused, saying why: a slot that is none of `SLOTS` (`high` among
    /// them: decision 23), a provider that is neither, a model the price
    /// book does not price (a ledger would refuse every call's
    /// reservation), and an OpenAI-compatible API's `base_url` that is not
    /// https on a host name (`loopback`: or a local fleet's fake at
    /// `http://127.0.0.1:<port>`), or names a query or a trailing `/`.
    pub fn parse(named: Option<&str>, book: &PriceBook, loopback: bool) -> Result<Models, String> {
        let mut models = Models::defaults();
        if let Some(text) = named {
            let n: Slots = serde_json::from_str(text).map_err(|e| format!("models: {e}"))?;
            for (slot, upstream) in [(&mut models.cheap, n.cheap), (&mut models.medium, n.medium), (&mut models.fallback, n.fallback), (&mut models.vision, n.vision)] {
                if let Some(u) = upstream {
                    *slot = u;
                }
            }
        }
        for (slot, upstream) in models.slots() {
            check(upstream, book, loopback).map_err(|e| format!("models.{slot}: {e}"))?;
        }
        Ok(models)
    }

    /// Each slot's name (`SLOTS`) and where it runs.
    pub fn slots(&self) -> [(&'static str, &Upstream); 4] {
        [(SLOTS[0], &self.cheap), (SLOTS[1], &self.medium), (SLOTS[2], &self.fallback), (SLOTS[3], &self.vision)]
    }

    /// The model a call naming `named` runs: its tier's, the vision or
    /// the fallback model; `high` is refused.
    pub fn route(&self, named: Named) -> Result<Model<'_>, Refusal> {
        let upstream = match named {
            Named::Tier(Tier::Cheap) => &self.cheap,
            Named::Tier(Tier::Medium) => &self.medium,
            Named::Tier(Tier::High) => return Err(Refusal::HighOff),
            Named::Fallback => &self.fallback,
            Named::Vision => &self.vision,
        };
        Ok(Model { named, upstream })
    }

    /// The deployment's fallback model (`Tries`).
    pub fn fallback(&self) -> Model<'_> {
        Model { named: Named::Fallback, upstream: &self.fallback }
    }

    /// `route`'s model, and the most one call of it writes.
    pub fn capped(&self, named: Named) -> Result<Capped<'_>, Refusal> {
        Ok(Capped { model: self.route(named)?, max_tokens: MAX_TOKENS })
    }
}

/// One slot's model, checked (`Models::parse`).
fn check(upstream: &Upstream, book: &PriceBook, loopback: bool) -> Result<(), String> {
    let model = upstream.model();
    if !book.models.iter().any(|m| m.model == model) {
        let priced: Vec<&str> = book.models.iter().map(|m| m.model.as_str()).collect();
        return Err(format!("the model {model:?} is not in the price book, which prices {}: add its prices (fragment_core::price) or name one of those", priced.join(", ")));
    }
    if let Upstream::Openai { base_url, model } = upstream {
        crate::catalog::row_url(base_url, loopback).map_err(|e| format!("base_url: {e}"))?;
        if base_url.ends_with('/') || base_url.contains('?') {
            return Err("base_url is named without a query or a trailing /: the route adds /chat/completions".into());
        }
        if !crate::catalog::valid_model_id(model) {
            return Err(format!("{model:?} is no model id (1-{} of letters, digits and . _ - : / @)", crate::catalog::MODEL_ID_MAX_BYTES));
        }
    }
    Ok(())
}

/// A model the route calls: the name it is called under (which slot's key
/// an OpenAI-compatible API's call carries) and where it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model<'a> {
    pub named: Named,
    pub upstream: &'a Upstream,
}

impl<'a> Model<'a> {
    /// Its model's id, and its row in the price book.
    pub fn id(&self) -> &'a str {
        self.upstream.model()
    }
}

/// A call's model and the most one call of it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capped<'a> {
    pub model: Model<'a>,
    pub max_tokens: u32,
}

/// What a call to the route names as its `model`: a tier, `vision`, or
/// `fallback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Tier(Tier),
    Vision,
    Fallback,
}

impl Named {
    pub fn as_str(self) -> &'static str {
        match self {
            Named::Tier(t) => t.as_str(),
            Named::Vision => VISION,
            Named::Fallback => FALLBACK,
        }
    }
}

/// Why a model call is refused before anything is reserved or sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `high` stays off until Cloudflare raises Unified Billing's Opus
    /// limit, about 2 calls a minute per edge machine (decision 23, S4).
    HighOff,
    /// `model` names no tier (a model id is never one).
    UnknownTier,
    /// The body is not a JSON object.
    NotAnObject,
    /// No `messages`, or none in it: the route makes chat completions only.
    NoMessages,
    /// A `max_tokens` (or `max_completion_tokens`) that is not a positive integer.
    MaxTokens,
    /// A `stream` that is not true or false.
    Stream,
}

impl Refusal {
    /// The refusal for people (and agents, which show it).
    pub fn message(self) -> String {
        match self {
            Refusal::HighOff => "the high tier is off until Cloudflare raises Unified Billing's limit on Opus; use cheap or medium".into(),
            Refusal::UnknownTier => "model names a tier: cheap or medium (a model id is never one)".into(),
            Refusal::NotAnObject => "a chat completion is a JSON object".into(),
            Refusal::NoMessages => "a chat completion has messages: at least one".into(),
            Refusal::MaxTokens => format!("max_tokens is a positive integer (at most {MAX_TOKENS} is used)"),
            Refusal::Stream => "stream is true or false".into(),
        }
    }
}

/// The tier a tier's name names (`model` in a body, an agent's setting, a
/// job's step); `None` is the default tier. `vision` is none.
pub fn tier_named(name: Option<&str>) -> Result<Tier, Refusal> {
    match name {
        None => Ok(fragment_proto::DEFAULT_TIER),
        Some(n) => Tier::parse(n).ok_or(Refusal::UnknownTier),
    }
}

/// What a call to the model route names: `vision`, `fallback`, or a tier
/// (`tier_named`).
pub fn route_named(name: Option<&str>) -> Result<Named, Refusal> {
    match name {
        Some(VISION) => Ok(Named::Vision),
        Some(FALLBACK) => Ok(Named::Fallback),
        other => tier_named(other).map(Named::Tier),
    }
}

/// Whether an answer's status, before any of its body, sends the call on
/// to the fallback model: the vendor's rate limit (429: Finite Private's
/// limiter answers it when its key's window is spent), or its own failure
/// (5xx: tonight's Workers AI answered 502 "could not route request to AI
/// model", then 500, for five minutes; Finite Private's limiter answers 502
/// when its model fails and 503 when finite.computer, which admits each
/// call, does not answer). A refusal of the request itself (another 4xx)
/// would be the fallback's too, and a 200 has begun the answer, which is
/// final.
pub fn falls_back(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// What one try of a call came to, before any of its answer's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tried {
    /// The model was not reached: the transport failed.
    Unreached,
    /// It answered this status.
    Answered(u16),
}

/// A call's tries: its model, then, when that one fails before answering
/// anything (`falls_back`, or not reached), the fallback model, once. At
/// most two tries, and the second only after the first has failed: no
/// race. A fallback that runs where the call's own model runs is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tries<'a> {
    first: Model<'a>,
    fallback: Option<Model<'a>>,
    fell: bool,
    /// A try's answer was the call's: what follows (a stream that breaks
    /// after it began) is no try.
    done: bool,
}

impl<'a> Tries<'a> {
    pub fn new(model: Model<'a>, fallback: Option<Model<'a>>) -> Tries<'a> {
        Tries { first: model, fallback: fallback.filter(|f| f.upstream != model.upstream), fell: false, done: false }
    }

    /// The model the next try calls, and, once the tries are done, the one
    /// whose answer is the call's: its usage is priced as that model's.
    pub fn model(&self) -> Model<'a> {
        match (self.fell, self.fallback) {
            (true, Some(f)) => f,
            _ => self.first,
        }
    }

    /// Whether the call fell back to the fallback model.
    pub fn fell(&self) -> bool {
        self.fell
    }

    /// After a try: the model to try next, or `None` when this try's
    /// answer is the call's (it answered, it refused the request, or it
    /// was the fallback's).
    pub fn after(&mut self, tried: Tried) -> Option<Model<'a>> {
        let failed = match tried {
            Tried::Unreached => true,
            Tried::Answered(status) => falls_back(status),
        };
        let next = match (self.done, failed, self.fell, self.fallback) {
            (false, true, false, Some(next)) => next,
            _ => {
                self.done = true;
                return None;
            }
        };
        self.fell = true;
        assert_ne!(next.upstream, self.first.upstream, "a fallback is another model");
        Some(next)
    }
}

/// What a guest's key says to name its agent: `agent:<label>--<suffix>`.
pub const AGENT_KEY_PREFIX: &str = "agent:";

/// Why a guest's model call names no agent to bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unnamed {
    /// Neither the header nor the key names one.
    Missing,
    /// The header, or a key that says `agent:`, names no agent fragment.
    Malformed,
    /// The header and the key name two agents.
    Disagree,
}

impl Unnamed {
    pub fn message(self) -> &'static str {
        match self {
            Unnamed::Missing => "name the agent this call is for (x-fragment-agent, or the key agent:<label>--<suffix>): its owner pays for it",
            Unnamed::Malformed => "the agent a call names is an agent fragment's name, <label>--<suffix>",
            Unnamed::Disagree => "x-fragment-agent and the key agent:<…> name two agents",
        }
    }
}

/// Who a guest's model call is for (docs/computers.md, Models), one rule
/// for every model route: the agent its `x-fragment-agent` names, or, from
/// a client that sends no header of its own (OpenAI's SDKs take a base URL
/// and a key, nothing more), its key, `Authorization: Bearer
/// agent:<label>--<suffix>`. A key that does not say `agent:` is the
/// guest's own placeholder (Hermes' `fragment-model`), not a name. Named
/// both ways, the two agree. The computer signs only for an agent that runs
/// on it, so a key names no more than the header could.
pub fn agent_named(header: Option<&str>, authorization: Option<&str>) -> Result<String, Unnamed> {
    let from_key = authorization
        .and_then(|a| a.trim().split_once(' ').filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer")).map(|(_, k)| k.trim()))
        .and_then(|k| k.strip_prefix(AGENT_KEY_PREFIX));
    let valid = |n: &str| fragment_proto::valid_fragment_name(n).then(|| n.to_string()).ok_or(Unnamed::Malformed);
    match (header.map(str::trim), from_key) {
        (None, None) => Err(Unnamed::Missing),
        (Some(h), None) => valid(h),
        (None, Some(k)) => valid(k),
        (Some(h), Some(k)) => {
            let (h, k) = (valid(h)?, valid(k)?);
            if h == k {
                Ok(h)
            } else {
                Err(Unnamed::Disagree)
            }
        }
    }
}

/// The request the platform sends for one call, and what it may cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounded<'a> {
    pub model: Model<'a>,
    /// What it asks for, at most the tier's cap.
    pub max_tokens: u32,
    /// The input `env.AI.run` takes: the body less `model`, bounded (an
    /// OpenAI-compatible API's call adds its model's id).
    pub input: Value,
    pub stream: bool,
}

impl Bounded<'_> {
    /// The call's worst case, reserved before it is made: every byte of the
    /// request a token in (a token is at least a byte, so this is an upper
    /// bound, the JSON's own bytes included), and every token it may write.
    pub fn worst(&self, body_bytes: usize) -> Usage {
        Usage::Tokens { model: self.model.id().to_string(), input: body_bytes as u64, cached_input: 0, cache_write: 0, output: u64::from(self.max_tokens) }
    }
}

/// A positive token count from a body's field, if it names one.
fn tokens(v: Option<&Value>) -> Result<Option<u64>, Refusal> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(n) => n.as_u64().filter(|n| *n >= 1).map(Some).ok_or(Refusal::MaxTokens),
    }
}

/// `body`, an OpenAI-shaped chat completion, bounded for its model `t`
/// (`Models::capped`).
pub fn bound(t: Capped<'_>, body: Value, stream: bool) -> Result<Bounded<'_>, Refusal> {
    let Value::Object(mut input) = body else { return Err(Refusal::NotAnObject) };
    match input.get("messages") {
        Some(Value::Array(m)) if !m.is_empty() => {}
        _ => return Err(Refusal::NoMessages),
    }
    if input.get("stream").is_some_and(|s| !s.is_boolean() && !s.is_null()) {
        return Err(Refusal::Stream);
    }
    // the smallest of what it asked for (in either name) and the cap
    let asked = [tokens(input.get("max_tokens"))?, tokens(input.get("max_completion_tokens"))?];
    let max_tokens = asked.into_iter().flatten().fold(u64::from(t.max_tokens), u64::min);
    let effort = match input.get("reasoning_effort").and_then(Value::as_str) {
        Some(e) if EFFORTS.contains(&e) => e.to_string(),
        _ => EFFORTS[0].to_string(),
    };
    input.remove("model");
    input.remove("max_completion_tokens");
    input.insert("max_tokens".into(), json!(max_tokens));
    input.insert("reasoning_effort".into(), json!(effort));
    input.insert("stream".into(), json!(stream));
    match stream {
        true => input.insert("stream_options".into(), json!({ "include_usage": true })),
        false => input.remove("stream_options"),
    };
    let max_tokens = u32::try_from(max_tokens).expect("at most the tier's cap, a u32");
    assert!(max_tokens >= 1 && max_tokens <= t.max_tokens, "a call writes at least a token and at most its tier's cap");
    Ok(Bounded { model: t.model, max_tokens, input: Value::Object(input), stream })
}

/// A count from a usage object's field.
fn count(v: &Value, k: &str) -> Option<u64> {
    match v.get(k) {
        None | Some(Value::Null) => Some(0),
        Some(n) => n.as_u64(),
    }
}

/// A call's usage, OpenAI-shaped (Workers AI's and the gateway's), as the
/// ledger meters it: `prompt_tokens` counts cached input and cache writes,
/// so the input is what is left of it. A usage that does not read so (a
/// field missing or negative, more cached than prompted) is `None`, which
/// the ledger settles at the reservation: the money path fails closed.
pub fn usage_of(model: &str, usage: &Value) -> Option<Usage> {
    let prompt = usage.get("prompt_tokens")?.as_u64()?;
    let output = usage.get("completion_tokens")?.as_u64()?;
    let details = usage.get("prompt_tokens_details").cloned().unwrap_or(Value::Null);
    let cached = count(&details, "cached_tokens")?;
    let cache_write = match count(&details, "cache_write_tokens")? {
        0 => count(usage, "cache_creation_input_tokens")?,
        n => n,
    };
    let input = prompt.checked_sub(cached)?.checked_sub(cache_write)?;
    Some(Usage::Tokens { model: model.to_string(), input, cached_input: cached, cache_write, output })
}

/// A streamed answer read line by line (server-sent events): what the
/// client is sent, and the usage to meter. Workers AI puts a per-chunk
/// delta of usage on every chunk and ends with `{"response": "", "usage":
/// {…}}`, no choices, carrying the whole call's: the client gets OpenAI's
/// semantics (usage dropped from chunks with choices, and that last line as
/// a chunk with none), and only the last cumulative usage is metered. A
/// provider that sends OpenAI's own last chunk (`choices: []` with usage)
/// is metered from that, when no line of Workers AI's follows.
#[derive(Debug, Default)]
pub struct Stream {
    line: Vec<u8>,
    /// The usage of the last line with no `choices` (Workers AI's whole call).
    cumulative: Option<Value>,
    /// The usage of the last chunk with an empty `choices` (OpenAI's last).
    trailing: Option<Value>,
    /// The answer's id, for the rewritten last line.
    id: Option<Value>,
    /// A line past `SSE_LINE_MAX`: the stream is out of its contract.
    overlong: bool,
    lines: u64,
}

impl Stream {
    /// Takes the next bytes; the client's lines go to `out` (none when
    /// only metering).
    pub fn push(&mut self, chunk: &[u8], mut out: Option<&mut Vec<u8>>) {
        // bounded by the chunk: each pass takes one line out of it
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            self.line.extend_from_slice(piece);
            if piece.last() == Some(&b'\n') {
                let line = std::mem::take(&mut self.line);
                self.take_line(&line, out.as_deref_mut());
            } else if self.line.len() > SSE_LINE_MAX {
                self.overlong = true;
                let line = std::mem::take(&mut self.line);
                if let Some(o) = out.as_deref_mut() {
                    o.extend_from_slice(&line);
                }
            }
        }
    }

    /// The end of the stream: a last line without its newline.
    pub fn finish(&mut self, out: Option<&mut Vec<u8>>) {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.take_line(&line, out);
        }
    }

    /// The usage to meter, when the stream told it whole.
    pub fn usage(&self) -> Option<&Value> {
        if self.overlong {
            return None;
        }
        self.cumulative.as_ref().or(self.trailing.as_ref())
    }

    /// Lines read so far.
    pub fn lines(&self) -> u64 {
        self.lines
    }

    fn take_line(&mut self, line: &[u8], out: Option<&mut Vec<u8>>) {
        self.lines += 1;
        let rewritten = self.read_line(line);
        if let Some(o) = out {
            o.extend_from_slice(rewritten.as_deref().unwrap_or(line));
        }
    }

    /// One line, read for its usage; the line the client gets instead, when
    /// it is not the line as it came.
    fn read_line(&mut self, line: &[u8]) -> Option<Vec<u8>> {
        let text = std::str::from_utf8(line).ok()?;
        let data = text.strip_prefix("data:")?.trim();
        if data == "[DONE]" {
            return None;
        }
        let Ok(Value::Object(mut chunk)) = serde_json::from_str::<Value>(data) else { return None };
        if let Some(id) = chunk.get("id").filter(|id| !id.is_null()) {
            self.id = Some(id.clone());
        }
        let ending = if text.ends_with("\r\n") { "\r\n" } else if text.ends_with('\n') { "\n" } else { "" };
        match (chunk.get("choices"), chunk.get("usage").cloned()) {
            (_, None) => None,
            (Some(Value::Array(choices)), Some(usage)) if choices.is_empty() => {
                self.trailing = Some(usage);
                None
            }
            (Some(_), Some(_)) => {
                chunk.remove("usage");
                Some(format!("data: {}{ending}", Value::Object(chunk)).into_bytes())
            }
            (None, Some(usage)) => {
                let last = json!({ "id": self.id, "object": "chat.completion.chunk", "choices": [], "usage": usage });
                self.cumulative = Some(usage);
                Some(format!("data: {last}{ending}").into_bytes())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static DEFAULTS: std::sync::LazyLock<Models> = std::sync::LazyLock::new(Models::defaults);

    fn tier(t: Tier) -> Capped<'static> {
        DEFAULTS.capped(Named::Tier(t)).unwrap()
    }

    /// Finite's own deployment's models (deploy/example.jsonc; decision
    /// 61): its tiers on Finite Private, its fallback and vision on
    /// Workers AI's GLM-5.3 Flash.
    const FINITE: &str = r#"{
        "cheap": { "provider": "openai", "base_url": "https://finite-private.finite.containers.tinfoil.dev/v1", "model": "glm-5-3-flash" },
        "medium": { "provider": "openai", "base_url": "https://finite-private.finite.containers.tinfoil.dev/v1", "model": "glm-5-3-flash" },
        "fallback": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3-flash" },
        "vision": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3-flash" }
    }"#;

    fn finite() -> Models {
        Models::parse(Some(FINITE), &PriceBook::defaults(), false).unwrap()
    }

    fn openai(base_url: &str, model: &str) -> Upstream {
        Upstream::Openai { base_url: base_url.into(), model: model.into() }
    }

    /// Goal: the route's `vision` runs the deployment's vision model, at
    /// the tiers' cap, and is no tier (an agent, a job's step and a
    /// manifest name only tiers). Method: the route's names, the tiers'
    /// names, and a call bounded on it.
    #[test]
    fn vision_is_the_routes_and_no_tier() {
        assert_eq!(route_named(Some("vision")), Ok(Named::Vision));
        assert_eq!(route_named(Some("medium")), Ok(Named::Tier(Tier::Medium)));
        assert_eq!(route_named(None), Ok(Named::Tier(fragment_proto::DEFAULT_TIER)), "none named: the default tier");
        assert_eq!(route_named(Some("Vision")), Err(Refusal::UnknownTier));
        assert_eq!(route_named(Some(CHEAP_MODEL)), Err(Refusal::UnknownTier), "a model id is never a name");
        assert_eq!(route_named(Some("high")).and_then(|n| DEFAULTS.capped(n)), Err(Refusal::HighOff));
        assert_eq!(tier_named(Some("vision")), Err(Refusal::UnknownTier), "vision is no tier");
        assert_eq!(Tier::parse("vision"), None);
        assert_eq!((Named::Vision.as_str(), Named::Tier(Tier::Cheap).as_str()), ("vision", "cheap"));
        let mut seeing = Models::defaults();
        seeing.vision = workers_ai("@cf/example/seeing");
        let v = seeing.capped(Named::Vision).unwrap();
        assert_eq!(v, Capped { model: Model { named: Named::Vision, upstream: &workers_ai("@cf/example/seeing") }, max_tokens: MAX_TOKENS });
        assert_eq!(seeing.capped(Named::Tier(Tier::Medium)).unwrap().model.id(), MEDIUM_MODEL, "a tier's call is the tier's");
        // an image part reaches the model as it came
        let image = json!([{ "type": "text", "text": "what is on the screen?" }, { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } }]);
        let b = bound(v, json!({ "model": "vision", "messages": [{ "role": "user", "content": image }] }), false).unwrap();
        assert_eq!((b.model.id(), b.input["messages"][0]["content"].clone(), b.input.get("model")), ("@cf/example/seeing", image, None));
    }

    /// Goal: the route's `fallback` runs the deployment's fallback model, at
    /// the tiers' cap, for a runtime's own switch when its model stalls
    /// (Hermes' `fallback_providers`), and is no tier. Method: the route's
    /// names, the tiers' names, and the model each is capped to, by
    /// default and on Finite's own deployment.
    #[test]
    fn fallback_is_the_routes_and_no_tier() {
        assert_eq!(route_named(Some("fallback")), Ok(Named::Fallback));
        assert_eq!(Named::Fallback.as_str(), "fallback");
        assert_eq!(route_named(Some("Fallback")), Err(Refusal::UnknownTier));
        assert_eq!(route_named(Some(FALLBACK_MODEL_DEFAULT)), Err(Refusal::UnknownTier), "a model id is never a name");
        assert_eq!(tier_named(Some("fallback")), Err(Refusal::UnknownTier), "fallback is no tier: no agent, step or manifest names it");
        assert_eq!(Tier::parse("fallback"), None);
        let f = DEFAULTS.capped(Named::Fallback).unwrap();
        assert_eq!((f.model.named, f.model.id(), f.max_tokens), (Named::Fallback, "@cf/deepseek-ai/deepseek-v4-flash-0731", MAX_TOKENS));
        assert_eq!(DEFAULTS.fallback(), f.model);
        // Hermes' stale switch on Finite's deployment: Workers AI's GLM-5.3 Flash
        let finite = finite();
        assert_eq!(finite.route(Named::Fallback).unwrap().upstream, &workers_ai(GLM_FLASH), "the deployment's, whatever it names");
    }

    /// Goal: a deployment's `models` name where each of the route's names
    /// runs, each slot it does not name as before; Finite's own (decision
    /// 61) puts its tiers on Finite Private and its fallback and vision on
    /// Workers AI's GLM-5.3 Flash. Method: none named, Finite's, and each
    /// slot alone.
    #[test]
    fn a_deployments_models_name_where_each_runs() {
        let book = PriceBook::defaults();
        assert_eq!(Models::parse(None, &book, false), Ok(Models::defaults()));
        assert_eq!(Models::parse(Some("{}"), &book, false), Ok(Models::defaults()), "an empty object names none");
        let d = Models::defaults();
        assert_eq!(
            d.slots().map(|(slot, u)| (slot, u.model().to_string())),
            [("cheap", GLM_FLASH.into()), ("medium", MEDIUM_MODEL.into()), ("fallback", DEEPSEEK_FLASH.into()), ("vision", GLM_FLASH.into())],
            "every slot on Workers AI, as before"
        );
        assert!(d.slots().iter().all(|(_, u)| matches!(u, Upstream::WorkersAi { .. })));
        let finite = finite();
        let private = openai("https://finite-private.finite.containers.tinfoil.dev/v1", FINITE_PRIVATE_FLASH);
        assert_eq!(finite, Models { cheap: private.clone(), medium: private.clone(), fallback: workers_ai(GLM_FLASH), vision: workers_ai(GLM_FLASH) });
        assert_eq!(finite.route(Named::Tier(Tier::Cheap)).unwrap(), Model { named: Named::Tier(Tier::Cheap), upstream: &private });
        assert_eq!(finite.route(Named::Tier(Tier::Medium)).unwrap().id(), "glm-5-3-flash");
        assert_eq!(finite.route(Named::Tier(Tier::High)), Err(Refusal::HighOff), "high stays off wherever the tiers run");
        // one slot alone: the rest as before
        let medium = Models::parse(Some(r#"{"medium": {"provider": "workers_ai", "model": "@cf/zai-org/glm-5.3-flash"}}"#), &book, false).unwrap();
        assert_eq!(medium, Models { medium: workers_ai(GLM_FLASH), ..Models::defaults() });
        // its wire form is the config's, less the key (the deploy's)
        assert_eq!(serde_json::to_value(&private).unwrap(), json!({ "provider": "openai", "base_url": "https://finite-private.finite.containers.tinfoil.dev/v1", "model": "glm-5-3-flash" }));
        assert_eq!(serde_json::to_value(workers_ai(GLM_FLASH)).unwrap(), json!({ "provider": "workers_ai", "model": GLM_FLASH }));
    }

    /// Goal: a deployment's `models` that the route could not serve is
    /// refused, saying which slot and why, before the deploy and at the
    /// cell's first request. Invalid: an unknown slot (`high` among them),
    /// an unknown provider, a missing or extra field (a key is the deploy's,
    /// never the cell's), a model the book does not price, and a base URL
    /// that is no https on a host name, names a query or a trailing /, or is
    /// a local fleet's fake where none may be. Valid: a local fleet's fake.
    #[test]
    fn a_deployments_models_out_of_shape_are_refused() {
        let book = PriceBook::defaults();
        let parse = |text: &str, loopback: bool| Models::parse(Some(text), &book, loopback);
        let slot = |row: Value| json!({ "cheap": row }).to_string();
        let refused = |text: &str, says: &str| {
            let e = parse(text, false).unwrap_err();
            assert!(e.contains(says), "{text}: {e}");
        };
        refused(r#"{"high": {"provider": "workers_ai", "model": "@cf/zai-org/glm-5.3"}}"#, "unknown field `high`");
        refused(r#"{"cheapest": {"provider": "workers_ai", "model": "@cf/zai-org/glm-5.3"}}"#, "unknown field `cheapest`");
        refused(&slot(json!({ "provider": "anthropic", "model": "glm-5-3-flash" })), "unknown variant `anthropic`");
        refused(&slot(json!({ "model": "glm-5-3-flash" })), "missing field `provider`");
        refused(&slot(json!({ "provider": "openai", "model": "glm-5-3-flash" })), "missing field `base_url`");
        refused(&slot(json!({ "provider": "openai", "base_url": "https://a.example/v1", "model": "glm-5-3-flash", "key": "fragment-finite-private-key" })), "unknown field `key`");
        refused(&slot(json!({ "provider": "workers_ai", "base_url": "https://a.example/v1", "model": GLM_FLASH })), "unknown field `base_url`");
        refused(&slot(json!({ "provider": "openai", "base_url": "https://a.example/v1", "model": "glm-6" })), "models.cheap: the model \"glm-6\" is not in the price book");
        refused(&slot(json!({ "provider": "workers_ai", "model": "@cf/meta/llama-4-scout-17b-16e-instruct" })), "not in the price book");
        refused(r#"{"vision": {"provider": "workers_ai", "model": ""}}"#, "models.vision: the model \"\" is not in the price book");
        for bad in ["http://a.example/v1", "https://a.example:8443/v1", "https://user@a.example/v1", "ftp://a.example/v1", "a.example/v1", "http://127.0.0.1:4810/v1"] {
            refused(&slot(json!({ "provider": "openai", "base_url": bad, "model": "glm-5-3-flash" })), "models.cheap: base_url:");
        }
        for bad in ["https://a.example/v1/", "https://a.example/v1?x=1"] {
            refused(&slot(json!({ "provider": "openai", "base_url": bad, "model": "glm-5-3-flash" })), "without a query or a trailing /");
        }
        refused("[]", "models:");
        // a local fleet's fake, where its egress may reach local addresses
        let local = slot(json!({ "provider": "openai", "base_url": "http://127.0.0.1:4810/v1", "model": "glm-5-3-flash" }));
        assert_eq!(parse(&local, true).unwrap().cheap, openai("http://127.0.0.1:4810/v1", FINITE_PRIVATE_FLASH));
        assert!(parse(&local, false).is_err(), "a deployment never names one");
        // a model the book prices, whichever slot names it
        let mut other = book.clone();
        other.models.retain(|m| m.model != FINITE_PRIVATE_FLASH);
        assert!(Models::parse(Some(FINITE), &other, false).unwrap_err().contains("models.cheap: the model \"glm-5-3-flash\" is not in the price book"));
    }

    /// Goal: one rule names the agent a guest's model call is for, chat and
    /// transcription alike: the header, or a key `agent:<name>` from a
    /// client that sends no header of its own (OpenAI's SDKs); any other
    /// key is the guest's placeholder. Invalid: none, a malformed name
    /// either way, and two that disagree are refused.
    #[test]
    fn the_header_or_the_key_names_the_agent() {
        let named = |h: Option<&str>, a: Option<&str>| agent_named(h, a);
        assert_eq!(named(Some("juniper--k3x9"), None), Ok("juniper--k3x9".into()));
        assert_eq!(named(Some("juniper--k3x9"), Some("Bearer fragment-model")), Ok("juniper--k3x9".into()), "Hermes' chat calls: the header, its key a placeholder");
        assert_eq!(named(None, Some("Bearer agent:juniper--k3x9")), Ok("juniper--k3x9".into()), "an OpenAI SDK's call: the key");
        assert_eq!(named(None, Some("bearer   agent:juniper--k3x9 ")), Ok("juniper--k3x9".into()));
        assert_eq!(named(Some("juniper--k3x9"), Some("Bearer agent:juniper--k3x9")), Ok("juniper--k3x9".into()), "both, agreeing");
        assert_eq!(named(None, None), Err(Unnamed::Missing));
        assert_eq!(named(None, Some("Bearer fragment-model")), Err(Unnamed::Missing), "a placeholder names no one");
        assert_eq!(named(None, Some("Basic agent:juniper--k3x9")), Err(Unnamed::Missing), "only a bearer key names");
        for bad in ["Bearer agent:", "Bearer agent:juniper", "Bearer agent:Juniper--K3X9", "Bearer agent:a b--k3x9", "Bearer agent:juniper--k3x9--x"] {
            assert_eq!(named(None, Some(bad)), Err(Unnamed::Malformed), "{bad}");
        }
        assert_eq!(named(Some("not a name"), None), Err(Unnamed::Malformed));
        assert_eq!(named(Some("juniper--k3x9"), Some("Bearer agent:willow--k3x9")), Err(Unnamed::Disagree));
        assert_eq!(named(Some("juniper--k3x9"), Some("Bearer agent:nope")), Err(Unnamed::Malformed), "a malformed key is refused whatever the header says");
        assert!(Unnamed::Missing.message().contains("agent:<label>--<suffix>"));
    }

    /// Goal: the deployment's vision model is GLM-5.3 Flash unless it names
    /// another, and one the price book does not price is refused (a ledger
    /// would refuse every call's reservation). Method: the default, a priced
    /// model named, and unpriced ones.
    #[test]
    fn the_vision_model_is_one_the_book_prices() {
        let book = PriceBook::defaults();
        let vision = |text: &str| Models::parse(Some(text), &book, false).map(|m| m.vision.model().to_string());
        assert_eq!(Models::defaults().vision, workers_ai(GLM_FLASH));
        assert_eq!(VISION_MODEL_DEFAULT, "@cf/zai-org/glm-5.3-flash");
        assert_ne!(VISION_MODEL_DEFAULT, FALLBACK_MODEL_DEFAULT, "the fallback reads no images");
        assert_eq!(vision(&json!({ "vision": { "provider": "workers_ai", "model": MEDIUM_MODEL } }).to_string()).as_deref(), Ok(MEDIUM_MODEL));
        let unpriced = vision(&json!({ "vision": { "provider": "workers_ai", "model": "@cf/meta/llama-4-scout-17b-16e-instruct" } }).to_string()).unwrap_err();
        assert!(unpriced.contains("models.vision") && unpriced.contains("not in the price book") && unpriced.contains(GLM_FLASH), "{unpriced}");
        assert_eq!(finite().vision, workers_ai(GLM_FLASH), "Finite's deployment keeps vision on Workers AI (decision 61)");
        // priced at Flash's prices
        let flash = book.models.iter().find(|m| m.model == VISION_MODEL_DEFAULT).unwrap();
        assert_eq!((flash.price.input, flash.price.cached_input, flash.price.output), (150_000, 30_000, 500_000));
        let mut other = book.clone();
        other.models.retain(|m| m.model != VISION_MODEL_DEFAULT);
        assert!(Models::parse(None, &other, false).is_err(), "the default too, were it unpriced");
    }

    /// Goal: Hermes' vision call fits the route at the size Hermes shrinks
    /// an image to after a 413 (its `_RESIZE_TARGET_BYTES`), so its one
    /// retry is answered. Method: the body of such a call, with a long
    /// prompt about a screen, against the cap.
    #[test]
    fn hermes_shrunk_image_fits_a_call() {
        let url = format!("data:image/jpeg;base64,{}", "A".repeat(IMAGE_DATA_URL_MAX_BYTES - "data:image/jpeg;base64,".len()));
        assert_eq!(url.len(), IMAGE_DATA_URL_MAX_BYTES);
        let prompt = "  [12] AXButton 'Save' (100, 200, 80, 24)\n".repeat(2_000);
        let body = json!({
            "model": "vision", "max_tokens": 4096, "temperature": 0.1,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": prompt }, { "type": "image_url", "image_url": { "url": url } }] }],
        });
        let bytes = serde_json::to_vec(&body).unwrap().len();
        assert!(bytes > 5 * 1024 * 1024 && bytes <= MODEL_BODY_MAX_BYTES, "{bytes} bytes against {MODEL_BODY_MAX_BYTES}");
    }

    /// Goal: a tier picks its model; `high` and model ids are refused.
    /// Method: every tier name, and names that are not.
    #[test]
    fn tiers_pick_models() {
        assert_eq!(tier(Tier::Cheap).model.id(), "@cf/zai-org/glm-5.3-flash", "GLM-5.3 Flash, DeepSeek its fallback (Paul, 2026-10-09)");
        assert_eq!(tier(Tier::Cheap).model.id(), CHEAP_MODEL);
        assert_eq!(tier(Tier::Medium).model.id(), MEDIUM_MODEL);
        assert_eq!(DEFAULTS.capped(Named::Tier(Tier::High)), Err(Refusal::HighOff));
        assert!(Refusal::HighOff.message().contains("off until Cloudflare raises"));
        assert_eq!(tier_named(None), Ok(Tier::Cheap));
        assert_eq!(tier_named(Some("medium")), Ok(Tier::Medium));
        assert_eq!(tier_named(Some("@cf/zai-org/glm-5.3")), Err(Refusal::UnknownTier));
        assert_eq!(tier_named(Some("z-ai/glm-5.3-flashx")), Err(Refusal::UnknownTier));
        // the book prices every model a tier runs
        let book = crate::price::PriceBook::defaults();
        for model in [CHEAP_MODEL, MEDIUM_MODEL] {
            assert!(book.models.iter().any(|m| m.model == model), "{model}");
        }
    }

    /// Goal: what reaches the vendor is the client's request bounded (lesson
    /// 7). Method: a request with a model, a large max_tokens in both names,
    /// GLM's dearest effort, and a client's own stream options.
    #[test]
    fn a_request_is_bounded() {
        let body = json!({
            "model": "medium", "messages": [{ "role": "user", "content": "hi" }], "tools": [{ "type": "function" }],
            "max_tokens": 100_000, "max_completion_tokens": 50_000, "reasoning_effort": "medium", "stream_options": { "x": 1 },
        });
        let b = bound(tier(Tier::Medium), body, false).unwrap();
        assert_eq!((b.model.id(), b.max_tokens, b.stream), (MEDIUM_MODEL, MAX_TOKENS, false));
        assert_eq!(
            b.input,
            json!({
                "messages": [{ "role": "user", "content": "hi" }], "tools": [{ "type": "function" }],
                "max_tokens": MAX_TOKENS, "reasoning_effort": "low", "stream": false,
            }),
            "no model, the cap, the effort clamped, no stream options when not streaming"
        );
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{}], "max_tokens": 64, "reasoning_effort": "high" }), true).unwrap();
        assert_eq!((b.model.id(), b.max_tokens), (CHEAP_MODEL, 64), "what it asked for, under the cap");
        assert_eq!(b.input["reasoning_effort"], "high");
        assert_eq!((b.input["stream"].clone(), b.input["stream_options"].clone()), (json!(true), json!({ "include_usage": true })));
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{}], "max_completion_tokens": 32 }), false).unwrap();
        assert_eq!((b.max_tokens, b.input.get("max_completion_tokens")), (32, None), "either name, sent as max_tokens");
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": [{}] }), false).unwrap().max_tokens, MAX_TOKENS, "none asked: the cap");
    }

    #[test]
    fn a_request_out_of_shape_is_refused() {
        let msgs = || json!([{ "role": "user", "content": "hi" }]);
        assert_eq!(DEFAULTS.capped(Named::Tier(Tier::High)).and_then(|t| bound(t, json!({ "messages": msgs() }), false)), Err(Refusal::HighOff));
        assert_eq!(bound(tier(Tier::Cheap), json!([1]), false), Err(Refusal::NotAnObject));
        assert_eq!(bound(tier(Tier::Cheap), json!({}), false), Err(Refusal::NoMessages));
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": [] }), false), Err(Refusal::NoMessages));
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": "hi" }), false), Err(Refusal::NoMessages));
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": msgs(), "max_tokens": 0 }), false), Err(Refusal::MaxTokens));
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": msgs(), "max_tokens": "9" }), false), Err(Refusal::MaxTokens));
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": msgs(), "max_completion_tokens": -1 }), false), Err(Refusal::MaxTokens));
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": msgs(), "stream": "yes" }), false), Err(Refusal::Stream));
    }

    /// Goal: the worst case is an upper bound: a byte a token in, the cap
    /// out. Method: priced at the defaults, it is above what the call costs.
    #[test]
    fn the_worst_case_bounds_the_call() {
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{ "role": "user", "content": "hello there" }], "max_tokens": 1000 }), false).unwrap();
        let worst = b.worst(80);
        assert_eq!(worst, Usage::Tokens { model: CHEAP_MODEL.into(), input: 80, cached_input: 0, cache_write: 0, output: 1000 });
        let book = crate::price::PriceBook::defaults();
        let used = usage_of(CHEAP_MODEL, &json!({ "prompt_tokens": 20, "completion_tokens": 1000 })).unwrap();
        assert!(book.price(&worst).unwrap().charge >= book.price(&used).unwrap().charge);
    }

    /// Goal: usage reads as the ledger meters it (S4's shapes). Method: an
    /// uncached call, a cached one, an Anthropic-style cache write, and
    /// usages that do not read.
    #[test]
    fn usage_reads_as_the_ledger_meters_it() {
        let u = |v: Value| usage_of(MEDIUM_MODEL, &v);
        let tokens = |input, cached_input, cache_write, output| Some(Usage::Tokens { model: MEDIUM_MODEL.into(), input, cached_input, cache_write, output });
        assert_eq!(
            u(json!({ "prompt_tokens": 187, "completion_tokens": 12, "total_tokens": 199, "prompt_tokens_details": { "cached_tokens": 0 }, "neurons": 3.09 })),
            tokens(187, 0, 0, 12)
        );
        assert_eq!(u(json!({ "prompt_tokens": 187, "completion_tokens": 12, "prompt_tokens_details": { "cached_tokens": 128 } })), tokens(59, 128, 0, 12));
        assert_eq!(u(json!({ "prompt_tokens": 100, "completion_tokens": 5, "cache_creation_input_tokens": 40 })), tokens(60, 0, 40, 5));
        assert_eq!(u(json!({ "prompt_tokens": 100, "completion_tokens": 5, "prompt_tokens_details": { "cache_write_tokens": 30 } })), tokens(70, 0, 30, 5));
        assert_eq!(u(json!({ "prompt_tokens": 10 })), None, "no completion count");
        assert_eq!(u(json!({ "prompt_tokens": -1, "completion_tokens": 1 })), None);
        assert_eq!(u(json!({ "prompt_tokens": 10, "completion_tokens": 1, "prompt_tokens_details": { "cached_tokens": 11 } })), None, "more cached than prompted");
        assert_eq!(u(json!({ "prompt_tokens": 10, "completion_tokens": 1, "prompt_tokens_details": { "cached_tokens": "x" } })), None);
    }

    /// S4's streamed answer (logs/raw-sse-cheap.txt), its per-chunk usage
    /// deltas and Workers AI's last line.
    const S4_STREAM: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"\",\"role\":\"assistant\"},\"finish_reason\":null,\"index\":0}],\"id\":\"153\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":23,\"completion_tokens\":0,\"total_tokens\":23}}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"1, 2, 3,\"},\"finish_reason\":null,\"index\":0}],\"id\":\"153\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":9,\"total_tokens\":9}}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\",\"index\":0}],\"id\":\"153\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":6,\"total_tokens\":6}}\n\n",
        "data: {\"choices\":[],\"id\":\"153\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}}\n\n",
        "data: {\"response\":\"\",\"usage\":{\"prompt_tokens\":23,\"completion_tokens\":15,\"total_tokens\":38,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
        "data: [DONE]\n\n",
    );

    fn data_lines(out: &[u8]) -> Vec<Value> {
        String::from_utf8(out.to_vec())
            .unwrap()
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .map(|d| serde_json::from_str(d).unwrap())
            .collect()
    }

    /// Goal: only the last, cumulative usage is metered, and the client
    /// reads OpenAI's shape (S4's two quirks). Method: S4's stream, fed in
    /// pieces that split lines, and fed whole: the same either way.
    #[test]
    fn a_stream_meters_its_last_cumulative_usage() {
        for piece in [1, 7, 64, S4_STREAM.len()] {
            let mut s = Stream::default();
            let mut out = Vec::new();
            for chunk in S4_STREAM.as_bytes().chunks(piece) {
                s.push(chunk, Some(&mut out));
            }
            s.finish(Some(&mut out));
            let usage = s.usage().cloned().unwrap();
            assert_eq!(usage_of(CHEAP_MODEL, &usage), Some(Usage::Tokens { model: CHEAP_MODEL.into(), input: 23, cached_input: 0, cache_write: 0, output: 15 }), "pieces of {piece}");
            let lines = data_lines(&out);
            assert_eq!(lines.len(), 5);
            assert!(lines[..3].iter().all(|l| l.get("usage").is_none() && l["choices"].as_array().is_some_and(|c| !c.is_empty())), "no deltas reach the client");
            assert_eq!(lines[4], json!({ "id": "153", "object": "chat.completion.chunk", "choices": [], "usage": usage }), "Workers AI's last line, as OpenAI's");
            assert!(String::from_utf8(out.clone()).unwrap().ends_with("data: [DONE]\n\n"));
        }
        // metering only, with no client: the same usage
        let mut s = Stream::default();
        s.push(S4_STREAM.as_bytes(), None);
        s.finish(None);
        assert_eq!(s.usage().and_then(|u| u["completion_tokens"].as_u64()), Some(15));
    }

    /// Goal: an OpenAI-style stream (usage once, on a last chunk with no
    /// choices) passes as it came and meters that usage; a stream that
    /// broke before its usage, or ran a line past the limit, meters none.
    #[test]
    fn other_streams() {
        let openai = "data: {\"id\":\"a\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"id\":\"a\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n";
        let mut s = Stream::default();
        let mut out = Vec::new();
        s.push(openai.as_bytes(), Some(&mut out));
        s.finish(Some(&mut out));
        assert_eq!(out, openai.as_bytes(), "nothing to rewrite");
        assert_eq!(s.usage().and_then(|u| u["prompt_tokens"].as_u64()), Some(3));
        let mut broken = Stream::default();
        broken.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"h", None);
        broken.finish(None);
        assert_eq!(broken.usage(), None, "it broke before its usage: metered at the reservation");
        let mut long = Stream::default();
        long.push(b"data: {\"response\":\"\",\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n", None);
        let mut out = Vec::new();
        long.push(&vec![b'x'; SSE_LINE_MAX + 1], Some(&mut out));
        long.finish(None);
        assert_eq!(out.len(), SSE_LINE_MAX + 1, "an overlong line passes through unread");
        assert_eq!(long.usage(), None, "and the call is metered at its reservation");
    }

    const FALLBACK: &str = FALLBACK_MODEL_DEFAULT;

    /// Goal: the deployment's fallback model is DeepSeek V4 Flash unless it
    /// names another, another model than the cheap tier's, priced at Workers
    /// AI's list price; one the book does not price is refused, as a vision
    /// model is. Method: the default, a priced model named, and unpriced
    /// ones.
    #[test]
    fn the_fallback_model_is_one_the_book_prices() {
        let book = PriceBook::defaults();
        let fallback = |text: &str| Models::parse(Some(text), &book, false).map(|m| m.fallback.model().to_string());
        assert_eq!(Models::defaults().fallback, workers_ai("@cf/deepseek-ai/deepseek-v4-flash-0731"));
        assert_ne!(FALLBACK, CHEAP_MODEL, "a fallback is another model");
        assert_eq!(fallback(&json!({ "fallback": { "provider": "workers_ai", "model": MEDIUM_MODEL } }).to_string()).as_deref(), Ok(MEDIUM_MODEL));
        let unpriced = fallback(&json!({ "fallback": { "provider": "workers_ai", "model": "anthropic/claude-haiku-4.5" } }).to_string()).unwrap_err();
        assert!(unpriced.contains("models.fallback: the model \"anthropic/claude-haiku-4.5\" is not in the price book") && unpriced.contains(FALLBACK), "{unpriced}");
        // both priced already: the cheap tier's GLM, its DeepSeek fallback
        let p = |model: &str| book.models.iter().find(|m| m.model == model).unwrap().price;
        let (d, g) = (p(FALLBACK), p(CHEAP_MODEL));
        assert_eq!((d.input, d.cached_input, d.cache_write, d.output), (440_000, 14_000, 440_000, 1_320_000));
        assert_eq!((g.input, g.cached_input, g.cache_write, g.output), (150_000, 30_000, 150_000, 500_000));
    }

    /// Goal: what falls back is the vendor failing before it answered
    /// anything: its rate limit (429) or its own failure (5xx). A refusal
    /// of the request (any other 4xx) would be the fallback's too, and an
    /// answer that began (2xx) is final. Method: every class of status.
    #[test]
    fn a_rate_limit_or_a_failure_falls_back() {
        for status in [429, 500, 502, 503, 504, 520, 599] {
            assert!(falls_back(status), "{status}");
        }
        for status in [100, 200, 204, 301, 400, 401, 402, 403, 404, 408, 413, 422, 428, 430, 499, 600, 0] {
            assert!(!falls_back(status), "{status}");
        }
    }

    /// Goal: a call is tried on its model, then, when that one failed
    /// before answering, on the fallback model once (no race, no ladder):
    /// the fallback's answer, or its failure, is the call's, and the model
    /// that answered is the one whose usage is priced. Method: each path
    /// through `Tries`, valid and invalid, on the default models and on
    /// Finite's (its tiers on Finite Private, Workers AI its fallback), and
    /// each again (a replay decides the same).
    #[test]
    fn a_failed_call_falls_back_once() {
        let finite = finite();
        for models in [&*DEFAULTS, &finite] {
            let cheap = models.route(Named::Tier(Tier::Cheap)).unwrap();
            let medium = models.route(Named::Tier(Tier::Medium)).unwrap();
            let fallback = models.fallback();
            for _replay in 0..2 {
                // the model is not reached: the fallback answers
                let mut t = Tries::new(cheap, Some(fallback));
                assert_eq!((t.model(), t.fell()), (cheap, false));
                assert_eq!(t.after(Tried::Unreached), Some(fallback));
                assert_eq!((t.model(), t.fell()), (fallback, true));
                assert_eq!(t.after(Tried::Answered(200)), None, "its answer is the call's");
                assert_eq!(t.model().id(), fallback.id(), "priced as the fallback's");
                // the model fails (tonight's 502), and so does the fallback: one fallback, never a third try
                let mut t = Tries::new(cheap, Some(fallback));
                assert_eq!(t.after(Tried::Answered(502)), Some(fallback));
                assert_eq!(t.after(Tried::Answered(502)), None, "the fallback's failure is the call's");
                assert_eq!(t.after(Tried::Unreached), None);
                assert_eq!((t.model(), t.fell()), (fallback, true));
                // a rate limit falls back too (Finite Private's key's window spent)
                let mut t = Tries::new(medium, Some(fallback));
                assert_eq!(t.after(Tried::Answered(429)), Some(fallback));
                // an answer that began is final: a stream that breaks after it is no try
                let mut t = Tries::new(cheap, Some(fallback));
                assert_eq!(t.after(Tried::Answered(200)), None);
                assert_eq!(t.after(Tried::Unreached), None, "a stream that began never falls back");
                assert_eq!(t.after(Tried::Answered(502)), None);
                assert_eq!((t.model(), t.fell()), (cheap, false));
                // a refusal of the request is the call's: the fallback would refuse it too
                let mut t = Tries::new(cheap, Some(fallback));
                assert_eq!(t.after(Tried::Answered(400)), None);
                assert_eq!(t.after(Tried::Answered(502)), None, "the tries are over");
                assert_eq!(t.model(), cheap);
                // no fallback, or one that runs where the call's own model does: the failure is the call's
                let same = Model { named: Named::Fallback, upstream: cheap.upstream };
                for fallback in [None, Some(same)] {
                    let mut t = Tries::new(cheap, fallback);
                    assert_eq!(t.after(Tried::Answered(503)), None, "{fallback:?}");
                    assert_eq!((t.model(), t.fell()), (cheap, false));
                }
            }
        }
        // the same model id on another provider is another model
        let private = openai("https://finite-private.finite.containers.tinfoil.dev/v1", FINITE_PRIVATE_FLASH);
        let other = openai("https://other.example/v1", FINITE_PRIVATE_FLASH);
        let mut t = Tries::new(Model { named: Named::Tier(Tier::Cheap), upstream: &private }, Some(Model { named: Named::Fallback, upstream: &other }));
        assert_eq!(t.after(Tried::Answered(503)).map(|m| m.upstream), Some(&other));
    }

    /// Goal: a call that fell back is held at its own model's worst case
    /// (its reservation, made before any try) and settled from the usage
    /// the fallback reported, priced as the fallback's; the hold covers a
    /// fallback's call of an agent's usual size. Method: a cheap call's
    /// worst case and the fallback's usage for a 30,000-token turn, priced
    /// with the default book, then Finite's.
    #[test]
    fn a_fallen_back_call_is_settled_as_the_fallbacks() {
        let book = PriceBook::defaults();
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{ "role": "user", "content": "hi" }] }), true).unwrap();
        let mut t = Tries::new(b.model, Some(DEFAULTS.fallback()));
        assert_eq!(t.after(Tried::Answered(502)).map(|m| m.id()), Some(FALLBACK));
        let held = b.worst(120_000);
        assert_eq!(held, Usage::Tokens { model: CHEAP_MODEL.into(), input: 120_000, cached_input: 0, cache_write: 0, output: u64::from(MAX_TOKENS) });
        let used = usage_of(t.model().id(), &json!({ "prompt_tokens": 30_000, "completion_tokens": 400, "prompt_tokens_details": { "cached_tokens": 0 } })).unwrap();
        assert_eq!(used, Usage::Tokens { model: FALLBACK.into(), input: 30_000, cached_input: 0, cache_write: 0, output: 400 });
        let (held, used) = (book.price(&held).unwrap(), book.price(&used).unwrap());
        // 30,000 in and 400 out at $0.44 and $1.32 a million: $0.013728
        assert_eq!(used.list, 13_728);
        assert!(used.charge < held.charge, "{used:?} within {held:?}");
        // the dearer fallback's worst case is past the cheap tier's hold: a call that large is charged in full, past it
        let fallback_worst = Usage::Tokens { model: FALLBACK.into(), input: 120_000, cached_input: 0, cache_write: 0, output: u64::from(MAX_TOKENS) };
        assert!(book.price(&fallback_worst).unwrap().charge > held.charge);

        // Finite's: held as Finite Private's, settled as Workers AI's GLM-5.3 Flash, at one price
        let finite = finite();
        let b = bound(finite.capped(Named::Tier(Tier::Cheap)).unwrap(), json!({ "messages": [{ "role": "user", "content": "hi" }] }), true).unwrap();
        assert_eq!(b.worst(1_000), Usage::Tokens { model: "glm-5-3-flash".into(), input: 1_000, cached_input: 0, cache_write: 0, output: u64::from(MAX_TOKENS) });
        let mut t = Tries::new(b.model, Some(finite.fallback()));
        assert_eq!(t.after(Tried::Answered(429)).map(|m| m.id()), Some(GLM_FLASH));
        let usage = json!({ "prompt_tokens": 30_000, "completion_tokens": 400 });
        let (on_private, fell) = (usage_of(b.model.id(), &usage).unwrap(), usage_of(t.model().id(), &usage).unwrap());
        assert_eq!(book.price(&on_private).unwrap(), book.price(&fell).unwrap(), "a person pays the same whichever answered (decision 61)");
        assert!(book.price(&fell).unwrap().charge < book.price(&b.worst(1_000)).unwrap().charge);
    }

    /// Goal: Finite Private's answers meter as Workers AI's do: SGLang's
    /// usage (no cached tokens told, its details null) reads with all its
    /// prompt uncached, and its stream (OpenAI's: each chunk's usage null,
    /// the whole call's on a last chunk with no choices, as its limiter
    /// asks for it) meters that last usage and reaches the client with
    /// no usage on a chunk with choices. Method: both shapes, fed in pieces.
    #[test]
    fn finite_privates_answers_meter_as_any() {
        let u = usage_of(FINITE_PRIVATE_FLASH, &json!({ "prompt_tokens": 1_200, "completion_tokens": 80, "total_tokens": 1_280, "prompt_tokens_details": null })).unwrap();
        assert_eq!(u, Usage::Tokens { model: FINITE_PRIVATE_FLASH.into(), input: 1_200, cached_input: 0, cache_write: 0, output: 80 });
        let sglang = concat!(
            "data: {\"id\":\"fp1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}],\"usage\":null}\n\n",
            "data: {\"id\":\"fp1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"terminal\",\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]}}],\"usage\":null}\n\n",
            "data: {\"id\":\"fp1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":null}\n\n",
            "data: {\"id\":\"fp1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":900,\"completion_tokens\":21,\"total_tokens\":921}}\n\n",
            "data: [DONE]\n\n",
        );
        for piece in [1, 9, sglang.len()] {
            let mut s = Stream::default();
            let mut out = Vec::new();
            for chunk in sglang.as_bytes().chunks(piece) {
                s.push(chunk, Some(&mut out));
            }
            s.finish(Some(&mut out));
            assert_eq!(s.usage().and_then(|v| usage_of(FINITE_PRIVATE_FLASH, v)), Some(Usage::Tokens { model: FINITE_PRIVATE_FLASH.into(), input: 900, cached_input: 0, cache_write: 0, output: 21 }), "pieces of {piece}");
            let lines = data_lines(&out);
            assert_eq!(lines.len(), 4);
            assert!(lines[..3].iter().all(|l| l.get("usage").is_none()), "no chunk with choices carries usage");
            assert_eq!(lines[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "terminal", "a tool call's delta as it came");
            assert_eq!(lines[3], json!({ "id": "fp1", "object": "chat.completion.chunk", "choices": [], "usage": { "prompt_tokens": 900, "completion_tokens": 21, "total_tokens": 921 } }), "the last chunk as it came");
        }
    }
}
