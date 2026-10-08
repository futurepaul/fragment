//! OpenAI's Responses API (`POST /v1/responses`), a person's own provider
//! with their API key, or with their Sign in with ChatGPT tokens
//! (developers.openai.com/cookbook/articles/sign-in-with-chatgpt; its
//! preview's limits, developers.openai.com/siwc/token-sharing-open-source/
//! preview-limitations, read 2026-10-08): an OpenAI chat completion
//! translated to it (`request`), and its stream translated back (`Stream`).
//!
//! - **Input.** System and developer messages are the `instructions` (the
//!   preview refuses a system message item); a user's text and images are
//!   a `message` of `input_text` and `input_image`; an assistant's text an
//!   `output_text` message, each of its tool calls a `function_call`; each
//!   tool's result a `function_call_output`. Always `store: false` and
//!   `stream: true` (the preview requires both); no `previous_response_id`:
//!   every call carries its whole conversation, as a chat completion does.
//! - **Tools** are functions, `strict: false` (the Responses API's default
//!   is strict, which our schemas are not written for). Under Sign in with
//!   ChatGPT they go in a namespace (`fragment`: the preview takes function
//!   tools grouped so), and a call's `function_call` names it.
//! - **Not sent:** `max_output_tokens` under Sign in with ChatGPT (the
//!   preview refuses it, with `temperature`, `top_p` and the like), GLM's
//!   `reasoning_effort`, and the hints (`strip_hints`'s).
//! - **The stream** becomes OpenAI's chunks: text deltas, function calls
//!   (`call_id` and name when the item is added, then the arguments as
//!   they come), a finish reason (`tool_calls` when it called any, `length`
//!   or `content_filter` for an incomplete answer, else `stop`), then the
//!   usage (`input_tokens` with its `cached_tokens`; `output_tokens`) and
//!   `[DONE]`. `response.failed` and `error` are a failure: a usage limit
//!   is not passing, an unavailable one is.

use serde_json::{json, Map, Value};

use super::{text_of, Chunks, Counted, Failure, Lines, Translate, Untranslatable};

pub const HOST: &str = "api.openai.com";
/// The namespace Sign in with ChatGPT's function tools go in.
pub const NAMESPACE: &str = "fragment";
/// Tool calls one answer is read for (`models::TOOL_CALLS_MAX`).
const CALLS_MAX: usize = crate::models::TOOL_CALLS_MAX;

/// The codes of a failure that may pass, tried again later
/// (developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery).
const PASSING: [&str; 5] = ["server_error", "rate_limit_exceeded", "subscription_sharing_usage_unavailable", "subscription_sharing_user_unavailable", "overloaded"];

/// A user's content as Responses' parts.
fn user_parts(content: &Value) -> Result<Vec<Value>, Untranslatable> {
    let mut out = vec![];
    match content {
        Value::String(s) if !s.is_empty() => out.push(json!({ "type": "input_text", "text": s })),
        Value::Array(parts) => {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => {
                        let text = part["text"].as_str().unwrap_or("");
                        if !text.is_empty() {
                            out.push(json!({ "type": "input_text", "text": text }));
                        }
                    }
                    Some("image_url") => {
                        let url = part["image_url"]["url"].as_str().or(part["image_url"].as_str()).ok_or_else(|| Untranslatable("an image_url part names its url".into()))?;
                        out.push(json!({ "type": "input_image", "image_url": url, "detail": "auto" }));
                    }
                    other => return Err(Untranslatable(format!("a content part of type {other:?} has no Responses part"))),
                }
            }
        }
        _ => {}
    }
    Ok(out)
}

