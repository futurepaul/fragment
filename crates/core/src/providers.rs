//! A person's own models (docs/optchat.md, "Your own models"; Paul,
//! 2026-10-08, which reverses decision 23's "no BYOK" on this branch).
//! Pure: cell/src/providers/ makes the calls.
//!
//! A person chooses a model for each **role**: `chat` (a conversation's
//! turns), `memory` (summaries and other bulk work) and `hands` (their
//! agents' calls through the model route). Each is Fragment's own (a tier,
//! billed on their ledger) or a model of a provider they connected:
//! `anthropic` (their API key), `openai` (their API key) or `chatgpt`
//! (Sign in with ChatGPT). A job's text step names its role (or its tier
//! says it: `Role::of_step`); an agent's call is `hands`. The choice
//! applies to every call its person pays for that they or their agents
//! make; anyone else's spending in their fragments runs as it names, under
//! its cap (`target`).
//!
//! Every caller speaks OpenAI's chat completions. A call on an own
//! provider is translated to the vendor's API and its answer back
//! (`anthropic`: the Messages API; `responses`: OpenAI's Responses API,
//! which Sign in with ChatGPT requires and an API key takes too), streamed
//! upstream always, and read as OpenAI's stream: chunks, one with the
//! finish reason, one with the usage and no choices, then `[DONE]`
//! (`Chunks`). What it used is counted as OpenAI's usage counts it
//! (`Counted`), so the ledger's reading (`models::usage_of`) reads it too.
//!
//! A content part may carry a hint the vendors' translations read and
//! Workers AI never sees (`strip_hints`): `"cache": "blocks"` on a text
//! part marks a growing text (the mind's view), which Anthropic's
//! translation sends in blocks of 4 lines with a cache mark on the last
//! whole one (UniiChat §3.3). An Anthropic answer's thinking blocks travel
//! on its assistant message as `thinking_blocks` (opaque: their text is
//! empty, Anthropic's display `omitted`), which a tool loop's next call
//! sends back as they came.

pub mod anthropic;
pub mod responses;

use std::collections::BTreeMap;

use fragment_proto::Tier;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::models::{Answer, SSE_LINE_MAX};
use crate::price::Usage;

/// A model's id at a provider, at most (Anthropic's and OpenAI's are a few
/// dozen bytes).
pub const MODEL_ID_MAX_BYTES: usize = 128;
/// The models a provider's list is read for, at most.
pub const MODELS_LISTED_MAX: usize = 200;
/// A content part's hint for the vendors' translations (the module's doc).
pub const CACHE_HINT: &str = "cache";
/// Its one value: a growing text, sent in blocks with a cache mark.
pub const CACHE_BLOCKS: &str = "blocks";
/// The non-standard field an assistant message carries Anthropic's thinking
/// blocks in (LiteLLM's name for the same).
pub const THINKING_BLOCKS: &str = "thinking_blocks";

/// Who a call is for, as its person's choice reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A conversation's turns.
    Chat,
    /// Summaries and other bulk work.
    Memory,
    /// The person's agents, through the model route.
    Hands,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Chat, Role::Memory, Role::Hands];

    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Chat => "chat",
            Role::Memory => "memory",
            Role::Hands => "hands",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// A job step's role: the one it names, `chat` or `memory` (`hands` is
    /// the agents'), else its tier's: `cheap` is memory, any other chat.
    pub fn of_step(named: Option<&str>, tier: Tier) -> Result<Role, Untranslatable> {
        match named {
            Some("chat") => Ok(Role::Chat),
            Some("memory") => Ok(Role::Memory),
            Some(other) => Err(Untranslatable(format!("a text step's role is chat or memory, not {other:?}"))),
            None if tier == Tier::Cheap => Ok(Role::Memory),
            None => Ok(Role::Chat),
        }
    }
}

/// A role's provider: Fragment's own, or one the person connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    /// The deployment's tiers, on Workers AI, billed.
    Fragment,
    /// Anthropic's Messages API, with the person's API key.
    Anthropic,
    /// OpenAI's Responses API, with the person's API key.
    Openai,
    /// OpenAI's Responses API, with the person's Sign in with ChatGPT
    /// tokens (their plan's usage).
    Chatgpt,
}

impl Vendor {
    pub const OWN: [Vendor; 3] = [Vendor::Anthropic, Vendor::Openai, Vendor::Chatgpt];

