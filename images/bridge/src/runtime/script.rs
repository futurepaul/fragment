//! The `script` runtime: a deterministic agent that answers from rules, so
//! the platform's lanes run with no agent runtime at all (the stub image,
//! and the proof that the platform holds nothing Hermes-specific). Each
//! turn is a pure function of its message:
//!
//! - default: two drafts, then the reply `echo: [<asker>] <text>`;
//! - `tool`: a step (`search`, its args, ok, an excerpt) before the reply;
//! - `approve` or `risky`: a step, then a prompt (`once`, `deny`) the
//!   owner answers; the reply says `(approved)`, `(denied)`, or
//!   `(not approved)` once it expired;
//! - `slow`: twenty drafts, `pace` apart, before the reply (for Stop);
//! - `fail`: the turn ends as an error;
//! - `silent`: the turn ends with no reply;
//! - `draw`: the reply carries a file (`drawing.txt`);
//! - `fetch <http url> with <placeholder> [in <header>]`: the guest's own
//!   request, as the agent, with a credential's placeholder in a header
//!   (`authorization: Bearer …` unless named): the reply is the answer's
//!   status and its first 300 characters (the computer's swap, decisions
//!   22 and 37);
//! - `think <text>`: one model call through the computer's model intercept
//!   (`$FRAGMENT_MODEL`, the cheap tier, as the agent): the reply is the
//!   model's answer;
//! - a message with attachments: the reply names them;
//! - `@<name>` of another agent in the reply's text hands off to it, as
//!   any reply's does (the bridge reads mentions, not this runtime).
//!
//! A Stop ends the turn as `stopped` at its next draft.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::records::{Outcome, PromptOption, Step};
use crate::runtime::{Command, Event, LocalFile, Runtime, RuntimeFuture, RuntimeIo, TurnStart};

#[derive(Debug, Clone)]
pub struct ScriptConfig {
    /// The wait between a turn's drafts.
    pub pace: Duration,
    /// Where files it writes go (scratch).
    pub scratch: PathBuf,
}

pub struct Script {
    pub config: ScriptConfig,
}

impl Runtime for Script {
    fn name(&self) -> &'static str {
        "script"
    }

    fn run(self: Box<Self>, io: RuntimeIo) -> RuntimeFuture {
        Box::pin(run(self.config, io))
    }
}

/// What a running scripted turn hears from the bridge.
enum Heard {
    Stop,
    Answer(Option<String>),
}

async fn run(cfg: ScriptConfig, mut io: RuntimeIo) -> Result<(), crate::runtime::RuntimeError> {
    let _ = std::fs::create_dir_all(&cfg.scratch);
    let mut turns: HashMap<String, mpsc::Sender<Heard>> = HashMap::new();
    let mut shutdown = io.shutdown.clone();
    // bounded by the bridge's life: one command per pass
    loop {
        let c = tokio::select! {
            c = io.commands.recv() => c,
            _ = crate::net::stopped(&mut shutdown) => return Ok(()),
        };
        let Some(c) = c else { return Ok(()) };
        turns.retain(|_, tx| !tx.is_closed());
        match c {
            Command::Start(ts) => {
                let (tx, rx) = mpsc::channel(8);
                turns.insert(ts.turn.clone(), tx);
                tokio::spawn(turn(cfg.clone(), ts, rx, io.events.clone()));
            }
            Command::Stop { turn } => {
                if let Some(tx) = turns.get(&turn) {
                    let _ = tx.send(Heard::Stop).await;
                }
            }
            Command::Answer { turn, option, .. } => {
                if let Some(tx) = turns.get(&turn) {
                    let _ = tx.send(Heard::Answer(option)).await;
                }
            }
            Command::Forget { turn } => {
                turns.remove(&turn);
            }
        }
    }
}

/// `fetch <url> with <placeholder> [in <header>]`, sent as `agent`: the
/// answer's status and body.
async fn fetch(agent: &str, text: &str) -> Result<(u16, String), String> {
    use http_body_util::{BodyExt, Empty, Limited};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let words: Vec<&str> = text.split_whitespace().collect();
    let (url, placeholder, header) = match words.as_slice() {
        [_, url, "with", p] => (*url, *p, "authorization"),
        [_, url, "with", p, "in", h] => (*url, *p, *h),
        _ => return Err("say `fetch <http url> with <placeholder> [in <header>]`".into()),
    };
    let value = if header == "authorization" { format!("Bearer {placeholder}") } else { placeholder.to_string() };
    let base = crate::net::Base::parse(url)?;
    let req = hyper::Request::get(url)
        .header("host", base.authority())
        .header("x-fragment-agent", agent)
        .header(header, value)
        .body(Empty::<bytes::Bytes>::new())
        .map_err(|e| e.to_string())?;
    let client = Client::builder(TokioExecutor::new()).build_http();
    let call = async {
        let res = client.request(req).await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let body = Limited::new(res.into_body(), 64 * 1024).collect().await.map_err(|e| e.to_string())?.to_bytes();
        Ok::<_, String>((status, String::from_utf8_lossy(&body).into_owned()))
    };
    tokio::time::timeout(Duration::from_millis(crate::limits::HTTP_TIMEOUT_MS), call).await.map_err(|_| format!("{url}: no answer"))?
}

