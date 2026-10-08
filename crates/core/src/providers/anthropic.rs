//! Anthropic's Messages API (`POST /v1/messages`), a person's own provider
//! with their API key: an OpenAI chat completion translated to it
//! (`request`), and its stream translated back (`Stream`).
//!
//! - **Messages.** System and developer messages are the `system` blocks;
//!   a user's text and images (a `data:` URL as base64, else by URL) are
//!   its blocks; an assistant's thinking blocks (`thinking_blocks`, as its
//!   answer carried them), text and tool calls (`tool_use`, the arguments
//!   parsed) are its blocks; each tool's result is a `tool_result` in the
//!   user's turn that follows. Turns of one role in a row are one turn.
//! - **Tools** are `{name, description, input_schema}`; `tool_choice`
//!   `none`, `auto`, `required` (Anthropic's `any`) or a function (`tool`).
//! - **Cache marks** (UniiChat §3.3): one on the last system block (the
//!   tools render before it, so the mark caches both), one on the last
//!   whole block of each text part hinted `cache: "blocks"` (`blocks`: the
//!   view in blocks of 4 lines; Anthropic looks back 20 blocks from a mark
//!   for an earlier entry, so the next call, its view a few lines longer,
//!   reads this one's), and one at the request's end; at most 4.
//! - **Not sent:** `temperature`, `top_p` and the like (newer models refuse
//!   them), GLM's `reasoning_effort`, and `thinking` (each model's default:
//!   adaptive on those that think, its text omitted).
//! - **The stream** becomes OpenAI's chunks: text deltas, tool calls (id
//!   and name first, then their arguments' JSON as it comes; `{}` for one
//!   with none), a finish reason (`end_turn` and `stop_sequence` are
//!   `stop`, `tool_use` `tool_calls`, `max_tokens` `length`, `refusal`
//!   `content_filter`), then the usage (`message_start`'s input, cache read
//!   and write; `message_delta`'s output) and `[DONE]`. Thinking blocks are
//!   kept whole (`thinking_blocks`) and go on the finish chunk's delta.

use serde_json::{json, Map, Value};

use super::{text_of, Chunks, Counted, Failure, Lines, Translate, Untranslatable, CACHE_BLOCKS, CACHE_HINT, THINKING_BLOCKS};

pub const HOST: &str = "api.anthropic.com";
/// The API version every call names (`anthropic-version`).
pub const VERSION: &str = "2023-06-01";
/// A hinted text's lines a block (UniiChat §3.3: with 16-line blocks, up to
/// 15 lines went unread on each call).
pub const BLOCK_LINES: usize = 4;
/// Cache marks one request carries, at most (Anthropic's limit).
pub const MARKS_MAX: usize = 4;
/// Tool calls one answer is read for (`models::TOOL_CALLS_MAX`).
const CALLS_MAX: usize = crate::models::TOOL_CALLS_MAX;
/// Thinking blocks one answer keeps, at most.
const THINKING_MAX: usize = 256;

fn mark() -> Value {
    json!({ "type": "ephemeral" })
}

/// A growing text in blocks of `BLOCK_LINES` lines, and its last whole
/// block, the one a cache mark goes on. Every line but the last goes in
/// blocks from the first; the last block takes what is left and the last
/// line, which may change at the next call (the view's `</chat>` moves as
/// lines are appended before it). Joined, the blocks are the text.
pub fn blocks(text: &str) -> (Vec<String>, Option<usize>) {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.len() <= 1 {
        return (vec![text.to_string()], None);
    }
    let whole = (lines.len() - 1) / BLOCK_LINES;
    let mut out: Vec<String> = lines[..whole * BLOCK_LINES].chunks(BLOCK_LINES).map(|c| c.concat()).collect();
    out.push(lines[whole * BLOCK_LINES..].concat());
    assert_eq!(out.concat(), text, "the blocks are the text");
    (out, whole.checked_sub(1))
}

/// A tool call's id as Anthropic takes one (`^[a-zA-Z0-9_-]+$`): another
/// character is `_`, the same for a call and its result.
fn tool_id(id: &str) -> String {
    let id: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if id.is_empty() {
        "call".into()
    } else {
        id
    }
}