    pub const fn as_str(self) -> &'static str {
        match self {
            Vendor::Fragment => "fragment",
            Vendor::Anthropic => "anthropic",
            Vendor::Openai => "openai",
            Vendor::Chatgpt => "chatgpt",
        }
    }

    pub fn parse(s: &str) -> Option<Vendor> {
        [Vendor::Fragment, Vendor::Anthropic, Vendor::Openai, Vendor::Chatgpt].into_iter().find(|v| v.as_str() == s)
    }

    /// The catalog row (`fragment_core::catalog`) whose own key it takes,
    /// or whose host it calls: Sign in with ChatGPT calls OpenAI's.
    pub const fn catalog_row(self) -> Option<&'static str> {
        match self {
            Vendor::Fragment => None,
            Vendor::Anthropic => Some("anthropic"),
            Vendor::Openai | Vendor::Chatgpt => Some("openai"),
        }
    }

    /// The host its calls go to.
    pub const fn host(self) -> Option<&'static str> {
        match self {
            Vendor::Fragment => None,
            Vendor::Anthropic => Some(anthropic::HOST),
            Vendor::Openai | Vendor::Chatgpt => Some(responses::HOST),
        }
    }
}

/// Whether `id` is a model's id as a provider names one: 1 to 128 of
/// letters, digits and `.`, `_`, `-`, `:`, `/`, `@`.
pub fn valid_model_id(id: &str) -> bool {
    (1..=MODEL_ID_MAX_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'@'))
}

/// A role's choice: a provider and its model (Fragment's: a tier's name,
/// `cheap` or `medium`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub provider: Vendor,
    pub model: String,
}

impl Choice {
    /// Why it is no choice, if it is none: a Fragment model that is no
    /// tier a role runs on, or a model id out of shape.
    pub fn check(&self) -> Result<(), String> {
        match self.provider {
            Vendor::Fragment => match Tier::parse(&self.model) {
                Some(Tier::Cheap | Tier::Medium) => Ok(()),
                _ => Err(format!("Fragment's models are cheap (GLM-5.3 Flash) and medium (GLM-5.3), not {:?}", self.model)),
            },
            _ if valid_model_id(&self.model) => Ok(()),
            _ => Err(format!("a model's id is 1 to {MODEL_ID_MAX_BYTES} letters, digits and . _ - : / @, not {:?}", self.model)),
        }
    }

    /// Whether it is the default, which is kept as no choice at all:
    /// Fragment's GLM-5.3 Flash (Paul, 2026-10-08), the tier a call names
    /// unless chosen otherwise.
    pub fn is_default(&self) -> bool {
        self.provider == Vendor::Fragment && self.model == Tier::Cheap.as_str()
    }
}

/// What a call runs on, its payer's choice applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Fragment(Tier),
    Own { vendor: Vendor, model: String },
}

/// The model a call naming `named` runs on: its payer's choice for its
/// role when the spender is the payer or an agent of theirs (`own_spend`),
/// else the tier it names. No choice is the tier it names.
pub fn target(choice: Option<&Choice>, named: Tier, own_spend: bool) -> Target {
    match choice.filter(|_| own_spend) {
        None => Target::Fragment(named),
        Some(c) if c.provider == Vendor::Fragment => Target::Fragment(Tier::parse(&c.model).unwrap_or(named)),
        Some(c) => Target::Own { vendor: c.provider, model: c.model.clone() },
    }
}

/// Why a call cannot be sent to a vendor (a part or a role it has no way
/// to say): the step's or the route's refusal, nothing sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untranslatable(pub String);

impl Untranslatable {
    pub fn message(&self) -> String {
        self.0.clone()
    }
}

/// `body`'s hints taken out: each content part's `cache`, and each
/// message's `thinking_blocks`, for a model that reads neither (Workers
/// AI's). Bounded by the body's messages and their parts.
pub fn strip_hints(body: &mut Value) {
    for m in body.get_mut("messages").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(m) = m.as_object_mut() {
            m.remove(THINKING_BLOCKS);
            for part in m.get_mut("content").and_then(Value::as_array_mut).into_iter().flatten() {
                if let Some(p) = part.as_object_mut() {
                    p.remove(CACHE_HINT);
                }
            }
        }
    }
}

