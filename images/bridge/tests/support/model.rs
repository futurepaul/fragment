//! A scripted model behind `FRAGMENT_MODEL` (lesson 13: a pure function of
//! the transcript), in OpenAI's chat-completions shape, streamed or not:
//!
//! - the answer is `scripted: <the last user message's text>`;
//! - a last user message asking to `use the terminal`, with no tool result
//!   yet, is answered with a `terminal` tool call (`echo tool-ran` after
//!   `sleep 2`; `use the terminal slowly`: after `sleep 8`; `use the
//!   terminal twice`: `echo first-ran`, then that one), and one saying
//!   `risky` with one Hermes flags (`rm -rf …`); once a tool result is in
//!   the transcript, the answer names it; `run: <command>` runs that, and
//!   `start: <command>` starts it as a background process (Hermes refuses
//!   a foreground `&`), each answer quoting what the tool said; `send:
//!   <command>` runs that, and its answer sends the file the command names
//!   (`made=<path>` in what it printed) as Hermes' `MEDIA:` tag;
//! - `browse: <url>` is a `browser_navigate` call, `look at your screen`
//!   a `computer_use` capture, `write: <path>` a `write_file` of one line
//!   there, and `code: <python>` an `execute_code` of that line (each
//!   through Hermes' `tool_call` bridge when it defers the tool); their
//!   answers quote what the tool said;
//! - `dm: <teammate>: <message>` is a `message_agent` call (Hermes' Bot
//!   Mode, in a Bot Chat), and its answer quotes what the tool said; a
//!   teammate's reply carried back to it (a message holding another
//!   bot's `scripted: …` answer to a `Message from …`, not as its start: a
//!   delivery's completion) is answered `scripted: relayed: <that reply>`;
//! - Hermes' smart-approval guardian is answered `ESCALATE`, so a person is
//!   asked;
//! - of a message with channel context before it (`[Recent channel
//!   messages]\n…\n\n[New message]\n…`, the platform's note after a cut
//!   turn), only the message after `[New message]` is acted on.
//!
//! It records each request's `model` and `x-fragment-agent` (a screenshot's
//! description comes as the route's `vision`: Hermes' auxiliary vision).
//!
//! It answers transcriptions too, `POST /v1/audio/transcriptions` in
//! OpenAI's multipart shape, as the route's `whisper` does (decision 9):
//! the text is the words a memo (`memo`, a WAV) says in a chunk of its own,
//! `{"text": …}`. As the intercept does, it refuses one that names no agent,
//! by `x-fragment-agent` or by its key (`Authorization: Bearer
//! agent:<name>`), and records the key, the form's fields and the audio's
//! size.

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
    /// The request as it came (a failure's detail: what the model was given);
    /// a transcription's, its form's fields but the audio.
    pub body: Value,
    /// Its `authorization`, as it came.
    pub authorization: Option<String>,
}

/// A second of silence, as a WAV, that says `words` in a chunk of its own
/// (`said`): a voice memo (the stub's scripted runtime and the Workers AI
/// fake read it the same way).
pub fn memo(words: &str) -> Vec<u8> {
    fragment_bridge::runtime::script::memo(words)
}

/// What a memo says: its `said` chunk, or nothing.
fn said_in(audio: &[u8]) -> Option<String> {
    let at = audio.windows(4).position(|w| w == b"said")?;
    let len = u32::from_le_bytes(audio.get(at + 4..at + 8)?.try_into().ok()?) as usize;
    Some(String::from_utf8_lossy(audio.get(at + 8..at + 8 + len)?).into_owned())
}

/// A multipart form's parts, `(name, bytes)`, read as OpenAI's SDKs write
/// one (test support, not a parser of every form).
fn form_parts(content_type: &str, body: &[u8]) -> Vec<(String, Vec<u8>)> {
    let Some(boundary) = content_type.split("boundary=").nth(1).map(|b| b.trim_matches('"')) else { return vec![] };
    let delimiter = format!("--{boundary}");
    let mut parts = vec![];
    let text = body;
    let mut starts = vec![];
    let d = delimiter.as_bytes();
    let mut i = 0;
    while i + d.len() <= text.len() {
        if &text[i..i + d.len()] == d {
            starts.push(i);
            i += d.len();
        } else {
            i += 1;
        }
    }
    for w in starts.windows(2) {
        let part = &text[w[0] + d.len()..w[1]];
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let part = part.strip_suffix(b"\r\n").unwrap_or(part);
        let Some(split) = part.windows(4).position(|x| x == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&part[..split]).into_owned();
        let Some(name) = head.split("name=\"").nth(1).and_then(|n| n.split('"').next()) else { continue };
        parts.push((name.to_string(), part[split + 4..].to_vec()));
    }
    parts
}

