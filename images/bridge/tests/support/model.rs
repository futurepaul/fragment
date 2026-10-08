//! A scripted model behind `FRAGMENT_MODEL` (lesson 13: a pure function of
//! the transcript), in OpenAI's chat-completions shape, streamed or not.
//! What it acts on is the last line of the last user message, a runtime's
//! own context (goose's `<turn-context>`) left out:
//!
//! - `run: <command>` is a call of the shell tool offered (`shell`, or an
//!   extension's `…__shell`) with that command;
//! - `zoom: <id> <n>` is a call of the zoom tool offered (a mind's, through
//!   `fragment mcp`);
//! - `call: <tool> <JSON arguments>` is a call of the tool offered named
//!   `<tool>` (or an extension's `…__<tool>`) with those arguments;
//! - once a tool's result is in the transcript, the answer quotes it:
//!   `scripted: the tool said: <result>`;
//! - anything else is answered `scripted: <the line>`.
//!
//! `/v1/decide` is Clef as the platform's fake decides (crates/fakes): a
//! `choice` is the first option the state names as a word, else the first;
//! a `noul` is 0.9 when a word of its question over 3 letters is in the
//! state, else 0.1.
//!
//! It records each request's path, `model` and `x-fragment-agent`.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::watch;

use fragment_bridge::net::{self, Body};

#[derive(Default, Debug, Clone)]
pub struct Call {
    pub path: String,
    pub model: String,
    pub agent: Option<String>,
    pub stream: bool,
    /// The request as it came (a failure's detail: what the model was given).
    pub body: Value,
}

pub struct Model {
    pub addr: SocketAddr,
    pub calls: Arc<Mutex<Vec<Call>>>,
    _stop: watch::Sender<bool>,
}

impl Model {
    pub async fn start(bind: &str) -> Model {
        let listener = tokio::net::TcpListener::bind(bind).await.expect("the model listens");
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (stop, rx) = watch::channel(false);
        let c = calls.clone();
        let handler = move |req: Request<Incoming>, _peer: SocketAddr| {
            let c = c.clone();
            async move { handle(req, c).await }
        };
        tokio::spawn(net::serve(listener, handler, rx));
        Model { addr, calls, _stop: stop }
    }
}

pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The text of a request's messages of `role`, joined.
pub fn texts(body: &Value, role: &str) -> String {
    body["messages"].as_array().map(|m| m.iter().filter(|m| m["role"] == role).map(|m| text_of(&m["content"])).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

/// The names of the tools a request offers.
pub fn tools(body: &Value) -> Vec<String> {
    body["tools"].as_array().map(|t| t.iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// A text with goose's `<turn-context>…</turn-context>` blocks left out.
fn without_context(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    // bounded by the text: each pass removes one block or ends
    while let Some(start) = rest.find("<turn-context>") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("</turn-context>") {
            Some(end) => &rest[start + end + "</turn-context>".len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// The tool offered whose name is `name` or ends with `__<name>`.
fn offered(body: &Value, name: &str) -> Option<String> {
    let suffix = format!("__{name}");
    body["tools"].as_array()?.iter().filter_map(|t| t["function"]["name"].as_str()).find(|n| *n == name || n.ends_with(&suffix)).map(str::to_string)
}

/// The answer for a transcript: `(text, tool call)`.
pub fn answer(body: &Value) -> (String, Option<Value>) {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last_user = messages.iter().rev().find(|m| m["role"] == "user").map(|m| without_context(&text_of(&m["content"]))).unwrap_or_default();
    let said = last_user.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string();
    let tool_result = messages.iter().rev().take_while(|m| m["role"] != "user").find(|m| m["role"] == "tool").map(|m| text_of(&m["content"]));
    if let Some(result) = tool_result {
        return (format!("scripted: the tool said: {}", result.trim().chars().take(4000).collect::<String>()), None);
    }
    let call = |name: String, args: Value| json!({ "index": 0, "id": "call_1", "type": "function", "function": { "name": name, "arguments": args.to_string() } });
    if let (Some(command), Some(shell)) = (said.strip_prefix("run: "), offered(body, "shell")) {
        return (String::new(), Some(call(shell, json!({ "command": command }))));
    }
    if let Some((tool, args)) = said.strip_prefix("call: ").and_then(|rest| rest.split_once(' ')) {
        if let (Some(name), Ok(args)) = (offered(body, tool), serde_json::from_str::<Value>(args)) {
            return (String::new(), Some(call(name, args)));
        }
    }
    if let (Some(at), Some(zoom)) = (said.strip_prefix("zoom: "), offered(body, "zoom")) {
        let n: Vec<u64> = at.split_whitespace().filter_map(|w| w.parse().ok()).collect();
        return (String::new(), Some(call(zoom, json!({ "id": n.first().copied().unwrap_or(0), "n": n.get(1).copied().unwrap_or(1) }))));
    }
    (format!("scripted: {said}"), None)
}

/// A decision's answers, as the platform's Clef fake gives them.
pub fn decide(input: &Value) -> Value {
    let state = match &input["state"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let words: Vec<String> = state.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    let named = |w: &str| words.iter().any(|x| x == w);
    let mut answers = serde_json::Map::new();
    for (id, q) in input["questions"].as_object().cloned().unwrap_or_default() {
        let a = match q["type"].as_str() {
            Some("choice") => {
                let options: Vec<String> = q["criteria"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                let chosen = options.iter().find(|o| named(&o.to_lowercase())).or(options.first()).cloned().unwrap_or_default();
                let probabilities: serde_json::Map<String, Value> = options.iter().map(|o| (o.clone(), json!(if *o == chosen { 0.9 } else { 0.0 }))).collect();
                json!({ "type": "choice", "choice": chosen, "probabilities": probabilities, "confidence": 0.8 })
            }
            _ => {
                let text = q["instructions"].as_str().unwrap_or("").to_lowercase();
                let yes = text.split(|c: char| !c.is_alphanumeric()).any(|w| w.chars().count() > 3 && named(w));
                json!({ "type": "noul", "noul": if yes { 0.9 } else { 0.1 } })
            }
        };
        answers.insert(id, a);
    }
    json!({ "answers": answers, "model": format!("@cf/cloudflare/{}", input["model"].as_str().unwrap_or("clef-flash")), "usage": { "input_tokens": 100, "output_tokens": 1 } })
}

async fn handle(req: Request<Incoming>, calls: Arc<Mutex<Vec<Call>>>) -> Response<Body> {
    let path = req.uri().path().to_string();
    let agent = req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()).map(str::to_string);
    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let stream = v["stream"] == json!(true);
    calls.lock().unwrap().push(Call { path: path.clone(), model: v["model"].as_str().unwrap_or("").into(), agent, stream, body: v.clone() });
    if path.ends_with("/v1/decide") {
        return net::json_answer(StatusCode::OK, &decide(&v));
    }
    // the platform's model route answers its completions and decisions alone (docs/computers.md, "Models")
    if !path.ends_with("/v1/chat/completions") {
        return net::refusal(StatusCode::NOT_FOUND, "not_found", "the scripted model answers /v1/chat/completions and /v1/decide");
    }
    let (text, tool) = answer(&v);
    let model = v["model"].as_str().unwrap_or("medium").to_string();
    let usage = json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 });
    let finish = if tool.is_some() { "tool_calls" } else { "stop" };
    if !stream {
        let mut message = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
        if let Some(t) = &tool {
            let mut t = t.clone();
            t.as_object_mut().unwrap().remove("index");
            message["tool_calls"] = json!([t]);
        }
        return net::json_answer(StatusCode::OK, &json!({ "id": "chatcmpl-scripted", "object": "chat.completion", "created": 0, "model": model, "choices": [{ "index": 0, "message": message, "finish_reason": finish }], "usage": usage }));
    }
    let chunk = |delta: Value, finish: Value| json!({ "id": "chatcmpl-scripted", "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] });
    let mut sse = String::new();
    sse.push_str(&format!("data: {}\n\n", chunk(json!({ "role": "assistant", "content": "" }), Value::Null)));
    if let Some(t) = tool {
        sse.push_str(&format!("data: {}\n\n", chunk(json!({ "tool_calls": [t] }), Value::Null)));
    } else {
        // In a few pieces, so a draft streams.
        let chars: Vec<char> = text.chars().collect();
        for piece in chars.chunks(chars.len().div_ceil(3).max(1)) {
            sse.push_str(&format!("data: {}\n\n", chunk(json!({ "content": piece.iter().collect::<String>() }), Value::Null)));
        }
    }
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage;
    sse.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
    Response::builder().status(StatusCode::OK).header("content-type", "text/event-stream").header("cache-control", "no-cache").body(http_body_util::Full::new(bytes::Bytes::from(sse))).unwrap()
}

#[test]
fn answers_are_the_transcripts() {
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "framing\n\nhi there" }] }));
    assert_eq!((t.as_str(), call), ("scripted: hi there", None));
    // goose's turn context is no part of what was said
    let noted = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello\n<turn-context>\n<current-time>now</current-time>\n</turn-context>" }] }] });
    assert_eq!(answer(&noted).0, "scripted: hello");
    let tools = json!([{ "type": "function", "function": { "name": "developer__shell" } }, { "type": "function", "function": { "name": "mind__zoom" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "run: echo hi" }], "tools": tools }));
    let call = call.unwrap();
    assert_eq!((call["function"]["name"].as_str(), call["function"]["arguments"].as_str()), (Some("developer__shell"), Some(r#"{"command":"echo hi"}"#)));
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "zoom: 4 2" }], "tools": tools }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "id": 4, "n": 2 }).to_string());
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "run: echo hi" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "hi\n" }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: the tool said: hi", None));
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "call: web_read {\"url\": \"https://example.com\"}" }], "tools": [{ "type": "function", "function": { "name": "web__web_read" } }] }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "url": "https://example.com" }).to_string());
    let d = decide(&json!({ "model": "clef", "state": "click the thing in cell 50", "questions": { "cell": { "type": "choice", "instructions": "which?", "criteria": { "1": null, "50": null } } } }));
    assert_eq!(d["answers"]["cell"]["choice"], "50");
    // no such tool offered: answered in words
    assert_eq!(answer(&json!({ "messages": [{ "role": "user", "content": "run: echo hi" }] })).0, "scripted: run: echo hi");
}