/// The text of a message's content: a string, or its text parts joined.
pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Server-sent events read as they come: each complete line's `data:`
/// payload, once. A line past `SSE_LINE_MAX` is out of the stream's
/// contract: it is dropped, and the stream marked (`overlong`).
#[derive(Debug, Default)]
pub struct Lines {
    line: Vec<u8>,
    /// Dropping the rest of a line past the limit, up to its newline.
    skipping: bool,
    overlong: bool,
}

impl Lines {
    /// The next bytes: `each` gets every payload they complete.
    pub fn push(&mut self, chunk: &[u8], mut each: impl FnMut(&str)) {
        // bounded by the chunk: each pass takes one line out of it
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            let ends = piece.last() == Some(&b'\n');
            if self.skipping {
                self.skipping = !ends;
                continue;
            }
            if self.line.len() + piece.len() > SSE_LINE_MAX {
                self.overlong = true;
                self.line.clear();
                self.skipping = !ends;
                continue;
            }
            self.line.extend_from_slice(piece);
            if ends {
                let line = std::mem::take(&mut self.line);
                if let Some(data) = data_of(&line) {
                    each(data);
                }
            }
        }
    }

    /// The end: a last line without its newline.
    pub fn finish(&mut self, mut each: impl FnMut(&str)) {
        let line = std::mem::take(&mut self.line);
        if let Some(data) = data_of(&line).filter(|_| !self.skipping) {
            each(data);
        }
    }

    pub fn overlong(&self) -> bool {
        self.overlong
    }
}

/// A line's `data:` payload, trimmed.
fn data_of(line: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(line).ok()?;
    let data = text.strip_prefix("data:")?.trim();
    (!data.is_empty()).then_some(data)
}

/// What a call used, as the vendor counted it: input that was neither read
/// from nor written to its cache, cached input read, cache writes, and
/// output (reasoning included).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counted {
    pub input: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub output: u64,
}

impl Counted {
    /// As OpenAI's usage counts it: `prompt_tokens` all of the input, the
    /// cached and written among it in `prompt_tokens_details`.
    pub fn openai(&self) -> Value {
        let prompt = self.input + self.cached + self.cache_write;
        json!({
            "prompt_tokens": prompt, "completion_tokens": self.output, "total_tokens": prompt + self.output,
            "prompt_tokens_details": { "cached_tokens": self.cached, "cache_write_tokens": self.cache_write },
        })
    }

    pub fn usage(&self, model: &str) -> Usage {
        Usage::Tokens { model: model.to_string(), input: self.input, cached_input: self.cached, cache_write: self.cache_write, output: self.output }
    }
}

/// An OpenAI stream's lines, as a vendor's translation writes them.
#[derive(Debug, Default)]
pub struct Chunks {
    pub id: String,
    pub model: String,
}

impl Chunks {
    pub fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        let c = json!({ "id": self.id, "object": "chat.completion.chunk", "created": 0, "model": self.model, "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] });
        format!("data: {c}\n\n")
    }

    /// The last chunk: no choices, the whole call's usage (OpenAI's own
    /// shape for `stream_options.include_usage`).
    pub fn usage(&self, counted: &Counted) -> String {
        let c = json!({ "id": self.id, "object": "chat.completion.chunk", "created": 0, "model": self.model, "choices": [], "usage": counted.openai() });
        format!("data: {c}\n\n")
    }

    /// A vendor's failure mid-stream, as OpenAI streams one.
    pub fn error(&self, failure: &Failure) -> String {
        let e = json!({ "error": { "message": failure.message, "type": failure.kind, "code": failure.kind } });
        format!("data: {e}\n\n")
    }

    pub const DONE: &'static str = "data: [DONE]\n\n";
}

/// A vendor's failure, said in its stream (or its refusal's body): its kind
/// (the vendor's code) and message, and whether it may pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: String,
    pub message: String,
    pub passing: bool,
}

