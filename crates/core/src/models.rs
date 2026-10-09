//! The model route's bounds (docs/cloudflare-v1.md, decision 23; "Lessons
//! from cloudflare/agents" 7 and 8; spike S4). Pure: cell/src/models.rs
//! makes the calls and meters them on the payer's ledger.
//!
//! A call names a tier, never a model: the tier picks the model and caps
//! what one call may write. The route takes one name besides the tiers,
//! `vision` (`Named`): the deployment's vision model (`vision_model`), for
//! an agent runtime's calls about an image (Hermes' auxiliary vision, which
//! reads its `computer_use` screenshots). It is no tier: an agent, a job's
//! step or a manifest names none but the tiers. What the platform sends is the client's
//! OpenAI-shaped chat completion with only these changes (`bound`): no
//! `model` (the tier's is the call's), `max_tokens` at most the tier's cap,
//! GLM's `reasoning_effort` clamped (GLM takes a missing or unknown one as
//! `max`, the dearest), and usage asked for when it streams. Nothing else
//! of the client's reaches the vendor: no header, no key.
//!
//! A call that its model fails before answering anything (it is not
//! reached, or it answers 429 or a 5xx) is made once more, the same, on the
//! deployment's fallback model (`Tries`; Paul, 2026-10-08: "we def need
//! fallback models"), DeepSeek V4 Flash unless it names another. Not a race:
//! the second call is made only after the first has failed. An answer that
//! began is final, and the fallback's failure is the call's.
//!
//! What a call may cost is reserved before it is made (`worst`); what it
//! cost is read from its answer's usage (`usage_of`), priced as the model
//! that answered it. A streamed answer
//! carries usage on every chunk, each a per-chunk delta, then a line of its
//! own with the whole call's (spike S4): only that last, cumulative one is
//! metered, and the client reads OpenAI's shape, usage once on a last chunk
//! with no choices (`Stream`).

use fragment_proto::Tier;
use serde_json::{json, Value};

use crate::price::{PriceBook, Usage};

/// The tiers' models (decision 23), as Workers AI's catalog names them.
pub const CHEAP_MODEL: &str = "@cf/zai-org/glm-5.3-flash";
pub const MEDIUM_MODEL: &str = "@cf/zai-org/glm-5.3";
/// The route's name for the deployment's vision model.
pub const VISION: &str = "vision";
/// The vision model unless the deployment names another
/// (`FRAGMENT_VISION_MODEL`; Paul, 2026-10-05): GLM-5.3 Flash, the cheap
/// tier's own, "Vision: Yes" in Workers AI's catalog
/// (developers.cloudflare.com/workers-ai/models/glm-5.3-flash/, read
/// 2026-10-05), already priced. GLM-5.3, the medium tier's, takes no
/// images. DeepSeek Flash's vision build (`deepseek-flash`) is only on
/// DeepSeek's own API: Workers AI's DeepSeek-V4-Flash-0731 has no vision,
/// and reaching DeepSeek's would take our own key (decision 23: no BYOK).
pub const VISION_MODEL_DEFAULT: &str = CHEAP_MODEL;
/// The fallback model unless the deployment names another
/// (`FRAGMENT_FALLBACK_MODEL`; Paul, 2026-10-08): DeepSeek V4 Flash, on
/// Workers AI, a model of its own beside the tiers' GLMs (its own replicas
/// and its own rate limit, 50 calls a minute per account on Unified
/// Billing), which takes the tiers' requests as they are (tools,
/// `reasoning_effort`; checked 2026-10-09). It reads no images, so the
/// route's `vision` has no fallback.
pub const FALLBACK_MODEL_DEFAULT: &str = "@cf/deepseek-ai/deepseek-v4-flash-0731";
/// The largest image Hermes sends for its vision call after a size
/// refusal: it shrinks one to this many bytes of base64 data URL and tries
/// again once (its `tools/vision_tools.py`, `_RESIZE_TARGET_BYTES`, which
/// its config does not set).
pub const IMAGE_DATA_URL_MAX_BYTES: usize = 5 * 1024 * 1024;
/// A call's request: an image of `IMAGE_DATA_URL_MAX_BYTES` and a MiB for
/// the rest of it (Hermes' prompt about a screenshot carries the screen's
/// element list), so the call Hermes retries after a 413 fits. GLM's
/// million-token window is about 4 MB of text; an agent's window sends a
/// few hundred KiB.
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

