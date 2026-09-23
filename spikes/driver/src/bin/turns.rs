//! Spike 3: deterministic agent turns in Workflows.
//!
//! `turns <spikes-dir> <out.json>` starts a scripted fake model and tool
//! service, runs libfx turns as Workflow instances on `celld dev`, and
//! SIGKILLs the node in the middle of a model call and in the middle of a
//! tool effect. After a restart each turn must finish with no model call or
//! tool effect repeated, except the one that was in flight when the node
//! died, and the in-flight tool effect must apply once (its key dedupes).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

#[path = "../celld.rs"]
mod celld;
use celld::Celld;

const PORT: u16 = 8821;
const TURN_TIMEOUT: Duration = Duration::from_secs(180);
const HANG: Duration = Duration::from_secs(30);

/// What the fake has seen, per turn.
#[derive(Default)]
struct Fake {
    /// (turn, model call index) -> attempts
    model_calls: BTreeMap<(String, u32), u32>,
    /// effect key -> executions (at-least-once)
    effect_runs: BTreeMap<String, u32>,
    /// effect keys applied (deduped by key)
    applied: BTreeSet<String>,
    /// hang the first attempt of this (turn, model call)
    hang_model: Option<(String, u32)>,
    /// hang the first execution of this effect key
    hang_effect: Option<String>,
}

type Shared = Arc<Mutex<Fake>>;

fn sse(parts: &[Value]) -> String {
    parts.iter().map(|p| format!("data: {p}\n\n")).collect()
}

/// The scripted model: add two notes, then answer.
fn model_answer(prompt: &Value) -> String {
    let tool_results = prompt
        .as_array()
        .map_or(0, |msgs| msgs.iter().filter(|m| m["role"] == "tool").count());
    // AI SDK LanguageModelV3 stream parts, as @ai-sdk/openai-compatible 3.x emits them
    let usage = json!({
        "inputTokens": { "total": 10, "noCache": 10, "cacheRead": 0 },
        "outputTokens": { "total": 5, "text": 5, "reasoning": 0 }
    });
    if tool_results < 2 {
        let text = if tool_results == 0 { "first" } else { "second" };
        let call_id = format!("call_{tool_results}");
        sse(&[
            json!({ "type": "stream-start", "warnings": [] }),
            json!({ "type": "tool-call", "toolCallId": call_id, "toolName": "add_note", "input": json!({ "text": text }).to_string() }),
            json!({ "type": "finish", "finishReason": { "unified": "tool-calls", "raw": "tool_calls" }, "usage": usage }),
        ])
    } else {
        sse(&[
            json!({ "type": "stream-start", "warnings": [] }),
            json!({ "type": "text-start", "id": "t0" }),
            json!({ "type": "text-delta", "id": "t0", "delta": "done: two notes added" }),
            json!({ "type": "text-end", "id": "t0" }),
            json!({ "type": "finish", "finishReason": { "unified": "stop", "raw": "stop" }, "usage": usage }),
        ])
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn handle(mut stream: TcpStream, fake: Shared) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").map_or(Ok(0), |v| v.parse())?;
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    match (method.as_str(), path.as_str()) {
        ("POST", "/model") => {
            let turn = headers.get("x-turn").cloned().unwrap_or_default();
            let call: u32 = headers.get("x-call").map_or(Ok(0), |v| v.parse())?;
            let options: Value = serde_json::from_slice(&body)?;
            let hang = {
                let mut f = fake.lock().expect("fake lock");
                let n = f.model_calls.entry((turn.clone(), call)).or_insert(0);
                *n += 1;
                *n == 1 && f.hang_model == Some((turn.clone(), call))
            };
            if hang {
                std::thread::sleep(HANG);
            }
            respond(&mut stream, 200, "text/event-stream", &model_answer(&options["prompt"]));
        }
        ("POST", "/effect") => {
            let e: Value = serde_json::from_slice(&body)?;
            let key = e["key"].as_str().context("effect key")?.to_string();
            let hang = {
                let mut f = fake.lock().expect("fake lock");
                let n = f.effect_runs.entry(key.clone()).or_insert(0);
                *n += 1;
                *n == 1 && f.hang_effect.as_deref() == Some(key.as_str())
            };
            if hang {
                std::thread::sleep(HANG);
            }
            let first = fake.lock().expect("fake lock").applied.insert(key.clone());
            respond(&mut stream, 200, "application/json", &json!({ "key": key, "first": first }).to_string());
        }
        _ => respond(&mut stream, 404, "text/plain", "not found"),
    }
    Ok(())
}

fn serve(fake: Shared) -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let fake = fake.clone();
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, fake) {
                    eprintln!("fake: {e:#}");
                }
            });
        }
    });
    Ok(port)
}