/// One of a vendor's translations of its stream (`anthropic::Stream`,
/// `responses::Stream`): bytes in, OpenAI's lines out.
pub trait Translate {
    /// The vendor's next bytes; OpenAI's lines they complete go to `out`.
    fn push(&mut self, chunk: &[u8], out: &mut Vec<u8>);
    /// The vendor's stream ended.
    fn finish(&mut self, out: &mut Vec<u8>);
    /// The answer and its usage are whole (`[DONE]` was written).
    fn done(&self) -> bool;
    /// What it used, once the vendor said.
    fn counted(&self) -> Option<Counted>;
    /// The vendor's failure, if it said one.
    fn failure(&self) -> Option<&Failure>;
    /// Anthropic's thinking blocks, for the assistant message.
    fn thinking_blocks(&self) -> &[Value] {
        &[]
    }
}

/// An answer's assistant message (`Answer::message`), with its thinking
/// blocks when it has any.
pub fn message_of(answer: &Answer, thinking: &[Value]) -> Value {
    let mut m = answer.message();
    if !thinking.is_empty() {
        m[THINKING_BLOCKS] = json!(thinking);
    }
    m
}

/// An unstreamed chat completion, from an answer folded from the stream.
pub fn completion(chunks: &Chunks, answer: &Answer, thinking: &[Value], counted: Option<&Counted>) -> Value {
    let mut c = json!({
        "id": chunks.id, "object": "chat.completion", "created": 0, "model": chunks.model,
        "choices": [{ "index": 0, "finish_reason": answer.finish_reason, "message": message_of(answer, thinking) }],
    });
    if let Some(u) = counted {
        c["usage"] = u.openai();
    }
    c
}

/// A model a provider lists, for the picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub id: String,
    pub name: String,
}

/// Anthropic's list (`GET /v1/models`: `{data: [{id, display_name}]}`,
/// newest first), as given.
pub fn anthropic_models(v: &Value) -> Vec<Listed> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().filter(|id| valid_model_id(id))?;
            Some(Listed { id: id.to_string(), name: m["display_name"].as_str().unwrap_or(id).chars().take(200).collect() })
        })
        .take(MODELS_LISTED_MAX)
        .collect()
}

/// Words in an OpenAI model's id that make it no chat model (the API's list
/// holds every model the key reaches: embeddings, speech, images…).
const NOT_CHAT: [&str; 10] = ["embedding", "audio", "realtime", "transcribe", "tts", "image", "dall-e", "whisper", "moderation", "search"];

/// OpenAI's list, either shape: Sign in with ChatGPT's (`{models: [{slug,
/// display_name, visibility}]}`, those to `list`, in its order) or the
/// API's (`{data: [{id}]}`, its chat models: `gpt-`, `o<digit>`, `chatgpt-`,
/// none of `NOT_CHAT`, newest first by `created`).
pub fn openai_models(v: &Value) -> Vec<Listed> {
    if let Some(models) = v["models"].as_array() {
        return models
            .iter()
            .filter(|m| m["visibility"] == "list")
            .filter_map(|m| {
                let id = m["slug"].as_str().filter(|id| valid_model_id(id))?;
                Some(Listed { id: id.to_string(), name: m["display_name"].as_str().unwrap_or(id).chars().take(200).collect() })
            })
            .take(MODELS_LISTED_MAX)
            .collect();
    }
    let chat = |id: &str| {
        let starts = id.starts_with("gpt-") || id.starts_with("chatgpt-") || (id.starts_with('o') && id[1..].starts_with(|c: char| c.is_ascii_digit()));
        starts && !NOT_CHAT.iter().any(|w| id.contains(w))
    };
    let mut found: Vec<(i64, Listed)> = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().filter(|id| valid_model_id(id) && chat(id))?;
            Some((m["created"].as_i64().unwrap_or(0), Listed { id: id.to_string(), name: id.to_string() }))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    found.into_iter().map(|(_, l)| l).take(MODELS_LISTED_MAX).collect()
}

