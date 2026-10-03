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
//! Replies are scripted (text, tool calls, nothing, or reasoning alone) and
//! answered in order before falling back to an echo of the last message.
//! Levers: the calls made (with the gateway metadata the cell would send),
//! failures queued for the next calls, a delay, the usage the next answers
//! report (`set_usage`), and a stream cut before its usage (`break_next`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// A scripted model reply.
#[derive(Clone, Debug)]
pub enum Reply {
    Text(String),
    /// Tool calls: (name, arguments).
    Tools(Vec<(String, Value)>),
    /// No text and no tool call.
    Empty,
    /// Reasoning, and nothing else.
    Thinking(String),
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
    let mut parts: Vec<(Value, u64)> = Vec::new();
    let mut cut = false;
    let finish = match reply {
        Reply::Text(text) => {
            let (text, was_cut) = within(text, &mut budget);
            cut = was_cut;
            parts.push((json!({ "content": text }), 0));
            "stop"
        }
        Reply::Empty => "stop",
        Reply::Thinking(text) => {
            parts.push((json!({ "reasoning_content": text }), 0));
            "stop"
        }
        Reply::Tools(calls) => {
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
                    parts.push((json!({ "tool_calls": [call] }), 0));
                }
            }
            "tool_calls"
        }
    };
    // the completion's tokens, spread over its chunks as deltas
    let n = parts.len().max(1) as u64;
    for (i, (delta, _)) in parts.iter().enumerate() {
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
    let last = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].clone()).unwrap_or(Value::Null);
    let scripted = s.script.pop_front();
    let used = s.usage.pop_front();
    s.sleep_ms = s.delays.pop_front().unwrap_or(0);
    let broken = s.breaks.pop_front().unwrap_or(false);
    if body["stream"] == true {
        let reply = scripted.unwrap_or_else(|| Reply::Text(format!("echo: {}", last.as_str().unwrap_or(""))));
        let written = match &reply {
            Reply::Text(t) | Reply::Thinking(t) => t.chars().count(),
            Reply::Tools(calls) => calls.iter().map(|(n, a)| n.len() + a.to_string().len()).sum(),
            Reply::Empty => 0,
        };
        let budget = body["max_tokens"].as_u64().map(|t| t as usize * CHARS_PER_TOKEN);
        let events = stream(&model, &reply, &mut s.tool_calls, usage_of(used, &body, written), budget, broken);
        return Response::bytes(200, "text/event-stream", events.into_bytes()).with_header("cf-aig-log-id", &log_id);
    }
    let content = match scripted {
        Some(Reply::Text(text)) => text,
        _ => format!("echo: {}", last.as_str().unwrap_or("")),
    };
    let used = usage_of(used, &body, content.chars().count());
    let answer = json!({
        "id": format!("fake{:08x}", s.answers), "object": "chat.completion", "created": 0, "model": model,
        "choices": [{ "index": 0, "finish_reason": "stop", "logprobs": null, "message": { "role": "assistant", "content": content } }],
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
}
