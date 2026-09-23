//! Drives spikes 1 and 2 against the Rust and the JavaScript supervisors.
//!
//! `spike-driver <spikes-dir> <out.json>` starts each project under
//! `celld dev`, runs the functional checks (valid, invalid, replay,
//! conflicting body, rollback, isolation, restart), and measures bundle
//! size, cold activation, warm latency, the facet image cost at 1/16/64
//! MiB, and the `cpuMs` limit on a facet call. Every check prints `ok` or
//! `FAIL`; the process exits non-zero when any check fails.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

mod celld;
use celld::Celld;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const WARM_SAMPLES: usize = 200;
const FACET_SAMPLES: usize = 30;
const COLD_RUNS: usize = 5;
const EVICT_S: u64 = 2;

fn operations() -> Value {
    json!({
        "add_todo": { "kind": "mutation" },
        "add_todo_emit": { "kind": "mutation" },
        "add_then_throw": { "kind": "mutation" },
        "emit_query": { "kind": "query" },
        "fill": { "kind": "mutation" },
        "list": { "kind": "query" },
        "count": { "kind": "query" },
        "size": { "kind": "query" },
        "tables": { "kind": "query" },
        "egress": { "kind": "query" },
        "alarm_attempt": { "kind": "query" },
        "spin_forever": { "kind": "query" }
    })
}

struct Api {
    http: reqwest::blocking::Client,
    base: String,
}

impl Api {
    fn new(base: &str) -> Api {
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(90)).build().expect("client");
        Api { http, base: base.to_string() }
    }

    fn req(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, Value, Duration)> {
        let t0 = Instant::now();
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().with_context(|| format!("{path}"))?;
        let status = resp.status().as_u16();
        let text = resp.text()?;
        let took = t0.elapsed();
        let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((status, value, took))
    }

    fn op(&self, cell: &str, body: Value) -> Result<(u16, Value, Duration)> {
        self.req(reqwest::Method::POST, &format!("/c/{cell}/op"), Some(body))
    }

    fn install(&self, cell: &str, source: &str, sha: &str, cpu_ms: Option<u32>) -> Result<()> {
        let mut body = json!({ "sha": sha, "source": source, "operations": operations() });
        if let Some(c) = cpu_ms {
            body["cpu_ms"] = json!(c);
        }
        let (s, v, _) = self.req(reqwest::Method::PUT, &format!("/c/{cell}/code"), Some(body))?;
        if s != 200 {
            bail!("install on {cell}: {s} {v}");
        }
        Ok(())
    }
}

#[derive(Default)]
struct Checks {
    failed: Vec<String>,
    passed: usize,
}

