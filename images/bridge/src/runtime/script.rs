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
//! - `fetch <http url> with <value> [in <header> | in query <param> | as
//!   basic <user>]`: the guest's own request, as any SDK sends one, with no
//!   header of ours: `<value>` is `$<NAME>`, the agent's credential in that
//!   environment variable as `GET /api/computer` lists it now, or a literal;
//!   it goes in a header (`authorization: Bearer …` unless named), a query
//!   parameter, or basic auth's password beside `<user>`. The reply is the
//!   answer's status and its first 300 characters (the computer's swap,
//!   decisions 22 and 37);
//! - `credentials`: what the agent's guest is given now (`GET
//!   /api/computer`): each credential's provider, its environment variables
//!   and its placeholder, `; ` between;
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
    // In process, it can take a turn from its start.
    let _ = io.events.send(Event::Connected(true)).await;
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
                tokio::spawn(turn(cfg.clone(), *ts, rx, io.events.clone()));
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

/// Where `fetch` puts its value.
#[derive(Debug, PartialEq, Eq)]
enum Place<'a> {
    Header(&'a str),
    Query(&'a str),
    Basic(&'a str),
}

/// `fetch`'s words: the URL, the value (`$NAME` or a literal), and where it goes.
fn fetch_words(text: &str) -> Result<(&str, &str, Place<'_>), String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        [_, url, "with", v] => Ok((url, v, Place::Header("authorization"))),
        [_, url, "with", v, "in", "query", q] => Ok((url, v, Place::Query(q))),
        [_, url, "with", v, "as", "basic", user] => Ok((url, v, Place::Basic(user))),
        [_, url, "with", v, "in", h] => Ok((url, v, Place::Header(h))),
        _ => Err("say `fetch <http url> with <$NAME or a value> [in <header> | in query <param> | as basic <user>]`".into()),
    }
}

/// `agent`'s credentials, as `GET /api/computer` lists them now.
async fn credentials_of(agent: &str) -> Result<Vec<crate::runtime::Credential>, String> {
    let api = std::env::var("FRAGMENT_API").map_err(|_| "no FRAGMENT_API".to_string())?;
    let computer = crate::api::Api::new(&api)?.computer().await.map_err(|e| format!("GET /api/computer: {e}"))?;
    let a = computer.agents.into_iter().find(|a| a.fragment == agent).ok_or_else(|| format!("{agent} is not on this computer"))?;
    Ok(a.credentials)
}

/// The placeholder in `name` for `agent` (what an image puts in that
/// environment variable).
async fn credential(agent: &str, name: &str) -> Result<String, String> {
    let all = credentials_of(agent).await?;
    all.iter().find(|c| c.env.iter().any(|e| e == name)).map(|c| c.placeholder.clone()).ok_or_else(|| format!("no credential of {agent}'s is in ${name}"))
}

/// `fetch <url> with <value> [in <header> | in query <param> | as basic
/// <user>]`, as `agent`'s guest sends it (no header of ours): the answer's
/// status and body.
async fn fetch(agent: &str, text: &str) -> Result<(u16, String), String> {
    use base64::Engine;
    use http_body_util::{BodyExt, Empty, Limited};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let (url, value, place) = fetch_words(text)?;
    let value = match value.strip_prefix('$') {
        Some(name) => credential(agent, name).await?,
        None => value.to_string(),
    };
    let (url, header, header_value) = match place {
        Place::Header("authorization") => (url.to_string(), "authorization", format!("Bearer {value}")),
        Place::Header(h) => (url.to_string(), h, value),
        Place::Query(q) => (format!("{url}{}{q}={value}", if url.contains('?') { '&' } else { '?' }), "accept", "*/*".to_string()),
        Place::Basic(user) => (url.to_string(), "authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{value}")))),
    };
    let base = crate::net::Base::parse(&url)?;
    let req = hyper::Request::get(&url)
        .header("host", base.authority())
        .header(header, header_value)
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
    if text.trim() == "credentials" {
        reply = match credentials_of(&ts.agent.fragment).await {
            Ok(all) if all.is_empty() => "credentials: none".to_string(),
            Ok(all) => format!("credentials: {}", all.iter().map(|c| format!("{} {} {}", c.provider, c.env.join(","), c.placeholder)).collect::<Vec<_>>().join("; ")),
            Err(e) => format!("credentials failed: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `fetch`'s grammar: a header (authorization by default), a query
    /// parameter, or basic auth; anything else is refused, saying how.
    #[test]
    fn fetch_reads_where_its_value_goes() {
        assert_eq!(fetch_words("fetch http://a.test/x with $PERPLEXITY_API_KEY").unwrap(), ("http://a.test/x", "$PERPLEXITY_API_KEY", Place::Header("authorization")));
        assert_eq!(fetch_words("fetch http://a.test/x with $K in xi-api-key").unwrap(), ("http://a.test/x", "$K", Place::Header("xi-api-key")));
        assert_eq!(fetch_words("fetch http://a.test/x with $K in query key").unwrap(), ("http://a.test/x", "$K", Place::Query("key")));
        assert_eq!(fetch_words("fetch http://a.test/x with fck_p_00 as basic api").unwrap(), ("http://a.test/x", "fck_p_00", Place::Basic("api")));
        for bad in ["fetch http://a.test/x", "fetch http://a.test/x with", "fetch http://a.test/x with $K in", "fetch http://a.test/x with $K as basic"] {
            assert!(fetch_words(bad).is_err(), "{bad}");
        }
    }
}