/// A call's model and the most one call of it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capped {
    pub model: &'static str,
    pub max_tokens: u32,
}

/// What a call to the route names as its `model`: a tier, or `vision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Tier(Tier),
    Vision,
}

impl Named {
    pub fn as_str(self) -> &'static str {
        match self {
            Named::Tier(t) => t.as_str(),
            Named::Vision => VISION,
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

/// The model `tier` runs, and its cap; `high` is refused.
pub fn model_of(tier: Tier) -> Result<Capped, Refusal> {
    match tier {
        Tier::Cheap => Ok(Capped { model: CHEAP_MODEL, max_tokens: MAX_TOKENS }),
        Tier::Medium => Ok(Capped { model: MEDIUM_MODEL, max_tokens: MAX_TOKENS }),
        Tier::High => Err(Refusal::HighOff),
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

/// What a call to the model route names: `vision`, or a tier
/// (`tier_named`).
pub fn route_named(name: Option<&str>) -> Result<Named, Refusal> {
    match name {
        Some(VISION) => Ok(Named::Vision),
        other => tier_named(other).map(Named::Tier),
    }
}

/// The model a route call runs, and its cap: its tier's, or the
/// deployment's vision model (`vision_model`'s) at the tiers' cap.
pub fn capped(named: Named, vision_model: &'static str) -> Result<Capped, Refusal> {
    match named {
        Named::Tier(t) => model_of(t),
        Named::Vision => Ok(Capped { model: vision_model, max_tokens: MAX_TOKENS }),
    }
}

/// The deployment's vision model: the one it names (`FRAGMENT_VISION_MODEL`)
/// or `VISION_MODEL_DEFAULT`. It must be one the price book prices: a
/// ledger refuses to reserve a call it cannot price, so a deployment that
/// names another is refused before it serves a call, saying why.
pub fn vision_model(named: Option<&str>, book: &PriceBook) -> Result<String, String> {
    priced_model("vision", named, VISION_MODEL_DEFAULT, book)
}

/// The deployment's fallback model (`Tries`): the one it names
/// (`FRAGMENT_FALLBACK_MODEL`) or `FALLBACK_MODEL_DEFAULT`, one the price
/// book prices, as the vision model is: its calls are settled at its
/// prices.
pub fn fallback_model(named: Option<&str>, book: &PriceBook) -> Result<String, String> {
    priced_model("fallback", named, FALLBACK_MODEL_DEFAULT, book)
}

/// A model a deployment names for `what`, or `default`, when the book
/// prices it.
fn priced_model(what: &str, named: Option<&str>, default: &str, book: &PriceBook) -> Result<String, String> {
    let model = named.map(str::trim).unwrap_or(default);
    if book.models.iter().any(|m| m.model == model) {
        return Ok(model.to_string());
    }
    let priced: Vec<&str> = book.models.iter().map(|m| m.model.as_str()).collect();
    Err(format!("the {what} model {model:?} is not in the price book, which prices {}: add its prices (fragment_core::price) or name one of those", priced.join(", ")))
}

/// Whether an answer's status, before any of its body, sends the call on
/// to the fallback model: the vendor's rate limit (429), or its own failure
/// (5xx: tonight's Workers AI answered 502 "could not route request to AI
/// model", then 500, for five minutes). A refusal of the request itself
/// (another 4xx) would be the fallback's too, and a 200 has begun the
/// answer, which is final.
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
/// race. A fallback that is the call's own model is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tries {
    first: &'static str,
    fallback: Option<&'static str>,
    fell: bool,
    /// A try's answer was the call's: what follows (a stream that breaks
    /// after it began) is no try.
    done: bool,
}

impl Tries {
    pub fn new(model: &'static str, fallback: Option<&'static str>) -> Tries {
        Tries { first: model, fallback: fallback.filter(|f| *f != model), fell: false, done: false }
    }

    /// The model the next try calls, and, once the tries are done, the one
    /// whose answer is the call's: its usage is priced as that model's.
    pub fn model(&self) -> &'static str {
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
    pub fn after(&mut self, tried: Tried) -> Option<&'static str> {
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
        assert_ne!(next, self.first, "a fallback is another model");
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
pub struct Bounded {
    pub model: &'static str,
    /// What it asks for, at most the tier's cap.
    pub max_tokens: u32,
    /// The input `env.AI.run` takes: the body less `model`, bounded.
    pub input: Value,
    pub stream: bool,
}

impl Bounded {
    /// The call's worst case, reserved before it is made: every byte of the
    /// request a token in (a token is at least a byte, so this is an upper
    /// bound, the JSON's own bytes included), and every token it may write.
    pub fn worst(&self, body_bytes: usize) -> Usage {
        Usage::Tokens { model: self.model.to_string(), input: body_bytes as u64, cached_input: 0, cache_write: 0, output: u64::from(self.max_tokens) }
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
/// (`model_of` a tier, or `capped`).
pub fn bound(t: Capped, body: Value, stream: bool) -> Result<Bounded, Refusal> {
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

    fn tier(t: Tier) -> Capped {
        model_of(t).unwrap()
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
        assert_eq!(route_named(Some("high")).and_then(|n| capped(n, CHEAP_MODEL)), Err(Refusal::HighOff));
        assert_eq!(tier_named(Some("vision")), Err(Refusal::UnknownTier), "vision is no tier");
        assert_eq!(Tier::parse("vision"), None);
        assert_eq!((Named::Vision.as_str(), Named::Tier(Tier::Cheap).as_str()), ("vision", "cheap"));
        let v = capped(Named::Vision, "@cf/example/seeing").unwrap();
        assert_eq!(v, Capped { model: "@cf/example/seeing", max_tokens: MAX_TOKENS });
        assert_eq!(capped(Named::Tier(Tier::Medium), "@cf/example/seeing").unwrap().model, MEDIUM_MODEL, "a tier's call is the tier's");
        // an image part reaches the model as it came
        let image = json!([{ "type": "text", "text": "what is on the screen?" }, { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } }]);
        let b = bound(v, json!({ "model": "vision", "messages": [{ "role": "user", "content": image }] }), false).unwrap();
        assert_eq!((b.model, b.input["messages"][0]["content"].clone(), b.input.get("model")), ("@cf/example/seeing", image, None));
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
        assert_eq!(vision_model(None, &book).as_deref(), Ok(CHEAP_MODEL));
        assert_eq!(VISION_MODEL_DEFAULT, "@cf/zai-org/glm-5.3-flash");
        assert_eq!(vision_model(Some(MEDIUM_MODEL), &book).as_deref(), Ok(MEDIUM_MODEL));
        assert_eq!(vision_model(Some(" @cf/zai-org/glm-5.3-flash "), &book).as_deref(), Ok(CHEAP_MODEL));
        let unpriced = vision_model(Some("@cf/meta/llama-4-scout-17b-16e-instruct"), &book).unwrap_err();
        assert!(unpriced.contains("the vision model") && unpriced.contains("not in the price book") && unpriced.contains(CHEAP_MODEL), "{unpriced}");
        assert!(vision_model(Some(""), &book).is_err());
        // priced at Flash's prices
        let flash = book.models.iter().find(|m| m.model == VISION_MODEL_DEFAULT).unwrap();
        assert_eq!((flash.price.input, flash.price.cached_input, flash.price.output), (150_000, 30_000, 500_000));
        let mut other = book.clone();
        other.models.retain(|m| m.model != CHEAP_MODEL);
        assert!(vision_model(None, &other).is_err(), "the default too, were it unpriced");
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
        assert_eq!(model_of(Tier::Cheap).unwrap().model, CHEAP_MODEL);
        assert_eq!(model_of(Tier::Medium).unwrap().model, MEDIUM_MODEL);
        assert_eq!(model_of(Tier::High), Err(Refusal::HighOff));
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
        assert_eq!((b.model, b.max_tokens, b.stream), (MEDIUM_MODEL, MAX_TOKENS, false));
        assert_eq!(
            b.input,
            json!({
                "messages": [{ "role": "user", "content": "hi" }], "tools": [{ "type": "function" }],
                "max_tokens": MAX_TOKENS, "reasoning_effort": "low", "stream": false,
            }),
            "no model, the cap, the effort clamped, no stream options when not streaming"
        );
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{}], "max_tokens": 64, "reasoning_effort": "high" }), true).unwrap();
        assert_eq!((b.model, b.max_tokens), (CHEAP_MODEL, 64), "what it asked for, under the cap");
        assert_eq!(b.input["reasoning_effort"], "high");
        assert_eq!((b.input["stream"].clone(), b.input["stream_options"].clone()), (json!(true), json!({ "include_usage": true })));
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{}], "max_completion_tokens": 32 }), false).unwrap();
        assert_eq!((b.max_tokens, b.input.get("max_completion_tokens")), (32, None), "either name, sent as max_tokens");
        assert_eq!(bound(tier(Tier::Cheap), json!({ "messages": [{}] }), false).unwrap().max_tokens, MAX_TOKENS, "none asked: the cap");
    }

    #[test]
    fn a_request_out_of_shape_is_refused() {
        let msgs = || json!([{ "role": "user", "content": "hi" }]);
        assert_eq!(model_of(Tier::High).and_then(|t| bound(t, json!({ "messages": msgs() }), false)), Err(Refusal::HighOff));
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

    const DEEPSEEK: &str = FALLBACK_MODEL_DEFAULT;

    /// Goal: the deployment's fallback model is DeepSeek V4 Flash unless it
    /// names another, priced at Workers AI's list price; one the book does
    /// not price is refused, as a vision model is. Method: the default, a
    /// priced model named, and unpriced ones.
    #[test]
    fn the_fallback_model_is_one_the_book_prices() {
        let book = PriceBook::defaults();
        assert_eq!(fallback_model(None, &book).as_deref(), Ok("@cf/deepseek-ai/deepseek-v4-flash-0731"));
        assert_eq!(fallback_model(Some(" @cf/zai-org/glm-5.3 "), &book).as_deref(), Ok(MEDIUM_MODEL));
        let unpriced = fallback_model(Some("anthropic/claude-haiku-4.5"), &book).unwrap_err();
        assert!(unpriced.contains("the fallback model \"anthropic/claude-haiku-4.5\" is not in the price book") && unpriced.contains(DEEPSEEK), "{unpriced}");
        assert!(fallback_model(Some(""), &book).is_err());
        let p = book.models.iter().find(|m| m.model == DEEPSEEK).unwrap().price;
        assert_eq!((p.input, p.cached_input, p.cache_write, p.output), (440_000, 14_000, 440_000, 1_320_000));
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
    /// through `Tries`, valid and invalid, and each again (a replay decides
    /// the same).
    #[test]
    fn a_failed_call_falls_back_once() {
        for _replay in 0..2 {
            // the model is not reached: the fallback answers
            let mut t = Tries::new(CHEAP_MODEL, Some(DEEPSEEK));
            assert_eq!((t.model(), t.fell()), (CHEAP_MODEL, false));
            assert_eq!(t.after(Tried::Unreached), Some(DEEPSEEK));
            assert_eq!((t.model(), t.fell()), (DEEPSEEK, true));
            assert_eq!(t.after(Tried::Answered(200)), None, "its answer is the call's");
            assert_eq!(t.model(), DEEPSEEK, "priced as the fallback's");
            // the model fails (tonight's 502), and so does the fallback: one fallback, never a third try
            let mut t = Tries::new(CHEAP_MODEL, Some(DEEPSEEK));
            assert_eq!(t.after(Tried::Answered(502)), Some(DEEPSEEK));
            assert_eq!(t.after(Tried::Answered(502)), None, "the fallback's failure is the call's");
            assert_eq!(t.after(Tried::Unreached), None);
            assert_eq!((t.model(), t.fell()), (DEEPSEEK, true));
            // a rate limit falls back too
            let mut t = Tries::new(MEDIUM_MODEL, Some(DEEPSEEK));
            assert_eq!(t.after(Tried::Answered(429)), Some(DEEPSEEK));
            // an answer that began is final: a stream that breaks after it is no try
            let mut t = Tries::new(CHEAP_MODEL, Some(DEEPSEEK));
            assert_eq!(t.after(Tried::Answered(200)), None);
            assert_eq!(t.after(Tried::Unreached), None, "a stream that began never falls back");
            assert_eq!(t.after(Tried::Answered(502)), None);
            assert_eq!((t.model(), t.fell()), (CHEAP_MODEL, false));
            // a refusal of the request is the call's: the fallback would refuse it too
            let mut t = Tries::new(CHEAP_MODEL, Some(DEEPSEEK));
            assert_eq!(t.after(Tried::Answered(400)), None);
            assert_eq!(t.after(Tried::Answered(502)), None, "the tries are over");
            assert_eq!(t.model(), CHEAP_MODEL);
            // no fallback, or one that is the call's own model: the failure is the call's
            for fallback in [None, Some(CHEAP_MODEL)] {
                let mut t = Tries::new(CHEAP_MODEL, fallback);
                assert_eq!(t.after(Tried::Answered(503)), None, "{fallback:?}");
                assert_eq!((t.model(), t.fell()), (CHEAP_MODEL, false));
            }
        }
    }

    /// Goal: a call that fell back is held at its own model's worst case
    /// (its reservation, made before any try) and settled from the usage
    /// the fallback reported, priced as the fallback's; the hold covers a
    /// fallback's call of an agent's usual size. Method: a cheap call's
    /// worst case and the fallback's usage for a 30,000-token turn, priced
    /// with the default book.
    #[test]
    fn a_fallen_back_call_is_settled_as_the_fallbacks() {
        let book = PriceBook::defaults();
        let b = bound(tier(Tier::Cheap), json!({ "messages": [{ "role": "user", "content": "hi" }] }), true).unwrap();
        let mut t = Tries::new(b.model, Some(DEEPSEEK));
        assert_eq!(t.after(Tried::Answered(502)), Some(DEEPSEEK));
        let held = b.worst(120_000);
        assert_eq!(held, Usage::Tokens { model: CHEAP_MODEL.into(), input: 120_000, cached_input: 0, cache_write: 0, output: u64::from(MAX_TOKENS) });
        let used = usage_of(t.model(), &json!({ "prompt_tokens": 30_000, "completion_tokens": 400, "prompt_tokens_details": { "cached_tokens": 0 } })).unwrap();
        assert_eq!(used, Usage::Tokens { model: DEEPSEEK.into(), input: 30_000, cached_input: 0, cache_write: 0, output: 400 });
        let (held, used) = (book.price(&held).unwrap(), book.price(&used).unwrap());
        // 30,000 in and 400 out at $0.44 and $1.32 a million: $0.013728
        assert_eq!(used.list, 13_728);
        assert!(used.charge < held.charge, "{used:?} within {held:?}");
    }
}
