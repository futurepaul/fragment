//! Workers AI as the platform's model route calls it in dev and the e2e
//! (cell/src/models.rs, `FRAGMENT_AI_URL`): `POST /run/<model>` with the
//! input `env.AI.run` takes (an OpenAI-shaped chat completion, less its
//! model), answered as the binding answers with `returnRawResponse`. A
//! lower rung, at the vendor boundary: what reaches the real gateway is the
//! hosted lane's to prove.
//!
//! Its answers keep the shapes spike S4 recorded (its `logs/`): unstreamed,
//! a `chat.completion` with `usage` (`prompt_tokens_details.cached_tokens`,
//! `neurons`); streamed, server-sent events whose every chunk carries a
//! per-chunk delta of usage, then a chunk with no choices, then Workers
//! AI's own last line, `{"response": "", "usage": {…}}`, with the whole
//! call's, and `[DONE]`. Each answer carries a `cf-aig-log-id`. A streamed
//! answer keeps to the request's `max_tokens` (`CHARS_PER_TOKEN` characters
//! each): one longer is cut there and ends `length`.
//!
//! The image model (`IMAGE_MODEL`, FLUX.1 [schnell]) answers as its
//! catalog's output schema says, `{"image": "<base64 JPEG>"}`, drawn from
//! the prompt (`image_bytes`), with no usage: Workers AI prices an image by
//! its tiles and steps, which the cell counts itself.
//!
//! The decision models (the Clef family: `fragment_core::decide::Model`)
//! take System One's input as their catalog schemas say: `model` their own
//! selector, `state`, `questions`, and media as base64 `data:` URLs,
//! `images` for all three and `audio` and `videos` for clef-omni alone
//! (another's is a 400, as its schema refuses an unknown field). Each
//! answers System One's answers and its input usage, counted as
//! `decision_tokens` does: the text's bytes at `CHARS_PER_TOKEN`, an
//! image `IMAGE_TOKENS`, audio and video a token a KiB. Past the model's
//! context window it is a 400, as Clef's request "fails". Its default
//! answer is mechanical: each choice's first option, each score's first
//! level, 0.5 for a predicate; `decide_next` scripts raw answers (a
//! malformed one too), never an accuracy claim.
//!
//! The transcription model (`TRANSCRIBE_MODEL`, Whisper) answers as its
//! catalog's output schema says, `{text, word_count, transcription_info:
//! {language, duration, …}, segments, vtt}`, from the audio's own bytes: a
//! WAV `spoken_wav` makes carries its words in a chunk of its own (`said`),
//! which is the text, and its length is its samples'; any other audio is
//! heard as `NO_WORDS`, its length its bytes at 16 kbps. Workers AI prices
//! it by that length, which the cell meters itself. The real answer also
//! carries a `usage` its schema does not name (the hosted lane, 2026-10-08),
//! whose contents are not recorded yet, so the fake sends none: the cell
//! reads none of it.
//!
//! A chat call may carry images (`image_url` parts, OpenAI's shape) to a
//! model that reads them (`TAKES_IMAGES`, as Workers AI's catalog marks
//! them: the route's vision model); one carrying an image to another, or a
//! part that is no image (a `data:` URL that is not base64 of a PNG, a JPEG,
//! a GIF or a WebP), answers 400. An image is described from its bytes
//! (`describe_image`: its kind and size), so a test sees which image a
//! model was shown.
//!
//! A call answers the text or tool calls a test set for it (`say_next`,
//! `reply_next`), else an echo of its last message, or, for a real agent
//! runtime (`transcripts`), `transcript_reply`: a pure function of the
//! transcript (lesson 13), which an agent's own auxiliary calls (titles, its
//! approval guardian) cannot put out of order; its answer after a tool's
//! result waits `FOLLOW_UP_MS`. Levers: the calls made (with the gateway
//! metadata the cell would send), failures queued for the next calls, a
//! delay, the usage the next answers report (`set_usage`), a stream cut
//! before its usage (`break_next`), a stream's pace (`pace_next`: its events
//! written apart in time, after reasoning, and its connection held open past
//! its `[DONE]`, as the gateway held one ~150 s on 2026-10-07; `hung_up`
//! counts the readers that dropped it first), and models that are down
//! (`down`): each call to one answers as Workers AI
//! answered every call to GLM-5.3 Flash for five minutes on 2026-10-09, a
//! 502 "could not route request to AI model" (its internal code 4002),
//! until the test brings it up again. A hang is a delay past its caller's
//! patience.
//!
//! **Its OpenAI-compatible face**, Finite Private as the route calls it on
//! Finite's own deployment (decision 61): `POST /v1/chat/completions`, the
//! body's `model` the model, answered as finite-mono's
//! `finite-private-limiter` answers in front of SGLang (read 2026-10-14):
//! - a request past 2 MiB is 413 (its body limit); one without the key the
//!   test let in (`accept_key`) as `Authorization: Bearer …` is 401, as is
//!   one with another key; a model it does not serve is 400
//!   (`unsupported_model`); each before any answer;
//! - unstreamed, a `chat.completion` whose `usage` names no cached tokens
//!   (SGLang reports none); streamed, OpenAI's chunks, each with `usage:
//!   null`, then one with no choices and the whole call's usage (the
//!   limiter always asks for it), and `[DONE]`; no `cf-aig-log-id`;
//! - the same levers as the Workers AI face, one queue for both: a failure
//!   queued is the limiter's error shape (a 429 its spent window's,
//!   `usage_limit`), and a model that is down answers its 502
//!   `upstream_unavailable`. It takes no images (they are not qualified
//!   there: the route sends them to Workers AI).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// The image model's catalog id (fragment_core::media::IMAGE_MODEL).
pub use fragment_core::media::IMAGE_MODEL;
/// The transcription model's (fragment_core::transcribe::TRANSCRIBE_MODEL).
pub use fragment_core::transcribe::TRANSCRIBE_MODEL;
/// The decision models (the Clef family): a typed-decision vendor
/// boundary, never proof of a real model's accuracy.
pub use fragment_core::decide::Model as DecisionModel;
/// What a decision's image counts as, in input tokens: a 512×512 image's
/// 32-pixel patches and its 3 markers (Clef's tokenization).
pub const IMAGE_TOKENS: u64 = 16 * 16 + 3;
/// What Whisper hears in audio that carries no words of `spoken_wav`'s.
pub const NO_WORDS: &str = "(no words)";

/// The chat models the platform calls that read images: those Workers AI's
/// catalog marks "Vision: Yes" (GLM-5.3 Flash, the cheap tier's and the
/// default vision model; DeepSeek V4 Flash, the fallback, and GLM-5.3, the
/// medium tier's, have none). Finite Private's is not among them: its
/// images are not qualified (decision 61).
pub const TAKES_IMAGES: [&str; 1] = [fragment_core::models::VISION_MODEL_DEFAULT];
/// The models the OpenAI-compatible face serves: Finite Private's GLM-5.3
/// Flash, by the id its limiter takes (its aliases are not the route's).
pub const OPENAI_MODELS: [&str; 1] = [fragment_core::models::FINITE_PRIVATE_FLASH];
/// The largest request the OpenAI-compatible face takes: Finite Private's
/// limiter's (axum's default body limit, 2 MiB).
pub const OPENAI_BODY_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Its path.
pub const OPENAI_PATH: &str = "/v1/chat/completions";

/// Which of the fake's faces a call came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Workers AI's (`/run/<model>`), as the AI binding answers.
    WorkersAi,
    /// An OpenAI-compatible API's (`/v1/chat/completions`), as Finite
    /// Private's limiter answers.
    OpenAi,
}
/// What a model is told of an image it was shown: `describe_image`'s
/// answer starts so.
pub const SEEN: &str = "I see an image";

/// A model's reply: an echo's, or `transcript_reply`'s.
#[derive(Clone, Debug)]
pub enum Reply {
    Text(String),
    /// Tool calls: (name, arguments).
    Tools(Vec<(String, Value)>),
    /// Text and tool calls in one answer, as a model narrates its call
    /// ("Let me check that." beside a `terminal` call): streamed, the
    /// text's deltas, then the calls'.
    Narrated(String, Vec<(String, Value)>),
}

/// The text a `narrate:` answer says beside its call (`transcript_reply`).
pub const NARRATION: &str = "Let me check that.";

/// A transcript's answer after a tool's result waits this long, as a
/// model's next call does: a runtime's tool progress is out before its
/// answer (Hermes sends its progress at most every 0.3 s, and drops what
/// it has not sent when the turn ends).
pub const FOLLOW_UP_MS: u64 = 1_000;

/// A tool's result came since the transcript's last user message.
fn follows_a_tool(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|m| m.iter().rev().take_while(|m| m["role"] != "user").any(|m| m["role"] == "tool"))
}

/// The characters one token of an answer stands for.
pub const CHARS_PER_TOKEN: usize = 4;
/// A tool call's arguments stream in pieces of at most this many characters.
const ARGS_PIECE_CHARS: usize = 1024;

/// One call as the fake took it.
#[derive(Clone, Debug)]
pub struct AiCall {
    /// The face it came to.
    pub face: Face,
    /// The catalog model its path named (Workers AI's face), or its body's
    /// `model` (the OpenAI-compatible face's).
    pub model: String,
    /// The input, as sent.
    pub body: Value,
    /// The gateway metadata the cell sent beside it (`x-fragment-ai-metadata`).
    pub metadata: Value,
    /// GLM's prefix-cache key (`x-session-affinity`).
    pub affinity: Option<String>,
    /// An `authorization` header, were one ever sent (a guest's key must
    /// never reach the vendor).
    pub authorization: Option<String>,
}

/// What an answer reports it used, when a test says (`set_usage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Used {
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
}

/// How a streamed answer is written (`pace_next`): its events `every_ms`
/// apart (its text in `TEXT_PIECES_PACED` pieces), after `thinking` deltas
/// of reasoning, and its connection held open `hold_ms` past its `[DONE]`
/// before the stream ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pace {
    pub every_ms: u64,
    pub thinking: usize,
    pub hold_ms: u64,
}

