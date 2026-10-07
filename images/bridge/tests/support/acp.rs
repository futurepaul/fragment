//! A scripted goose: an ACP agent on an in-process pipe, which the `goose`
//! runtime is handed in place of `goose acp` (`Spawn`). What it says is a
//! function of the prompt's last line (lesson 13):
//!
//! - default: `scripted: <the line>`, in two chunks, then `end_turn`;
//! - `tool`: "Let me look.", a `shell` call (`ls`) that completes with
//!   `a.txt`, then "Found a.txt.";
//! - `slow`: a chunk every 20 ms until its session is cancelled (then
//!   `cancelled`), or 500 of them;
//! - `permission`: it asks permission for a tool (`session/request_permission`),
//!   and once answered says `scripted: asked`;
//! - `die`: it exits without answering (its stdout ends).
//!
//! It keeps what it was told (`Log`), across every goose it is spawned as.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use fragment_bridge::runtime::goose::{Pipe, Spawn};
use fragment_bridge::runtime::Agent;

#[derive(Default, Debug)]
pub struct Log {
    /// The agent of each goose spawned, in order.
    pub spawned: Vec<String>,
    /// Each `session/new`'s params.
    pub sessions: Vec<Value>,
    /// Each `session/prompt`'s text.
    pub prompts: Vec<String>,
    /// The sessions cancelled (`session/cancel`).
    pub cancels: Vec<String>,
    /// The answers to its permission asks.
    pub permissions: Vec<Value>,
}

#[derive(Clone, Default)]
pub struct FakeGoose {
    pub log: Arc<Mutex<Log>>,
}

impl Spawn for FakeGoose {
    fn spawn(&self, agent: &Agent) -> Result<Pipe, String> {
        self.log.lock().unwrap().spawned.push(agent.fragment.clone());
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let (read, write) = tokio::io::split(ours);
        let (kill, killed) = oneshot::channel::<()>();
        tokio::spawn(goose(theirs, self.log.clone(), killed));
        Ok(Pipe { read: Box::new(read), write: Box::new(write), keep: Box::new(kill) })
    }
}

/// A slow prompt being streamed: its call's id, and the chunks sent.
struct Slow {
    id: Value,
    sent: u32,
}

async fn goose(io: tokio::io::DuplexStream, log: Arc<Mutex<Log>>, mut killed: oneshot::Receiver<()>) {
    let (read, mut write) = tokio::io::split(io);
    let mut lines = BufReader::new(read).lines();
    let mut sessions = 0u32;
    let mut slow: HashMap<String, Slow> = HashMap::new();
    // a prompt waiting on its permission's answer: (session, call id)
    let mut asking: Option<(String, Value)> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut out: Vec<Value> = Vec::new();
    // bounded by its pipe: it ends when the runtime lets it go
    loop {
        tokio::select! {
            _ = &mut killed => return,
            _ = tick.tick() => {
                for (session, s) in slow.iter_mut() {
                    s.sent += 1;
                    out.push(chunk(session, "more ", "m1"));
                }
                let done: Vec<String> = slow.iter().filter(|(_, s)| s.sent >= 500).map(|(k, _)| k.clone()).collect();
                for session in done {
                    let s = slow.remove(&session).unwrap();
                    out.push(json!({ "jsonrpc": "2.0", "id": s.id, "result": { "stopReason": "end_turn" } }));
                }
            }
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let m: Value = serde_json::from_str(&line).expect("the runtime writes JSON lines");
                let id = m["id"].clone();
                match m["method"].as_str() {
                    Some("initialize") => out.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": 1, "agentCapabilities": {}, "authMethods": [] } })),
                    Some("session/new") => {
                        sessions += 1;
                        log.lock().unwrap().sessions.push(m["params"].clone());
                        out.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "sessionId": format!("s{sessions}") } }));
                    }
                    Some("session/prompt") => {
                        let session = m["params"]["sessionId"].as_str().unwrap_or("").to_string();
                        let text = m["params"]["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                        log.lock().unwrap().prompts.push(text.clone());
                        let said = text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string();
                        match said.as_str() {
                            "die" => return,
                            "slow" => {
                                slow.insert(session, Slow { id, sent: 0 });
                            }
                            "permission" => {
                                out.push(json!({ "jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission", "params": {
                                    "sessionId": session,
                                    "toolCall": { "toolCallId": "c9", "title": "developer: shell · rm -rf /" },
                                    "options": [{ "optionId": "allow_once", "name": "allow_once", "kind": "allow_once" }, { "optionId": "reject_once", "name": "reject_once", "kind": "reject_once" }],
                                } }));
                                asking = Some((session, id));
                            }
                            "tool" => {
                                out.push(chunk(&session, "Let me look.", "m1"));
                                out.push(update(&session, json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "developer: shell · ls", "status": "pending", "rawInput": { "command": "ls" }, "_meta": { "goose": { "toolCall": { "toolName": "developer__shell", "extensionName": "developer" } } } })));
                                out.push(update(&session, json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "content": [{ "type": "content", "content": { "type": "text", "text": "a.txt\n" } }] })));
                                out.push(chunk(&session, "Found ", "m2"));
                                out.push(chunk(&session, "a.txt.", "m2"));
                                out.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } }));
                            }
                            line => {
                                let answer = format!("scripted: {line}");
                                let (a, b) = answer.split_at(answer.len() / 2);
                                out.push(chunk(&session, a, "m1"));
                                out.push(chunk(&session, b, "m1"));
                                out.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } }));
                            }
                        }
                    }
                    Some("session/cancel") => {
                        let session = m["params"]["sessionId"].as_str().unwrap_or("").to_string();
                        log.lock().unwrap().cancels.push(session.clone());
                        if let Some(s) = slow.remove(&session) {
                            out.push(json!({ "jsonrpc": "2.0", "id": s.id, "result": { "stopReason": "cancelled" } }));
                        }
                    }
                    // the answer to its permission ask
                    None if id == "perm-1" => {
                        log.lock().unwrap().permissions.push(m["result"].clone());
                        if let Some((session, id)) = asking.take() {
                            out.push(chunk(&session, "scripted: asked", "m1"));
                            out.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } }));
                        }
                    }
                    _ => {}
                }
            }
        }
        for m in out.drain(..) {
            if write.write_all(format!("{m}\n").as_bytes()).await.is_err() {
                return;
            }
        }
    }
}

fn update(session: &str, u: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": u } })
}

fn chunk(session: &str, text: &str, message: &str) -> Value {
    update(session, json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text }, "messageId": message }))
}