impl Checks {
    fn ok(&mut self, label: &str, cond: bool, detail: impl std::fmt::Display) {
        if cond {
            self.passed += 1;
            println!("ok    {label}");
        } else {
            self.failed.push(label.to_string());
            println!("FAIL  {label}: {detail}");
        }
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

fn summary(mut xs: Vec<f64>) -> Value {
    assert!(!xs.is_empty(), "a summary needs samples");
    xs.sort_by(|a, b| a.partial_cmp(b).expect("no NaN latencies"));
    let at = |q: f64| xs[((xs.len() as f64 - 1.0) * q).round() as usize];
    json!({ "n": xs.len(), "p50_ms": at(0.5), "p95_ms": at(0.95), "max_ms": at(1.0),
            "mean_ms": (xs.iter().sum::<f64>() / xs.len() as f64 * 100.0).round() / 100.0 })
}

fn functional(api: &Api, source: &str, c: &mut Checks, label: &str) -> Result<()> {
    let cell = "check";
    api.install(cell, source, "v1", None)?;
    let (s, v, _) = api.op(cell, json!({ "id": "a1", "name": "add_todo", "input": { "text": "one" } }))?;
    c.ok(&format!("[{label}] mutation runs"), s == 200 && v["replayed"] == false, &v);
    let first = v["result"].clone();
    let (s, v, _) = api.op(cell, json!({ "id": "a1", "name": "add_todo", "input": { "text": "one" } }))?;
    c.ok(&format!("[{label}] replay returns the stored result"), s == 200 && v["replayed"] == true && v["result"] == first, &v);
    let (s, v, _) = api.op(cell, json!({ "id": "a1", "name": "add_todo", "input": { "text": "two" } }))?;
    c.ok(&format!("[{label}] conflicting body is 409"), s == 409 && v["error"] == "conflicting_body", &v);
    let (s, v, _) = api.op(cell, json!({ "id": "bad id!", "name": "add_todo", "input": { "text": "x" } }))?;
    c.ok(&format!("[{label}] invalid operation id is 400"), s == 400, &v);
    let (s, v, _) = api.op(cell, json!({ "id": "u1", "name": "nope" }))?;
    c.ok(&format!("[{label}] unknown operation is 404"), s == 404 && v["error"] == "unknown_operation", &v);
    let (s, v, _) = api.op(cell, json!({ "id": "e1", "name": "add_todo", "input": { "text": "" } }))?;
    c.ok(&format!("[{label}] app refusal is 422 and not ledgered"), s == 422, &v);
    let (s, v, _) = api.op(cell, json!({ "id": "e1", "name": "add_todo", "input": { "text": "" } }))?;
    c.ok(&format!("[{label}] a refused id stays unused"), s == 422, &v);
    let big = "x".repeat(300 * 1024);
    let (s, v, _) = api.op(cell, json!({ "id": "big", "name": "add_todo", "input": { "text": big } }))?;
    c.ok(&format!("[{label}] input over 256 KiB is 413"), s == 413, &v);

    let count = |api: &Api| -> Result<i64> {
        let (_, v, _) = api.op(cell, json!({ "id": "q", "name": "count" }))?;
        Ok(v["result"]["n"].as_i64().unwrap_or(-1))
    };
    let before = count(api)?;
    let (s, v, _) = api.op(cell, json!({ "id": "t1", "name": "add_then_throw", "input": { "text": "rolled back" } }))?;
    c.ok(&format!("[{label}] an author throw is 422"), s == 422, &v);
    c.ok(&format!("[{label}] an author throw rolls back its write"), count(api)? == before, before);
    let (s, v, _) = api.op(cell, json!({ "id": "t1", "name": "add_then_throw", "input": { "text": "rolled back" } }))?;
    c.ok(&format!("[{label}] an author throw leaves the id unused"), s == 422, &v);
    let (s, v, _) = api.op(cell, json!({ "id": "as1", "name": "add_todo_emit", "input": { "text": "async" } }))?;
    c.ok(&format!("[{label}] an async mutation is refused"),
        s == 422 && v["message"].as_str().unwrap_or("").contains("mutations are synchronous"), &v);
    c.ok(&format!("[{label}] an async mutation's write rolls back"), count(api)? == before, before);

    let (s, v, _) = api.op(cell, json!({ "id": "f1", "name": "add_todo", "input": { "text": "committed" }, "fail_after_app": true }))?;
    c.ok(&format!("[{label}] supervisor failure after the facet committed answers 500"), s == 500, &v);
    c.ok(&format!("[{label}] the facet's write stands"), count(api)? == before + 1, before + 1);
    let (s, v, _) = api.op(cell, json!({ "id": "f1", "name": "add_todo", "input": { "text": "committed" } }))?;
    c.ok(&format!("[{label}] the retry is a replay from the facet ledger"), s == 200 && v["replayed"] == true, &v);
    c.ok(&format!("[{label}] the retry did not write again"), count(api)? == before + 1, before + 1);

    let (_, v, _) = api.op(cell, json!({ "id": "t", "name": "tables" }))?;
    let tables: Vec<String> = serde_json::from_value(v["result"]["tables"].clone()).unwrap_or_default();
    c.ok(&format!("[{label}] the facet cannot see supervisor tables"),
        tables.contains(&"todos".to_string()) && !tables.iter().any(|t| t == "ops" || t == "channel" || t == "code"), format!("{tables:?}"));
    let (_, v, _) = api.op(cell, json!({ "id": "g", "name": "egress" }))?;
    c.ok(&format!("[{label}] globalOutbound null refuses fetch"), v["result"]["reached"] == false, &v);
    let (_, v, _) = api.op(cell, json!({ "id": "al", "name": "alarm_attempt" }))?;
    c.ok(&format!("[{label}] a facet cannot set an alarm"), v["result"]["armed"] == false, &v);
    let (s, v, _) = api.op(cell, json!({ "id": "cap1", "name": "emit_query", "input": { "text": "from a query" } }))?;
    c.ok(&format!("[{label}] a query's capability call re-enters the supervisor"), s == 200 && v["result"]["seq"].as_i64().unwrap_or(0) > 0, &v);

    // alarm and a hibernatable WebSocket on the supervisor
    api.req(reqwest::Method::POST, &format!("/c/{cell}/alarm"), Some(json!({ "in_ms": 300 })))?;
    std::thread::sleep(Duration::from_millis(1500));
    let (_, v, _) = api.req(reqwest::Method::GET, &format!("/c/{cell}/alarms"), None)?;
    c.ok(&format!("[{label}] supervisor alarm fires"), v["alarms"].as_array().map_or(0, |a| a.len()) == 1, &v);
    let ws_url = format!("{}/c/{cell}/ws", api.base.replace("http://", "ws://"));
    let (mut ws, _) = tungstenite::connect(ws_url.as_str())?;
    ws.send(tungstenite::Message::text("hello"))?;
    let reply = ws.read()?;
    let seq = serde_json::from_str::<Value>(reply.to_text()?)?["seq"].as_i64().unwrap_or(0);
    c.ok(&format!("[{label}] WebSocket frame appended and answered"), seq > 0, reply);
    ws.close(None).ok();
    Ok(())
}

fn warm(api: &Api, source: &str) -> Result<Value> {
    let cell = "warm";
    api.install(cell, source, "v1", None)?;
    api.op(cell, json!({ "id": "w0", "name": "add_todo", "input": { "text": "warmup" } }))?;
    let mut noop = vec![];
    let mut ping = vec![];
    let mut mutation = vec![];
    let mut query = vec![];
    for i in 0..WARM_SAMPLES {
        noop.push(ms(api.req(reqwest::Method::GET, &format!("/c/{cell}/noop"), None)?.2));
        ping.push(ms(api.req(reqwest::Method::POST, &format!("/c/{cell}/ping"), None)?.2));
        let (s, _, t) = api.op(cell, json!({ "id": format!("m{i}"), "name": "add_todo", "input": { "text": format!("todo {i}") } }))?;
        assert_eq!(s, 200, "warm mutation succeeds");
        mutation.push(ms(t));
        query.push(ms(api.op(cell, json!({ "id": "q", "name": "count" }))?.2));
    }
    Ok(json!({ "noop": summary(noop), "supervisor_write": summary(ping), "mutation": summary(mutation), "query": summary(query) }))
}

/// Objects the dev store received since `t0_ms`: (count, bytes).
fn written_since(project: &Path, t0_ms: u128) -> Result<(u64, u64)> {
    std::thread::sleep(Duration::from_secs(3)); // uploads trail the answer
    let q = format!("SELECT COUNT(*) || ' ' || COALESCE(SUM(LENGTH(body)), 0) FROM objects WHERE modified_ms >= {t0_ms}");
    let out = Command::new("sqlite3").arg("-readonly").arg(project.join(".celld/dev/objects.sqlite3")).arg(q).output()?;
    let text = String::from_utf8(out.stdout)?;
    let mut it = text.split_whitespace().map(|n| n.parse::<u64>());
    Ok((it.next().context("count")??, it.next().context("bytes")??))
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after 1970").as_millis()
}

fn facet_cost(api: &Api, source: &str, project: &Path) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for mib in [1u64, 16, 64] {
        let cell = format!("facet{mib}");
        api.install(&cell, source, "v1", None)?;
        let mut bytes = 0;
        let mut step = 0;
        while bytes < mib * 1024 * 1024 {
            let (s, v, _) = api.op(&cell, json!({ "id": format!("fill{step}"), "name": "fill", "input": { "mib": mib } }))?;
            if s != 200 {
                bail!("fill {mib} MiB: {s} {v}");
            }
            bytes = v["result"]["bytes"].as_u64().expect("fill returns bytes");
            step += 1;
        }
        let mut mutation = vec![];
        let t0 = now_ms();
        for i in 0..FACET_SAMPLES {
            let (s, v, t) = api.op(&cell, json!({ "id": format!("m{i}"), "name": "add_todo", "input": { "text": "x" } }))?;
            assert_eq!(s, 200, "facet-cost mutation succeeds: {v}");
            mutation.push(ms(t));
        }
        let (m_objects, m_bytes) = written_since(project, t0)?;
        let mut ping = vec![];
        let t0 = now_ms();
        for _ in 0..FACET_SAMPLES {
            ping.push(ms(api.req(reqwest::Method::POST, &format!("/c/{cell}/ping"), None)?.2));
        }
        let (p_objects, p_bytes) = written_since(project, t0)?;
        out.insert(format!("{mib}MiB"), json!({
            "facet_bytes": bytes,
            "mutation": summary(mutation),
            "mutation_replicated_bytes_per_op": m_bytes / FACET_SAMPLES as u64,
            "mutation_objects": m_objects,
            "supervisor_write": summary(ping),
            "supervisor_write_replicated_bytes_per_op": p_bytes / FACET_SAMPLES as u64,
            "supervisor_write_objects": p_objects,
        }));
    }
    Ok(Value::Object(out))
}

fn cpu_limit(api: &Api, source: &str, c: &mut Checks, label: &str) -> Result<Value> {
    let cell = "cpu";
    api.install(cell, source, "cpu", Some(200))?;
    let t0 = Instant::now();
    let (s, v, t) = match api.op(cell, json!({ "id": "spin", "name": "spin_forever" })) {
        Ok(r) => r,
        Err(e) => {
            c.ok(&format!("[{label}] cpuMs stops a runaway facet call"), false, format!("no answer after {:?}: {e:#}", t0.elapsed()));
            return Ok(json!({ "stopped": false, "no_answer_after_ms": ms(t0.elapsed()) }));
        }
    };
    c.ok(&format!("[{label}] cpuMs stops a runaway facet call"), s == 422 && t < Duration::from_secs(10), format!("{s} {v} after {t:?}"));
    let (after, v_after, _) = api.op(cell, json!({ "id": "after", "name": "count" }))?;
    c.ok(&format!("[{label}] the cell answers after a cpuMs stop"), after == 200, &v_after);
    Ok(json!({ "cpu_ms": 200, "spin_status": s, "stopped_after_ms": ms(t), "spin_message": v["message"], "next_call_status": after }))
}

fn cold(project: &Path, port: u16, source: &str, env: &[(&str, String)]) -> Result<Value> {
    // A populated cell to wake, from a fresh state.
    let (d, _) = Celld::start(project, port, true, env)?;
    let api = Api::new(&d.base);
    api.install("cold", source, "v1", None)?;
    api.op("cold", json!({ "id": "c0", "name": "add_todo", "input": { "text": "seed" } }))?;
    d.stop()?;

    let mut startup = vec![];
    let mut first_noop = vec![];
    let mut first_query = vec![];
    for _ in 0..COLD_RUNS {
        let (d, s) = Celld::start(project, port, false, env)?;
        startup.push(ms(s));
        let api = Api::new(&d.base);
        first_noop.push(ms(api.req(reqwest::Method::GET, "/c/cold/noop", None)?.2));
        let (st, v, t) = api.op("cold", json!({ "id": "q", "name": "count" }))?;
        assert!(st == 200 && v["result"]["n"] == 1, "restart keeps the facet's data: {v}");
        first_query.push(ms(t));
        d.stop()?;
    }
    // Idle eviction with the process warm.
    let mut e = env.to_vec();
    e.push(("CELLD_IDLE_EVICT_S", EVICT_S.to_string()));
    let (d, _) = Celld::start(project, port, false, &e)?;
    let api = Api::new(&d.base);
    api.req(reqwest::Method::GET, "/c/cold/noop", None)?;
    let mut evicted_noop = vec![];
    let mut evicted_query = vec![];
    for _ in 0..COLD_RUNS {
        std::thread::sleep(Duration::from_secs(EVICT_S * 3));
        evicted_noop.push(ms(api.req(reqwest::Method::GET, "/c/cold/noop", None)?.2));
        std::thread::sleep(Duration::from_secs(EVICT_S * 3));
        evicted_query.push(ms(api.op("cold", json!({ "id": "q", "name": "count" }))?.2));
    }
    d.stop()?;
    Ok(json!({
        "process_start_to_ready": summary(startup),
        "first_request_after_restart_noop": summary(first_noop),
        "first_request_after_restart_facet_query": summary(first_query),
        "after_idle_eviction_noop": summary(evicted_noop),
        "after_idle_eviction_facet_query": summary(evicted_query),
    }))
}

fn restart_checks(project: &Path, port: u16, source: &str, env: &[(&str, String)], c: &mut Checks, label: &str) -> Result<()> {
    let (d, _) = Celld::start(project, port, true, env)?;
    let api = Api::new(&d.base);
    api.install("restart", source, "v1", None)?;
    let (_, v, _) = api.op("restart", json!({ "id": "r1", "name": "add_todo", "input": { "text": "survives" } }))?;
    let stored = v["result"].clone();
    d.stop()?;
    let (d, _) = Celld::start(project, port, false, env)?;
    let api = Api::new(&d.base);
    let (s, v, _) = api.op("restart", json!({ "id": "r1", "name": "add_todo", "input": { "text": "survives" } }))?;
    c.ok(&format!("[{label}] restart: replay still returns the stored result"), s == 200 && v["replayed"] == true && v["result"] == stored, &v);
    let (_, v, _) = api.op("restart", json!({ "id": "q", "name": "list" }))?;
    c.ok(&format!("[{label}] restart: the facet's rows survive"), v["result"]["todos"].as_array().map_or(0, |a| a.len()) == 1, &v);
    d.stop()
}

fn bundle(project: &Path) -> Value {
    let size = |p: PathBuf| fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    let gz = |p: PathBuf| -> u64 {
        Command::new("gzip").args(["-9", "-c"]).arg(&p).output().map(|o| o.stdout.len() as u64).unwrap_or(0)
    };
    json!({
        "wasm_bytes": size(project.join("build/index_bg.wasm")),
        "wasm_gzip_bytes": gz(project.join("build/index_bg.wasm")),
        "glue_js_bytes": size(project.join("build/index.js")),
        "shim_js_bytes": size(project.join("entry.mjs")),
        "baseline_js_bytes": size(project.join("index.js")),
    })
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        bail!("usage: spike-driver <spikes-dir> <out.json>");
    }
    let spikes = PathBuf::from(&args[1]).canonicalize()?;
    let source = fs::read_to_string(spikes.join("apps/todo.mjs"))?;
    let platform = fs::read_to_string(spikes.join("apps/platform.mjs"))?;
    if fs::read_to_string(spikes.join("cells-js/platform.txt"))? != platform {
        bail!("cells-js/platform.txt differs from apps/platform.mjs; copy it again");
    }
    let esbuild = std::env::var("CELLD_ESBUILD").context("set CELLD_ESBUILD to an esbuild binary")?;
    let env = [("CELLD_ESBUILD", esbuild)];
    let mut checks = Checks::default();
    let mut report = serde_json::Map::new();
    for (label, dir, port) in [("rs", "cells-rs", 8811u16), ("js", "cells-js", 8812u16)] {
        let project = spikes.join(dir);
        println!("\n# {label}");
        let (d, _) = Celld::start(&project, port, true, &env)?;
        let api = Api::new(&d.base);
        functional(&api, &source, &mut checks, label)?;
        let warm = warm(&api, &source)?;
        let facet = facet_cost(&api, &source, &project)?;
        // last: an unenforced limit leaves a spinning isolate behind
        let cpu = cpu_limit(&api, &source, &mut checks, label)?;
        d.stop()?;
        restart_checks(&project, port, &source, &env, &mut checks, label)?;
        let cold = cold(&project, port, &source, &env)?;
        report.insert(label.into(), json!({ "bundle": bundle(&project), "cpu_limit": cpu, "warm": warm, "facet_image": facet, "cold": cold }));
    }
    report.insert("checks".into(), json!({ "passed": checks.passed, "failed": checks.failed }));
    fs::write(&args[2], serde_json::to_string_pretty(&Value::Object(report))?)?;
    println!("\n{} passed, {} failed", checks.passed, checks.failed.len());
    if !checks.failed.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