/// The Responses API's request for `input`, a chat completion bounded for
/// the route, on `model`. `chatgpt`: under Sign in with ChatGPT's preview
/// (tools in a namespace, no `max_output_tokens`).
pub fn request(input: &Value, model: &str, chatgpt: bool) -> Result<Value, Untranslatable> {
    let mut instructions: Vec<String> = vec![];
    let mut items: Vec<Value> = vec![];
    for m in input["messages"].as_array().into_iter().flatten() {
        match m["role"].as_str() {
            Some("system" | "developer") => {
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    instructions.push(text);
                }
            }
            Some("user") => {
                let parts = user_parts(&m["content"])?;
                if !parts.is_empty() {
                    items.push(json!({ "type": "message", "role": "user", "content": parts }));
                }
            }
            Some("assistant") => {
                let text = text_of(&m["content"]);
                if !text.is_empty() {
                    items.push(json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": text }] }));
                }
                for call in m["tool_calls"].as_array().into_iter().flatten() {
                    let args = call["function"]["arguments"].as_str().filter(|a| !a.trim().is_empty()).unwrap_or("{}");
                    let mut item = json!({ "type": "function_call", "call_id": call["id"], "name": call["function"]["name"], "arguments": args });
                    if chatgpt {
                        item["namespace"] = json!(NAMESPACE);
                    }
                    items.push(item);
                }
            }
            Some("tool") => items.push(json!({ "type": "function_call_output", "call_id": m["tool_call_id"], "output": text_of(&m["content"]) })),
            other => return Err(Untranslatable(format!("a message's role is system, developer, user, assistant or tool, not {other:?}"))),
        }
    }
    if items.is_empty() {
        return Err(Untranslatable("a call to OpenAI has a message besides its instructions".into()));
    }
    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    if !instructions.is_empty() {
        out.insert("instructions".into(), json!(instructions.join("\n\n")));
    }
    out.insert("input".into(), Value::Array(items));
    let functions: Vec<Value> = input["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let f = &t["function"];
            let mut tool = json!({ "type": "function", "name": f["name"], "parameters": if f["parameters"].is_object() { f["parameters"].clone() } else { json!({ "type": "object", "properties": {} }) }, "strict": false });
            if let Some(d) = f["description"].as_str() {
                tool["description"] = json!(d);
            }
            tool
        })
        .collect();
    if !functions.is_empty() {
        let tools = match chatgpt {
            true => json!([{ "type": "namespace", "name": NAMESPACE, "description": "The tools of this conversation.", "tools": functions }]),
            false => Value::Array(functions),
        };
        out.insert("tools".into(), tools);
        let choice = match &input["tool_choice"] {
            Value::String(s) if matches!(s.as_str(), "none" | "auto" | "required") => Some(json!(s)),
            Value::Object(o) => o.get("function").and_then(|f| f["name"].as_str()).map(|n| json!({ "type": "function", "name": n })),
            _ => None,
        };
        if let Some(c) = choice {
            out.insert("tool_choice".into(), c);
        }
    }
    if !chatgpt {
        if let Some(n) = input["max_tokens"].as_u64() {
            out.insert("max_output_tokens".into(), json!(n));
        }
    }
    out.insert("store".into(), json!(false));
    out.insert("stream".into(), json!(true));
    Ok(Value::Object(out))
}

/// A refusal's body and status as a failure: OpenAI's `{error: {code,
/// type, message}}`, or the direct route's admission `{detail}`.
pub fn failure_of(status: u16, body: &[u8]) -> Failure {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let e = &v["error"];
    let kind = e["code"].as_str().or(e["type"].as_str()).or(e.as_str()).unwrap_or("error").to_string();
    let message = e["message"].as_str().or(v["detail"].as_str()).map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(500).collect());
    Failure { passing: (matches!(status, 429 | 500..=599) && kind != "subscription_sharing_usage_limit_exceeded") || PASSING.contains(&kind.as_str()), kind, message }
}

/// A function call of the answer, as it streams.
#[derive(Debug)]
struct Call {
    /// Its output item's index.
    item: u64,
    args: bool,
}

/// The Responses API's stream, read as OpenAI's (the module's doc).
#[derive(Debug, Default)]
pub struct Stream {
    lines: Lines,
    chunks: Chunks,
    calls: Vec<Call>,
    started: bool,
    counted: Option<Counted>,
    failure: Option<Failure>,
    done: bool,
}

impl Stream {
    pub fn new(model: &str) -> Stream {
        Stream { chunks: Chunks { id: String::new(), model: model.to_string() }, ..Stream::default() }
    }

    fn start(&mut self, response: &Value, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        self.chunks.id = response["id"].as_str().unwrap_or("resp").to_string();
        out.extend(self.chunks.chunk(json!({ "role": "assistant", "content": "" }), None).as_bytes());
    }

    fn call_of(&mut self, item: Option<u64>) -> Option<(usize, &mut Call)> {
        let item = item?;
        self.calls.iter_mut().enumerate().find(|(_, c)| c.item == item)
    }

    fn args(&mut self, item: Option<u64>, args: &str, out: &mut Vec<u8>) {
        let chunks = std::mem::take(&mut self.chunks);
        if let Some((k, call)) = self.call_of(item) {
            if !args.is_empty() {
                call.args = true;
                out.extend(chunks.chunk(json!({ "tool_calls": [{ "index": k, "function": { "arguments": args } }] }), None).as_bytes());
            }
        }
        self.chunks = chunks;
    }

