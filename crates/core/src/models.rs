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
//! What a call may cost is reserved before it is made (`worst`); what it
//! cost is read from its answer's usage (`usage_of`). A streamed answer
//! carries usage on every chunk, each a per-chunk delta, then a line of its
//! own with the whole call's (spike S4): only that last, cumulative one is
//! metered, and the client reads OpenAI's shape, usage once on a last chunk
//! with no choices (`Stream`).
//!
//! A job's text step (`text_body`) is such a call, its tools bounded
//! (`TOOLS_MAX`, `TOOLS_MAX_BYTES`), and its answer read into one message
//! (`Answer`), whole or from its stream: the text, the tool calls, why it
//! stopped, and never the model's reasoning.

use fragment_proto::{valid_channel_name, valid_op_id, Tier, BUILTIN_CHANNELS};
use serde::Serialize;
use serde_json::{json, Value};

use crate::price::{PriceBook, Usage};
use crate::steps::{AiText, ToolChoice};

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
/// A text step's tools: at most this many, in at most this many bytes of
/// JSON (a long prompt's worth: what the model reads on every call).
pub const TOOLS_MAX: usize = 64;
pub const TOOLS_MAX_BYTES: usize = 64 * 1024;
/// A function's name, as OpenAI takes one: 1 to 64 of letters, digits,
/// `_` and `-`.
pub const TOOL_NAME_MAX_BYTES: usize = 64;
/// The tool calls one answer is read for: a delta for a later index is
/// out of the stream's contract, and dropped.
pub const TOOL_CALLS_MAX: usize = 128;

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
    /// A step's tools past `TOOLS_MAX` or `TOOLS_MAX_BYTES`, a name that is
    /// not a function's, or one named twice.
    Tools,
    /// A `tool_choice` with no tools, or naming a function not among them.
    ToolChoice,
    /// A draft to a channel that is no app's, or under a turn that is not
    /// `^[A-Za-z0-9._:-]{1,128}$`.
    Draft,
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
            Refusal::Tools => format!("ai.text takes at most {TOOLS_MAX} tools in {TOOLS_MAX_BYTES} bytes, each a function named 1 to {TOOL_NAME_MAX_BYTES} letters, digits, '_' or '-', once"),
            Refusal::ToolChoice => "tool_choice is none, auto, required, or a function among the tools".into(),
            Refusal::Draft => "a draft names an app's channel and a turn matching ^[A-Za-z0-9._:-]{1,128}$".into(),
        }
    }
}

fn valid_tool_name(name: &str) -> bool {
    (1..=TOOL_NAME_MAX_BYTES).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A job's text step as the chat completion it asks for (`bound` bounds it
/// for its tier): its messages (or its prompt, as one user message), and
/// what it named of `max_tokens`, `reasoning_effort`, `tools` and
/// `tool_choice`. Its draft's channel is checked here for its shape; the
/// cell checks the app declares it.
pub fn text_body(t: &AiText) -> Result<Value, Refusal> {
    let messages = match (&t.messages, &t.prompt) {
        (Some(m), _) => Value::Array(m.clone()),
        (None, Some(prompt)) => json!([{ "role": "user", "content": prompt }]),
        (None, None) => return Err(Refusal::NoMessages),
    };
    let mut body = json!({ "messages": messages });
    if let Some(n) = t.max_tokens {
        body["max_tokens"] = json!(n);
    }
    if let Some(effort) = &t.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    let tools = t.tools.as_deref().unwrap_or_default();
    if !tools.is_empty() {
        let mut names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let bytes = serde_json::to_vec(tools).expect("tools serialize").len();
        if tools.len() > TOOLS_MAX || bytes > TOOLS_MAX_BYTES || names.len() < tools.len() || !names.iter().all(|n| valid_tool_name(n)) {
            return Err(Refusal::Tools);
        }
        body["tools"] = json!(tools);
    }
    if let Some(choice) = &t.tool_choice {
        let named = match choice {
            ToolChoice::Mode(_) => true,
            ToolChoice::Named { function, .. } => tools.iter().any(|t| t.function.name == function.name),
        };
        if tools.is_empty() || !named {
            return Err(Refusal::ToolChoice);
        }
        body["tool_choice"] = json!(choice);
    }
    if let Some(d) = &t.draft {
        if !valid_channel_name(&d.channel) || BUILTIN_CHANNELS.contains(&d.channel.as_str()) || !valid_op_id(&d.turn) {
            return Err(Refusal::Draft);
        }
    }
    Ok(body)
}

/// One tool call of an answer, as OpenAI's message carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: CalledFunction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CalledFunction {
    pub name: String,
    /// JSON text, as the model wrote it (it may not parse).
    pub arguments: String,
}

