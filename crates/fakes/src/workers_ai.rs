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
//! A chat call may carry images (`image_url` parts, OpenAI's shape) to a
//! model that reads them (`TAKES_IMAGES`, as Workers AI's catalog marks
//! them: the route's vision model); one carrying an image to another, or a
//! part that is no image (a `data:` URL that is not base64 of a PNG, a JPEG,
//! a GIF or a WebP), answers 400. An image is described from its bytes
//! (`describe_image`: its kind and size), so a test sees which image a
//! model was shown.
//!
//! Replies are scripted (text, tool calls, text beside tool calls, nothing,
//! or reasoning alone) and answered in order before falling back to an echo
//! of the last message, or, for a real agent runtime (`transcripts`), to
//! `transcript_reply`: a pure function of the transcript (lesson 13), which
//! an agent's own auxiliary calls (titles, its approval guardian) cannot put
//! out of order; its answer after a tool's result waits `FOLLOW_UP_MS`.
//! Levers: the calls made (with the gateway metadata the cell would send),
//! failures queued for the next calls, a delay, the usage the next answers
//! report (`set_usage`), and a stream cut before its usage (`break_next`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// The image model's catalog id (fragment_core::media::IMAGE_MODEL).
pub use fragment_core::media::IMAGE_MODEL;

/// The chat models the platform calls that read images: those Workers AI's
/// catalog marks "Vision: Yes" (GLM-5.3 Flash, the cheap tier's and the
/// default vision model; GLM-5.3, the medium tier's, has none).
pub const TAKES_IMAGES: [&str; 1] = [fragment_core::models::CHEAP_MODEL];
/// What a model is told of an image it was shown: `describe_image`'s
/// answer starts so.
pub const SEEN: &str = "I see an image";

/// A scripted model reply.
#[derive(Clone, Debug)]
pub enum Reply {
    Text(String),
    /// Tool calls: (name, arguments).
    Tools(Vec<(String, Value)>),
    /// Text and tool calls in one answer, as a model narrates its call
    /// ("Let me check that." beside a `terminal` call): streamed, the
    /// text's deltas, then the calls'.
    Narrated(String, Vec<(String, Value)>),
    /// No text and no tool call.
    Empty,
    /// Reasoning, and nothing else.
    Thinking(String),
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
    /// The catalog model its path named.
    pub model: String,
    /// The input, as sent.
    pub body: Value,
    /// The gateway metadata the cell sent beside it (`x-fragment-ai-metadata`).
    pub metadata: Value,
    /// GLM's prefix-cache key (`x-session-affinity`).
    pub affinity: Option<String>,
}

/// What an answer reports it used, when a test says (`set_usage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Used {
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
}

#[derive(Default)]
struct State {
    calls: Vec<AiCall>,
    /// How the next calls answer, in order: a status, or `None` as they would.
    failures: VecDeque<Option<u16>>,
    script: VecDeque<Reply>,
    /// The usage the next answers report, in order; then the default.
    usage: VecDeque<Used>,
    /// The next streamed answers end before their usage, in order.
    breaks: VecDeque<bool>,
    delays: VecDeque<u64>,
    sleep_ms: u64,
    tool_calls: u64,
    answers: u64,
    /// Unscripted calls answer `transcript_reply`, in pieces.
    transcripts: bool,
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
/// - Hermes' smart-approval guardian, asking for one word: `ESCALATE`, so a
///   person decides;
/// - a user message whose newest line is `run: <command>`, when the call
///   offers a `terminal` tool: that tool, called with the rest of the line;
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
    if said.contains("Respond with exactly one word: APPROVE, DENY, or ESCALATE") {
        return Reply::Text("ESCALATE".into());
    }
    let has_terminal = body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "terminal"));
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
    if newest.contains("look at your screen") {
        let offered = |name: &str| body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == name));
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

