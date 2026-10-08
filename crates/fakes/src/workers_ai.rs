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
//! a GIF or a WebP), answers 400.
//!
//! A call answers the text a test set for it (`say_next`), else
//! (`plain_reply`), when it offers tools (and `tool_choice` is not `none`)
//! and its last message (a person's or a tool's; of one in parts, its last
//! text part) holds `[[call NAME {json}]]`, a call of that tool; after a
//! tool's result, `TOOL_SAID` and
//! the result's first 200 characters; and otherwise an echo of its last
//! message. A streamed text comes in `PIECES`.
//!
//! Clef (`CLEF_MODELS`, a job's `ai.decide`) answers as its catalog's
//! output schema says, decided from the words of its input (`clef_answer`),
//! its usage in input and output tokens.
//!
//! Levers: the calls made (with the gateway metadata the cell would send),
//! failures queued for the next calls, a delay, the usage the next answers
//! report (`set_usage`), a stream cut before its usage (`break_next`), and
//! the most calls it held at once (`most_at_once`).

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

/// A model's reply.
#[derive(Clone, Debug)]
pub enum Reply {
    Text(String),
    /// Tool calls: (name, arguments).
    Tools(Vec<(String, Value)>),
}

/// The characters one token of an answer stands for.
pub const CHARS_PER_TOKEN: usize = 4;
/// A streamed text comes in this many deltas, so a client's draft grows.
pub const PIECES: usize = 3;
/// What an answer to a tool's result starts with, before the result's
/// first `TOOL_SAID_CHARS` characters.
pub const TOOL_SAID: &str = "the tool said: ";
pub const TOOL_SAID_CHARS: usize = 200;
/// Clef's two sizes, as the catalog names them.
pub const CLEF_MODELS: [&str; 2] = [fragment_core::decide::CLEF_MODEL, fragment_core::decide::CLEF_FLASH_MODEL];
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
    /// The texts the next calls answer, in order; then as they would.
    said: VecDeque<String>,
    /// The usage the next answers report, in order; then the default.
    usage: VecDeque<Used>,
    /// The next streamed answers end before their usage, in order.
    breaks: VecDeque<bool>,
    delays: VecDeque<u64>,
    sleep_ms: u64,
    tool_calls: u64,
    answers: u64,
    /// Calls being answered now, and the most there were at once.
    at_once: usize,
    most_at_once: usize,
}