/// A paced answer's text streams in this many pieces, so a draft grows.
pub const TEXT_PIECES_PACED: usize = 6;
/// What a paced answer reasons, each of its `thinking` deltas.
pub const THOUGHT: &str = "considering";

#[derive(Default)]
struct State {
    calls: Vec<AiCall>,
    /// How the next calls answer, in order: a status, or `None` as they would.
    failures: VecDeque<Option<u16>>,
    /// The replies the next calls answer, in order; then as they would.
    said: VecDeque<Reply>,
    /// How the next streamed answers are written, in order; then at once.
    paces: VecDeque<Pace>,
    /// Paced answers whose reader dropped the connection while it was held
    /// past their `[DONE]`.
    hung_up: Arc<AtomicU64>,
    /// The usage the next answers report, in order; then the default.
    usage: VecDeque<Used>,
    /// Raw System One answers queued by tests, including malformed ones.
    decisions: VecDeque<Value>,
    /// The next streamed answers end before their usage, in order.
    breaks: VecDeque<bool>,
    delays: VecDeque<u64>,
    sleep_ms: u64,
    tool_calls: u64,
    answers: u64,
    /// Calls answer `transcript_reply`, in pieces.
    transcripts: bool,
    /// Models every call to which answers `unroutable` (or, on the
    /// OpenAI-compatible face, the limiter's `upstream_unavailable`).
    down: Vec<String>,
    /// The key the OpenAI-compatible face lets in (`accept_key`).
    key: Option<String>,
}

/// Workers AI's answer when it cannot reach a model (as the binding passed
/// it through to an agent on 2026-10-09; `requestId` its own each time).
fn unroutable(n: usize) -> Response {
    let id = format!("00000000-0000-4000-8000-{n:012}");
    Response::json(
        502,
        &json!({ "name": "AiError", "internalCode": 4002, "httpCode": 502, "message": format!("AiError: AiError: could not route request to AI model ({id})"), "description": "could not route request to AI model", "requestId": id }),
    )
}

/// The images a chat's messages carry (`image_url` parts' URLs), or why a
/// part is none.
fn images_of(body: &Value) -> Result<Vec<String>, String> {
    let mut urls = vec![];
    for m in body["messages"].as_array().into_iter().flatten() {
        for part in m["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "image_url") {
            let url = part["image_url"]["url"].as_str().ok_or("an image_url part has image_url.url, a string")?;
            image_bytes_of(url)?;
            urls.push(url.to_string());
        }
    }
    Ok(urls)
}

/// Why a chat call's images are refused: a part that is no image, or an
/// image sent to a model that reads none (`TAKES_IMAGES`).
fn image_refusal(model: &str, body: &Value) -> Option<String> {
    match images_of(body) {
        Err(why) => Some(why),
        Ok(images) if !images.is_empty() && !TAKES_IMAGES.contains(&model) => Some(format!("{model} takes no image input: send images to a model that reads them")),
        Ok(_) => None,
    }
}

/// An image's bytes from its `data:` URL, if it is base64 of an image.
fn image_bytes_of(url: &str) -> Result<Vec<u8>, String> {
    let Some((head, data)) = url.strip_prefix("data:").and_then(|r| r.split_once(',')) else { return Err("an image_url is a data: URL (a fetched URL is the hosted lane's)".into()) };
    if !head.starts_with("image/") || !head.ends_with(";base64") {
        return Err(format!("an image_url's data: URL is base64 of an image, not {head:?}"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("an image_url's data is not base64: {e}"))?;
    kind_of(&bytes).ok_or("an image_url's data is no PNG, JPEG, GIF or WebP")?;
    Ok(bytes)
}

/// An image's kind, by its first bytes, as a model detects it.
fn kind_of(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("PNG")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("JPEG")
    } else if b.starts_with(b"GIF8") {
        Some("GIF")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("WebP")
    } else {
        None
    }
}

/// What a model says of an image (a `data:` URL): `I see an image, a
/// 1280x800 PNG` (its size, when its header tells it), or of so many bytes.
pub fn describe_image(url: &str) -> String {
    let Ok(bytes) = image_bytes_of(url) else { return format!("{SEEN} I cannot read") };
    let kind = kind_of(&bytes).unwrap_or("image");
    let size = match kind {
        "PNG" if bytes.len() >= 24 && &bytes[12..16] == b"IHDR" => {
            let be = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            Some((be(16), be(20)))
        }
        "JPEG" => fragment_core::media::jpeg_size(&bytes).map(|(w, h)| (u32::from(w), u32::from(h))),
        _ => None,
    };
    match size {
        Some((w, h)) => format!("{SEEN}, a {w}x{h} {kind}"),
        None => format!("{SEEN}, a {kind} of {} bytes", bytes.len()),
    }
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// The answer for a transcript, as an agent runtime's lane needs it:
///
/// - after a tool's result (since the last user message):
///   `scripted: the tool ran: <its first line>`;
/// - a user message whose newest line is `run: <command>`, when the call
///   offers a `terminal` tool: that tool, called with the rest of the line;
/// - `clarify: <question> | <choice> | …`, when the call offers Hermes'
///   `clarify`: that question asked with those choices (a card);
/// - one saying `look at your screen`: a `computer_use` capture of the
///   screen, called directly or through Hermes' `tool_call` bridge (Hermes
///   defers the tool behind `tool_search`), and once its result is in, an
///   answer quoting what the vision model said of the screenshot
///   (`scripted: the screen: I see an image, …`);
///   The newest line is the last a person said (Hermes puts `[name] ` before
///   each, and merges two user messages a restart left side by side into
///   one), else the first (a runtime appends its own notes after it, which
///   may quote an earlier command);
/// - `narrate: <command>`, as `run:`: the same call, with `NARRATION` as
///   its text in the same answer (`Reply::Narrated`);
/// - a user message with an image: what is seen of it (`describe_image`):
///   `scripted: I see an image, a 1456x816 PNG`;
/// - otherwise `scripted: <the message's first line> [<user messages in
///   the transcript>]`, so a restored conversation shows in its count.
pub fn transcript_reply(body: &Value) -> Reply {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let users: Vec<&Value> = messages.iter().filter(|m| m["role"] == "user").collect();
    let last = users.last().map(|m| &m["content"]).cloned().unwrap_or(Value::Null);
    let said = text_of(&last);
    let result = messages.iter().rev().take_while(|m| m["role"] != "user").find(|m| m["role"] == "tool").map(|m| text_of(&m["content"]));
    if let Some(result) = result {
        // a screenshot's description, as the vision model gave it (in the
        // tool's JSON, maybe inside the bridge's): up to its first quote
        if let Some(at) = result.find(SEEN) {
            let seen: String = result[at..].chars().take_while(|c| *c != '"' && *c != '\\').take(200).collect();
            return Reply::Text(format!("scripted: the screen: {seen}"));
        }
        let first = result.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(200).collect::<String>();
        return Reply::Text(format!("scripted: the tool ran: {first}"));
    }
    let offered = |name: &str| body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == name));
    let has_terminal = offered("terminal");
    // a person's line: `[name] text`, the name one word
    let named = |l: &str| l.strip_prefix('[').and_then(|r| r.split_once("] ")).filter(|(n, _)| !n.is_empty() && !n.contains(' ')).map(|(_, t)| t.to_string());
    let lines: Vec<&str> = said.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let newest = lines.iter().rev().find_map(|l| named(l)).or_else(|| lines.first().map(|l| l.to_string())).unwrap_or_default();
    if let (Some(command), true) = (newest.strip_prefix("run: ").map(str::trim), has_terminal) {
        return Reply::Tools(vec![("terminal".into(), json!({ "command": command }))]);
    }
    if let (Some(command), true) = (newest.strip_prefix("narrate: ").map(str::trim), has_terminal) {
        return Reply::Narrated(NARRATION.into(), vec![("terminal".into(), json!({ "command": command }))]);
    }
    if let (Some(asked), true) = (newest.strip_prefix("clarify: "), offered("clarify")) {
        let mut parts = asked.split('|').map(str::trim);
        let question = parts.next().unwrap_or_default();
        return Reply::Tools(vec![("clarify".into(), json!({ "questions": [{ "question": question, "choices": parts.collect::<Vec<_>>() }] }))]);
    }
    if newest.contains("look at your screen") {
        let listed = body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "tool_search" && t["function"]["description"].as_str().is_some_and(|d| d.contains("computer_use"))));
        let capture = json!({ "action": "capture", "mode": "vision", "app": "screen" });
        if offered("computer_use") {
            return Reply::Tools(vec![("computer_use".into(), capture)]);
        }
        if listed && offered("tool_call") {
            return Reply::Tools(vec![("tool_call".into(), json!({ "calls": [{ "name": "computer_use", "arguments": capture }] }))]);
        }
        return Reply::Text("scripted: no computer_use among my tools".into());
    }
    let shown = last.as_array().and_then(|parts| parts.iter().find(|p| p["type"] == "image_url")).and_then(|p| p["image_url"]["url"].as_str());
    if let Some(url) = shown {
        return Reply::Text(format!("scripted: {}", describe_image(url)));
    }
    Reply::Text(format!("scripted: {newest} [{}]", users.len()))
}

/// The usage of an answer: a test's, or the request's bytes in and the
/// reply's characters out, as a model would count them.
fn usage_of(scripted: Option<Used>, body: &Value, written: usize) -> Used {
    scripted.unwrap_or(Used { prompt: (body.to_string().len() / CHARS_PER_TOKEN).max(1) as u64, cached: 0, completion: (written / CHARS_PER_TOKEN).max(1) as u64 })
}

fn usage_json(u: Used) -> Value {
    // neurons as S4 found them for Flash: tokens × the catalog price ÷ $0.000011
    let neurons = ((u.prompt - u.cached) as f64 * 0.15 + u.cached as f64 * 0.03 + u.completion as f64 * 0.5) / 11.0;
    json!({
        "prompt_tokens": u.prompt, "completion_tokens": u.completion, "total_tokens": u.prompt + u.completion,
        "prompt_tokens_details": { "cached_tokens": u.cached }, "neurons": neurons,
    })
}