/// An image part as Anthropic's image block.
fn image(part: &Value) -> Result<Value, Untranslatable> {
    let url = part["image_url"]["url"].as_str().or(part["image_url"].as_str()).ok_or_else(|| Untranslatable("an image_url part names its url".into()))?;
    if let Some((head, data)) = url.strip_prefix("data:").and_then(|r| r.split_once(',')) {
        let media = head.strip_suffix(";base64").ok_or_else(|| Untranslatable("an image's data: URL is base64".into()))?;
        return Ok(json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": data } }));
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return Ok(json!({ "type": "image", "source": { "type": "url", "url": url } }));
    }
    Err(Untranslatable("an image is a data: URL or an http(s) one".into()))
}

/// A user's content as blocks, and the blocks a cache mark goes on (the
/// last whole block of each hinted text).
fn user_blocks(content: &Value) -> Result<(Vec<Value>, Vec<usize>), Untranslatable> {
    let mut out = vec![];
    let mut marks = vec![];
    match content {
        Value::String(s) if !s.trim().is_empty() => out.push(json!({ "type": "text", "text": s })),
        Value::Array(parts) => {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => {
                        let text = part["text"].as_str().unwrap_or("");
                        if text.trim().is_empty() {
                            continue;
                        }
                        if part[CACHE_HINT] == CACHE_BLOCKS {
                            let (bs, whole) = blocks(text);
                            let at = out.len();
                            out.extend(bs.into_iter().filter(|b| !b.is_empty()).map(|b| json!({ "type": "text", "text": b })));
                            if let Some(w) = whole {
                                marks.push(at + w);
                            }
                        } else {
                            out.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    Some("image_url") => out.push(image(part)?),
                    other => return Err(Untranslatable(format!("a content part of type {other:?} has no Anthropic block"))),
                }
            }
        }
        _ => {}
    }
    Ok((out, marks))
}

/// An assistant's message as blocks: its thinking blocks as its answer
/// carried them, its text, its tool calls.
fn assistant_blocks(m: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = m[THINKING_BLOCKS].as_array().into_iter().flatten().filter(|b| matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking"))).cloned().collect();
    let text = text_of(&m["content"]);
    if !text.trim().is_empty() {
        out.push(json!({ "type": "text", "text": text }));
    }
    for call in m["tool_calls"].as_array().into_iter().flatten() {
        let raw = call["function"]["arguments"].as_str().unwrap_or("{}");
        let input = match serde_json::from_str::<Value>(if raw.trim().is_empty() { "{}" } else { raw }) {
            Ok(v @ Value::Object(_)) => v,
            // what the model wrote that is no object, kept for it to see
            _ => json!({ "arguments": raw }),
        };
        out.push(json!({ "type": "tool_use", "id": tool_id(call["id"].as_str().unwrap_or("")), "name": call["function"]["name"], "input": input }));
    }
    out
}

/// Appends `blocks` to the conversation as `role`'s, to the turn before
/// when it is that role's; `marks` are indices into `blocks`.
fn append(messages: &mut Vec<(String, Vec<Value>)>, role: &str, blocks: Vec<Value>, marks: &[usize], marked: &mut Vec<(usize, usize)>) {
    if blocks.is_empty() {
        return;
    }
    if messages.last().is_none_or(|(r, _)| r != role) {
        messages.push((role.to_string(), vec![]));
    }
    let at = messages.len() - 1;
    let offset = messages[at].1.len();
    marked.extend(marks.iter().map(|m| (at, offset + m)));
    messages[at].1.extend(blocks);
}

/// The Messages API's request for `input`, a chat completion bounded for
/// the route (`models::bound`: its `max_tokens` capped), on `model`.
pub fn request(input: &Value, model: &str) -> Result<Value, Untranslatable> {
    let max_tokens = input["max_tokens"].as_u64().filter(|n| *n >= 1).ok_or_else(|| Untranslatable("a call names its max_tokens".into()))?;
    let mut system: Vec<Value> = vec![];
    let mut messages: Vec<(String, Vec<Value>)> = vec![];
    let mut marked: Vec<(usize, usize)> = vec![];
    for m in input["messages"].as_array().into_iter().flatten() {
        match m["role"].as_str() {
            Some("system" | "developer") => {
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    system.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("user") => {
                let (blocks, marks) = user_blocks(&m["content"])?;
                append(&mut messages, "user", blocks, &marks, &mut marked);
            }
            Some("assistant") => append(&mut messages, "assistant", assistant_blocks(m), &[], &mut marked),
            Some("tool") => {
                let mut result = json!({ "type": "tool_result", "tool_use_id": tool_id(m["tool_call_id"].as_str().unwrap_or("")) });
                let text = text_of(&m["content"]);
                if !text.is_empty() {
                    result["content"] = json!(text);
                }
                append(&mut messages, "user", vec![result], &[], &mut marked);
            }
            other => return Err(Untranslatable(format!("a message's role is system, developer, user, assistant or tool, not {other:?}"))),
        }
    }
    if messages.is_empty() {
        return Err(Untranslatable("a call to Anthropic has a user's message".into()));
    }
    // a conversation starts with the user's turn
    if messages[0].0 != "user" {
        messages.insert(0, ("user".into(), vec![json!({ "type": "text", "text": "(the conversation so far)" })]));
        for m in &mut marked {
            m.0 += 1;
        }
    }
    // the marks: the system's, the hinted texts' last ones, the request's end
    let mut marks = 0;
    if let Some(last) = system.last_mut() {
        last["cache_control"] = mark();
        marks += 1;
    }
    let ends_at = messages.len() - 1;
    let end = messages[ends_at].1.iter().rposition(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking"))).map(|b| (ends_at, b));
    let room = MARKS_MAX - marks - usize::from(end.is_some());
    let skip = marked.len().saturating_sub(room);
    for &(m, b) in marked.iter().skip(skip) {
        messages[m].1[b]["cache_control"] = mark();
    }
    if let Some((m, b)) = end {
        messages[m].1[b]["cache_control"] = mark();
    }
    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("max_tokens".into(), json!(max_tokens));
    if !system.is_empty() {
        out.insert("system".into(), json!(system));
    }
    out.insert("messages".into(), Value::Array(messages.into_iter().map(|(role, content)| json!({ "role": role, "content": content })).collect()));
    let tools: Vec<Value> = input["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let f = &t["function"];
            let mut tool = json!({ "name": f["name"], "input_schema": if f["parameters"].is_object() { f["parameters"].clone() } else { json!({ "type": "object", "properties": {} }) } });
            if let Some(d) = f["description"].as_str() {
                tool["description"] = json!(d);
            }
            tool
        })
        .collect();
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
        let choice = match &input["tool_choice"] {
            Value::String(s) if s == "none" => Some(json!({ "type": "none" })),
            Value::String(s) if s == "required" => Some(json!({ "type": "any" })),
            Value::String(s) if s == "auto" => Some(json!({ "type": "auto" })),
            Value::Object(o) => o.get("function").and_then(|f| f["name"].as_str()).map(|n| json!({ "type": "tool", "name": n })),
            _ => None,
        };
        if let Some(c) = choice {
            out.insert("tool_choice".into(), c);
        }
    }
    let stops: Vec<&str> = match &input["stop"] {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let stops: Vec<&str> = stops.into_iter().filter(|s| !s.trim().is_empty()).collect();
    if !stops.is_empty() {
        out.insert("stop_sequences".into(), json!(stops));
    }
    out.insert("stream".into(), json!(true));
    Ok(Value::Object(out))
}

/// A refusal's body (`{type: "error", error: {type, message}}`) and status
/// as a failure: a rate limit, an overload or a server's error may pass.
pub fn failure_of(status: u16, body: &[u8]) -> Failure {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let kind = v["error"]["type"].as_str().unwrap_or("error").to_string();
    let message = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(500).collect());
    Failure { passing: matches!(status, 429 | 500..=599) || matches!(kind.as_str(), "overloaded_error" | "rate_limit_error" | "api_error"), kind, message }
}

/// A content block of the answer, as it streams.
#[derive(Debug)]
enum Block {
    Text,
    /// A tool call: its index among the answer's calls, and whether its
    /// arguments came.
    Tool { call: usize, args: bool },
    Thinking(Value),
    Other,
}

/// Anthropic's stream, read as OpenAI's (the module's doc).
#[derive(Debug, Default)]
pub struct Stream {
    lines: Lines,
    chunks: Chunks,
    blocks: Vec<Block>,
    calls: usize,
    thinking: Vec<Value>,
    counted: Option<Counted>,
    stop: Option<String>,
    failure: Option<Failure>,
    done: bool,
}

impl Stream {
    pub fn new(model: &str) -> Stream {
        Stream { chunks: Chunks { id: String::new(), model: model.to_string() }, ..Stream::default() }
    }

    fn event(&mut self, data: &str, out: &mut Vec<u8>) {
        if self.done || self.failure.is_some() {
            return;
        }
        let Ok(e) = serde_json::from_str::<Value>(data) else { return };
        match e["type"].as_str() {
            Some("message_start") => {
                let m = &e["message"];
                self.chunks.id = m["id"].as_str().unwrap_or("msg").to_string();
                self.count(&m["usage"]);
                out.extend(self.chunks.chunk(json!({ "role": "assistant", "content": "" }), None).as_bytes());
            }
            Some("content_block_start") => {
                let b = &e["content_block"];
                let block = match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str().filter(|t| !t.is_empty()) {
                            out.extend(self.chunks.chunk(json!({ "content": t }), None).as_bytes());
                        }
                        Block::Text
                    }
                    Some("tool_use") if self.calls < CALLS_MAX => {
                        let call = self.calls;
                        self.calls += 1;
                        let delta = json!({ "tool_calls": [{ "index": call, "id": b["id"], "type": "function", "function": { "name": b["name"], "arguments": "" } }] });
                        out.extend(self.chunks.chunk(delta, None).as_bytes());
                        Block::Tool { call, args: false }
                    }
                    Some("thinking") => Block::Thinking(json!({ "type": "thinking", "thinking": b["thinking"].as_str().unwrap_or(""), "signature": b["signature"].as_str().unwrap_or("") })),
                    Some("redacted_thinking") => {
                        if self.thinking.len() < THINKING_MAX {
                            self.thinking.push(b.clone());
                        }
                        Block::Other
                    }
                    _ => Block::Other,
                };
                let index = e["index"].as_u64().map_or(self.blocks.len(), |i| i as usize);
                if index == self.blocks.len() {
                    self.blocks.push(block);
                }
            }
            Some("content_block_delta") => {
                let d = &e["delta"];
                let Some(block) = e["index"].as_u64().and_then(|i| self.blocks.get_mut(i as usize)) else { return };
                match (block, d["type"].as_str()) {
                    (Block::Text, Some("text_delta")) => {
                        if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) {
                            out.extend(self.chunks.chunk(json!({ "content": t }), None).as_bytes());
                        }
                    }
                    (Block::Tool { call, args }, Some("input_json_delta")) => {
                        if let Some(p) = d["partial_json"].as_str().filter(|p| !p.is_empty()) {
                            *args = true;
                            let delta = json!({ "tool_calls": [{ "index": *call, "function": { "arguments": p } }] });
                            out.extend(self.chunks.chunk(delta, None).as_bytes());
                        }
                    }
                    (Block::Thinking(t), Some("thinking_delta")) => {
                        let so_far = t["thinking"].as_str().unwrap_or("").to_string();
                        t["thinking"] = json!(so_far + d["thinking"].as_str().unwrap_or(""));
                    }
                    (Block::Thinking(t), Some("signature_delta")) => {
                        let so_far = t["signature"].as_str().unwrap_or("").to_string();
                        t["signature"] = json!(so_far + d["signature"].as_str().unwrap_or(""));
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => match e["index"].as_u64().and_then(|i| self.blocks.get_mut(i as usize)) {
                Some(Block::Tool { call, args: false }) => {
                    let delta = json!({ "tool_calls": [{ "index": *call, "function": { "arguments": "{}" } }] });
                    out.extend(self.chunks.chunk(delta, None).as_bytes());
                }
                Some(Block::Thinking(t)) => {
                    let t = std::mem::take(t);
                    if self.thinking.len() < THINKING_MAX {
                        self.thinking.push(t);
                    }
                }
                _ => {}
            },
            Some("message_delta") => {
                if let Some(s) = e["delta"]["stop_reason"].as_str() {
                    self.stop = Some(s.to_string());
                }
                self.count(&e["usage"]);
            }
            Some("message_stop") => {
                let finish = match self.stop.as_deref() {
                    Some("tool_use") => "tool_calls",
                    Some("max_tokens" | "model_context_window_exceeded") => "length",
                    Some("refusal") => "content_filter",
                    _ => "stop",
                };
                let mut delta = json!({});
                if !self.thinking.is_empty() {
                    delta[THINKING_BLOCKS] = json!(self.thinking);
                }
                out.extend(self.chunks.chunk(delta, Some(finish)).as_bytes());
                if let Some(c) = &self.counted {
                    out.extend(self.chunks.usage(c).as_bytes());
                }
                out.extend(Chunks::DONE.as_bytes());
                self.done = true;
            }
            Some("error") => {
                let f = Failure {
                    kind: e["error"]["type"].as_str().unwrap_or("error").to_string(),
                    message: e["error"]["message"].as_str().unwrap_or("the model's stream failed").to_string(),
                    passing: matches!(e["error"]["type"].as_str(), Some("overloaded_error" | "rate_limit_error" | "api_error")),
                };
                out.extend(self.chunks.error(&f).as_bytes());
                self.failure = Some(f);
            }
            _ => {}
        }
    }

    /// A usage's counts, as far as it says them (`message_start`'s input,
    /// `message_delta`'s output, cumulative).
    fn count(&mut self, usage: &Value) {
        if !usage.is_object() {
            return;
        }
        let c = self.counted.get_or_insert_with(Counted::default);
        let n = |k: &str| usage[k].as_u64();
        c.input = n("input_tokens").unwrap_or(c.input);
        c.cached = n("cache_read_input_tokens").unwrap_or(c.cached);
        c.cache_write = n("cache_creation_input_tokens").unwrap_or(c.cache_write);
        c.output = n("output_tokens").unwrap_or(c.output);
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

    fn thinking_blocks(&self) -> &[Value] {
        &self.thinking
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Stream as OpenAiStream;

    /// Goal: a growing text's blocks keep its last line out of every whole
    /// block, and join back to the text. Method: views of 1 to 12 lines.
    #[test]
    fn a_view_in_blocks_of_four_lines() {
        let view = |n: usize| {
            let mut s = String::from("<chat>\n");
            for i in 0..n {
                s += &format!("{i}+1|line {i}\n");
            }
            s + "</chat>"
        };
        assert_eq!(blocks("one line"), (vec!["one line".to_string()], None));
        // 3 lines: <chat>, 1 line, </chat>: no whole block
        let (b, w) = blocks(&view(1));
        assert_eq!((b.len(), w), (1, None));
        // 5 lines: the first 4 are a whole block, </chat> the tail
        let (b, w) = blocks(&view(3));
        assert_eq!((b.len(), w, b[1].as_str()), (2, Some(0), "</chat>"));
        assert_eq!(b[0], "<chat>\n0+1|line 0\n1+1|line 1\n2+1|line 2\n");
        for n in 0..12 {
            let v = view(n);
            let (b, w) = blocks(&v);
            assert_eq!(b.concat(), v);
            let total = n + 2;
            assert_eq!(w, ((total - 1) / BLOCK_LINES).checked_sub(1), "{n} lines");
            assert!(b.iter().take(b.len() - 1).all(|x| x.matches('\n').count() == BLOCK_LINES), "every block but the tail is 4 lines");
            assert!(b.last().unwrap().ends_with("</chat>"));
        }
        // the next call's view, a line longer, keeps this one's marked block
        let (now, w) = blocks(&view(10));
        let (next, _) = blocks(&view(11));
        assert_eq!(now[..=w.unwrap()], next[..=w.unwrap()], "a marked block is the same in the next call");
    }

    /// A mind's turn, as templates/mind sends it: the system prompt, the
    /// view hinted, the turn's state and words, then a tool call and its
    /// result.
    fn turn() -> Value {
        let view = format!("<chat>\n{}</chat>", (0..9).map(|i| format!("{i}+1|line {i}\n")).collect::<String>());
        json!({
            "max_tokens": 16384, "stream": true, "reasoning_effort": "low", "stream_options": { "include_usage": true },
            "messages": [
                { "role": "system", "content": "You are Mind." },
                { "role": "user", "content": [{ "type": "text", "text": view, "cache": "blocks" }, { "type": "text", "text": "Now: 2026-10-08" }, { "type": "text", "text": "what did I say?" }] },
                { "role": "assistant", "content": "", "thinking_blocks": [{ "type": "thinking", "thinking": "", "signature": "sig" }],
                  "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "zoom", "arguments": "{\"id\":3,\"n\":1}" } }] },
                { "role": "tool", "tool_call_id": "call_1", "content": "3+0|user: tomatoes" },
            ],
            "tools": [{ "type": "function", "function": { "name": "zoom", "description": "Open a line", "parameters": { "type": "object", "properties": { "id": { "type": "integer" } } } } },
                      { "type": "function", "function": { "name": "date" } }],
            "tool_choice": "auto",
        })
    }

    /// Goal: a chat completion is the Messages API's request: the system
    /// its blocks, the view in blocks, the tool call and its result,
    /// thinking blocks back as they came, the tools and their choice, and
    /// nothing Anthropic refuses. Method: a mind's turn translated.
    #[test]
    fn a_turn_translates() {
        let r = request(&turn(), "claude-sonnet-5-5").unwrap();
        assert_eq!((r["model"].clone(), r["max_tokens"].clone(), r["stream"].clone()), (json!("claude-sonnet-5-5"), json!(16384), json!(true)));
        for absent in ["reasoning_effort", "stream_options", "temperature", "thinking"] {
            assert!(r.get(absent).is_none(), "{absent}");
        }
        assert_eq!(r["system"], json!([{ "type": "text", "text": "You are Mind.", "cache_control": { "type": "ephemeral" } }]));
        let m = r["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3);
        let user = m[0]["content"].as_array().unwrap();
        // 11 lines: two whole blocks of 4 and a tail of 3, then the state and the words
        assert_eq!(user.len(), 5);
        assert_eq!(user[0]["text"], "<chat>\n0+1|line 0\n1+1|line 1\n2+1|line 2\n");
        assert_eq!(user[1]["cache_control"], json!({ "type": "ephemeral" }), "the view's last whole block is marked");
        assert!(user[0].get("cache_control").is_none() && user[2].get("cache_control").is_none() && user[4].get("cache_control").is_none());
        assert!(user.iter().all(|b| b.get("cache").is_none()), "the hint is not sent");
        assert_eq!(m[1]["role"], "assistant");
        assert_eq!(m[1]["content"][0], json!({ "type": "thinking", "thinking": "", "signature": "sig" }), "thinking first, as it came");
        assert_eq!(m[1]["content"][1], json!({ "type": "tool_use", "id": "call_1", "name": "zoom", "input": { "id": 3, "n": 1 } }));
        assert_eq!(m[2]["content"][0]["type"], "tool_result");
        assert_eq!((m[2]["content"][0]["tool_use_id"].clone(), m[2]["content"][0]["content"].clone()), (json!("call_1"), json!("3+0|user: tomatoes")));
        assert_eq!(m[2]["content"][0]["cache_control"], json!({ "type": "ephemeral" }), "the request's end is marked");
        let marks = r.to_string().matches("cache_control").count();
        assert_eq!(marks, 3, "the system, the view, the end");
        assert_eq!(r["tools"][0], json!({ "name": "zoom", "description": "Open a line", "input_schema": { "type": "object", "properties": { "id": { "type": "integer" } } } }));
        assert_eq!(r["tools"][1]["input_schema"], json!({ "type": "object", "properties": {} }));
        assert_eq!(r["tool_choice"], json!({ "type": "auto" }));
    }

    /// Goal: the other shapes a caller sends translate, and what Anthropic
    /// has no way to say is refused, nothing sent. Method: goose-like
    /// calls: a string content, an image, results in a row, a named tool,
    /// `none`, arguments that are no object, and an audio part.
    #[test]
    fn other_shapes() {
        let png = "data:image/png;base64,iVBORw0KGgo=";
        let r = request(
            &json!({ "max_tokens": 100, "stop": ["END", ""], "messages": [
                { "role": "assistant", "content": "earlier" },
                { "role": "user", "content": [{ "type": "image_url", "image_url": { "url": png } }, { "type": "text", "text": "  " }] },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "a.1", "type": "function", "function": { "name": "f", "arguments": "" } },
                    { "id": "b", "type": "function", "function": { "name": "g", "arguments": "[1]" } }] },
                { "role": "tool", "tool_call_id": "a.1", "content": "one" },
                { "role": "tool", "tool_call_id": "b", "content": "" },
                { "role": "user", "content": "and now?" },
            ], "tools": [{ "type": "function", "function": { "name": "f" } }], "tool_choice": { "type": "function", "function": { "name": "f" } } }),
            "claude-haiku-4-5",
        )
        .unwrap();
        let m = r["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"][0]["text"], "(the conversation so far)", "a conversation starts with the user");
        assert_eq!(m[2]["content"][0]["source"], json!({ "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=" }));
        assert_eq!(m[2]["content"].as_array().unwrap().len(), 1, "a blank text is no block");
        assert_eq!(m[3]["content"][0]["input"], json!({}), "no arguments: an empty object");
        assert_eq!(m[3]["content"][0]["id"], "a_1");
        assert_eq!(m[3]["content"][1]["input"], json!({ "arguments": "[1]" }));
        let results = m[4]["content"].as_array().unwrap();
        assert_eq!(results.len(), 3, "both results and the words after them are one user turn");
        assert_eq!((results[0]["tool_use_id"].clone(), results[1].get("content"), results[2]["text"].clone()), (json!("a_1"), None, json!("and now?")));
        assert_eq!(r["tool_choice"], json!({ "type": "tool", "name": "f" }));
        assert_eq!(r["stop_sequences"], json!(["END"]));
        assert!(r.get("system").is_none());
        let none = request(&json!({ "max_tokens": 9, "messages": [{ "role": "user", "content": "x" }], "tools": [{ "type": "function", "function": { "name": "f" } }], "tool_choice": "none" }), "m").unwrap();
        assert_eq!(none["tool_choice"], json!({ "type": "none" }));
        assert_eq!(none["messages"][0]["content"][0]["cache_control"], json!({ "type": "ephemeral" }), "the end is marked with no system");
        let audio = json!({ "max_tokens": 9, "messages": [{ "role": "user", "content": [{ "type": "input_audio", "input_audio": {} }] }] });
        assert!(request(&audio, "m").unwrap_err().message().contains("input_audio"));
        assert!(request(&json!({ "max_tokens": 9, "messages": [{ "role": "system", "content": "s" }] }), "m").is_err(), "no user's message");
        assert!(request(&json!({ "max_tokens": 9, "messages": [{ "role": "robot", "content": "s" }] }), "m").is_err());
    }

    /// Goal: at most four marks, the newest views kept. Method: a call with
    /// three hinted texts and a system prompt.
    #[test]
    fn at_most_four_marks() {
        let view = "a\nb\nc\nd\ne\nf";
        let parts: Vec<Value> = (0..3).map(|_| json!({ "type": "text", "text": view, "cache": "blocks" })).collect();
        let r = request(&json!({ "max_tokens": 9, "messages": [{ "role": "system", "content": "s" }, { "role": "user", "content": parts }, { "role": "user", "content": "end" }] }), "m").unwrap();
        assert_eq!(r.to_string().matches("cache_control").count(), MARKS_MAX);
        let content = r["messages"][0]["content"].as_array().unwrap();
        assert!(content[0].get("cache_control").is_none(), "the first view's mark went: the newest are kept");
        assert!(content[2].get("cache_control").is_some() && content[4].get("cache_control").is_some());
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect()
    }

    /// An answer that thinks, says something, and calls a tool, streamed as
    /// Anthropic streams it.
    fn answer() -> String {
        sse(&[
            json!({ "type": "message_start", "message": { "id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": "claude-sonnet-5-5",
                "usage": { "input_tokens": 12, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 80, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig-1" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "ping" }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Let me " } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "look." } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "zoom", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"id\": " } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "3}" } }),
            json!({ "type": "content_block_stop", "index": 2 }),
            json!({ "type": "content_block_start", "index": 3, "content_block": { "type": "tool_use", "id": "toolu_2", "name": "date", "input": {} } }),
            json!({ "type": "content_block_stop", "index": 3 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "output_tokens": 57 } }),
            json!({ "type": "message_stop" }),
        ])
    }

    /// Goal: Anthropic's stream reads as OpenAI's however its bytes are cut:
    /// the text, the tool calls (one with no arguments `{}`), the finish
    /// reason, the thinking blocks whole, and the usage with its cache read
    /// and write, metered as the ledger reads it. Method: the answer fed in
    /// pieces of every size, its OpenAI lines folded by `models::Stream`.
    #[test]
    fn a_stream_translates() {
        let text = answer();
        for n in [1, 3, 17, 256, text.len()] {
            let mut t = Stream::new("claude-sonnet-5-5");
            let mut out = vec![];
            for piece in text.as_bytes().chunks(n) {
                t.push(piece, &mut out);
            }
            t.finish(&mut out);
            assert!(t.done() && t.failure().is_none());
            let mut folded = OpenAiStream::answering();
            folded.push(&out, None);
            assert!(folded.done(), "[DONE] ends it");
            let a = folded.answer().unwrap();
            assert_eq!((a.content.as_str(), a.finish_reason.as_deref()), ("Let me look.", Some("tool_calls")), "pieces of {n}");
            assert_eq!(a.tool_calls.len(), 2);
            assert_eq!((a.tool_calls[0].id.as_str(), a.tool_calls[0].function.name.as_str(), a.tool_calls[0].function.arguments.as_str()), ("toolu_1", "zoom", "{\"id\": 3}"));
            assert_eq!(a.tool_calls[1].function.arguments, "{}");
            assert_eq!(t.thinking_blocks(), [json!({ "type": "thinking", "thinking": "", "signature": "sig-1" })]);
            let counted = t.counted().unwrap();
            assert_eq!(counted, Counted { input: 12, cached: 900, cache_write: 80, output: 57 });
            let usage = crate::models::usage_of("claude-sonnet-5-5", folded.usage().unwrap());
            assert_eq!(usage, Some(counted.usage("claude-sonnet-5-5")), "the ledger's reading of the usage chunk");
            // the thinking blocks ride on the finish chunk, for a route's client
            let finish = String::from_utf8(out.clone()).unwrap().lines().find(|l| l.contains("\"finish_reason\":\"tool_calls\"")).unwrap().to_string();
            assert!(finish.contains("thinking_blocks") && finish.contains("sig-1"));
            let message = super::super::message_of(a, t.thinking_blocks());
            assert_eq!(message["thinking_blocks"][0]["signature"], "sig-1");
            // and the next call sends them back as they came
            let next = request(&json!({ "max_tokens": 9, "messages": [{ "role": "user", "content": "q" }, message, { "role": "tool", "tool_call_id": "toolu_1", "content": "r" }, { "role": "tool", "tool_call_id": "toolu_2", "content": "d" }] }), "m").unwrap();
            assert_eq!(next["messages"][1]["content"][0]["signature"], "sig-1");
            assert_eq!(next["messages"][1]["content"][1], json!({ "type": "text", "text": "Let me look." }));
        }
    }

    /// Goal: the other ends: a text answer stops; one cut by max_tokens is
    /// `length`; a refusal `content_filter`; an error mid-stream is a
    /// failure (an overload passing) and no usage; a stream that ends
    /// before `message_stop` is not done. Method: each, translated.
    #[test]
    fn other_ends() {
        let run = |events: &[Value]| {
            let mut t = Stream::new("m");
            let mut out = vec![];
            t.push(sse(events).as_bytes(), &mut out);
            t.finish(&mut out);
            let mut folded = OpenAiStream::answering();
            folded.push(&out, None);
            (t, folded.answer().cloned().unwrap(), String::from_utf8(out).unwrap())
        };
        let start = json!({ "type": "message_start", "message": { "id": "m1", "usage": { "input_tokens": 5, "output_tokens": 1 } } });
        let text = |s: &str| [json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": s } }), json!({ "type": "content_block_stop", "index": 0 })];
        let end = |reason: &str| [json!({ "type": "message_delta", "delta": { "stop_reason": reason }, "usage": { "output_tokens": 3 } }), json!({ "type": "message_stop" })];
        for (reason, finish) in [("end_turn", "stop"), ("stop_sequence", "stop"), ("max_tokens", "length"), ("refusal", "content_filter")] {
            let events: Vec<Value> = [start.clone()].into_iter().chain(text("hi")).chain(end(reason)).collect();
            let (t, a, _) = run(&events);
            assert_eq!((a.content.as_str(), a.finish_reason.as_deref()), ("hi", Some(finish)), "{reason}");
            assert_eq!(t.counted(), Some(Counted { input: 5, cached: 0, cache_write: 0, output: 3 }));
            assert!(t.thinking_blocks().is_empty());
        }
        let overloaded = json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } });
        let events: Vec<Value> = [start.clone()].into_iter().chain(text("par")).chain([overloaded]).chain(end("end_turn")).collect();
        let (t, a, out) = run(&events);
        assert_eq!(t.failure().map(|f| (f.kind.as_str(), f.passing)), Some(("overloaded_error", true)));
        assert!(!t.done() && t.counted().is_none() && a.finish_reason.is_none(), "nothing after a failure");
        assert!(out.contains("\"error\":{\"code\":\"overloaded_error\""));
        let events: Vec<Value> = [start].into_iter().chain(text("cut")).collect();
        let (t, a, _) = run(&events);
        assert!(!t.done() && a.finish_reason.is_none() && t.counted().is_none(), "no message_stop: not whole");
        assert!(failure_of(529, br#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#).passing);
        let bad = failure_of(400, br#"{"type":"error","error":{"type":"invalid_request_error","message":"no"}}"#);
        assert_eq!((bad.kind.as_str(), bad.message.as_str(), bad.passing), ("invalid_request_error", "no", false));
        assert!(!failure_of(401, b"not json").passing);
    }
}