/// What a text call answered, read from its whole answer
/// (`Answer::of_completion`) or folded from its stream's deltas
/// (`Stream::answering`): its text, its tool calls, and why it stopped.
/// The model's reasoning is never kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

impl Answer {
    /// The first choice of an unstreamed chat completion.
    pub fn of_completion(v: &Value) -> Answer {
        let choice = &v["choices"][0];
        let message = &choice["message"];
        let mut a = Answer { content: message["content"].as_str().unwrap_or("").to_string(), finish_reason: choice["finish_reason"].as_str().map(str::to_string), ..Answer::default() };
        for (i, call) in message["tool_calls"].as_array().into_iter().flatten().take(TOOL_CALLS_MAX).enumerate() {
            a.fold_call(i, call);
        }
        a
    }

    /// One streamed choice's delta, and its finish reason when it has one.
    fn take(&mut self, choice: &Value) {
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str() {
            self.content.push_str(text);
        }
        for (n, call) in delta["tool_calls"].as_array().into_iter().flatten().enumerate() {
            let index = call["index"].as_u64().map_or(n, |i| usize::try_from(i).unwrap_or(usize::MAX));
            if index < TOOL_CALLS_MAX {
                self.fold_call(index, call);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_string());
        }
    }

    /// A tool call, or a delta of one: its id and name as given, its
    /// arguments appended.
    fn fold_call(&mut self, index: usize, call: &Value) {
        assert!(index < TOOL_CALLS_MAX, "a tool call's index is bounded before it is folded");
        if self.tool_calls.len() <= index {
            self.tool_calls.resize(index + 1, ToolCall { kind: "function", ..ToolCall::default() });
        }
        let to = &mut self.tool_calls[index];
        if let Some(id) = call["id"].as_str().filter(|s| !s.is_empty()) {
            to.id = id.to_string();
        }
        if let Some(name) = call["function"]["name"].as_str().filter(|s| !s.is_empty()) {
            to.function.name = name.to_string();
        }
        if let Some(args) = call["function"]["arguments"].as_str() {
            to.function.arguments.push_str(args);
        }
    }

    /// The assistant's message, as the next call's messages take it back:
    /// `{role, content, tool_calls?}`.
    pub fn message(&self) -> Value {
        let mut m = json!({ "role": "assistant", "content": self.content });
        if !self.tool_calls.is_empty() {
            m["tool_calls"] = json!(self.tool_calls);
        }
        m
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
    let model = named.map(str::trim).unwrap_or(VISION_MODEL_DEFAULT);
    if book.models.iter().any(|m| m.model == model) {
        return Ok(model.to_string());
    }
    let priced: Vec<&str> = book.models.iter().map(|m| m.model.as_str()).collect();
    Err(format!("the vision model {model:?} is not in the price book, which prices {}: add its prices (fragment_core::price) or name one of those", priced.join(", ")))
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
    /// The answer so far, when one is read (`answering`).
    answer: Option<Answer>,
}

impl Stream {
    /// A stream whose answer is read as it comes (a job's text step), as
    /// well as its usage.
    pub fn answering() -> Stream {
        Stream { answer: Some(Answer::default()), ..Stream::default() }
    }

    /// The answer so far (`answering`'s; `None` for a stream only metered).
    pub fn answer(&self) -> Option<&Answer> {
        self.answer.as_ref()
    }