    fn end(&mut self, response: &Value, finish: &str, out: &mut Vec<u8>) {
        let u = &response["usage"];
        if u.is_object() {
            let input = u["input_tokens"].as_u64().unwrap_or(0);
            let cached = u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0).min(input);
            self.counted = Some(Counted { input: input - cached, cached, cache_write: 0, output: u["output_tokens"].as_u64().unwrap_or(0) });
        }
        let finish = if finish == "stop" && !self.calls.is_empty() { "tool_calls" } else { finish };
        out.extend(self.chunks.chunk(json!({}), Some(finish)).as_bytes());
        if let Some(c) = &self.counted {
            out.extend(self.chunks.usage(c).as_bytes());
        }
        out.extend(Chunks::DONE.as_bytes());
        self.done = true;
    }

    fn fail(&mut self, e: &Value, out: &mut Vec<u8>) {
        let kind = e["code"].as_str().or(e["type"].as_str()).unwrap_or("error").to_string();
        let f = Failure { passing: PASSING.contains(&kind.as_str()), message: e["message"].as_str().unwrap_or("the model's stream failed").to_string(), kind };
        out.extend(self.chunks.error(&f).as_bytes());
        self.failure = Some(f);
    }

    fn event(&mut self, data: &str, out: &mut Vec<u8>) {
        if self.done || self.failure.is_some() {
            return;
        }
        let Ok(e) = serde_json::from_str::<Value>(data) else { return };
        let item = e["output_index"].as_u64();
        match e["type"].as_str() {
            Some("response.created" | "response.in_progress") => self.start(&e["response"], out),
            Some("response.output_item.added") => {
                self.start(&e["response"], out);
                let it = &e["item"];
                if it["type"] == "function_call" && self.calls.len() < CALLS_MAX {
                    let k = self.calls.len();
                    let args = it["arguments"].as_str().unwrap_or("");
                    self.calls.push(Call { item: item.unwrap_or(k as u64), args: !args.is_empty() });
                    let delta = json!({ "tool_calls": [{ "index": k, "id": it["call_id"], "type": "function", "function": { "name": it["name"], "arguments": args } }] });
                    out.extend(self.chunks.chunk(delta, None).as_bytes());
                }
            }
            Some("response.output_text.delta" | "response.refusal.delta") => {
                if let Some(t) = e["delta"].as_str().filter(|t| !t.is_empty()) {
                    self.start(&Value::Null, out);
                    out.extend(self.chunks.chunk(json!({ "content": t }), None).as_bytes());
                }
            }
            Some("response.function_call_arguments.delta") => self.args(item, e["delta"].as_str().unwrap_or(""), out),
            Some("response.function_call_arguments.done") => {
                if self.call_of(item).is_some_and(|(_, c)| !c.args) {
                    let args = e["arguments"].as_str().filter(|a| !a.is_empty()).unwrap_or("{}").to_string();
                    self.args(item, &args, out);
                }
            }
            Some("response.output_item.done") => {
                if e["item"]["type"] == "function_call" && self.call_of(item).is_some_and(|(_, c)| !c.args) {
                    let args = e["item"]["arguments"].as_str().filter(|a| !a.is_empty()).unwrap_or("{}").to_string();
                    self.args(item, &args, out);
                }
            }
            Some("response.completed") => self.end(&e["response"], "stop", out),
            Some("response.incomplete") => {
                let finish = match e["response"]["incomplete_details"]["reason"].as_str() {
                    Some("content_filter") => "content_filter",
                    _ => "length",
                };
                self.end(&e["response"], finish, out);
            }
            Some("response.failed") => {
                let err = e["response"]["error"].clone();
                self.fail(&err, out);
            }
            Some("error") => {
                let err = if e["error"].is_object() { e["error"].clone() } else { e.clone() };
                self.fail(&err, out);
            }
            _ => {}
        }
    }
}

impl Translate for Stream {
    fn push(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        let mut lines = std::mem::take(&mut self.lines);
        lines.push(chunk, |d| self.event(d, out));
        self.lines = lines;
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        let mut lines = std::mem::take(&mut self.lines);
        lines.finish(|d| self.event(d, out));
        self.lines = lines;
    }

    fn done(&self) -> bool {
        self.done
    }

    fn counted(&self) -> Option<Counted> {
        self.counted.filter(|_| self.done && !self.lines.overlong())
    }

    fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Stream as OpenAiStream;