/// What the picker suggests for each role of a provider's list: for
/// Anthropic the newest Sonnet for chat and hands and the newest Haiku for
/// memory; for OpenAI the first listed for chat and hands, and the first
/// `mini` for memory; each the first listed when its kind is not there.
pub fn suggested(vendor: Vendor, models: &[Listed]) -> BTreeMap<Role, String> {
    let first_with = |word: &str| models.iter().find(|m| m.id.contains(word)).or(models.first()).map(|m| m.id.clone());
    let (chat, memory) = match vendor {
        Vendor::Anthropic => (first_with("sonnet"), first_with("haiku")),
        _ => (models.first().map(|m| m.id.clone()), first_with("mini")),
    };
    let mut out = BTreeMap::new();
    for (role, id) in [(Role::Chat, chat.clone()), (Role::Memory, memory), (Role::Hands, chat)] {
        if let Some(id) = id {
            out.insert(role, id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a step's role is the one it names, else its tier's; an agent's
    /// is never a step's. Method: each name and tier.
    #[test]
    fn a_steps_role() {
        assert_eq!(Role::of_step(Some("chat"), Tier::Cheap), Ok(Role::Chat), "named: whatever the tier");
        assert_eq!(Role::of_step(Some("memory"), Tier::Medium), Ok(Role::Memory));
        assert_eq!(Role::of_step(None, Tier::Cheap), Ok(Role::Memory));
        assert_eq!(Role::of_step(None, Tier::Medium), Ok(Role::Chat));
        assert!(Role::of_step(Some("hands"), Tier::Cheap).is_err(), "hands is the agents'");
        assert!(Role::of_step(Some("Chat"), Tier::Cheap).is_err());
        assert_eq!(Role::ALL.map(Role::as_str), ["chat", "memory", "hands"]);
        assert_eq!(Role::parse("hands"), Some(Role::Hands));
    }

    /// Goal: a choice applies to its person's own spending alone, Fragment's
    /// default is no choice, and a choice out of shape is refused. Method:
    /// each kind of choice, spent by the owner and by someone else.
    #[test]
    fn a_choice_and_its_target() {
        let claude = Choice { provider: Vendor::Anthropic, model: "claude-sonnet-5-5".into() };
        assert_eq!(target(Some(&claude), Tier::Cheap, true), Target::Own { vendor: Vendor::Anthropic, model: "claude-sonnet-5-5".into() });
        assert_eq!(target(Some(&claude), Tier::Cheap, false), Target::Fragment(Tier::Cheap), "someone else's spending runs as it names");
        assert_eq!(target(None, Tier::Medium, true), Target::Fragment(Tier::Medium));
        let glm = Choice { provider: Vendor::Fragment, model: "medium".into() };
        assert_eq!(target(Some(&glm), Tier::Cheap, true), Target::Fragment(Tier::Medium), "a Fragment model chosen is the role's");
        assert!(Choice { provider: Vendor::Fragment, model: "cheap".into() }.is_default());
        assert!(!glm.is_default() && !claude.is_default());
        assert!(claude.check().is_ok() && glm.check().is_ok());
        assert!(Choice { provider: Vendor::Fragment, model: "high".into() }.check().is_err(), "high is off");
        assert!(Choice { provider: Vendor::Fragment, model: "@cf/zai-org/glm-5.3".into() }.check().is_err(), "a tier, not a model id");
        assert!(Choice { provider: Vendor::Openai, model: "gpt 5".into() }.check().is_err());
        assert!(Choice { provider: Vendor::Openai, model: "x".repeat(129) }.check().is_err());
        assert!(serde_json::from_value::<Choice>(json!({ "provider": "anthropic", "model": "m", "x": 1 })).is_err());
        assert_eq!(Vendor::parse("chatgpt"), Some(Vendor::Chatgpt));
        assert_eq!((Vendor::Chatgpt.catalog_row(), Vendor::Chatgpt.host()), (Some("openai"), Some("api.openai.com")));
    }

    /// Goal: Workers AI never sees a hint. Method: a turn's messages with a
    /// view part, its thinking blocks, and other parts.
    #[test]
    fn hints_are_stripped() {
        let mut body = json!({ "messages": [
            { "role": "system", "content": "s" },
            { "role": "user", "content": [{ "type": "text", "text": "<chat>…</chat>", "cache": "blocks" }, { "type": "text", "text": "hi" }] },
            { "role": "assistant", "content": "", "thinking_blocks": [{ "type": "thinking", "thinking": "", "signature": "x" }] },
        ] });
        let tier = crate::models::model_of(Tier::Cheap).unwrap();
        let sent = crate::models::bound(tier, body.clone(), true).unwrap();
        assert_eq!(sent.input["messages"][1]["content"][0], json!({ "type": "text", "text": "<chat>…</chat>" }), "Workers AI's call is bound without them");
        assert_eq!(crate::models::bound_hinted(tier, body.clone(), true).unwrap().input["messages"][1]["content"][0]["cache"], "blocks", "an own provider's keeps them");
        strip_hints(&mut body);
        assert_eq!(body["messages"][1]["content"][0], json!({ "type": "text", "text": "<chat>…</chat>" }));
        assert_eq!(body["messages"][2], json!({ "role": "assistant", "content": "" }));
        assert_eq!(body["messages"][0], json!({ "role": "system", "content": "s" }));
    }

    /// Goal: a stream's lines are read however its bytes are cut, and one
    /// past the limit is dropped and said. Method: pieces of every size, and
    /// a long line between two short ones.
    #[test]
    fn lines_are_read_whole() {
        let text = "event: a\ndata: {\"x\":1}\n\ndata: [DONE]\n";
        for n in [1, 2, 5, text.len()] {
            let mut l = Lines::default();
            let mut got = vec![];
            for piece in text.as_bytes().chunks(n) {
                l.push(piece, |d| got.push(d.to_string()));
            }
            l.finish(|d| got.push(d.to_string()));
            assert_eq!(got, ["{\"x\":1}", "[DONE]"], "pieces of {n}");
        }
        let mut l = Lines::default();
        let mut got = vec![];
        let long = format!("data: {}\n", "y".repeat(SSE_LINE_MAX + 10));
        for piece in [b"data: 1\n".as_slice(), long.as_bytes(), b"data: 2".as_slice()] {
            for c in piece.chunks(4096) {
                l.push(c, |d| got.push(d.to_string()));
            }
        }
        l.finish(|d| got.push(d.to_string()));
        assert_eq!(got, ["1", "2"]);
        assert!(l.overlong());
    }

    /// Goal: what a call used reads as the ledger reads OpenAI's usage.
    /// Method: a count with a cache read and a write, through `usage_of`.
    #[test]
    fn counted_reads_as_openais_usage() {
        let c = Counted { input: 10, cached: 900, cache_write: 90, output: 40 };
        let v = c.openai();
        assert_eq!((v["prompt_tokens"].clone(), v["prompt_tokens_details"]["cached_tokens"].clone()), (json!(1000), json!(900)));
        assert_eq!(crate::models::usage_of("claude-x", &v), Some(c.usage("claude-x")));
    }

    /// Goal: the picker lists chat models and suggests the gist's. Method:
    /// Anthropic's list, OpenAI's two shapes.
    #[test]
    fn models_are_listed_and_suggested() {
        let a = anthropic_models(&json!({ "data": [
            { "type": "model", "id": "claude-opus-5-5", "display_name": "Claude Opus 5.5" },
            { "type": "model", "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5" },
            { "type": "model", "id": "claude-haiku-5-5", "display_name": "Claude Haiku 5.5" },
            { "type": "model", "id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5" },
            { "type": "model", "id": "bad id" },
        ], "has_more": false }));
        assert_eq!(a.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-haiku-4-5"]);
        let s = suggested(Vendor::Anthropic, &a);
        assert_eq!((s[&Role::Chat].as_str(), s[&Role::Memory].as_str(), s[&Role::Hands].as_str()), ("claude-sonnet-5-5", "claude-haiku-5-5", "claude-sonnet-5-5"));
        let chatgpt = openai_models(&json!({ "models": [
            { "slug": "gpt-6.1-sol", "display_name": "GPT-6.1 Sol", "visibility": "list" },
            { "slug": "gpt-6.1-sol-mini", "display_name": "GPT-6.1 Sol mini", "visibility": "list" },
            { "slug": "internal", "display_name": "x", "visibility": "hide" },
        ] }));
        assert_eq!(chatgpt.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-6.1-sol", "gpt-6.1-sol-mini"]);
        assert_eq!(suggested(Vendor::Chatgpt, &chatgpt)[&Role::Memory], "gpt-6.1-sol-mini");
        let api = openai_models(&json!({ "object": "list", "data": [
            { "id": "text-embedding-3-large", "created": 9 }, { "id": "gpt-5", "created": 5 }, { "id": "o4-mini", "created": 7 },
            { "id": "gpt-realtime", "created": 8 }, { "id": "omni-moderation-latest", "created": 3 }, { "id": "dall-e-3", "created": 1 },
        ] }));
        assert_eq!(api.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["o4-mini", "gpt-5"], "chat models, newest first");
        assert!(suggested(Vendor::Openai, &[]).is_empty());
    }
}