    /// Whether a line ran past `SSE_LINE_MAX` (its content was not read).
    pub fn overlong(&self) -> bool {
        self.overlong
    }

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
        if let (Some(answer), Some(choice)) = (self.answer.as_mut(), chunk.get("choices").and_then(|c| c.get(0))) {
            answer.take(choice);
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
        let unpriced = vision_model(Some("@cf/deepseek-ai/deepseek-v4-flash-0731"), &book).unwrap_err();
        assert!(unpriced.contains("not in the price book") && unpriced.contains(CHEAP_MODEL), "{unpriced}");
        assert!(vision_model(Some(""), &book).is_err());
        // priced at Flash's prices: no new row, so no new book version
        let flash = book.models.iter().find(|m| m.model == VISION_MODEL_DEFAULT).unwrap();
        assert_eq!((flash.price.input, flash.price.cached_input, flash.price.output), (150_000, 30_000, 500_000));
        assert_eq!(book.version, 1);
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

    fn text(args: Value) -> AiText {
        match crate::steps::Step::from_parts("ai.text", args) {
            Ok(crate::steps::Step::AiText(t)) => t,
            other => panic!("{other:?}"),
        }
    }

    fn lookup(name: &str) -> Value {
        json!({ "type": "function", "function": { "name": name, "description": "Looks a word up.", "parameters": { "type": "object", "properties": { "word": { "type": "string" } } } } })
    }

    /// Goal: a text step asks for what it named, its tools and tool choice
    /// as OpenAI's, and a conversation carrying an assistant's tool calls
    /// and their results reaches the model as it came. Method: a prompt, a
    /// tool turn's messages, through `bound`.
    #[test]
    fn a_text_step_asks_for_what_it_named() {
        let body = text_body(&text(json!({ "prompt": "hi", "max_tokens": 10 }))).unwrap();
        assert_eq!(body, json!({ "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 10 }));
        let messages = json!([
            { "role": "system", "content": "be brief" },
            { "role": "user", "content": "what is a shard?" },
            { "role": "assistant", "content": "", "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "lookup", "arguments": "{\"word\":\"shard\"}" } }] },
            { "role": "tool", "tool_call_id": "call_1", "content": "a small fragment" },
        ]);
        let t = text(json!({ "messages": messages, "tools": [lookup("lookup")], "tool_choice": "auto", "reasoning_effort": "high", "draft": { "channel": "log", "turn": "turn:t_0123456789abcdef" } }));
        let body = text_body(&t).unwrap();
        let b = bound(tier(Tier::Medium), body, true).unwrap();
        assert_eq!(b.input["messages"], messages, "tool calls and their results pass through");
        assert_eq!((b.input["tools"].clone(), b.input["tool_choice"].clone()), (json!([lookup("lookup")]), json!("auto")));
        assert_eq!((b.input["stream"].clone(), b.input["reasoning_effort"].clone()), (json!(true), json!("high")));
        let named = text(json!({ "prompt": "p", "tools": [lookup("lookup")], "tool_choice": { "type": "function", "function": { "name": "lookup" } } }));
        assert_eq!(text_body(&named).unwrap()["tool_choice"], json!({ "type": "function", "function": { "name": "lookup" } }));
        let bare = text_body(&text(json!({ "prompt": "p", "tools": [] }))).unwrap();
        assert!(bare.get("tools").is_none(), "no tools is none sent");
    }

    /// Goal: a text step's tools, tool choice and draft are bounded, and
    /// refused typed past them, before anything is reserved. Method: each
    /// bound at and past its edge.
    #[test]
    fn a_text_steps_tools_and_draft_are_bounded() {
        let check = |args: Value| text_body(&text(args));
        assert_eq!(check(json!({})), Err(Refusal::NoMessages));
        let tools = |n: usize| Value::Array((0..n).map(|i| lookup(&format!("t{i}"))).collect());
        assert!(check(json!({ "prompt": "p", "tools": tools(TOOLS_MAX) })).is_ok());
        assert_eq!(check(json!({ "prompt": "p", "tools": tools(TOOLS_MAX + 1) })), Err(Refusal::Tools));
        let long = json!({ "type": "function", "function": { "name": "big", "description": "d".repeat(TOOLS_MAX_BYTES) } });
        assert_eq!(check(json!({ "prompt": "p", "tools": [long] })), Err(Refusal::Tools), "past the bytes");
        assert_eq!(check(json!({ "prompt": "p", "tools": [lookup("a"), lookup("a")] })), Err(Refusal::Tools), "a name twice");
        assert_eq!(check(json!({ "prompt": "p", "tools": [lookup("look up")] })), Err(Refusal::Tools));
        assert_eq!(check(json!({ "prompt": "p", "tools": [lookup(&"x".repeat(TOOL_NAME_MAX_BYTES + 1))] })), Err(Refusal::Tools));
        assert!(check(json!({ "prompt": "p", "tools": [lookup(&"x".repeat(TOOL_NAME_MAX_BYTES))] })).is_ok());
        assert_eq!(check(json!({ "prompt": "p", "tool_choice": "auto" })), Err(Refusal::ToolChoice), "a choice with no tools");
        let other = json!({ "type": "function", "function": { "name": "other" } });
        assert_eq!(check(json!({ "prompt": "p", "tools": [lookup("lookup")], "tool_choice": other })), Err(Refusal::ToolChoice));
        assert_eq!(check(json!({ "prompt": "p", "draft": { "channel": "events", "turn": "t" } })), Err(Refusal::Draft), "a built-in channel is no app's");
        assert_eq!(check(json!({ "prompt": "p", "draft": { "channel": "Log", "turn": "t" } })), Err(Refusal::Draft));
        assert_eq!(check(json!({ "prompt": "p", "draft": { "channel": "log", "turn": "a turn" } })), Err(Refusal::Draft));
        assert_eq!(check(json!({ "prompt": "p", "draft": { "channel": "log", "turn": "t".repeat(129) } })), Err(Refusal::Draft));
        assert!(Refusal::Tools.message().contains("at most 64 tools"));
    }

    /// Goal: an unstreamed answer reads as one message, its tool calls as
    /// OpenAI's, and never its reasoning. Method: an answer with text, two
    /// calls and reasoning; one with none of them.
    #[test]
    fn an_answer_is_its_message_without_reasoning() {
        let v = json!({ "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": "Let me look.", "reasoning_content": "secret thoughts",
            "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "lookup", "arguments": "{\"word\":\"a\"}" } },
                { "id": "call_2", "type": "function", "function": { "name": "zoom", "arguments": "{}" } },
            ],
        } }] });
        let a = Answer::of_completion(&v);
        assert_eq!((a.content.as_str(), a.finish_reason.as_deref(), a.tool_calls.len()), ("Let me look.", Some("tool_calls"), 2));
        let m = a.message();
        assert_eq!(m["tool_calls"][1], json!({ "id": "call_2", "type": "function", "function": { "name": "zoom", "arguments": "{}" } }));
        assert!(!m.to_string().contains("secret"), "no reasoning: {m}");
        let plain = Answer::of_completion(&json!({ "choices": [{ "finish_reason": "stop", "message": { "role": "assistant", "content": null } }] }));
        assert_eq!(plain.message(), json!({ "role": "assistant", "content": "" }), "no calls: no tool_calls key");
        assert_eq!(Answer::of_completion(&json!({})), Answer::default());
    }

    /// A streamed tool turn as Workers AI streams one: reasoning, text,
    /// then two calls whose arguments come in pieces, and its usage.
    const TOOL_STREAM: &str = concat!(
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":40,\"completion_tokens\":0}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"hidden\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":3}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Let me \"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":2}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"look.\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":2}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"wo\"}}]},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":4}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":null,\"function\":{\"arguments\":\"rd\\\":\\\"a\\\"}\"}}]},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":4}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_2\",\"type\":\"function\",\"function\":{\"name\":\"zoom\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":2}}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":9999999999,\"id\":\"call_x\",\"function\":{\"name\":\"x\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"9\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\n",
        "data: {\"response\":\"\",\"usage\":{\"prompt_tokens\":40,\"completion_tokens\":17,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
        "data: [DONE]\n\n",
    );

    /// Goal: a streamed answer is read into the same message an unstreamed
    /// one is, its reasoning dropped, a call's arguments joined from their
    /// pieces, a delta past `TOOL_CALLS_MAX` dropped, and its last usage
    /// metered. Method: the tool stream, fed in pieces that split lines.
    #[test]
    fn a_stream_is_read_into_its_answer() {
        for piece in [1, 5, 64, TOOL_STREAM.len()] {
            let mut s = Stream::answering();
            for chunk in TOOL_STREAM.as_bytes().chunks(piece) {
                s.push(chunk, None);
            }
            s.finish(None);
            let a = s.answer().cloned().unwrap();
            assert_eq!((a.content.as_str(), a.finish_reason.as_deref()), ("Let me look.", Some("tool_calls")), "pieces of {piece}");
            assert_eq!(a.tool_calls.len(), 2, "the call past the bound is dropped");
            assert_eq!(a.tool_calls[0], ToolCall { id: "call_1".into(), kind: "function", function: CalledFunction { name: "lookup".into(), arguments: "{\"word\":\"a\"}".into() } });
            assert_eq!(a.tool_calls[1].function.name, "zoom");
            assert!(!a.message().to_string().contains("hidden"), "no reasoning");
            assert_eq!(s.usage().and_then(|u| usage_of(CHEAP_MODEL, u)), Some(Usage::Tokens { model: CHEAP_MODEL.into(), input: 40, cached_input: 0, cache_write: 0, output: 17 }));
        }
        let mut metered = Stream::default();
        metered.push(TOOL_STREAM.as_bytes(), None);
        assert_eq!(metered.answer(), None, "a stream only metered keeps no answer");
        let mut text = Stream::answering();
        text.push(S4_STREAM.as_bytes(), None);
        assert_eq!(text.answer().map(|a| (a.content.as_str(), a.finish_reason.as_deref())), Some(("1, 2, 3,", Some("stop"))));
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
}