fn delta_usage(prompt: u64, completion: u64) -> Value {
    json!({ "prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion, "prompt_tokens_details": { "cached_tokens": 0 } })
}

/// A call's usage as SGLang reports it behind Finite Private's limiter: no
/// cached tokens told (a test's `cached` is counted as prompt).
fn openai_usage(u: Used) -> Value {
    json!({ "prompt_tokens": u.prompt, "completion_tokens": u.completion, "total_tokens": u.prompt + u.completion, "prompt_tokens_details": null })
}

fn chunk(id: &str, model: &str, delta: Value, finish: Option<&str>, usage: Value) -> String {
    let c = json!({
        "id": id, "object": "chat.completion.chunk", "created": 0, "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish, "logprobs": null }],
        "usage": usage,
    });
    format!("data: {c}\n\n")
}

/// At most `budget` characters of `text` (all of it with no budget), and
/// whether it was cut.
fn within(text: &str, budget: &mut Option<usize>) -> (String, bool) {
    let Some(left) = budget.as_mut() else { return (text.to_string(), false) };
    let kept: String = text.chars().take(*left).collect();
    let n = kept.chars().count();
    *left -= n;
    (kept, n < text.chars().count())
}

/// A reply streamed as Workers AI streams it (S4's shapes), or, on the
/// OpenAI-compatible face, as OpenAI does (each chunk's usage null, the
/// whole call's on a last chunk with no choices), after `thinking` deltas
/// of reasoning; `broken` ends it before the usage and `[DONE]`, as a
/// dropped stream does.
#[allow(clippy::too_many_arguments)]
fn stream(face: Face, model: &str, reply: &Reply, ids: &mut u64, used: Used, mut budget: Option<usize>, broken: bool, pieces: usize, thinking: usize) -> String {
    *ids += 1;
    let id = format!("fake{ids:08x}");
    // Workers AI's per-chunk deltas of usage; OpenAI's none
    let delta = |prompt: u64, completion: u64| match face {
        Face::WorkersAi => delta_usage(prompt, completion),
        Face::OpenAi => Value::Null,
    };
    let mut out = chunk(&id, model, json!({ "role": "assistant", "content": "" }), None, delta(used.prompt, 0));
    // GLM's reasoning first (`reasoning_content`), when the test paces it so
    let mut parts: Vec<Value> = vec![json!({ "reasoning_content": THOUGHT }); thinking];
    // text, in pieces, so a client's draft grows
    let text_parts = |text: &str, parts: &mut Vec<Value>, budget: &mut Option<usize>| -> bool {
        let (text, was_cut) = within(text, budget);
        let chars: Vec<char> = text.chars().collect();
        for piece in chars.chunks(chars.len().div_ceil(pieces.max(1)).max(1)) {
            parts.push(json!({ "content": piece.iter().collect::<String>() }));
        }
        if chars.is_empty() {
            parts.push(json!({ "content": "" }));
        }
        was_cut
    };
    // tool calls, each's arguments in pieces
    let call_parts = |calls: &[(String, Value)], parts: &mut Vec<Value>, budget: &mut Option<usize>, ids: &mut u64, mut cut: bool| -> bool {
        for (i, (name, args)) in calls.iter().enumerate() {
            if cut {
                break;
            }
            *ids += 1;
            let (args, was_cut) = within(&args.to_string(), budget);
            cut = was_cut;
            let chars: Vec<char> = args.chars().collect();
            let mut pieces: Vec<String> = chars.chunks(ARGS_PIECE_CHARS).map(|c| c.iter().collect()).collect();
            if pieces.is_empty() {
                pieces.push(String::new());
            }
            for (n, piece) in pieces.iter().enumerate() {
                let call = match n {
                    0 => json!({ "index": i, "id": format!("call_{ids}"), "type": "function", "function": { "name": name, "arguments": piece } }),
                    _ => json!({ "index": i, "id": null, "function": { "arguments": piece } }),
                };
                parts.push(json!({ "tool_calls": [call] }));
            }
        }
        cut
    };
    let (finish, cut) = match reply {
        Reply::Text(text) => ("stop", text_parts(text, &mut parts, &mut budget)),
        Reply::Tools(calls) => ("tool_calls", call_parts(calls, &mut parts, &mut budget, ids, false)),
        // the text's deltas first, then the calls', as a model streams a
        // narrated call
        Reply::Narrated(text, calls) => {
            let cut = text_parts(text, &mut parts, &mut budget);
            ("tool_calls", call_parts(calls, &mut parts, &mut budget, ids, cut))
        }
    };
    // the completion's tokens, spread over its chunks as deltas
    let n = parts.len().max(1) as u64;
    for (i, part) in parts.iter().enumerate() {
        let share = used.completion / n + u64::from((i as u64) < used.completion % n);
        out += &chunk(&id, model, part.clone(), None, delta(0, share));
    }
    if broken {
        return out;
    }
    let finish = if cut { "length" } else { finish };
    out += &chunk(&id, model, json!({}), Some(finish), delta(0, 0));
    let last = match face {
        Face::WorkersAi => delta_usage(0, 0),
        Face::OpenAi => openai_usage(used),
    };
    out += &format!("data: {}\n\n", json!({ "id": id, "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [], "usage": last }));
    if face == Face::WorkersAi {
        out += &format!("data: {}\n\n", json!({ "response": "", "usage": usage_json(used) }));
    }
    out + "data: [DONE]\n\n"
}

fn problem(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "errors": [{ "message": message, "code": status }], "success": false }))
}

/// The size an image prompt draws at: 1536×1024 (six tiles) for one
/// naming "wide", else the model's 1024×1024.
pub fn image_size(prompt: &str) -> (u16, u16) {
    if prompt.contains("wide") {
        (1536, 1024)
    } else {
        (1024, 1024)
    }
}

/// The bytes an image prompt draws: a JPEG's head (SOI, APP0, SOF0 of
/// `image_size`, a scan's start), then the prompt as filler. One naming
/// "large" is over 1 MiB (a blob), one naming "mid" is 300 KiB (between
/// an app's write limit and a blob's size: bug 1's), any other 2 KiB. One
/// naming "png" is a PNG instead, which the platform refuses.
pub fn image_bytes(prompt: &str) -> Vec<u8> {
    let n = if prompt.contains("large") {
        1024 * 1024 + 4096
    } else if prompt.contains("mid") {
        300 * 1024
    } else {
        2048
    };
    if prompt.contains("png") {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend(prompt.bytes().cycle().take(n));
        return out;
    }
    let (width, height) = image_size(prompt);
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];
    out.extend([0xFF, 0xC0, 0x00, 0x11, 8]);
    out.extend(height.to_be_bytes());
    out.extend(width.to_be_bytes());
    out.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1, 0xFF, 0xDA, 0x00, 0x0C]);
    out.extend(prompt.bytes().cycle().take(n));
    out
}

/// 16 kHz, 16-bit mono PCM: 32,000 bytes a second.
const WAV_BYTES_PER_SECOND: u32 = 32_000;