    fn turn() -> Value {
        json!({
            "max_tokens": 16384, "stream": true, "reasoning_effort": "low",
            "messages": [
                { "role": "system", "content": "You are Mind." },
                { "role": "user", "content": [{ "type": "text", "text": "<chat>\n</chat>", "cache": "blocks" }, { "type": "text", "text": "what did I say?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo=" } }] },
                { "role": "assistant", "content": "Looking.", "thinking_blocks": [{ "type": "thinking" }],
                  "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "zoom", "arguments": "{\"id\":3}" } }] },
                { "role": "tool", "tool_call_id": "call_1", "content": "3+0|user: tomatoes" },
                { "role": "developer", "content": "Be brief." },
            ],
            "tools": [{ "type": "function", "function": { "name": "zoom", "description": "Open a line", "parameters": { "type": "object" } } }],
            "tool_choice": "auto",
        })
    }

    /// Goal: a chat completion is the Responses API's request: the system
    /// and developer messages its instructions, the user's parts, the
    /// assistant's text and call, the result, the tools not strict, store
    /// off and streamed, and with an API key its output cap. Method: a
    /// mind's turn translated.
    #[test]
    fn a_turn_translates_with_a_key() {
        let r = request(&turn(), "gpt-5", false).unwrap();
        assert_eq!(r["instructions"], "You are Mind.\n\nBe brief.");
        assert_eq!((r["store"].clone(), r["stream"].clone(), r["max_output_tokens"].clone()), (json!(false), json!(true), json!(16384)));
        let input = r["input"].as_array().unwrap();
        assert_eq!(input[0]["content"][0], json!({ "type": "input_text", "text": "<chat>\n</chat>" }), "no hint");
        assert_eq!(input[0]["content"][2], json!({ "type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo=", "detail": "auto" }));
        assert_eq!(input[1], json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Looking." }] }));
        assert_eq!(input[2], json!({ "type": "function_call", "call_id": "call_1", "name": "zoom", "arguments": "{\"id\":3}" }));
        assert_eq!(input[3], json!({ "type": "function_call_output", "call_id": "call_1", "output": "3+0|user: tomatoes" }));
        assert_eq!(input.len(), 4, "the developer message is an instruction");
        assert_eq!(r["tools"], json!([{ "type": "function", "name": "zoom", "description": "Open a line", "parameters": { "type": "object" }, "strict": false }]));
        assert_eq!(r["tool_choice"], "auto");
        for absent in ["reasoning_effort", "messages", "previous_response_id", "temperature", "thinking_blocks"] {
            assert!(r.get(absent).is_none() && !r.to_string().contains("thinking"), "{absent}");
        }
    }

    /// Goal: under Sign in with ChatGPT the preview's rules hold: the tools
    /// in a namespace, a call naming it, and no output cap. Method: the
    /// same turn, as ChatGPT's.
    #[test]
    fn a_turn_translates_for_chatgpt() {
        let r = request(&turn(), "gpt-6.1-sol", true).unwrap();
        assert!(r.get("max_output_tokens").is_none());
        assert_eq!(r["tools"][0]["type"], "namespace");
        assert_eq!(r["tools"][0]["name"], NAMESPACE);
        assert_eq!(r["tools"][0]["tools"][0]["name"], "zoom");
        assert_eq!(r["input"][2]["namespace"], NAMESPACE);
        let named = request(&json!({ "messages": [{ "role": "user", "content": "x" }], "tools": [{ "type": "function", "function": { "name": "f" } }], "tool_choice": { "type": "function", "function": { "name": "f" } } }), "m", true).unwrap();
        assert_eq!(named["tool_choice"], json!({ "type": "function", "name": "f" }));
        assert!(request(&json!({ "messages": [{ "role": "system", "content": "only" }] }), "m", false).is_err());
        assert!(request(&json!({ "messages": [{ "role": "user", "content": [{ "type": "file" }] }] }), "m", false).is_err());
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect()
    }

    /// An answer that says something and calls two tools, streamed as the
    /// Responses API streams it (with a reasoning item first).
    fn answer() -> String {
        sse(&[
            json!({ "type": "response.created", "response": { "id": "resp_1", "status": "in_progress" } }),
            json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "reasoning", "id": "rs_1" } }),
            json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "reasoning", "id": "rs_1" } }),
            json!({ "type": "response.output_item.added", "output_index": 1, "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] } }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "Let me " }),
            json!({ "type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "look." }),
            json!({ "type": "response.output_item.added", "output_index": 2, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_a", "name": "zoom", "namespace": "fragment", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_1", "delta": "{\"id\":" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_1", "delta": "3}" }),
            json!({ "type": "response.function_call_arguments.done", "output_index": 2, "item_id": "fc_1", "arguments": "{\"id\":3}" }),
            json!({ "type": "response.output_item.added", "output_index": 3, "item": { "type": "function_call", "id": "fc_2", "call_id": "call_b", "name": "date", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.done", "output_index": 3, "item_id": "fc_2", "arguments": "{\"id\":1}" }),
            json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed",
                "usage": { "input_tokens": 1000, "input_tokens_details": { "cached_tokens": 896 }, "output_tokens": 40, "output_tokens_details": { "reasoning_tokens": 12 } } } }),
        ])
    }

    /// Goal: the Responses stream reads as OpenAI's however its bytes are
    /// cut: the text, both calls (one whose arguments came only whole), the
    /// finish reason, the usage with its cached input, as the ledger reads
    /// it. Method: the answer in pieces of every size, folded.
    #[test]
    fn a_stream_translates() {
        let text = answer();
        for n in [1, 5, 64, text.len()] {
            let mut t = Stream::new("gpt-5");
            let mut out = vec![];
            for piece in text.as_bytes().chunks(n) {
                t.push(piece, &mut out);
            }
            t.finish(&mut out);
            assert!(t.done() && t.failure().is_none());
            let mut folded = OpenAiStream::answering();
            folded.push(&out, None);
            let a = folded.answer().unwrap();
            assert_eq!((a.content.as_str(), a.finish_reason.as_deref()), ("Let me look.", Some("tool_calls")), "pieces of {n}");
            assert_eq!(a.tool_calls.len(), 2);
            assert_eq!((a.tool_calls[0].id.as_str(), a.tool_calls[0].function.name.as_str(), a.tool_calls[0].function.arguments.as_str()), ("call_a", "zoom", "{\"id\":3}"));
            assert_eq!((a.tool_calls[1].id.as_str(), a.tool_calls[1].function.arguments.as_str()), ("call_b", "{\"id\":1}"));
            assert_eq!(t.counted(), Some(Counted { input: 104, cached: 896, cache_write: 0, output: 40 }));
            assert_eq!(crate::models::usage_of("gpt-5", folded.usage().unwrap()), Some(t.counted().unwrap().usage("gpt-5")));
            assert!(t.thinking_blocks().is_empty());
        }
    }

    /// Goal: the other ends: an incomplete answer is `length`, a failure
    /// (a usage limit, not passing; unavailable, passing) leaves no usage,
    /// and a stream cut before its end is not done. Method: each.
    #[test]
    fn other_ends() {
        let run = |events: &[Value]| {
            let mut t = Stream::new("m");
            let mut out = vec![];
            t.push(sse(events).as_bytes(), &mut out);
            t.finish(&mut out);
            let mut folded = OpenAiStream::answering();
            folded.push(&out, None);
            (t, folded.answer().cloned().unwrap())
        };
        let created = json!({ "type": "response.created", "response": { "id": "r" } });
        let said = json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "par" });
        let (t, a) = run(&[created.clone(), said.clone(), json!({ "type": "response.incomplete", "response": { "incomplete_details": { "reason": "max_output_tokens" }, "usage": { "input_tokens": 5, "output_tokens": 9 } } })]);
        assert_eq!((a.content.as_str(), a.finish_reason.as_deref(), t.counted().map(|c| c.output)), ("par", Some("length"), Some(9)));
        let (t, a) = run(&[created.clone(), said.clone(), json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 5, "output_tokens": 2 } } })]);
        assert_eq!(a.finish_reason.as_deref(), Some("stop"));
        assert!(t.done());
        let limit = json!({ "type": "response.failed", "response": { "error": { "code": "subscription_sharing_usage_limit_exceeded", "message": "limit" } } });
        let (t, a) = run(&[created.clone(), said.clone(), limit]);
        assert_eq!(t.failure().map(|f| (f.kind.as_str(), f.passing)), Some(("subscription_sharing_usage_limit_exceeded", false)));
        assert!(a.finish_reason.is_none() && t.counted().is_none() && !t.done());
        let (t, _) = run(&[created.clone(), json!({ "type": "error", "code": "subscription_sharing_usage_unavailable", "message": "later" })]);
        assert!(t.failure().unwrap().passing);
        let (t, a) = run(&[created, said]);
        assert!(!t.done() && a.finish_reason.is_none());
        assert!(!failure_of(429, br#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"m"}}"#).passing, "a usage limit waits for the person");
        assert!(failure_of(503, br#"{"detail":"direct routing unavailable"}"#).passing);
        let f = failure_of(401, br#"{"detail":"bad token"}"#);
        assert_eq!((f.message.as_str(), f.passing), ("bad token", false));
    }
}