/// One model call, `said` as the user's message, as `agent`: the answer's
/// text.
async fn think(agent: &str, said: &str) -> Result<String, String> {
    use http_body_util::{BodyExt, Full, Limited};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let model = std::env::var("FRAGMENT_MODEL").map_err(|_| "no FRAGMENT_MODEL".to_string())?;
    let base = crate::net::Base::parse(&model)?;
    let body = serde_json::json!({ "model": "cheap", "messages": [{ "role": "user", "content": said }] }).to_string();
    let req = hyper::Request::post(base.url("/v1/chat/completions"))
        .header("host", base.authority())
        .header("x-fragment-agent", agent)
        .header("content-type", "application/json")
        .header("content-length", body.len())
        .body(Full::new(bytes::Bytes::from(body)))
        .map_err(|e| e.to_string())?;
    let client = Client::builder(TokioExecutor::new()).build_http();
    let call = async {
        let res = client.request(req).await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let bytes = Limited::new(res.into_body(), 256 * 1024).collect().await.map_err(|e| e.to_string())?.to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        match v["choices"][0]["message"]["content"].as_str() {
            Some(answer) if status == 200 => Ok(answer.to_string()),
            _ => Err(format!("{status} {}", String::from_utf8_lossy(&bytes).chars().take(300).collect::<String>())),
        }
    };
    tokio::time::timeout(Duration::from_millis(crate::limits::HTTP_TIMEOUT_MS), call).await.map_err(|_| "the model did not answer".to_string())?
}

/// Waits `pace`, hearing a Stop meanwhile (true).
async fn pause(pace: Duration, rx: &mut mpsc::Receiver<Heard>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(pace) => false,
        h = rx.recv() => matches!(h, Some(Heard::Stop) | None),
    }
}

async fn turn(cfg: ScriptConfig, ts: TurnStart, mut rx: mpsc::Receiver<Heard>, events: mpsc::Sender<Event>) {
    let id = ts.turn.clone();
    let emit = |e: Event| {
        let events = events.clone();
        async move {
            let _ = events.send(e).await;
        }
    };
    emit(Event::Accepted { turn: id.clone() }).await;
    let text = ts.text.to_lowercase();
    if text.contains("fail") {
        emit(Event::End { turn: id, outcome: Outcome::Error("the script was asked to fail".into()) }).await;
        return;
    }
    if text.contains("silent") {
        emit(Event::End { turn: id, outcome: Outcome::Idle }).await;
        return;
    }
    let mut reply = format!("echo: [{}] {}", ts.asker_name, ts.text);
    if let Some(said) = ts.text.strip_prefix("think ") {
        reply = match think(&ts.agent.fragment, said).await {
            Ok(answer) => format!("thought: {answer}"),
            Err(e) => format!("think failed: {e}"),
        };
    }
    if text.starts_with("fetch ") {
        reply = match fetch(&ts.agent.fragment, &ts.text).await {
            Ok((status, body)) => format!("fetched {status}: {}", body.chars().take(300).collect::<String>()),
            Err(e) => format!("fetch failed: {e}"),
        };
    }
    if text.contains("tool") {
        let step = Step { tool: "search".into(), args: format!("{{\"q\":\"{}\"}}", ts.text.chars().take(40).collect::<String>()), ok: true, excerpt: "3 results".into(), text: "Let me look.".into() };
        emit(Event::Step { turn: id.clone(), step }).await;
    }
    if text.contains("approve") || text.contains("risky") {
        emit(Event::Step { turn: id.clone(), step: Step { tool: "terminal".into(), args: "rm -rf ./scratch".into(), ok: true, excerpt: String::new(), text: String::new() } }).await;
        let options = vec![
            PromptOption { id: "once".into(), label: "Allow once".into(), style: Some("primary".into()) },
            PromptOption { id: "deny".into(), label: "Deny".into(), style: Some("danger".into()) },
        ];
        let prompt = format!("p-{}", &id[..12]);
        emit(Event::Prompt { turn: id.clone(), prompt, text: "Run `rm -rf ./scratch`?".into(), options, ttl_ms: None }).await;
        let said = match rx.recv().await {
            Some(Heard::Answer(Some(o))) if o == "once" => "approved",
            Some(Heard::Answer(Some(_))) => "denied",
            Some(Heard::Answer(None)) => "not approved",
            Some(Heard::Stop) | None => {
                emit(Event::End { turn: id, outcome: Outcome::Stopped }).await;
                return;
            }
        };
        reply = format!("{reply} ({said})");
    }
    if !ts.files.is_empty() {
        let names: Vec<&str> = ts.files.iter().map(|f| f.name.as_str()).collect();
        reply = format!("{reply} [got {}: {}]", ts.files.len(), names.join(", "));
    }
    let drafts = if text.contains("slow") { 20 } else { 2 };
    for i in 1..=drafts {
        let cut = reply.chars().count() * i / (drafts + 1);
        emit(Event::Draft { turn: id.clone(), text: reply.chars().take(cut.max(1)).collect() }).await;
        if pause(cfg.pace, &mut rx).await {
            emit(Event::End { turn: id, outcome: Outcome::Stopped }).await;
            return;
        }
    }
    emit(Event::Reply { turn: id.clone(), part: 1, text: reply }).await;
    if text.contains("draw") {
        let path = cfg.scratch.join(format!("{id}-drawing.txt"));
        let body = format!("a drawing for {}\n", ts.asker_name);
        if tokio::fs::write(&path, &body).await.is_ok() {
            let file = LocalFile { path, media_type: "text/plain".into(), name: "drawing.txt".into(), size: body.len() as u64 };
            emit(Event::Attachment { turn: id.clone(), part: 1, file }).await;
        }
    }
    emit(Event::End { turn: id, outcome: Outcome::Idle }).await;
}