/// A reply streamed as Workers AI streams it (S4's shapes); `broken` ends
/// it before the usage and `[DONE]`, as a dropped stream does.
fn stream(model: &str, reply: &Reply, ids: &mut u64, used: Used, mut budget: Option<usize>, broken: bool, pieces: usize) -> String {
    *ids += 1;
    let id = format!("fake{ids:08x}");
    let mut out = chunk(&id, model, json!({ "role": "assistant", "content": "" }), None, delta_usage(used.prompt, 0));
    let mut parts: Vec<Value> = Vec::new();
    let mut cut = false;
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
    let finish = match reply {
        Reply::Text(text) => {
            cut = text_parts(text, &mut parts, &mut budget);
            "stop"
        }
        Reply::Empty => "stop",
        Reply::Thinking(text) => {
            parts.push(json!({ "reasoning_content": text }));
            "stop"
        }
        Reply::Tools(calls) => {
            cut = call_parts(calls, &mut parts, &mut budget, ids, false);
            "tool_calls"
        }
        // the text's deltas first, then the calls', as a model streams a
        // narrated call
        Reply::Narrated(text, calls) => {
            cut = text_parts(text, &mut parts, &mut budget);
            cut = call_parts(calls, &mut parts, &mut budget, ids, cut);
            "tool_calls"
        }
    };
    // the completion's tokens, spread over its chunks as deltas
    let n = parts.len().max(1) as u64;
    for (i, delta) in parts.iter().enumerate() {
        let share = used.completion / n + u64::from((i as u64) < used.completion % n);
        out += &chunk(&id, model, delta.clone(), None, delta_usage(0, share));
    }
    if broken {
        return out;
    }
    let finish = if cut { "length" } else { finish };
    out += &chunk(&id, model, json!({}), Some(finish), delta_usage(0, 0));
    out += &format!("data: {}\n\n", json!({ "id": id, "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [], "usage": delta_usage(0, 0) }));
    out += &format!("data: {}\n\n", json!({ "response": "", "usage": usage_json(used) }));
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

/// The image model's answer to `input`, as the binding answers it raw.
fn image_answer(input: &Value, log_id: &str) -> Response {
    let Some(prompt) = input["prompt"].as_str() else { return problem(400, "Type mismatch of '/prompt', 'undefined' not in 'string'") };
    let image = base64::engine::general_purpose::STANDARD.encode(image_bytes(prompt));
    Response::json(200, &json!({ "image": image })).with_header("cf-aig-log-id", log_id)
}

fn answer(s: &mut State, req: &Request) -> Response {
    let Some(model) = req.path.strip_prefix("/run/").map(crate::http::decode) else { return problem(404, "no such route") };
    if req.method != "POST" {
        return problem(405, "the binding runs a model with a POST");
    }
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let metadata = req.header("x-fragment-ai-metadata").and_then(|m| serde_json::from_str(m).ok()).unwrap_or(Value::Null);
    let affinity = req.header("x-session-affinity").map(str::to_string);
    s.calls.push(AiCall { model: model.clone(), body: body.clone(), metadata, affinity });
    if let Some(Some(status)) = s.failures.pop_front() {
        return problem(status, "a failure the test asked for");
    }
    s.answers += 1;
    let log_id = format!("01FAKE{:020}", s.answers);
    if model == IMAGE_MODEL {
        return image_answer(&body, &log_id);
    }
    if let Some(why) = image_refusal(&model, &body) {
        return problem(400, &why);
    }
    let last = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].clone()).unwrap_or(Value::Null);
    let scripted = s.script.pop_front();
    let used = s.usage.pop_front();
    s.sleep_ms = s.delays.pop_front().unwrap_or(0);
    if s.transcripts && scripted.is_none() && follows_a_tool(&body) {
        s.sleep_ms = s.sleep_ms.max(FOLLOW_UP_MS);
    }
    let broken = s.breaks.pop_front().unwrap_or(false);
    let unscripted = |s: &State| if s.transcripts { transcript_reply(&body) } else { Reply::Text(format!("echo: {}", last.as_str().unwrap_or(""))) };
    if body["stream"] == true {
        let reply = scripted.unwrap_or_else(|| unscripted(s));
        let called = |calls: &[(String, Value)]| calls.iter().map(|(n, a)| n.len() + a.to_string().len()).sum::<usize>();
        let written = match &reply {
            Reply::Text(t) | Reply::Thinking(t) => t.chars().count(),
            Reply::Tools(calls) => called(calls),
            Reply::Narrated(t, calls) => t.chars().count() + called(calls),
            Reply::Empty => 0,
        };
        let budget = body["max_tokens"].as_u64().map(|t| t as usize * CHARS_PER_TOKEN);
        let pieces = if s.transcripts { 3 } else { 1 };
        let events = stream(&model, &reply, &mut s.tool_calls, usage_of(used, &body, written), budget, broken, pieces);
        return Response::bytes(200, "text/event-stream", events.into_bytes()).with_header("cf-aig-log-id", &log_id);
    }
    let reply = match scripted {
        Some(r @ Reply::Text(_)) => r,
        _ if s.transcripts => transcript_reply(&body),
        _ => Reply::Text(format!("echo: {}", last.as_str().unwrap_or(""))),
    };
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
        Reply::Empty | Reply::Thinking(_) => (json!({ "role": "assistant", "content": "" }), "stop", 0),
    };
    let used = usage_of(used, &body, written);
    let answer = json!({
        "id": format!("fake{:08x}", s.answers), "object": "chat.completion", "created": 0, "model": model,
        "choices": [{ "index": 0, "finish_reason": finish, "logprobs": null, "message": message }],
        "usage": usage_json(used),
    });
    Response::json(200, &answer).with_header("cf-aig-log-id", &log_id)
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
            // unlocked, after its script was consumed
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

    /// Replies the next calls answer, in order (an unstreamed one takes text).
    pub fn script(&self, replies: &[Reply]) {
        self.state().script.extend(replies.iter().cloned());
    }

    /// Drops any scripted replies, usages, breaks and delays not yet used.
    pub fn clear_script(&self) {
        let mut s = self.state();
        s.script.clear();
        s.usage.clear();
        s.breaks.clear();
        s.delays.clear();
    }

    /// What the next answers report they used, in order.
    pub fn set_usage(&self, used: &[Used]) {
        self.state().usage.extend(used.iter().copied());
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


    /// Unscripted calls answer from their transcript (`transcript_reply`),
    /// in pieces (`true`), or echo their last message.
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
        let text = stream("@cf/zai-org/glm-5.3-flash", &Reply::Text("1, 2, 3".into()), &mut 0, used, None, false, 1);
        let lines: Vec<Value> = text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect();
        let deltas: u64 = lines.iter().filter(|l| l.get("choices").is_some()).map(|l| l["usage"]["completion_tokens"].as_u64().unwrap()).sum();
        assert_eq!(deltas, 15, "the chunks' deltas add up to the completion");
        let last = lines.last().unwrap();
        assert_eq!((last["response"].clone(), last["usage"]["prompt_tokens"].clone(), last["usage"]["completion_tokens"].clone()), (json!(""), json!(23), json!(15)));
        assert!(text.ends_with("data: [DONE]\n\n"));
        let broken = stream("m", &Reply::Text("hi".into()), &mut 0, used, None, true, 1);
        assert!(!broken.contains("\"response\"") && !broken.contains("[DONE]"), "a broken stream ends before its usage");
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
        let text = stream("m", &reply, &mut 0, used, None, false, 3);
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
        let short = stream("m", &reply, &mut 0, used, Some(4), false, 1);
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
        let flash = fragment_core::models::CHEAP_MODEL;
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
    fn a_transcript_decides_its_answer() {
        let say = |text: &str| json!({ "messages": [{ "role": "user", "content": text }] });
        assert!(matches!(transcript_reply(&say("[paul] hi there\nnotes")), Reply::Text(t) if t == "scripted: hi there [1]"));
        let tools = json!([{ "type": "function", "function": { "name": "terminal" } }]);
        let run = json!({ "messages": [{ "role": "user", "content": "[paul] run: echo tool-ran" }], "tools": tools });
        assert!(matches!(transcript_reply(&run), Reply::Tools(c) if c == vec![("terminal".to_string(), json!({ "command": "echo tool-ran" }))]));
        let ran = json!({ "messages": [{ "role": "user", "content": "run: echo x" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "tool-ran\n" }], "tools": tools });
        assert!(matches!(transcript_reply(&ran), Reply::Text(t) if t == "scripted: the tool ran: tool-ran"));
        assert!(matches!(transcript_reply(&say("Respond with exactly one word: APPROVE, DENY, or ESCALATE.")), Reply::Text(t) if t == "ESCALATE"));
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