/// A WAV of `seconds` of silence that says `words` (in a chunk of its own,
/// `said`, which the fake reads as what was spoken): a voice memo for a
/// test.
pub fn spoken_wav(words: &str, seconds: u32) -> Vec<u8> {
    let mut said = words.as_bytes().to_vec();
    if said.len() % 2 == 1 {
        said.push(0);
    }
    let data = (seconds * WAV_BYTES_PER_SECOND) as usize;
    let mut w = Vec::with_capacity(44 + said.len() + data);
    w.extend(b"RIFF");
    w.extend(((4 + 24 + 8 + said.len() + 8 + data) as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend(16_000u32.to_le_bytes());
    w.extend(WAV_BYTES_PER_SECOND.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"said");
    w.extend((words.len() as u32).to_le_bytes());
    w.extend(&said);
    w.extend(b"data");
    w.extend((data as u32).to_le_bytes());
    w.resize(w.len() + data, 0);
    w
}

/// What a WAV says (its `said` chunk) and how long it is (its data at its
/// byte rate); any other audio says nothing, and lasts its bytes at 16 kbps.
pub fn heard(audio: &[u8]) -> (String, f64) {
    let otherwise = (NO_WORDS.to_string(), audio.len() as f64 / 2_000.0);
    if audio.len() < 12 || &audio[..4] != b"RIFF" || &audio[8..12] != b"WAVE" {
        return otherwise;
    }
    let (mut words, mut rate, mut data) = (None, None, None);
    let mut at = 12;
    // bounded by the audio: each pass moves past one chunk
    while at + 8 <= audio.len() {
        let id = &audio[at..at + 4];
        let len = u32::from_le_bytes(audio[at + 4..at + 8].try_into().expect("four bytes")) as usize;
        let body = &audio[at + 8..(at + 8 + len).min(audio.len())];
        match id {
            b"fmt " if body.len() >= 12 => rate = Some(u32::from_le_bytes(body[8..12].try_into().expect("four bytes"))),
            b"said" => words = Some(String::from_utf8_lossy(body).into_owned()),
            b"data" => data = Some(body.len()),
            _ => {}
        }
        at += 8 + len + len % 2;
    }
    match (rate, data) {
        (Some(r), Some(d)) if r > 0 => (words.unwrap_or_else(|| NO_WORDS.to_string()), d as f64 / f64::from(r)),
        _ => otherwise,
    }
}

/// Whisper's answer to `input`, as its catalog's output schema says.
fn transcription_answer(input: &Value, log_id: &str) -> Response {
    let Some(audio) = input["audio"].as_str() else { return problem(400, "Type mismatch of '/audio', 'undefined' not in 'string'") };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(audio) else { return problem(400, "audio is base64") };
    let (text, duration) = heard(&bytes);
    let answer = json!({
        "text": text, "word_count": text.split_whitespace().count(),
        "transcription_info": { "language": input["language"].as_str().unwrap_or("en"), "language_probability": 0.99, "duration": duration, "duration_after_vad": duration },
        "segments": [{ "start": 0.0, "end": duration, "text": text }],
        "vtt": format!("WEBVTT\n\n00:00.000 --> 00:{duration:06.3}\n{text}\n"),
    });
    Response::json(200, &answer).with_header("cf-aig-log-id", log_id)
}

/// The image model's answer to `input`, as the binding answers it raw.
fn image_answer(input: &Value, log_id: &str) -> Response {
    let Some(prompt) = input["prompt"].as_str() else { return problem(400, "Type mismatch of '/prompt', 'undefined' not in 'string'") };
    let image = base64::engine::general_purpose::STANDARD.encode(image_bytes(prompt));
    Response::json(200, &json!({ "image": image })).with_header("cf-aig-log-id", log_id)
}

/// Finite Private's limiter's refusal: OpenAI's error shape, its `code`
/// and `type` as the limiter names them.
fn limiter_error(status: u16, kind: &str, code: &str, message: &str) -> Response {
    let mut error = json!({ "message": message, "type": kind, "code": code });
    if kind == "usage_limit" {
        let extra = json!({ "retry_after": 600, "reset_at": "2026-10-14T18:00:00Z", "dashboard_url": "https://finite.computer/dashboard", "request_id": "fp_req_fake" });
        error.as_object_mut().expect("an object").extend(extra.as_object().expect("an object").clone());
    }
    Response::json(status, &json!({ "error": error }))
}

/// A failure a test queued, as the face it came to says it.
fn failure(face: Face, status: u16) -> Response {
    match (face, status) {
        (Face::WorkersAi, _) => problem(status, "a failure the test asked for"),
        (Face::OpenAi, 429) => limiter_error(429, "usage_limit", "burst_window_limit_exceeded", "a failure the test asked for: this key's usage window is spent"),
        (Face::OpenAi, s) if s >= 500 => limiter_error(s, "server_error", "upstream_unavailable", "a failure the test asked for"),
        (Face::OpenAi, s) => limiter_error(s, "invalid_request_error", "test_failure", "a failure the test asked for"),
    }
}

/// A decision's input tokens as the fake counts them (the module's doc),
/// or why its input is refused: a selector not its own, media its model
/// does not take, a field that is no list of base64 `data:` URLs of its
/// kind, or more than its context window.
pub fn decision_tokens(model: DecisionModel, input: &Value) -> Result<u64, String> {
    use fragment_core::decide::Kind;
    if input["model"] != model.name() {
        return Err(format!("{}: model is {:?}", model.id(), model.name()));
    }
    let text = json!({ "state": input["state"], "questions": input["questions"] }).to_string().len();
    let mut tokens = (text / CHARS_PER_TOKEN).max(1) as u64;
    for (field, kind, prefix) in [("images", Kind::Image, "image/"), ("audio", Kind::Audio, "audio/"), ("videos", Kind::Video, "video/")] {
        let Some(items) = input.get(field) else { continue };
        if !model.takes(kind) {
            return Err(format!("{} takes no {field}: an unknown field", model.id()));
        }
        let items = items.as_array().ok_or(format!("{field} is a list"))?;
        for url in items {
            let url = url.as_str().ok_or(format!("{field} holds data: URLs"))?;
            let Some((head, data)) = url.strip_prefix("data:").and_then(|r| r.split_once(',')) else { return Err(format!("{field} holds data: URLs; a remote URL is not accepted")) };
            if !head.starts_with(prefix) || !head.ends_with(";base64") {
                return Err(format!("{field} holds base64 {prefix}* data: URLs, not {head:?}"));
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("{field}: not base64: {e}"))?;
            tokens += match kind {
                Kind::Image => {
                    kind_of(&bytes).filter(|k| *k != "GIF").ok_or("an image is a PNG, a JPEG or a WebP")?;
                    IMAGE_TOKENS
                }
                Kind::Audio | Kind::Video => (bytes.len() as u64).div_ceil(1024).max(1),
            };
        }
    }
    if tokens > model.context_tokens() {
        return Err(format!("the request's media and questions ({tokens} tokens) exceed the context window of {}", model.id()));
    }
    Ok(tokens)
}

/// A decision model's answer (the module's doc). Tests supply semantic
/// answers explicitly (`decide_next`); a refused input takes none of them.
fn decision_answer(s: &mut State, model: DecisionModel, input: &Value, log_id: &str) -> Response {
    let counted = match decision_tokens(model, input) {
        Ok(n) => n,
        Err(why) => return problem(400, &why),
    };
    let answer = s.decisions.pop_front().unwrap_or_else(|| {
        let mut answers = serde_json::Map::new();
        for (id, q) in input["questions"].as_object().into_iter().flatten() {
            let a = match q["type"].as_str() {
                Some("noul") => json!({ "type": "noul", "noul": 0.5 }),
                Some("choice") => {
                    let keys: Vec<&String> = q["criteria"].as_object().into_iter().flatten().map(|(k, _)| k).collect();
                    let ps: serde_json::Map<String, Value> = keys.iter().enumerate().map(|(i, k)| ((*k).clone(), json!(if i == 0 { 1.0 } else { 0.0 }))).collect();
                    json!({ "type": "choice", "choice": keys.first(), "probabilities": ps, "confidence": 1.0 })
                },
                Some("score") => {
                    let ps: serde_json::Map<String, Value> = (0..q["criteria"].as_array().map_or(0, Vec::len)).map(|i| (i.to_string(), json!(if i == 0 { 1.0 } else { 0.0 }))).collect();
                    json!({ "type": "score", "score": 0.0, "probabilities": ps, "confidence": 1.0 })
                },
                _ => Value::Null,
            };
            answers.insert(id.clone(), a);
        }
        let tokens = s.usage.pop_front().map_or(counted, |u| u.prompt);
        json!({ "model": model.name(), "answers": answers, "usage": { "input_tokens": tokens, "output_tokens": 0 } })
    });
    Response::json(200, &answer).with_header("cf-aig-log-id", log_id)
}

fn answer(s: &mut State, req: &Request) -> Response {
    if req.path == OPENAI_PATH {
        return openai_answer(s, req);
    }
    let Some(model) = req.path.strip_prefix("/run/").map(crate::http::decode) else { return problem(404, "no such route") };
    if req.method != "POST" {
        return problem(405, "the binding runs a model with a POST");
    }
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let metadata = req.header("x-fragment-ai-metadata").and_then(|m| serde_json::from_str(m).ok()).unwrap_or(Value::Null);
    let affinity = req.header("x-session-affinity").map(str::to_string);
    let authorization = req.header("authorization").map(str::to_string);
    s.calls.push(AiCall { face: Face::WorkersAi, model: model.clone(), body: body.clone(), metadata, affinity, authorization });
    if let Some(Some(status)) = s.failures.pop_front() {
        return failure(Face::WorkersAi, status);
    }
    if s.down.contains(&model) {
        return unroutable(s.calls.len());
    }
    s.answers += 1;
    let log_id = format!("01FAKE{:020}", s.answers);
    if model == IMAGE_MODEL {
        return image_answer(&body, &log_id);
    }
    if model == TRANSCRIBE_MODEL {
        return transcription_answer(&body, &log_id);
    }
    if let Some(decides) = DecisionModel::of_id(&model) {
        return decision_answer(s, decides, &body, &log_id);
    }
    if let Some(why) = image_refusal(&model, &body) {
        return problem(400, &why);
    }
    chat_answer(s, Face::WorkersAi, &model, &body).with_header("cf-aig-log-id", &log_id)
}

/// The OpenAI-compatible face (the module's doc): Finite Private, as its
/// limiter answers.
fn openai_answer(s: &mut State, req: &Request) -> Response {
    if req.method != "POST" {
        return limiter_error(405, "invalid_request_error", "method_not_allowed", "chat completions are a POST");
    }
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let model = body["model"].as_str().unwrap_or_default().to_string();
    let authorization = req.header("authorization").map(str::to_string);
    s.calls.push(AiCall { face: Face::OpenAi, model: model.clone(), body: body.clone(), metadata: Value::Null, affinity: req.header("x-session-affinity").map(str::to_string), authorization: authorization.clone() });
    if req.body.len() > OPENAI_BODY_MAX_BYTES {
        return limiter_error(413, "invalid_request_error", "payload_too_large", "length limit exceeded");
    }
    let presented = authorization.as_deref().and_then(|a| a.strip_prefix("Bearer ")).map(str::trim);
    match (presented, s.key.as_deref()) {
        (None, _) => return limiter_error(401, "invalid_api_key", "missing_authorization", "Finite Private API key is required."),
        (Some(p), Some(k)) if p == k => {}
        (Some(_), _) => return limiter_error(401, "usage_limit", "invalid_api_key", "Finite Private API key is invalid or revoked."),
    }
    if !OPENAI_MODELS.contains(&model.as_str()) {
        return limiter_error(400, "invalid_request_error", "unsupported_model", "The requested model is not available through Finite Private.");
    }
    if let Some(Some(status)) = s.failures.pop_front() {
        return failure(Face::OpenAi, status);
    }
    if s.down.contains(&model) {
        return limiter_error(502, "server_error", "upstream_unavailable", "Finite Private's model did not answer.");
    }
    s.answers += 1;
    if let Some(why) = image_refusal(&model, &body) {
        return limiter_error(400, "invalid_request_error", "invalid_request", &why);
    }
    chat_answer(s, Face::OpenAi, &model, &body)
}

/// A chat call's answer on `face`, from the test's levers or as it would.
fn chat_answer(s: &mut State, face: Face, model: &str, body: &Value) -> Response {
    let last = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].clone()).unwrap_or(Value::Null);
    let said = s.said.pop_front();
    let used = s.usage.pop_front();
    s.sleep_ms = s.delays.pop_front().unwrap_or(0);
    if s.transcripts && said.is_none() && follows_a_tool(body) {
        s.sleep_ms = s.sleep_ms.max(FOLLOW_UP_MS);
    }
    let broken = s.breaks.pop_front().unwrap_or(false);
    let reply = match said {
        Some(reply) => reply,
        None if s.transcripts => transcript_reply(body),
        None => Reply::Text(format!("echo: {}", last.as_str().unwrap_or(""))),
    };
    if body["stream"] == true {
        let called = |calls: &[(String, Value)]| calls.iter().map(|(n, a)| n.len() + a.to_string().len()).sum::<usize>();
        let written = match &reply {
            Reply::Text(t) => t.chars().count(),
            Reply::Tools(calls) => called(calls),
            Reply::Narrated(t, calls) => t.chars().count() + called(calls),
        };
        let budget = body["max_tokens"].as_u64().map(|t| t as usize * CHARS_PER_TOKEN);
        let pace = s.paces.pop_front();
        let pieces = match pace {
            Some(_) => TEXT_PIECES_PACED,
            None if s.transcripts => 3,
            None => 1,
        };
        let thinking = pace.map_or(0, |p| p.thinking);
        let events = stream(face, model, &reply, &mut s.tool_calls, usage_of(used, body, written), budget, broken, pieces, thinking);
        return match pace {
            Some(pace) => paced(events, pace, Arc::clone(&s.hung_up)),
            None => Response::bytes(200, "text/event-stream", events.into_bytes()),
        };
    }
    let tool_calls = |calls: &[(String, Value)]| -> Vec<Value> {
        calls.iter().enumerate().map(|(i, (name, args))| json!({ "id": format!("call_{}_{i}", s.answers), "type": "function", "function": { "name": name, "arguments": args.to_string() } })).collect()
    };
    let (message, finish, written) = match reply {
        Reply::Tools(calls) => (json!({ "role": "assistant", "content": null, "tool_calls": tool_calls(&calls) }), "tool_calls", 16),
        Reply::Narrated(text, calls) => {
            let n = text.chars().count() + 16;
            (json!({ "role": "assistant", "content": text, "tool_calls": tool_calls(&calls) }), "tool_calls", n)
        }
        Reply::Text(text) => {
            let n = text.chars().count();
            (json!({ "role": "assistant", "content": text }), "stop", n)
        }
    };
    let used = usage_of(used, body, written);
    let usage = match face {
        Face::WorkersAi => usage_json(used),
        Face::OpenAi => openai_usage(used),
    };
    let answer = json!({
        "id": format!("fake{:08x}", s.answers), "object": "chat.completion", "created": 0, "model": model,
        "choices": [{ "index": 0, "finish_reason": finish, "logprobs": null, "message": message }],
        "usage": usage,
    });
    Response::json(200, &answer)
}

