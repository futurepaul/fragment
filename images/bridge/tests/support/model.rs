//! A scripted model behind `FRAGMENT_MODEL` (lesson 13: a pure function of
//! the transcript), in OpenAI's chat-completions shape, streamed or not:
//!
//! - the answer is `scripted: <the last user message's text>`;
//! - a last user message asking to `use the terminal`, with no tool result
//!   yet, is answered with a `terminal` tool call (`echo tool-ran`; `use the
//!   terminal slowly`: `sleep 8` first), and one saying `risky` with one
//!   Hermes flags (`rm -rf …`); once a tool result is in the transcript, the
//!   answer names it;
//! - Hermes' smart-approval guardian is answered `ESCALATE`, so a person is
//!   asked.
//!
//! It records each request's `model` and `x-fragment-agent`.

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

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// The answer for a transcript: `(text, tool call)`.
pub fn answer(body: &Value) -> (String, Option<Value>) {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last_user = messages.iter().rev().find(|m| m["role"] == "user").map(|m| text_of(&m["content"])).unwrap_or_default();
    let tool_result = messages.iter().rev().take_while(|m| m["role"] != "user").find(|m| m["role"] == "tool").map(|m| text_of(&m["content"]));
    let has_terminal = body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "terminal"));
    if let Some(result) = tool_result {
        let ran = if result.contains("tool-ran") { "the tool ran" } else { "the tool said something else" };
        return (format!("scripted: {ran}"), None);
    }
    // Hermes' smart-approval guardian asks for one word: a person decides.
    if last_user.contains("Respond with exactly one word: APPROVE, DENY, or ESCALATE") {
        return ("ESCALATE".into(), None);
    }
    let command = if last_user.contains("risky") {
        Some("rm -rf /tmp/fragment-risky && echo tool-ran")
    } else if last_user.contains("use the terminal slowly") {
        Some("sleep 8 && echo tool-ran")
    } else if last_user.contains("use the terminal") {
        Some("echo tool-ran")
    } else {
        None
    };
    if let (Some(command), true) = (command, has_terminal) {
        let call = json!({ "index": 0, "id": "call_1", "type": "function", "function": { "name": "terminal", "arguments": json!({ "command": command }).to_string() } });
        return (String::new(), Some(call));
    }
    // The first line of what the user said: Hermes appends its own notes
    // (a first contact's introduction) after it.
    let said = last_user.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    (format!("scripted: {said}"), None)
}

async fn handle(req: Request<Incoming>, calls: Arc<Mutex<Vec<Call>>>) -> Response<Body> {
    let path = req.uri().path().to_string();
    let agent = req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()).map(str::to_string);
    if path.ends_with("/models") {
        return net::json_answer(StatusCode::OK, &json!({ "object": "list", "data": [{ "id": "cheap", "object": "model" }, { "id": "medium", "object": "model" }, { "id": "high", "object": "model" }] }));
    }
    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let stream = v["stream"] == json!(true);
    calls.lock().unwrap().push(Call { path: path.clone(), model: v["model"].as_str().unwrap_or("").into(), agent, stream });
    if !path.ends_with("/chat/completions") {
        return net::refusal(StatusCode::NOT_FOUND, "not_found", "the scripted model answers /v1/chat/completions");
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
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] hi there" }] }));
    assert_eq!((t.as_str(), call), ("scripted: [paul] hi there", None));
    let tools = json!([{ "type": "function", "function": { "name": "terminal" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "please use the terminal" }], "tools": tools }));
    assert!(call.is_some());
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "please use the terminal" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "tool-ran\n" }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: the tool ran", None));
}
