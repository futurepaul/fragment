//! A scripted model behind `FRAGMENT_MODEL` (lesson 13: a pure function of
//! the transcript), in OpenAI's chat-completions shape, streamed or not:
//!
//! - the answer is `scripted: <the last user message's text>`;
//! - a last user message asking to `use the terminal`, with no tool result
//!   yet, is answered with a `terminal` tool call (`echo tool-ran`; `use the
//!   terminal slowly`: `sleep 8` first), and one saying `risky` with one
//!   Hermes flags (`rm -rf …`); once a tool result is in the transcript, the
//!   answer names it;
//! - `browse: <url>` is a `browser_navigate` call, and `look at your screen`
//!   a `computer_use` capture (through Hermes' `tool_call` bridge when it
//!   defers the tool); their answers quote what the tool said;
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
    let offered = |name: &str| body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == name));
    let has_terminal = offered("terminal");
    // `run: <command>` on a line of what the user said: that command, and an
    // answer that quotes what it printed
    let run = last_user.lines().find_map(|l| l.split_once("run: ").map(|(_, c)| c.trim().to_string())).filter(|c| !c.is_empty());
    // `browse: <url>`: the browser tool goes there; `look at your screen`:
    // computer_use captures it. Each answer quotes what its tool said.
    let browse = last_user.lines().find_map(|l| l.split_once("browse: ").map(|(_, u)| u.trim().to_string())).filter(|u| !u.is_empty());
    let look = last_user.contains("look at your screen");
    if let Some(result) = tool_result {
        if run.is_some() || browse.is_some() || look {
            return (format!("scripted: the tool said: {}", result.chars().take(4000).collect::<String>()), None);
        }
        let ran = if result.contains("tool-ran") { "the tool ran" } else { "the tool said something else" };
        return (format!("scripted: {ran}"), None);
    }
    // Hermes' smart-approval guardian asks for one word: a person decides.
    if last_user.contains("Respond with exactly one word: APPROVE, DENY, or ESCALATE") {
        return ("ESCALATE".into(), None);
    }
    let command = if let Some(c) = run.as_deref() {
        Some(c)
    } else if last_user.contains("risky") {
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
    let call = |name: &str, args: Value| json!({ "index": 0, "id": "call_1", "type": "function", "function": { "name": name, "arguments": args.to_string() } });
    // Hermes defers computer_use behind its tool_search bridge: listed in
    // tool_search's description, invoked through tool_call
    let deferred = |name: &str| offered("tool_call") && body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "tool_search" && t["function"]["description"].as_str().is_some_and(|d| d.contains(name))));
    let capture = json!({ "action": "capture", "mode": "vision", "app": "screen" });
    match (browse, look) {
        (Some(url), _) if offered("browser_navigate") => return (String::new(), Some(call("browser_navigate", json!({ "url": url })))),
        (Some(_), _) => return ("scripted: no browser_navigate among my tools".into(), None),
        (None, true) if offered("computer_use") => return (String::new(), Some(call("computer_use", capture))),
        (None, true) if deferred("computer_use") => return (String::new(), Some(call("tool_call", json!({ "calls": [{ "name": "computer_use", "arguments": capture }] })))),
        (None, true) => return ("scripted: no computer_use among my tools".into(), None),
        _ => {}
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
    calls.lock().unwrap().push(Call { path: path.clone(), model: v["model"].as_str().unwrap_or("").into(), agent, stream, body: v.clone() });
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
    // `run:` runs its command, and the answer quotes what it printed
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] run: fragment list --json" }], "tools": tools }));
    assert!(call.is_some_and(|c| c["function"]["arguments"].as_str().unwrap().contains("fragment list --json")));
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] run: fragment list" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "skills.paul (editor)" }], "tools": tools }));
    assert_eq!(t, "scripted: the tool said: skills.paul (editor)");
    // the browser, and computer_use directly or behind Hermes' tool_search
    let browser = json!([{ "type": "function", "function": { "name": "browser_navigate" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] browse: https://example.com" }], "tools": browser }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "url": "https://example.com" }).to_string());
    let bridged = json!([{ "type": "function", "function": { "name": "tool_search", "description": "… computer_use: Background desktop control …" } }, { "type": "function", "function": { "name": "tool_call" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] look at your screen" }], "tools": bridged }));
    let call = call.unwrap();
    assert!(call["function"]["name"] == "tool_call" && call["function"]["arguments"].as_str().unwrap().contains("\"computer_use\""), "{call}");
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] look at your screen" }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: no computer_use among my tools", None));
}