/// A streamed answer written as `pace` says: chunked, an event at a time,
/// `every_ms` apart, then its connection held `hold_ms` past its `[DONE]`
/// with nothing more sent, as the gateway held one; a reader that drops it
/// meanwhile is counted in `hung_up`.
fn paced(events: String, pace: Pace, hung_up: Arc<AtomicU64>) -> Response {
    let events: Vec<String> = events.split_inclusive("\n\n").map(str::to_string).collect();
    let write = move |mut s: std::net::TcpStream| {
        let chunk = |e: &str| format!("{:x}\r\n{e}\r\n", e.len());
        for (i, e) in events.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(pace.every_ms));
            }
            if s.write_all(chunk(e).as_bytes()).and_then(|()| s.flush()).is_err() {
                return;
            }
        }
        // held open: a reader that hangs up reads as the socket's end here
        let until = Instant::now() + Duration::from_millis(pace.hold_ms);
        let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
        let mut buf = [0u8; 64];
        // bounded by the hold: each pass waits at most 100 ms
        while Instant::now() < until {
            match s.read(&mut buf) {
                Ok(0) => {
                    hung_up.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => {
                    hung_up.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
        let _ = s.write_all(b"0\r\n\r\n");
    };
    let headers = vec![("content-type".into(), "text/event-stream".into()), ("transfer-encoding".into(), "chunked".into())];
    Response { status: 200, headers, body: Vec::new(), unanswered: false, upgrade: Some(Box::new(write)) }
}

pub struct WorkersAi {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

impl WorkersAi {
    /// Serves on `port` (0: a free one).
    pub fn start(port: u16) -> std::io::Result<WorkersAi> {
        let state: Arc<Mutex<State>> = Arc::default();
        let st = Arc::clone(&state);
        let handler: Handler = Arc::new(move |req: &Request| {
            // an answer held back (`delay_next`) waits here, with the state
            // unlocked, after its levers were consumed
            let (response, sleep_ms) = {
                let mut s = st.lock().expect("workers ai state");
                let response = answer(&mut s, req);
                (response, std::mem::take(&mut s.sleep_ms))
            };
            if sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            }
            response
        });
        let server = Server::start(port, handler)?;
        Ok(WorkersAi { url: server.url.clone(), state, _server: server })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("workers ai state")
    }

    /// Drops any replies, usages, breaks, paces and delays set and not yet used.
    pub fn clear_script(&self) {
        let mut s = self.state();
        s.said.clear();
        s.usage.clear();
        s.decisions.clear();
        s.breaks.clear();
        s.paces.clear();
        s.delays.clear();
    }

    /// The texts the next calls answer, in order (a model's JSON, say).
    pub fn say_next(&self, texts: &[&str]) {
        self.state().said.extend(texts.iter().map(|t| Reply::Text(t.to_string())));
    }

    /// The replies the next calls answer, in order: text, tool calls, or both.
    pub fn reply_next(&self, replies: &[Reply]) {
        self.state().said.extend(replies.iter().cloned());
    }

    /// How the next streamed answers are written, in order (`Pace`).
    pub fn pace_next(&self, paces: &[Pace]) {
        self.state().paces.extend(paces.iter().copied());
    }

    /// Paced answers whose reader dropped the connection while it was held
    /// open past their `[DONE]`.
    pub fn hung_up(&self) -> u64 {
        self.state().hung_up.load(Ordering::Relaxed)
    }

    /// What the next answers report they used, in order.
    pub fn set_usage(&self, used: &[Used]) {
        self.state().usage.extend(used.iter().copied());
    }

    /// Supply the decision model's raw response, rather than claiming the
    /// deterministic fake can classify real input.
    pub fn decide_next(&self, answers: &[Value]) {
        self.state().decisions.extend(answers.iter().cloned());
    }

    /// The next streamed answer ends before its usage (a dropped stream).
    pub fn break_next(&self) {
        self.state().breaks.push_back(true);
    }

    /// Holds the next answers back this long (ms), in order.
    pub fn delay_next(&self, ms: &[u64]) {
        self.state().delays.extend(ms);
    }

    /// The next calls answer these statuses, in order (after any passed
    /// through by `pass_next`).
    pub fn fail_next(&self, statuses: &[u16]) {
        self.state().failures.extend(statuses.iter().map(|s| Some(*s)));
    }

    /// The next `n` calls answer as they would, before any failure queued after.
    pub fn pass_next(&self, n: usize) {
        self.state().failures.extend(std::iter::repeat_n(None, n));
    }


    /// These models are down: every call to one answers Workers AI's 502
    /// "could not route request to AI model" until it is brought up again
    /// (`down(&[])`).
    pub fn down(&self, models: &[&str]) {
        self.state().down = models.iter().map(|m| m.to_string()).collect();
    }

    /// The key the OpenAI-compatible face lets in (as Finite Private's
    /// limiter lets in a key finite.computer issued): every call with
    /// another, or none, is its 401.
    pub fn accept_key(&self, key: &str) {
        self.state().key = Some(key.to_string());
    }

    /// Calls answer from their transcript (`transcript_reply`), in pieces
    /// (`true`), or echo their last message.
    pub fn transcripts(&self, on: bool) {
        self.state().transcripts = on;
    }

    pub fn calls(&self) -> Vec<AiCall> {
        self.state().calls.clone()
    }

    /// The calls' inputs, in order (what a chat completion's body carried, less its model).
    pub fn chats(&self) -> Vec<Value> {
        self.state().calls.iter().map(|c| c.body.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a stream reads as S4 recorded it: deltas of usage on its
    /// chunks, then the whole call's on a line of Workers AI's own. Method:
    /// a text reply, and the usage summed two ways.
    #[test]
    fn a_stream_is_shaped_as_workers_ai_streams() {
        let used = Used { prompt: 23, cached: 0, completion: 15 };
        let text = stream(Face::WorkersAi, "@cf/zai-org/glm-5.3-flash", &Reply::Text("1, 2, 3".into()), &mut 0, used, None, false, 1, 0);
        let lines: Vec<Value> = text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect();
        let deltas: u64 = lines.iter().filter(|l| l.get("choices").is_some()).map(|l| l["usage"]["completion_tokens"].as_u64().unwrap()).sum();
        assert_eq!(deltas, 15, "the chunks' deltas add up to the completion");
        let last = lines.last().unwrap();
        assert_eq!((last["response"].clone(), last["usage"]["prompt_tokens"].clone(), last["usage"]["completion_tokens"].clone()), (json!(""), json!(23), json!(15)));
        assert!(text.ends_with("data: [DONE]\n\n"));
        let broken = stream(Face::WorkersAi, "m", &Reply::Text("hi".into()), &mut 0, used, None, true, 1, 0);
        assert!(!broken.contains("\"response\"") && !broken.contains("[DONE]"), "a broken stream ends before its usage");
    }

    /// A paced answer to `asked`, as a reader on its own socket takes it:
    /// each event's arrival (ms after the head), and the events.
    fn read_paced(ai: &WorkersAi, asked: &Value, until_done: bool) -> (Vec<(u128, String)>, std::net::TcpStream) {
        let body = serde_json::to_vec(asked).unwrap();
        let mut socket = std::net::TcpStream::connect(ai.url.trim_start_matches("http://")).unwrap();
        let head = format!("POST /run/{} HTTP/1.1\r\nhost: fake\r\ncontent-length: {}\r\n\r\n", fragment_core::models::CHEAP_MODEL, body.len());
        socket.write_all(&[head.as_bytes(), &body].concat()).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let t0 = Instant::now();
        let (mut seen, mut buf, mut events) = (String::new(), [0u8; 4096], Vec::new());
        // bounded by the answer: its events end at [DONE], or the read times out
        while !(until_done && seen.contains("data: [DONE]")) {
            let n = socket.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            for line in seen.lines().filter(|l| l.starts_with("data: ")).skip(events.len()).collect::<Vec<_>>() {
                events.push((t0.elapsed().as_millis(), line.to_string()));
            }
        }
        (events, socket)
    }

    /// Goal: a paced answer streams as a slow model does: reasoning
    /// first, its text in pieces, its events apart in time, then its
    /// connection held open past `[DONE]`; a reader that hangs up then is
    /// counted. Method: a paced call read on a raw socket, closed once its
    /// `[DONE]` came; another read to the stream's end.
    #[test]
    fn a_paced_answer_streams_apart_and_is_held_past_its_done() {
        let ai = WorkersAi::start(0).unwrap();
        let asked = json!({ "stream": true, "messages": [{ "role": "user", "content": "slowly" }] });
        ai.pace_next(&[Pace { every_ms: 30, thinking: 2, hold_ms: 3_000 }]);
        let (events, socket) = read_paced(&ai, &asked, true);
        let deltas: Vec<Value> = events.iter().filter_map(|(_, l)| serde_json::from_str::<Value>(&l[6..]).ok()).filter_map(|v| v["choices"][0].get("delta").cloned()).collect();
        assert_eq!(deltas.iter().filter(|d| d["reasoning_content"] == THOUGHT).count(), 2, "reasoning first: {deltas:?}");
        let said: Vec<&str> = deltas.iter().filter_map(|d| d["content"].as_str()).filter(|t| !t.is_empty()).collect();
        assert_eq!((said.len(), said.concat()), (TEXT_PIECES_PACED, "echo: slowly".to_string()), "its text in pieces");
        let (first, last) = (events.first().unwrap().0, events.last().unwrap().0);
        assert!(last - first >= 30 * (events.len() as u128 - 2), "apart in time: {first}..{last} ms for {}", events.len());
        assert_eq!(ai.hung_up(), 0);
        drop(socket);
        let t0 = Instant::now();
        while ai.hung_up() == 0 && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(ai.hung_up(), 1, "a reader that hangs up past [DONE] is counted");
        // read to its end: the stream ends only after the hold
        ai.pace_next(&[Pace { every_ms: 0, thinking: 0, hold_ms: 600 }]);
        let t0 = Instant::now();
        let (events, _socket) = read_paced(&ai, &asked, false);
        assert!(events.last().is_some_and(|(_, l)| l == "data: [DONE]") && t0.elapsed() >= Duration::from_millis(600), "held {:?}", t0.elapsed());
        assert_eq!(ai.hung_up(), 1, "a reader that waited out the hold is none");
    }

    /// Goal: a narrated call is one answer carrying text and a tool call,
    /// streamed as a model streams one: the text's deltas, then the call's,
    /// ending `tool_calls`; unstreamed, one message with both. Method: the
    /// stream's deltas in order; the unstreamed answer through the server;
    /// and a budget that cuts the text, so no call follows.
    #[test]
    fn a_narrated_call_streams_its_text_then_its_call() {
        let tools = json!([{ "type": "function", "function": { "name": "terminal" } }]);
        let asked = json!({ "messages": [{ "role": "user", "content": "[paul] narrate: echo narrated-ran" }], "tools": tools });
        let reply = transcript_reply(&asked);
        assert!(matches!(&reply, Reply::Narrated(t, c) if t == NARRATION && *c == vec![("terminal".to_string(), json!({ "command": "echo narrated-ran" }))]), "{reply:?}");
        // without a terminal tool, `narrate:` is only words
        assert!(matches!(transcript_reply(&json!({ "messages": [{ "role": "user", "content": "narrate: ls" }] })), Reply::Text(t) if t == "scripted: narrate: ls [1]"));
        // its answer after the tool's result waits as a model's next call
        // does; an answer to a person's message does not
        let ran = json!({ "messages": [{ "role": "user", "content": "narrate: echo x" }, { "role": "assistant", "content": NARRATION, "tool_calls": [] }, { "role": "tool", "content": "x" }] });
        assert!(follows_a_tool(&ran) && !follows_a_tool(&asked));
        let again = json!({ "messages": [{ "role": "tool", "content": "x" }, { "role": "user", "content": "hi" }] });
        assert!(!follows_a_tool(&again), "a tool's result before the newest message is an earlier turn's");

        let used = Used { prompt: 10, cached: 0, completion: 9 };
        let text = stream(Face::WorkersAi, "m", &reply, &mut 0, used, None, false, 3, 0);
        let deltas: Vec<Value> = text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str::<Value>(d).unwrap()).filter_map(|l| l["choices"][0]["delta"].as_object().cloned().map(Value::Object)).collect();
        let kinds: Vec<&str> = deltas.iter().filter_map(|d| if d["tool_calls"].is_array() { Some("call") } else if d["content"].as_str().is_some_and(|c| !c.is_empty()) { Some("text") } else { None }).collect();
        assert_eq!(kinds, ["text", "text", "text", "call"], "the text's pieces, then the call: {deltas:?}");
        let said: String = deltas.iter().filter_map(|d| d["content"].as_str()).collect();
        assert_eq!(said, NARRATION);
        let call = deltas.iter().find(|d| d["tool_calls"].is_array()).unwrap();
        assert_eq!(call["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(serde_json::from_str::<Value>(call["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap(), json!({ "command": "echo narrated-ran" }));
        assert!(text.contains("\"finish_reason\":\"tool_calls\""), "{text}");

        // a budget that cuts the text: no call, and it ends `length`
        let short = stream(Face::WorkersAi, "m", &reply, &mut 0, used, Some(4), false, 1, 0);
        assert!(!short.contains("tool_calls\":[") && short.contains("\"finish_reason\":\"length\""), "{short}");

        // unstreamed, one message carries both
        let ai = WorkersAi::start(0).unwrap();
        ai.transcripts(true);
        let body = serde_json::to_vec(&asked).unwrap();
        let mut socket = std::net::TcpStream::connect(ai.url.trim_start_matches("http://")).unwrap();
        let head = format!("POST /run/m HTTP/1.1\r\nhost: fake\r\ncontent-length: {}\r\n\r\n", body.len());
        std::io::Write::write_all(&mut socket, &[head.as_bytes(), &body].concat()).unwrap();
        let mut answer = String::new();
        std::io::Read::read_to_string(&mut socket, &mut answer).unwrap();
        let r: Value = serde_json::from_str(answer.split_once("\r\n\r\n").unwrap().1).unwrap();
        let message = &r["choices"][0]["message"];
        assert_eq!((message["content"].clone(), r["choices"][0]["finish_reason"].clone()), (json!(NARRATION), json!("tool_calls")), "{r}");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "terminal", "{r}");
    }

    /// Goal: the image model answers as its catalog says, a JPEG the cell
    /// reads its size from, in the bands the e2e needs. Method: the answer
    /// through the core's own reader, for each kind of prompt.
    #[test]
    fn an_image_answers_as_the_catalog_says() {
        let r = image_answer(&json!({ "prompt": "a wide lighthouse", "steps": 4 }), "01FAKE");
        let drawn = fragment_core::media::image_of(&r.body).unwrap();
        assert_eq!((drawn.width, drawn.height, drawn.bytes), (1536, 1024, image_bytes("a wide lighthouse")));
        assert!(r.headers.iter().any(|(k, _)| k == "cf-aig-log-id"));
        assert_eq!(fragment_core::media::jpeg_size(&image_bytes("a lighthouse")), Some((1024, 1024)));
        assert!(image_bytes("a large mural").len() > fragment_core::blob::BLOB_MIN_BYTES);
        assert!((256 * 1024..fragment_core::blob::BLOB_MIN_BYTES).contains(&image_bytes("a mid mural").len()));
        assert_eq!(fragment_core::media::jpeg_size(&image_bytes("a png")), None);
        assert_eq!(image_answer(&json!({ "steps": 4 }), "01FAKE").status, 400, "no prompt");
    }

    /// A 1×1 PNG, as a data URL.
    fn png_url() -> String {
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
            0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00,
            0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(PNG))
    }

    fn shown(url: &str) -> Value {
        json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Describe this screenshot." }, { "type": "image_url", "image_url": { "url": url } }] }] })
    }

    /// Goal: an image reaches only a model that reads it (Workers AI's
    /// catalog: GLM-5.3 Flash, not GLM-5.3), and a part that is no image
    /// is refused, as a model refuses it. Method: images, and parts that
    /// are not, to each model, by the refusal and over HTTP.
    #[test]
    fn images_go_only_to_a_model_that_reads_them() {
        let flash = fragment_core::models::VISION_MODEL_DEFAULT;
        let glm = fragment_core::models::MEDIUM_MODEL;
        assert_eq!(image_refusal(flash, &shown(&png_url())), None);
        assert!(image_refusal(glm, &shown(&png_url())).is_some_and(|w| w.contains("takes no image input")), "the medium tier's model reads no images");
        assert_eq!(image_refusal(glm, &json!({ "messages": [{ "role": "user", "content": "hi" }] })), None, "text alone is any chat model's");
        let jpeg = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(image_bytes("a wide screen")));
        assert_eq!(image_refusal(flash, &shown(&jpeg)), None);
        for (bad, why) in [
            ("https://example.com/a.png".to_string(), "a data: URL"),
            ("data:text/plain;base64,aGk=".to_string(), "base64 of an image"),
            ("data:image/png;base64,!!!".to_string(), "not base64"),
            (format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(b"no image")), "no PNG"),
        ] {
            assert!(image_refusal(flash, &shown(&bad)).is_some_and(|w| w.contains(why)), "{bad}: {:?}", image_refusal(flash, &shown(&bad)));
        }
        let no_url = json!({ "messages": [{ "role": "user", "content": [{ "type": "image_url", "image_url": {} }] }] });
        assert!(image_refusal(flash, &no_url).is_some());
        // over HTTP: 400 for the medium tier's model, the image described by the vision model's
        let ai = WorkersAi::start(0).unwrap();
        ai.transcripts(true);
        let post = |model: &str, body: &Value| crate::http::post(&format!("{}/run/{model}", ai.url), &[("content-type", "application/json")], body.to_string().as_bytes()).unwrap();
        assert_eq!(post(glm, &shown(&png_url())), 400);
        assert_eq!(post(flash, &shown(&png_url())), 200);
        let calls = ai.calls();
        assert_eq!(calls.len(), 2, "each call recorded, the refused one too");
        assert!(calls[1].model == flash && calls[1].body["messages"][0]["content"][1]["image_url"]["url"] == png_url().as_str());
    }

    /// Goal: a model that is down answers every call as Workers AI did
    /// when it could not reach GLM-5.3 Flash (a 502, internal code 4002),
    /// while other models answer; brought up, it answers again. Method:
    /// calls to two models, with one down, then none.
    #[test]
    fn a_model_that_is_down_cannot_be_routed_to() {
        let ai = WorkersAi::start(0).unwrap();
        let (flash, deepseek) = ("@cf/zai-org/glm-5.3-flash", "@cf/deepseek-ai/deepseek-v4-flash-0731");
        let ask = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        let post = |model: &str| crate::http::post(&format!("{}/run/{model}", ai.url), &[("content-type", "application/json")], ask.to_string().as_bytes()).unwrap();
        ai.down(&[flash]);
        assert_eq!((post(flash), post(flash), post(deepseek)), (502, 502, 200));
        let answer = unroutable(7);
        let body: Value = serde_json::from_slice(&answer.body).unwrap();
        assert_eq!((answer.status, body["internalCode"].clone(), body["description"].clone()), (502, json!(4002), json!("could not route request to AI model")));
        ai.down(&[]);
        assert_eq!(post(flash), 200, "up again");
        assert_eq!(ai.calls().len(), 4, "each call recorded, the failed ones too");
    }

    /// The data lines of a streamed answer, `[DONE]` left out.
    fn events(body: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(body).lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect()
    }

    /// Goal: the OpenAI-compatible face answers as Finite Private's limiter
    /// does: OpenAI's shapes (unstreamed, streamed with its usage last, a
    /// tool call), no cached tokens and no gateway log id; its refusals
    /// before any answer (no key, another key, another model, a body past
    /// 2 MiB); and the shared levers in its own error shapes (a spent
    /// window's 429, a 5xx, a model down). Every call is recorded, with the
    /// key it carried. Method: its handler, each way, and a call again (a
    /// replay answers the same).
    #[test]
    fn the_openai_face_answers_as_finite_privates_limiter() {
        let mut s = State { key: Some("fpk_live_test".into()), ..State::default() };
        let ask = |body: &Value, auth: Option<&str>| {
            let mut h = vec![("content-type", "application/json")];
            h.extend(auth.map(|a| ("authorization", a)));
            Request::new("POST", OPENAI_PATH, &h, body.to_string().as_bytes())
        };
        let chat = |stream: bool| json!({ "model": "glm-5-3-flash", "stream": stream, "messages": [{ "role": "user", "content": "hello" }] });
        let key = Some("Bearer fpk_live_test");
        let json_of = |r: &Response| serde_json::from_slice::<Value>(&r.body).unwrap();
        for _replay in 0..2 {
            let r = answer(&mut s, &ask(&chat(false), key));
            let v = json_of(&r);
            assert_eq!((r.status, v["choices"][0]["message"]["content"].clone(), v["model"].clone()), (200, json!("echo: hello"), json!("glm-5-3-flash")), "{v}");
            assert_eq!(v["usage"]["prompt_tokens_details"], Value::Null, "SGLang tells no cached tokens");
            assert!(v["usage"]["completion_tokens"].as_u64().is_some() && v["usage"].get("neurons").is_none() && !r.headers.iter().any(|(k, _)| k == "cf-aig-log-id"));
        }
        // streamed: each chunk's usage null, the whole call's last, as the route's meter reads it
        s.usage.push_back(Used { prompt: 40, cached: 0, completion: 12 });
        let r = answer(&mut s, &ask(&chat(true), key));
        let lines = events(&r.body);
        assert!(lines.iter().all(|l| l.get("response").is_none()), "no Workers AI line");
        let (last, chunks) = lines.split_last().unwrap();
        assert!(chunks.iter().all(|l| l["usage"].is_null() && l["choices"].as_array().is_some_and(|c| !c.is_empty())));
        assert_eq!((last["choices"].clone(), last["usage"]["prompt_tokens"].clone(), last["usage"]["completion_tokens"].clone()), (json!([]), json!(40), json!(12)));
        assert!(String::from_utf8_lossy(&r.body).ends_with("data: [DONE]\n\n"));
        let mut metered = fragment_core::models::Stream::default();
        metered.push(&r.body, None);
        metered.finish(None);
        assert_eq!(metered.usage().and_then(|u| fragment_core::models::usage_of("glm-5-3-flash", u)), Some(fragment_core::price::Usage::Tokens { model: "glm-5-3-flash".into(), input: 40, cached_input: 0, cache_write: 0, output: 12 }));
        // a tool call, streamed
        s.transcripts = true;
        let tools = json!([{ "type": "function", "function": { "name": "terminal" } }]);
        let run = json!({ "model": "glm-5-3-flash", "stream": true, "tools": tools, "messages": [{ "role": "user", "content": "[paul] run: ls" }] });
        let lines = events(&answer(&mut s, &ask(&run, key)).body);
        let called = lines.iter().find_map(|l| l["choices"][0]["delta"]["tool_calls"][0]["function"]["name"].as_str().map(str::to_string));
        assert_eq!(called.as_deref(), Some("terminal"));
        assert!(lines.iter().any(|l| l["choices"][0]["finish_reason"] == "tool_calls"));
        s.transcripts = false;

        // refused before any answer, each recorded
        let before = s.calls.len();
        let refusal = |r: Response| (r.status, json_of(&r)["error"]["code"].as_str().unwrap_or_default().to_string());
        assert_eq!(refusal(answer(&mut s, &ask(&chat(false), None))), (401, "missing_authorization".into()));
        assert_eq!(refusal(answer(&mut s, &ask(&chat(false), Some("Bearer fpk_live_other")))), (401, "invalid_api_key".into()));
        assert_eq!(refusal(answer(&mut s, &ask(&json!({ "model": "@cf/zai-org/glm-5.3-flash", "messages": [] }), key))), (400, "unsupported_model".into()));
        let large = json!({ "model": "glm-5-3-flash", "messages": [{ "role": "user", "content": "x".repeat(OPENAI_BODY_MAX_BYTES) }] });
        assert_eq!(refusal(answer(&mut s, &ask(&large, key))), (413, "payload_too_large".into()));
        let image = json!({ "model": "glm-5-3-flash", "messages": [{ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": png_url() } }] }] });
        assert_eq!(answer(&mut s, &ask(&image, key)).status, 400, "no images: they are not qualified there");
        assert_eq!(s.calls.len(), before + 5);
        assert!(s.calls[before..].iter().all(|c| c.face == Face::OpenAi));
        assert_eq!(s.calls[before + 1].authorization.as_deref(), Some("Bearer fpk_live_other"), "the key each carried, for a test to read");

        // the shared levers, in the limiter's shapes
        s.failures.extend([Some(429), Some(503)]);
        let r = answer(&mut s, &ask(&chat(false), key));
        let e = json_of(&r)["error"].clone();
        assert_eq!((r.status, e["type"].clone(), e["code"].clone()), (429, json!("usage_limit"), json!("burst_window_limit_exceeded")));
        assert!(e["retry_after"].is_u64() && e["reset_at"].is_string(), "{e}");
        assert_eq!(refusal(answer(&mut s, &ask(&chat(false), key))), (503, "upstream_unavailable".into()));
        s.down = vec!["glm-5-3-flash".into()];
        assert_eq!(refusal(answer(&mut s, &ask(&chat(true), key))), (502, "upstream_unavailable".into()));
        s.down.clear();
        assert_eq!(answer(&mut s, &ask(&chat(false), key)).status, 200, "up again");
        // the Workers AI face is as it was, beside it
        let flash = Request::new("POST", "/run/@cf/zai-org/glm-5.3-flash", &[], json!({ "messages": [{ "role": "user", "content": "hi" }] }).to_string().as_bytes());
        let r = answer(&mut s, &flash);
        assert!(r.status == 200 && r.headers.iter().any(|(k, _)| k == "cf-aig-log-id") && json_of(&r)["usage"]["neurons"].is_number());
        assert_eq!(s.calls.last().map(|c| c.face), Some(Face::WorkersAi));
    }

    /// Goal: what a model says of an image tells which image it was:
    /// its kind and size. Method: a PNG, JPEGs of two sizes, and bytes
    /// that are none.
    #[test]
    fn an_image_is_described_from_its_bytes() {
        assert_eq!(describe_image(&png_url()), "I see an image, a 1x1 PNG");
        let jpeg = |prompt: &str| format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(image_bytes(prompt)));
        assert_eq!(describe_image(&jpeg("a wide screen")), "I see an image, a 1536x1024 JPEG");
        assert_eq!(describe_image(&jpeg("a screen")), "I see an image, a 1024x1024 JPEG");
        assert_eq!(describe_image("data:image/png;base64,aGk="), "I see an image I cannot read");
        assert!(matches!(transcript_reply(&shown(&jpeg("a wide screen"))), Reply::Text(t) if t == "scripted: I see an image, a 1536x1024 JPEG"));
    }

    /// Goal: `look at your screen` is a computer_use capture, called as
    /// Hermes offers the tool (directly, or deferred behind its
    /// `tool_search` and called through `tool_call`), and its answer quotes
    /// what the vision model said of the screenshot. Method: each way it is
    /// offered, and a capture's result as Hermes gives it.
    #[test]
    fn a_look_at_the_screen_is_a_capture() {
        let capture = json!({ "action": "capture", "mode": "vision", "app": "screen" });
        let ask = |tools: Value| json!({ "messages": [{ "role": "user", "content": "[paul] look at your screen" }], "tools": tools });
        let direct = json!([{ "type": "function", "function": { "name": "computer_use" } }]);
        assert!(matches!(transcript_reply(&ask(direct)), Reply::Tools(c) if c == vec![("computer_use".to_string(), capture.clone())]));
        let bridged = json!([{ "type": "function", "function": { "name": "tool_search", "description": "… computer_use: Background desktop control …" } }, { "type": "function", "function": { "name": "tool_call" } }]);
        assert!(matches!(transcript_reply(&ask(bridged)), Reply::Tools(c) if c == vec![("tool_call".to_string(), json!({ "calls": [{ "name": "computer_use", "arguments": capture }] }))]));
        assert!(matches!(transcript_reply(&ask(json!([]))), Reply::Text(t) if t == "scripted: no computer_use among my tools"));
        // the capture's result: Hermes' JSON, the vision model's words in it
        let result = json!({ "mode": "vision", "width": 1456, "summary": "capture mode=vision 1456x816", "vision_analysis": "scripted: I see an image, a 1456x816 PNG", "vision_analysis_routed_via": "auxiliary.vision" }).to_string();
        let ran = json!({ "messages": [{ "role": "user", "content": "[paul] look at your screen" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": json!([{ "result": result }]).to_string() }] });
        assert!(matches!(transcript_reply(&ran), Reply::Text(t) if t == "scripted: the screen: I see an image, a 1456x816 PNG"));
    }

    /// Goal: a runtime's transcript decides its answer, whatever else it
    /// asked meanwhile. Method: each rule, from its transcript alone.
    #[test]
    fn a_memo_is_heard_as_it_says() {
        let wav = spoken_wav("hello from a voice memo", 3);
        assert_eq!(heard(&wav), ("hello from a voice memo".to_string(), 3.0));
        let odd = spoken_wav("hi!", 1);
        assert_eq!(heard(&odd), ("hi!".to_string(), 1.0), "an odd chunk is padded and read past");
        assert_eq!(heard(b"OggS not a wav"), (NO_WORDS.to_string(), 14.0 / 2_000.0));
        let input = json!({ "audio": base64::engine::general_purpose::STANDARD.encode(&wav), "task": "transcribe" });
        let r = transcription_answer(&input, "log");
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!((v["text"].clone(), v["transcription_info"]["duration"].clone(), v["word_count"].clone()), (json!("hello from a voice memo"), json!(3.0), json!(5)));
        assert_eq!(fragment_core::transcribe::usage_of(&v), Some(fragment_core::price::Usage::Neurons { milli: 2_332 }), "the cell meters what the answer says");
    }

    /// Goal: each Clef model answers System One's shape, mechanically and
    /// the same each time, counts its media as input, and refuses what its
    /// schema refuses. Method: a choice, a predicate and a score with an
    /// image on each model, audio on each, and an answer past the context.
    #[test]
    fn decisions_answer_as_the_clef_family_does() {
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let ask = |model: DecisionModel, extra: Value| {
            let mut input = json!({ "model": model.name(), "state": "chips", "questions": {
                "aisle": { "type": "choice", "instructions": "Aisle?", "criteria": { "snacks": "Chips", "produce": "Vegetables" } },
                "snack": { "type": "noul", "instructions": "A snack?" },
                "urgency": { "type": "score", "instructions": "How soon?", "criteria": ["Later", "Now"] }
            } });
            input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            input
        };
        let mut s = State::default();
        let answered = |s: &mut State, model: DecisionModel, input: &Value| {
            let r = decision_answer(s, model, input, "log");
            (r.status, serde_json::from_slice::<Value>(&r.body).unwrap())
        };
        let text = decision_tokens(DecisionModel::ClefFlash, &ask(DecisionModel::ClefFlash, json!({}))).unwrap();
        for model in fragment_core::decide::MODELS {
            let input = ask(model, json!({ "images": [png_url()] }));
            let (status, v) = answered(&mut s, model, &input);
            assert_eq!(status, 200, "{v}");
            assert_eq!(v["model"], model.name());
            assert_eq!(v["answers"]["aisle"], json!({ "type": "choice", "choice": "produce", "probabilities": { "produce": 1.0, "snacks": 0.0 }, "confidence": 1.0 }), "the first option, as the criteria map orders it");
            assert_eq!(v["answers"]["snack"], json!({ "type": "noul", "noul": 0.5 }));
            assert_eq!(v["answers"]["urgency"]["score"], 0.0);
            assert_eq!(v["usage"]["input_tokens"], text + IMAGE_TOKENS, "an image counts as its tokens");
            let call = fragment_core::decide::bound(&json!({ "model": model.name(), "input": "chips", "questions": {
                "aisle": { "type": "choice", "instructions": "Aisle?", "options": { "snacks": "Chips", "produce": "Vegetables" } },
                "snack": { "type": "predicate", "instructions": "A snack?" },
                "urgency": { "type": "score", "instructions": "How soon?", "levels": ["Later", "Now"] }
            } }), fragment_core::decide::Asker::Job).unwrap();
            assert!(call.answer(&v).is_ok(), "the cell reads the fake's answer as valid");
            let heard = ask(model, json!({ "audio": [format!("data:audio/wav;base64,{}", b64(&spoken_wav("hi", 1)))] }));
            let (status, v) = answered(&mut s, model, &heard);
            if model == DecisionModel::ClefOmni {
                assert_eq!((status, v["usage"]["input_tokens"].clone()), (200, json!(text + 32)), "32 KiB of WAV: a token a KiB");
            } else {
                assert_eq!(status, 400, "{} hears no audio: {v}", model.name());
            }
        }
        let wrong = ask(DecisionModel::Clef, json!({}));
        assert_eq!(answered(&mut s, DecisionModel::ClefOmni, &wrong).0, 400, "a selector not its own");
        for bad in [json!({ "images": ["https://example.com/a.png"] }), json!({ "images": [format!("data:audio/wav;base64,{}", b64(b"RIFF"))] }), json!({ "videos": ["data:video/mp4;base64,!!"] })] {
            assert_eq!(answered(&mut s, DecisionModel::ClefOmni, &ask(DecisionModel::ClefOmni, bad.clone())).0, 400, "{bad}");
        }
        let long = ask(DecisionModel::ClefFlash, json!({ "state": "x".repeat(4 * 24_577) }));
        assert_eq!(answered(&mut s, DecisionModel::ClefFlash, &long).0, 400, "past its context window");
        s.decisions.push_back(json!({ "answers": {} }));
        assert_eq!(answered(&mut s, DecisionModel::ClefFlash, &wrong).0, 400, "a refused input takes no scripted answer");
        assert_eq!(answered(&mut s, DecisionModel::Clef, &wrong).1, json!({ "answers": {} }), "the next does");
    }

    #[test]
    fn a_transcript_decides_its_answer() {
        let say = |text: &str| json!({ "messages": [{ "role": "user", "content": text }] });
        assert!(matches!(transcript_reply(&say("[paul] hi there\nnotes")), Reply::Text(t) if t == "scripted: hi there [1]"));
        let tools = json!([{ "type": "function", "function": { "name": "terminal" } }]);
        let run = json!({ "messages": [{ "role": "user", "content": "[paul] run: echo tool-ran" }], "tools": tools });
        assert!(matches!(transcript_reply(&run), Reply::Tools(c) if c == vec![("terminal".to_string(), json!({ "command": "echo tool-ran" }))]));
        let ran = json!({ "messages": [{ "role": "user", "content": "run: echo x" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "tool-ran\n" }], "tools": tools });
        assert!(matches!(transcript_reply(&ran), Reply::Text(t) if t == "scripted: the tool ran: tool-ran"));
        // a question with choices, when Hermes offers its clarify: a card
        let clarify = json!([{ "type": "function", "function": { "name": "clarify" } }]);
        let asked = json!({ "messages": [{ "role": "user", "content": "[paul] clarify: Which pot? | clay | plastic" }], "tools": clarify });
        assert!(matches!(transcript_reply(&asked), Reply::Tools(c) if c == vec![("clarify".to_string(), json!({ "questions": [{ "question": "Which pot?", "choices": ["clay", "plastic"] }] }))]));
        assert!(matches!(transcript_reply(&say("[paul] clarify: Which pot? | clay")), Reply::Text(t) if t == "scripted: clarify: Which pot? | clay [1]"), "without the tool, only words");
        let image = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "look" }, { "type": "image_url", "image_url": { "url": png_url() } }] }] });
        assert!(matches!(transcript_reply(&image), Reply::Text(t) if t == "scripted: I see an image, a 1x1 PNG"));
        // without a terminal tool, `run:` is only words
        assert!(matches!(transcript_reply(&say("run: ls")), Reply::Text(t) if t == "scripted: run: ls [1]"));
        // a command quoted after the first line is a note, not a request
        let quoted = json!({ "messages": [{ "role": "user", "content": "[paul] do you remember\n> run: rm -rf x" }], "tools": tools });
        assert!(matches!(transcript_reply(&quoted), Reply::Text(t) if t == "scripted: do you remember [1]"));
        // two of a person's messages merged into one: the newest is the request
        let merged = json!({ "messages": [{ "role": "user", "content": "[paul] run: rm -rf x\n\n[paul] do you remember" }], "tools": tools });
        assert!(matches!(transcript_reply(&merged), Reply::Text(t) if t == "scripted: do you remember [1]"));
    }
}