/// A transcription, answered as the route's `whisper` answers it.
fn transcription(content_type: &str, body: &[u8], agent: &Option<String>, authorization: &Option<String>) -> (Value, Response<Body>) {
    let parts = form_parts(content_type, body);
    let field = |n: &str| parts.iter().find(|(k, _)| k == n).map(|(_, v)| String::from_utf8_lossy(v).into_owned());
    let audio = parts.iter().find(|(k, _)| k == "file").map(|(_, v)| v.clone()).unwrap_or_default();
    let fields = json!({ "model": field("model"), "language": field("language"), "prompt": field("prompt"), "response_format": field("response_format"), "audio_bytes": audio.len() });
    let keyed = authorization.as_deref().and_then(|a| a.strip_prefix("Bearer agent:"));
    if agent.is_none() && keyed.is_none() {
        return (fields, net::refusal(StatusCode::UNAUTHORIZED, "unauthenticated", "name the agent this call is for"));
    }
    if field("model").as_deref() != Some("whisper") {
        return (fields, net::refusal(StatusCode::BAD_REQUEST, "invalid", "a transcription's model is \"whisper\""));
    }
    let text = said_in(&audio).unwrap_or_else(|| "(no words)".into());
    let answer = match field("response_format").as_deref() {
        Some("text") => Response::builder().status(StatusCode::OK).header("content-type", "text/plain").body(http_body_util::Full::new(bytes::Bytes::from(text))).unwrap(),
        _ => net::json_answer(StatusCode::OK, &json!({ "text": text })),
    };
    (fields, answer)
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

/// What `write: <path>` has Hermes' write_file put there.
pub const WRITTEN: &str = "written by the agent\n";

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
    // Hermes renders an inbound's read-only context before the message it
    // comes with (`[Recent channel messages]\n…\n\n[New message]\n[name]
    // text`): the context is reference (the platform's note on a cut turn),
    // the message what it answers. A message joined after a cut request
    // (`[paul] do the risky thing\n\n[paul] good morning`) has no marker,
    // and is answered whole.
    let last_user = match last_user.rsplit_once("[New message]\n") {
        Some((_, message)) => message.to_string(),
        None => last_user,
    };
    let tool_result = messages.iter().rev().take_while(|m| m["role"] != "user").find(|m| m["role"] == "tool").map(|m| text_of(&m["content"]));
    let offered = |name: &str| body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == name));
    let has_terminal = offered("terminal");
    // `use the terminal twice`: a quick command, then, at once, one that
    // sleeps 2 s (the second's progress line comes within Hermes' 1.5 s edit
    // interval of the first's, as a real model's quick second call does),
    // then the answer
    let results = messages.iter().rev().take_while(|m| m["role"] != "user").filter(|m| m["role"] == "tool").count();
    if last_user.contains("use the terminal twice") && has_terminal && results < 2 {
        let command = if results == 0 { "echo first-ran" } else { "sleep 2 && echo tool-ran" };
        let call = json!({ "index": 0, "id": format!("call_{}", results + 1), "type": "function", "function": { "name": "terminal", "arguments": json!({ "command": command }).to_string() } });
        return (String::new(), Some(call));
    }
    // `run: <command>` on a line of what the user said: that command, and an
    // answer that quotes what it printed
    let run = last_user.lines().find_map(|l| l.split_once("run: ").map(|(_, c)| c.trim().to_string())).filter(|c| !c.is_empty());
    // `start: <command>`: that command as Hermes' background process
    let start = last_user.lines().find_map(|l| l.split_once("start: ").map(|(_, c)| c.trim().to_string())).filter(|c| !c.is_empty());
    // `send: <command>`: that command, then the file it names sent
    let send = last_user.lines().find_map(|l| l.split_once("send: ").map(|(_, c)| c.trim().to_string())).filter(|c| !c.is_empty());
    // `browse: <url>`: the browser tool goes there; `look at your screen`:
    // computer_use captures it. Each answer quotes what its tool said.
    let browse = last_user.lines().find_map(|l| l.split_once("browse: ").map(|(_, u)| u.trim().to_string())).filter(|u| !u.is_empty());
    let look = last_user.contains("look at your screen");
    // `write: <path>`: Hermes' write_file puts `WRITTEN` there
    let write = last_user.lines().find_map(|l| l.split_once("write: ").map(|(_, p)| p.trim().to_string())).filter(|p| !p.is_empty());
    // `code: <python>`: Hermes' execute_code runs that line
    let code = last_user.lines().find_map(|l| l.split_once("code: ").map(|(_, p)| p.trim().to_string())).filter(|p| !p.is_empty());
    // `dm: <teammate>: <message>`: Bot Mode's message_agent sends it
    let dm = last_user.lines().find_map(|l| l.split_once("dm: ").and_then(|(_, rest)| rest.split_once(": ")).map(|(t, m)| (t.trim().to_string(), m.trim().to_string()))).filter(|(t, m)| !t.is_empty() && !m.is_empty());
    // a teammate's reply, carried back to the bot that messaged it
    let relayed = last_user.find("scripted: ").filter(|at| *at > 0 && last_user.contains("Message from")).map(|at| {
        let rest = &last_user[at..];
        rest.split(['\n', '"']).next().unwrap_or(rest).trim_end_matches('\\').to_string()
    });
    if let Some(result) = tool_result {
        if send.is_some() {
            let made: String = result.split_once("made=").map(|(_, rest)| rest.chars().take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\\' | ',')).collect()).unwrap_or_default();
            return (format!("scripted: sent\nMEDIA:{made}"), None);
        }
        if run.is_some() || start.is_some() || browse.is_some() || look || write.is_some() || code.is_some() || dm.is_some() {
            return (format!("scripted: the tool said: {}", result.chars().take(4000).collect::<String>()), None);
        }
        let ran = if result.contains("tool-ran") { "the tool ran" } else { "the tool said something else" };
        return (format!("scripted: {ran}"), None);
    }
    // Hermes' smart-approval guardian asks for one word: a person decides.
    if last_user.contains("Respond with exactly one word: APPROVE, DENY, or ESCALATE") {
        return ("ESCALATE".into(), None);
    }
    if let Some(reply) = relayed {
        return (format!("scripted: relayed: {reply}"), None);
    }
    if let Some((target, message)) = dm {
        return if offered("message_agent") {
            let call = json!({ "index": 0, "id": "call_1", "type": "function", "function": { "name": "message_agent", "arguments": json!({ "target": target, "message": message }).to_string() } });
            (String::new(), Some(call))
        } else {
            ("scripted: no message_agent among my tools".into(), None)
        };
    }
    if let (Some(command), true) = (start.as_deref(), has_terminal) {
        let call = json!({ "index": 0, "id": "call_1", "type": "function", "function": { "name": "terminal", "arguments": json!({ "command": command, "background": true }).to_string() } });
        return (String::new(), Some(call));
    }
    let command = if let Some(c) = run.as_deref().or(send.as_deref()) {
        Some(c)
    } else if last_user.contains("risky") {
        Some("rm -rf /tmp/fragment-risky && echo tool-ran")
    } else if last_user.contains("use the terminal slowly") {
        Some("sleep 8 && echo tool-ran")
    } else if last_user.contains("use the terminal") {
        // Hermes sends a tool's progress line (its step) only if its turn
        // runs on past its 0.3 s progress poll (docs/technical-debt-ledger.md,
        // "A quick tool's step can be lost in Hermes")
        Some("sleep 2 && echo tool-ran")
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
    if let Some(path) = write {
        let args = json!({ "path": path, "content": WRITTEN });
        return if offered("write_file") {
            (String::new(), Some(call("write_file", args)))
        } else if deferred("write_file") {
            (String::new(), Some(call("tool_call", json!({ "calls": [{ "name": "write_file", "arguments": args }] }))))
        } else {
            ("scripted: no write_file among my tools".into(), None)
        };
    }
    if let Some(code) = code {
        let args = json!({ "code": code });
        return if offered("execute_code") {
            (String::new(), Some(call("execute_code", args)))
        } else if deferred("execute_code") {
            (String::new(), Some(call("tool_call", json!({ "calls": [{ "name": "execute_code", "arguments": args }] }))))
        } else {
            ("scripted: no execute_code among my tools".into(), None)
        };
    }
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
    let authorization = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
    let content_type = req.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if path.ends_with("/models") {
        return net::json_answer(StatusCode::OK, &json!({ "object": "list", "data": [{ "id": "cheap", "object": "model" }, { "id": "medium", "object": "model" }, { "id": "high", "object": "model" }, { "id": "vision", "object": "model" }] }));
    }
    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    if path.ends_with("/audio/transcriptions") {
        let (fields, answer) = transcription(&content_type, &body, &agent, &authorization);
        calls.lock().unwrap().push(Call { path, model: fields["model"].as_str().unwrap_or("").into(), agent, stream: false, body: fields, authorization });
        return answer;
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let stream = v["stream"] == json!(true);
    calls.lock().unwrap().push(Call { path: path.clone(), model: v["model"].as_str().unwrap_or("").into(), agent, stream, body: v.clone(), authorization });
    if !path.ends_with("/chat/completions") {
        return net::refusal(StatusCode::NOT_FOUND, "not_found", "the scripted model answers /v1/chat/completions");
    }
    let (text, tool) = answer(&v);
    let model = v["model"].as_str().unwrap_or("cheap").to_string();
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
fn a_memo_is_transcribed_as_the_route_would() {
    let memo = memo("hello from a voice memo");
    let form = |fields: &[(&str, &str)]| {
        let mut b = Vec::new();
        for (k, v) in fields {
            b.extend(format!("--xx\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
        }
        b.extend(b"--xx\r\nContent-Disposition: form-data; name=\"file\"; filename=\"m.wav\"\r\nContent-Type: audio/wav\r\n\r\n");
        b.extend(&memo);
        b.extend(b"\r\n--xx--\r\n");
        b
    };
    let key = Some("Bearer agent:juniper--k3x9".to_string());
    let (fields, r) = transcription("multipart/form-data; boundary=xx", &form(&[("model", "whisper"), ("response_format", "json")]), &None, &key);
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!((fields["model"].clone(), fields["language"].clone(), fields["audio_bytes"].clone()), (json!("whisper"), Value::Null, json!(memo.len())));
    let (_, r) = transcription("multipart/form-data; boundary=xx", &form(&[("model", "whisper")]), &None, &Some("Bearer fragment-model".into()));
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "a placeholder names no agent");
    let (_, r) = transcription("multipart/form-data; boundary=xx", &form(&[("model", "whisper-1")]), &None, &key);
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(said_in(&memo).as_deref(), Some("hello from a voice memo"));
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
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] run: fragment list" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "skills--k3x9 (editor)" }], "tools": tools }));
    assert_eq!(t, "scripted: the tool said: skills--k3x9 (editor)");
    // `send:` runs its command, and the answer sends the file it named
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] send: echo made=/t/a.txt" }], "tools": tools }));
    assert!(call.is_some_and(|c| c["function"]["arguments"].as_str().unwrap().contains("echo made=/t/a.txt")));
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] send: echo made=/t/a.txt" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "{\"output\": \"made=/t/a.txt\", \"exit_code\": 0}" }], "tools": tools }));
    assert_eq!(t, "scripted: sent\nMEDIA:/t/a.txt");
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
    // `start:` is a background process, and the answer quotes what it said
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] start: chromium about:blank" }], "tools": tools }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "command": "chromium about:blank", "background": true }).to_string());
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] start: chromium" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "{\"session_id\": \"proc_1\"}" }], "tools": tools }));
    assert_eq!(t, "scripted: the tool said: {\"session_id\": \"proc_1\"}");
    // write_file, directly or behind tool_search, and the answer quotes it
    let files = json!([{ "type": "function", "function": { "name": "write_file" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] write: ~/notes.txt" }], "tools": files }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "path": "~/notes.txt", "content": WRITTEN }).to_string());
    let bridged = json!([{ "type": "function", "function": { "name": "tool_search", "description": "… write_file: Write content …" } }, { "type": "function", "function": { "name": "tool_call" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] write: ~/notes.txt" }], "tools": bridged }));
    assert!(call.is_some_and(|c| c["function"]["name"] == "tool_call" && c["function"]["arguments"].as_str().unwrap().contains("\"write_file\"")));
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] write: ~/notes.txt" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "{\"path\": \"/h/notes.txt\"}" }], "tools": files }));
    assert_eq!(t, "scripted: the tool said: {\"path\": \"/h/notes.txt\"}");
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] write: ~/notes.txt" }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: no write_file among my tools", None));
    // execute_code, directly or behind tool_search, and the answer quotes it
    let code = json!([{ "type": "function", "function": { "name": "execute_code" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] code: print(1)" }], "tools": code }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "code": "print(1)" }).to_string());
    let bridged = json!([{ "type": "function", "function": { "name": "tool_search", "description": "… execute_code: Run a Python script …" } }, { "type": "function", "function": { "name": "tool_call" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] code: print(1)" }], "tools": bridged }));
    assert!(call.is_some_and(|c| c["function"]["name"] == "tool_call" && c["function"]["arguments"].as_str().unwrap().contains("\"execute_code\"")));
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] code: print(1)" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "{\"output\": \"1\\n\"}" }], "tools": code }));
    assert_eq!(t, "scripted: the tool said: {\"output\": \"1\\n\"}");
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] code: print(1)" }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: no execute_code among my tools", None));
    // Bot Mode: `dm:` is message_agent where it is offered; a teammate's
    // reply carried back is relayed; a teammate's message is answered as any
    let bots = json!([{ "type": "function", "function": { "name": "message_agent" } }]);
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] dm: maple: ping from juniper" }], "tools": bots }));
    assert_eq!(call.unwrap()["function"]["arguments"], json!({ "target": "maple", "message": "ping from juniper" }).to_string());
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] dm: maple: ping" }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: no message_agent among my tools", None));
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] dm: maple: ping" }, { "role": "assistant", "tool_calls": [] }, { "role": "tool", "content": "{\"status\": \"queued\"}" }], "tools": bots }));
    assert_eq!(t, "scripted: the tool said: {\"status\": \"queued\"}");
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": "Message from 🤖 juniper (@juniper--k3x9): ping" }], "tools": bots }));
    assert_eq!(t, "scripted: Message from 🤖 juniper (@juniper--k3x9): ping", "a teammate's message, answered");
    let done = "[SYSTEM: Background process proc_1 completed]\nReply from @maple--k3x9:\n{\"reply\": \"scripted: Message from 🤖 juniper (@juniper--k3x9): ping\", \"status\": \"settled\"}";
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": done }], "tools": bots }));
    assert_eq!(t, "scripted: relayed: scripted: Message from 🤖 juniper (@juniper--k3x9): ping");
    // as the bridge hands a teammate's message to it in its own chat, and
    // Hermes' delivery prints its answer (JSON, the emoji escaped)
    let done = "[SYSTEM: Background process proc_2 completed]\n{\"reply\": \"scripted: [juniper] Message from \\ud83e\\udd16 juniper (@juniper--k3x9): ping\", \"status\": \"settled\"}";
    let (t, _) = answer(&json!({ "messages": [{ "role": "user", "content": done }], "tools": bots }));
    assert_eq!(t, "scripted: relayed: scripted: [juniper] Message from \\ud83e\\udd16 juniper (@juniper--k3x9): ping");
    // a note on a cut risky turn is context: the message after it is answered
    let noted = "[Recent channel messages]\nYour previous turn… It was answering: “do the risky thing”\n\n[New message]\n[paul] good morning";
    let (t, call) = answer(&json!({ "messages": [{ "role": "user", "content": noted }], "tools": tools }));
    assert_eq!((t.as_str(), call), ("scripted: [paul] good morning", None));
    // joined to the cut request, it is answered whole: the request is redone
    let (_, call) = answer(&json!({ "messages": [{ "role": "user", "content": "[paul] do the risky thing\n\n[paul] good morning" }], "tools": tools }));
    assert!(call.is_some_and(|c| c["function"]["arguments"].as_str().unwrap().contains("rm -rf")));
}