struct Run {
    http: reqwest::blocking::Client,
    base: String,
}

impl Run {
    fn start_turn(&self, id: &str, fake_port: u16) -> Result<()> {
        let r = self.http.post(format!("{}/turn", self.base))
            .json(&json!({ "id": id, "prompt": "Add two notes.", "fake": format!("http://127.0.0.1:{fake_port}") }))
            .send()?;
        if !r.status().is_success() {
            bail!("create {id}: {} {}", r.status(), r.text()?);
        }
        Ok(())
    }

    fn wait(&self, id: &str) -> Result<(Value, Duration)> {
        let t0 = Instant::now();
        loop {
            let st: Value = self.http.get(format!("{}/turn/{id}", self.base)).send()?.json()?;
            match st["status"].as_str() {
                Some("complete") | Some("errored") | Some("terminated") => return Ok((st, t0.elapsed())),
                _ => {}
            }
            if t0.elapsed() > TURN_TIMEOUT {
                bail!("turn {id} did not finish in {TURN_TIMEOUT:?}: {st}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) -> Result<()> {
    let t0 = Instant::now();
    while !cond() {
        if t0.elapsed() > Duration::from_secs(60) {
            bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

struct Checks(Vec<String>, usize);

impl Checks {
    fn ok(&mut self, label: &str, cond: bool, detail: impl std::fmt::Display) {
        if cond {
            self.1 += 1;
            println!("ok    {label}");
        } else {
            self.0.push(label.into());
            println!("FAIL  {label}: {detail}");
        }
    }
}

fn model_attempts(fake: &Shared, turn: &str) -> Vec<u32> {
    let f = fake.lock().expect("fake lock");
    (0..3).map(|i| *f.model_calls.get(&(turn.to_string(), i)).unwrap_or(&0)).collect()
}

fn effect_runs(fake: &Shared, turn: &str) -> Vec<u32> {
    let f = fake.lock().expect("fake lock");
    (0..2).map(|i| *f.effect_runs.get(&format!("{turn}:tool-{i}")).unwrap_or(&0)).collect()
}

fn applied(fake: &Shared, turn: &str) -> usize {
    fake.lock().expect("fake lock").applied.iter().filter(|k| k.starts_with(&format!("{turn}:"))).count()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        bail!("usage: turns <spikes-dir> <out.json>");
    }
    let project = PathBuf::from(&args[1]).canonicalize()?.join("agent-turns");
    let esbuild = std::env::var("CELLD_ESBUILD").context("set CELLD_ESBUILD")?;
    // A dead node's instance resumes from its armed alarm; a short waker
    // tick keeps the test from waiting out the 60 s default scan.
    let env = [("CELLD_ESBUILD", esbuild), ("CELLD_WAKER_TICK_MS", "2000".to_string())];
    let fake: Shared = Arc::new(Mutex::new(Fake::default()));
    let fake_port = serve(fake.clone())?;
    let mut c = Checks(vec![], 0);
    let mut report = serde_json::Map::new();
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;

    // 1. control: a turn with no crash
    let (node, _) = Celld::start(&project, PORT, true, &env)?;
    let run = Run { http: http.clone(), base: node.base.clone() };
    run.start_turn("control", fake_port)?;
    let (st, took) = run.wait("control")?;
    c.ok("control turn completes", st["status"] == "complete", &st);
    c.ok("control answer is the model's text", st["output"]["text"] == "done: two notes added", &st["output"]);
    c.ok("control made 3 model calls and 2 tool calls", st["output"]["modelCalls"] == 3 && st["output"]["toolCalls"] == 2, &st["output"]);
    c.ok("control: each model call ran once", model_attempts(&fake, "control") == [1, 1, 1], format!("{:?}", model_attempts(&fake, "control")));
    c.ok("control: each tool effect ran once", effect_runs(&fake, "control") == [1, 1], format!("{:?}", effect_runs(&fake, "control")));
    report.insert("control".into(), json!({ "turn_ms": took.as_millis(), "checkpoint_bytes": st["output"]["checkpointBytes"] }));

    // 2. SIGKILL during the third model call
    fake.lock().expect("fake lock").hang_model = Some(("k1".into(), 2));
    run.start_turn("k1", fake_port)?;
    wait_for("k1's third model call", || model_attempts(&fake, "k1")[2] == 1)?;
    node.crash()?;
    let t0 = Instant::now();
    let (node, _) = Celld::start(&project, PORT, false, &env)?;
    let run = Run { http: http.clone(), base: node.base.clone() };
    let (st, _) = run.wait("k1")?;
    let resumed = t0.elapsed();
    c.ok("k1 completes after a crash mid model call", st["status"] == "complete", &st);
    c.ok("k1: finished model calls did not repeat; the in-flight one did", model_attempts(&fake, "k1") == [1, 1, 2], format!("{:?}", model_attempts(&fake, "k1")));
    c.ok("k1: no tool effect repeated", effect_runs(&fake, "k1") == [1, 1], format!("{:?}", effect_runs(&fake, "k1")));
    c.ok("k1: the answer is intact", st["output"]["text"] == "done: two notes added", &st["output"]);
    report.insert("kill_mid_model".into(), json!({ "restart_to_complete_ms": resumed.as_millis(), "model_attempts": model_attempts(&fake, "k1"), "effect_runs": effect_runs(&fake, "k1") }));

    // 3. SIGKILL during the second tool effect
    fake.lock().expect("fake lock").hang_effect = Some("k2:tool-1".into());
    run.start_turn("k2", fake_port)?;
    wait_for("k2's second tool effect", || effect_runs(&fake, "k2")[1] == 1)?;
    node.crash()?;
    let t0 = Instant::now();
    let (node, _) = Celld::start(&project, PORT, false, &env)?;
    let run = Run { http, base: node.base.clone() };
    let (st, _) = run.wait("k2")?;
    let resumed = t0.elapsed();
    c.ok("k2 completes after a crash mid tool effect", st["status"] == "complete", &st);
    c.ok("k2: model calls did not repeat", model_attempts(&fake, "k2") == [1, 1, 1], format!("{:?}", model_attempts(&fake, "k2")));
    c.ok("k2: the finished effect did not repeat; the in-flight one ran again", effect_runs(&fake, "k2") == [1, 2], format!("{:?}", effect_runs(&fake, "k2")));
    c.ok("k2: each effect applied once (its key dedupes)", applied(&fake, "k2") == 2, applied(&fake, "k2"));
    report.insert("kill_mid_tool".into(), json!({ "restart_to_complete_ms": resumed.as_millis(), "model_attempts": model_attempts(&fake, "k2"), "effect_runs": effect_runs(&fake, "k2") }));
    node.stop()?;

    report.insert("checks".into(), json!({ "passed": c.1, "failed": c.0 }));
    fs::write(&args[2], serde_json::to_string_pretty(&Value::Object(report))?)?;
    println!("\n{} passed, {} failed", c.1, c.0.len());
    if !c.0.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