/// The images a chat's messages carry (`image_url` parts' URLs), or why a
/// part is none.
fn images_of(body: &Value) -> Result<Vec<String>, String> {
    let mut urls = vec![];
    for m in body["messages"].as_array().into_iter().flatten() {
        for part in m["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "image_url") {
            let url = part["image_url"]["url"].as_str().ok_or("an image_url part has image_url.url, a string")?;
            check_image(url)?;
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

/// Whether a `data:` URL is base64 of an image: a PNG, a JPEG, a GIF or a
/// WebP, by its first bytes, as a model detects one.
fn check_image(url: &str) -> Result<(), String> {
    let Some((head, data)) = url.strip_prefix("data:").and_then(|r| r.split_once(',')) else { return Err("an image_url is a data: URL (a fetched URL is the hosted lane's)".into()) };
    if !head.starts_with("image/") || !head.ends_with(";base64") {
        return Err(format!("an image_url's data: URL is base64 of an image, not {head:?}"));
    }
    let b = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("an image_url's data is not base64: {e}"))?;
    let image = b.starts_with(b"\x89PNG\r\n\x1a\n") || b.starts_with(&[0xFF, 0xD8, 0xFF]) || b.starts_with(b"GIF8") || (b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP");
    image.then_some(()).ok_or_else(|| "an image_url's data is no PNG, JPEG, GIF or WebP".into())
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// The words a message ends with: its text, or of one in parts its last
/// text part's (a mind's turn sends its view and its state first, which
/// may quote earlier words, and the person's words last).
fn last_words(content: &Value) -> String {
    match content {
        Value::Array(parts) => parts.iter().rev().find_map(|p| p["text"].as_str()).unwrap_or("").to_string(),
        other => text_of(other),
    }
}

/// A tool call a message asks for: `[[call NAME {json}]]`, its arguments
/// the one JSON value after the name.
pub fn directive(text: &str) -> Option<(String, Value)> {
    let rest = &text[text.find("[[call ")? + "[[call ".len()..];
    let (name, rest) = rest.split_once(' ')?;
    let mut values = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    let args = values.next()?.ok()?;
    rest[values.byte_offset()..].trim_start().starts_with("]]").then(|| (name.to_string(), args))
}

/// The answer a call gets when no test scripted one and no agent runtime
/// reads it: a tool call its last message (a person's or a tool's) asks
/// for with `[[call NAME {json}]]`, when the call offers that tool and does
/// not say `tool_choice: "none"` (as a model calls none then); else, after
/// a tool's result, `TOOL_SAID` and the result's first `TOOL_SAID_CHARS`
/// characters; else an echo of its last message.
pub fn plain_reply(body: &Value) -> Reply {
    let last = body["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or(Value::Null);
    let said = text_of(&last["content"]);
    let offered = |name: &str| body["tool_choice"] != "none" && body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == name));
    if last["role"] == "user" || last["role"] == "tool" {
        if let Some((name, args)) = directive(&last_words(&last["content"])).filter(|(name, _)| offered(name)) {
            return Reply::Tools(vec![(name, args)]);
        }
    }
    if last["role"] == "tool" {
        return Reply::Text(format!("{TOOL_SAID}{}", said.chars().take(TOOL_SAID_CHARS).collect::<String>()));
    }
    Reply::Text(format!("echo: {}", last["content"].as_str().unwrap_or("")))
}

/// Text's words, lowercased: its runs of letters and digits.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Text, or JSON as its text: what Clef reads of a state or a question.
fn read_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Clef's answer to one question about `state` (its words), decided from
/// the words alone: a `noul` is 0.9 when a word of its instructions over 3
/// letters is a word of the state, else 0.1; a `choice` is the first
/// option (by id) the state names, else the first; a `score` is the
/// highest level whose description the state holds, else 0.
fn clef_decides(question: &Value, state: &[String]) -> Result<Value, String> {
    let named = |w: &str| state.iter().any(|s| s == w);
    match question["type"].as_str() {
        Some("noul") => {
            let yes = words(&read_as_text(&question["instructions"])).iter().any(|w| w.chars().count() > 3 && named(w));
            Ok(json!({ "type": "noul", "noul": if yes { 0.9 } else { 0.1 } }))
        }
        Some("choice") => {
            let options: Vec<&String> = question["criteria"].as_object().map(|o| o.keys().collect()).unwrap_or_default();
            let first = *options.first().ok_or("a choice has options")?;
            let chosen = options.iter().copied().find(|o| named(&o.to_lowercase())).unwrap_or(first);
            let rest = 0.1 / (options.len().max(2) - 1) as f64;
            let probabilities: serde_json::Map<String, Value> = options.iter().map(|o| ((*o).clone(), json!(if *o == chosen { 0.9 } else { rest }))).collect();
            Ok(json!({ "type": "choice", "choice": chosen, "probabilities": probabilities, "confidence": 0.8 }))
        }
        Some("score") => {
            let levels = question["criteria"].as_array().ok_or("a score has levels")?;
            let state_text = format!(" {} ", state.join(" "));
            let held = |l: &Value| {
                let w = words(&read_as_text(l));
                !w.is_empty() && state_text.contains(&format!(" {} ", w.join(" ")))
            };
            let level = levels.iter().rposition(held).unwrap_or(0);
            let legend: serde_json::Map<String, Value> = levels.iter().enumerate().map(|(i, l)| (i.to_string(), l.clone())).collect();
            let probabilities: serde_json::Map<String, Value> = (0..levels.len()).map(|i| (i.to_string(), json!(if i == level { 1.0 } else { 0.0 }))).collect();
            Ok(json!({ "type": "score", "score": level as f64, "legend": legend, "probabilities": probabilities, "confidence": 1.0 }))
        }
        other => Err(format!("a question's type is noul, choice or score, not {other:?}")),
    }
}

/// Clef's answer to `input` (its catalog's input and output schemas):
/// `{model, answers, usage: {input_tokens, output_tokens}}`, or a 400 for
/// an input it refuses (a `model` that is not the path's size, no
/// questions or more than 64, a question of no type it knows).
pub fn clef_answer(model: &str, input: &Value, used: Option<Used>) -> Result<Value, String> {
    let size = model.strip_prefix("@cf/cloudflare/").unwrap_or(model);
    if input["model"].as_str().map(str::trim) != Some(size) {
        return Err(format!("/model must be {size:?} for {model}"));
    }
    let questions = input["questions"].as_object().filter(|q| (1..=64).contains(&q.len())).ok_or("/questions has 1 to 64 questions")?;
    if input.get("state").is_none_or(Value::is_null) {
        return Err("/state is required".into());
    }
    let state = words(&read_as_text(&input["state"]));
    let answers = questions.iter().map(|(id, q)| clef_decides(q, &state).map(|a| (id.clone(), a))).collect::<Result<serde_json::Map<String, Value>, String>>()?;
    let used = used.unwrap_or(Used { prompt: (input.to_string().len() / CHARS_PER_TOKEN).max(1) as u64, cached: 0, completion: questions.len() as u64 });
    Ok(json!({ "model": size, "answers": answers, "usage": { "input_tokens": used.prompt, "output_tokens": used.completion } }))
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
fn stream(model: &str, reply: &Reply, ids: &mut u64, used: Used, mut budget: Option<usize>, broken: bool) -> String {
    *ids += 1;
    let id = format!("fake{ids:08x}");
    let mut out = chunk(&id, model, json!({ "role": "assistant", "content": "" }), None, delta_usage(used.prompt, 0));
    let mut parts: Vec<Value> = Vec::new();
    let (finish, mut cut) = match reply {
        // text, in pieces, so a client's draft grows
        Reply::Text(text) => {
            let (text, cut) = within(text, &mut budget);
            let chars: Vec<char> = text.chars().collect();
            for piece in chars.chunks(chars.len().div_ceil(PIECES).max(1)) {
                parts.push(json!({ "content": piece.iter().collect::<String>() }));
            }
            ("stop", cut)
        }
        Reply::Tools(_) => ("tool_calls", false),
    };
    // tool calls, each's arguments in pieces
    let calls: &[(String, Value)] = match reply {
        Reply::Tools(calls) => calls,
        Reply::Text(_) => &[],
    };
    for (i, (name, args)) in calls.iter().enumerate() {
        if cut {
            break;
        }
        *ids += 1;
        let (args, was_cut) = within(&args.to_string(), &mut budget);
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
    if parts.is_empty() {
        parts.push(json!({ "content": "" }));
    }
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
    if CLEF_MODELS.contains(&model.as_str()) {
        return match clef_answer(&model, &body, s.usage.pop_front()) {
            Ok(answer) => Response::json(200, &answer).with_header("cf-aig-log-id", &log_id),
            Err(why) => problem(400, &why),
        };
    }
    if let Some(why) = image_refusal(&model, &body) {
        return problem(400, &why);
    }
    let said = s.said.pop_front();
    let used = s.usage.pop_front();
    s.sleep_ms = s.delays.pop_front().unwrap_or(0);
    let broken = s.breaks.pop_front().unwrap_or(false);
    let reply = match said {
        Some(text) => Reply::Text(text),
        None => plain_reply(&body),
    };
    if body["stream"] == true {
        let written = match &reply {
            Reply::Text(t) => t.chars().count(),
            Reply::Tools(calls) => calls.iter().map(|(n, a)| n.len() + a.to_string().len()).sum::<usize>(),
        };
        let budget = body["max_tokens"].as_u64().map(|t| t as usize * CHARS_PER_TOKEN);
        let events = stream(&model, &reply, &mut s.tool_calls, usage_of(used, &body, written), budget, broken);
        return Response::bytes(200, "text/event-stream", events.into_bytes()).with_header("cf-aig-log-id", &log_id);
    }
    let tool_calls = |calls: &[(String, Value)]| -> Vec<Value> {
        calls.iter().enumerate().map(|(i, (name, args))| json!({ "id": format!("call_{}_{i}", s.answers), "type": "function", "function": { "name": name, "arguments": args.to_string() } })).collect()
    };
    let (message, finish, written) = match reply {
        Reply::Tools(calls) => (json!({ "role": "assistant", "content": null, "tool_calls": tool_calls(&calls) }), "tool_calls", 16),
        Reply::Text(text) => {
            let n = text.chars().count();
            (json!({ "role": "assistant", "content": text }), "stop", n)
        }
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
            // unlocked, after its levers were consumed
            let (response, sleep_ms) = {
                let mut s = st.lock().expect("workers ai state");
                s.at_once += 1;
                s.most_at_once = s.most_at_once.max(s.at_once);
                let response = answer(&mut s, req);
                (response, std::mem::take(&mut s.sleep_ms))
            };
            if sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            }
            st.lock().expect("workers ai state").at_once -= 1;
            response
        });
        let server = Server::start(port, handler)?;
        Ok(WorkersAi { url: server.url.clone(), state, _server: server })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("workers ai state")
    }

    /// Drops any texts, usages, breaks and delays set and not yet used.
    pub fn clear_script(&self) {
        let mut s = self.state();
        s.said.clear();
        s.usage.clear();
        s.breaks.clear();
        s.delays.clear();
    }

    /// The texts the next calls answer, in order (a model's JSON, say).
    pub fn say_next(&self, texts: &[&str]) {
        self.state().said.extend(texts.iter().map(|t| t.to_string()));
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

    pub fn calls(&self) -> Vec<AiCall> {
        self.state().calls.clone()
    }

    /// The most calls it was answering at once since it started, or since
    /// the last `reset_at_once` (a delay, `delay_next`, holds calls long
    /// enough to overlap).
    pub fn most_at_once(&self) -> usize {
        self.state().most_at_once
    }

    pub fn reset_at_once(&self) {
        let mut s = self.state();
        s.most_at_once = s.at_once;
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
        let text = stream("@cf/zai-org/glm-5.3-flash", &Reply::Text("1, 2, 3".into()), &mut 0, used, None, false);
        let lines: Vec<Value> = text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect();
        let deltas: u64 = lines.iter().filter(|l| l.get("choices").is_some()).map(|l| l["usage"]["completion_tokens"].as_u64().unwrap()).sum();
        assert_eq!(deltas, 15, "the chunks' deltas add up to the completion");
        let last = lines.last().unwrap();
        assert_eq!((last["response"].clone(), last["usage"]["prompt_tokens"].clone(), last["usage"]["completion_tokens"].clone()), (json!(""), json!(23), json!(15)));
        assert!(text.ends_with("data: [DONE]\n\n"));
        let broken = stream("m", &Reply::Text("hi".into()), &mut 0, used, None, true);
        assert!(!broken.contains("\"response\"") && !broken.contains("[DONE]"), "a broken stream ends before its usage");
    }

    fn lookup_tools() -> Value {
        json!([{ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object" } } }])
    }

    /// Goal: a call offering tools answers the call its last message asks
    /// for, a tool's result is answered quoting it, and anything else is
    /// echoed. Method: each case, and directives that do not read or name
    /// a tool not offered.
    #[test]
    fn a_plain_call_answers_its_directive_or_its_tools_result() {
        let ask = |text: &str, tools: Value| json!({ "messages": [{ "role": "user", "content": text }], "tools": tools });
        let call = plain_reply(&ask("look it up [[call lookup {\"word\": \"shard]]\", \"n\": [[1]]}]] please", lookup_tools()));
        assert!(matches!(&call, Reply::Tools(c) if *c == vec![("lookup".to_string(), json!({ "word": "shard]]", "n": [[1]] }))]), "{call:?}");
        assert!(matches!(plain_reply(&ask("[[call lookup {\"word\": \"x\"}]]", json!([]))), Reply::Text(t) if t.starts_with("echo: ")), "no tools offered");
        assert!(matches!(plain_reply(&ask("[[call zoom {}]]", lookup_tools())), Reply::Text(_)), "a tool not offered");
        assert!(matches!(plain_reply(&ask("[[call lookup {\"word\": ]]", lookup_tools())), Reply::Text(_)), "arguments that do not read");
        assert!(matches!(plain_reply(&ask("[[call lookup {} no end", lookup_tools())), Reply::Text(_)));
        let long = "x".repeat(300);
        let ran = json!({ "messages": [
            { "role": "user", "content": "[[call lookup {}]]" },
            { "role": "assistant", "content": "", "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "lookup", "arguments": "{}" } }] },
            { "role": "tool", "tool_call_id": "call_1", "content": format!("a small fragment {long}") },
        ], "tools": lookup_tools() });
        let said = match plain_reply(&ran) {
            Reply::Text(t) => t,
            other => panic!("{other:?}"),
        };
        assert!(said.starts_with("the tool said: a small fragment") && said.chars().count() == TOOL_SAID.len() + TOOL_SAID_CHARS, "{said}");
        let again = json!({ "messages": [{ "role": "tool", "content": "then [[call lookup {\"word\": \"more\"}]]" }], "tools": lookup_tools() });
        assert!(matches!(plain_reply(&again), Reply::Tools(c) if c[0].1 == json!({ "word": "more" })), "a tool's result may ask for another call");
        assert!(matches!(plain_reply(&ask("hi", json!(null))), Reply::Text(t) if t == "echo: hi"));
        let parts = json!({ "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "<chat>\n0+1|user: an old [[call lookup {\"word\": \"old\"}]]\n</chat>" },
            { "type": "text", "text": "Chat: t_1 \"x [[call lookup {\\\"word\\\": \\\"esc\\\"}]]\"" },
            { "type": "text", "text": "now [[call lookup {\"word\": \"new\"}]]" },
        ] }], "tools": lookup_tools() });
        assert!(matches!(plain_reply(&parts), Reply::Tools(c) if c[0].1 == json!({ "word": "new" })), "of a message in parts, the words of its last");
        let none = json!({ "messages": [{ "role": "user", "content": "[[call lookup {}]]" }], "tools": lookup_tools(), "tool_choice": "none" });
        assert!(matches!(plain_reply(&none), Reply::Text(t) if t.starts_with("echo: ")), "tools offered, none to be called (a compaction)");
    }

    /// Goal: a plain streamed text comes in pieces, and a directive's call
    /// streams as a model streams one; both through the server. Method: the
    /// streamed answers' deltas.
    #[test]
    fn a_plain_stream_comes_in_pieces() {
        let ai = WorkersAi::start(0).unwrap();
        let post = |body: &Value| {
            let body = serde_json::to_vec(body).unwrap();
            let mut socket = std::net::TcpStream::connect(ai.url.trim_start_matches("http://")).unwrap();
            let head = format!("POST /run/{} HTTP/1.1\r\nhost: fake\r\ncontent-length: {}\r\n\r\n", fragment_core::models::CHEAP_MODEL, body.len());
            std::io::Write::write_all(&mut socket, &[head.as_bytes(), &body].concat()).unwrap();
            let mut answer = String::new();
            std::io::Read::read_to_string(&mut socket, &mut answer).unwrap();
            answer.split_once("\r\n\r\n").unwrap().1.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str::<Value>(d).unwrap()).collect::<Vec<Value>>()
        };
        let lines = post(&json!({ "stream": true, "messages": [{ "role": "user", "content": "a long thought about gardens" }] }));
        let texts: Vec<&str> = lines.iter().filter_map(|l| l["choices"][0]["delta"]["content"].as_str()).filter(|t| !t.is_empty()).collect();
        assert_eq!((texts.len(), texts.concat()), (PIECES, "echo: a long thought about gardens".to_string()));
        let lines = post(&json!({ "stream": true, "tools": lookup_tools(), "messages": [{ "role": "user", "content": "[[call lookup {\"word\": \"a\"}]]" }] }));
        let call = lines.iter().find_map(|l| l["choices"][0]["delta"]["tool_calls"][0].as_object().cloned()).unwrap();
        assert_eq!((call["function"]["name"].clone(), call["function"]["arguments"].clone()), (json!("lookup"), json!("{\"word\":\"a\"}")));
        assert!(lines.iter().any(|l| l["choices"][0]["finish_reason"] == "tool_calls"));
    }

    /// Goal: Clef answers as its catalog's output schema says, decided
    /// from the words of its input, and refuses what it would. Method: each
    /// question type, decided both ways, and refused inputs.
    #[test]
    fn clef_decides_from_the_words_of_its_input() {
        let input = json!({
            "model": "clef-flash", "state": "We planned the garden: tomatoes and basil, all minor work.",
            "questions": {
                "garden": { "type": "noul", "instructions": "Plants or garden?" },
                "money": { "type": "noul", "instructions": "Money or taxes, any?" },
                "room": { "type": "choice", "instructions": "Which room?", "criteria": { "kitchen": "cooking", "garden": "outside", "attic": null } },
                "nothing": { "type": "choice", "instructions": "Which?", "criteria": { "b": "", "a": "" } },
                "size": { "type": "score", "instructions": "How big?", "criteria": ["none", "minor work", "major work"] },
            },
        });
        let a = clef_answer(fragment_core::decide::CLEF_FLASH_MODEL, &input, None).unwrap();
        assert_eq!(a["model"], "clef-flash");
        assert_eq!((a["answers"]["garden"]["noul"].clone(), a["answers"]["money"]["noul"].clone()), (json!(0.9), json!(0.1)), "'garden' is in the state; 'any' is no word over 3 letters");
        assert_eq!(a["answers"]["room"]["choice"], "garden", "the option the state names");
        assert_eq!(a["answers"]["room"]["probabilities"]["garden"], 0.9);
        assert_eq!(a["answers"]["nothing"]["choice"], "a", "none named: the first by id");
        assert_eq!((a["answers"]["size"]["score"].clone(), a["answers"]["size"]["legend"]["2"].clone()), (json!(1.0), json!("major work")));
        assert_eq!(a["usage"]["output_tokens"], 5);
        let used = clef_answer(fragment_core::decide::CLEF_MODEL, &json!({ "model": "clef", "state": { "k": "v" }, "questions": { "q": { "type": "noul", "instructions": "?" } } }), Some(Used { prompt: 70, cached: 0, completion: 1 })).unwrap();
        assert_eq!(used["usage"], json!({ "input_tokens": 70, "output_tokens": 1 }));
        let refused = |model: &str, input: Value| clef_answer(model, &input, None).unwrap_err();
        assert!(refused(fragment_core::decide::CLEF_MODEL, json!({ "model": "clef-flash", "state": "s", "questions": { "q": { "type": "noul", "instructions": "?" } } })).contains("/model"));
        assert!(refused(fragment_core::decide::CLEF_MODEL, json!({ "model": "clef", "state": "s", "questions": {} })).contains("/questions"));
        assert!(refused(fragment_core::decide::CLEF_MODEL, json!({ "model": "clef", "questions": { "q": { "type": "noul", "instructions": "?" } } })).contains("/state"));
        assert!(refused(fragment_core::decide::CLEF_MODEL, json!({ "model": "clef", "state": "s", "questions": { "q": { "type": "rank" } } })).contains("noul, choice or score"));
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
        // over HTTP: 400 for the medium tier's model, an answer from the vision model's
        let ai = WorkersAi::start(0).unwrap();
        let post = |model: &str, body: &Value| crate::http::post(&format!("{}/run/{model}", ai.url), &[("content-type", "application/json")], body.to_string().as_bytes()).unwrap();
        assert_eq!(post(glm, &shown(&png_url())), 400);
        assert_eq!(post(flash, &shown(&png_url())), 200);
        let calls = ai.calls();
        assert_eq!(calls.len(), 2, "each call recorded, the refused one too");
        assert!(calls[1].model == flash && calls[1].body["messages"][0]["content"][1]["image_url"]["url"] == png_url().as_str());
    }
}
