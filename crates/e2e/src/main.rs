//! The fragment end-to-end suite: a real `celld dev` node serving the real
//! cell, called over signed HTTP the way the CLI calls it.
//!
//! `cargo xtask e2e [--only <section>]`. Each section makes its own
//! fragments, so any one can run alone. Every check prints `ok` or `FAIL`;
//! the process exits non-zero when any check fails.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use fragment_nip98::Keys;
use fragment_proto::{InstallCode, OpDecl, OpKind};
use serde_json::{json, Value};

const TODO_APP: &str = include_str!("../fixtures/todo.mjs");

fn now_s() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_secs() as i64
}

fn todo_install(sha: &str) -> InstallCode {
    let ops: BTreeMap<String, OpDecl> = [
        ("add_todo", OpKind::Mutation),
        ("add_then_throw", OpKind::Mutation),
        ("add_async", OpKind::Mutation),
        ("list", OpKind::Query),
        ("count", OpKind::Query),
    ]
    .into_iter()
    .map(|(n, kind)| (n.to_string(), OpDecl { kind }))
    .collect();
    InstallCode { sha: sha.into(), source: TODO_APP.into(), operations: ops, cpu_ms: None }
}

struct Reply {
    status: u16,
    body: Value,
}

impl std::fmt::Display for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.body)
    }
}

impl Reply {
    fn error(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }
}

struct Api {
    http: reqwest::blocking::Client,
    base: String,
}

impl Api {
    fn new(base: &str) -> Api {
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client");
        Api { http, base: base.to_string() }
    }

    fn send(&self, method: &str, path: &str, body: Option<&Value>, auth: Option<String>) -> Result<Reply> {
        let url = format!("{}{path}", self.base);
        let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
        let mut req = self.http.request(method.parse()?, &url).body(bytes);
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        let resp = req.send().with_context(|| format!("{method} {path}"))?;
        let status = resp.status().as_u16();
        let text = resp.text()?;
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(Reply { status, body })
    }

    /// A request signed by `keys` (NIP-98), as the CLI sends it.
    fn signed(&self, keys: &Keys, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
        let auth = keys.header(method, &format!("{}{path}", self.base), &bytes, now_s());
        self.send(method, path, body, Some(auth))
    }

    fn create(&self, keys: &Keys, name: &str) -> Result<Reply> {
        self.signed(keys, "POST", "/api/fragments", Some(&json!({ "name": name })))
    }

    fn install(&self, keys: &Keys, name: &str, code: &InstallCode) -> Result<Reply> {
        self.signed(keys, "PUT", &format!("/api/f/{name}/code"), Some(&serde_json::to_value(code)?))
    }

    fn op(&self, keys: &Keys, name: &str, op: &str, id: &str, input: Value) -> Result<Reply> {
        self.signed(keys, "POST", &format!("/api/f/{name}/ops/{op}"), Some(&json!({ "id": id, "input": input })))
    }

    fn count(&self, keys: &Keys, name: &str) -> Result<i64> {
        let r = self.op(keys, name, "count", "q", json!({}))?;
        r.body["result"]["n"].as_i64().with_context(|| format!("count: {r}"))
    }
}

struct Suite {
    only: Option<String>,
    passed: usize,
    failed: Vec<String>,
    tools: devstack::Tools,
    node: Option<devstack::Node>,
    port: u16,
    /// Distinguishes this run's fragment names from any earlier state.
    run: String,
}

impl Suite {
    fn section(&self, name: &str) -> bool {
        if self.only.as_deref().is_some_and(|o| o != name) {
            return false;
        }
        println!("\n# {name}");
        true
    }

    fn ok(&mut self, label: &str, cond: bool, detail: impl std::fmt::Display) {
        if cond {
            self.passed += 1;
            println!("ok    {label}");
        } else {
            self.failed.push(label.to_string());
            println!("FAIL  {label}: {detail}");
        }
    }

    fn name(&self, base: &str) -> String {
        format!("{base}-{}", self.run)
    }

    fn start(&mut self, clean: bool) -> Result<Api> {
        assert!(self.node.is_none(), "one node at a time");
        let opts = devstack::NodeOptions { port: self.port, clean, watch: false, env: vec![] };
        let (node, _) = devstack::Node::start(&self.tools, &opts)?;
        let api = Api::new(&node.base);
        self.node = Some(node);
        Ok(api)
    }

    fn stop(&mut self) -> Result<()> {
        self.node.take().expect("a running node").stop()
    }

    fn crash(&mut self) -> Result<()> {
        self.node.take().expect("a running node").crash()
    }
}

fn auth(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("auth") {
        return Ok(());
    }
    let keys = Keys::generate();
    let name = s.name("auth");
    let body = json!({ "name": name });
    let r = api.send("POST", "/api/fragments", Some(&body), None)?;
    s.ok("an unsigned create is 401", r.status == 401 && r.error() == "unauthenticated", &r);
    let bytes = body.to_string().into_bytes();
    let elsewhere = keys.header("POST", &format!("{}/api/f/{name}/status", api.base), &bytes, now_s());
    let r = api.send("POST", "/api/fragments", Some(&body), Some(elsewhere))?;
    s.ok("a signature for another URL is 401", r.status == 401, &r);
    let stale = keys.header("POST", &format!("{}/api/fragments", api.base), &bytes, now_s() - 120);
    let r = api.send("POST", "/api/fragments", Some(&body), Some(stale))?;
    s.ok("a two-minute-old signature is 401", r.status == 401, &r);
    let other_body = keys.header("POST", &format!("{}/api/fragments", api.base), br#"{"name":"x"}"#, now_s());
    let r = api.send("POST", "/api/fragments", Some(&body), Some(other_body))?;
    s.ok("a signature over another body is 401", r.status == 401, &r);
    Ok(())
}

fn create(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("create") {
        return Ok(());
    }
    let owner = Keys::generate();
    let other = Keys::generate();
    let name = s.name("create");
    let r = api.create(&owner, &name)?;
    s.ok("a signed create succeeds", r.status == 200 && r.body["owner"] == owner.pubkey_hex(), &r);
    let r = api.create(&other, &name)?;
    s.ok("creating an existing name is 409", r.status == 409 && r.error() == "already_exists", &r);
    let r = api.create(&owner, "Bad_Name")?;
    s.ok("an invalid name is 400", r.status == 400 && r.error() == "invalid_request", &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/status"), None)?;
    s.ok("the owner reads status", r.status == 200 && r.body["name"] == name.as_str() && r.body["code_sha"].is_null(), &r);
    let r = api.signed(&other, "GET", &format!("/api/f/{name}/status"), None)?;
    s.ok("another key reading status is 403", r.status == 403 && r.error() == "forbidden", &r);
    let r = api.install(&other, &name, &todo_install("v1"))?;
    s.ok("another key installing code is 403", r.status == 403, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{}/status", s.name("nobody")), None)?;
    s.ok("an unknown fragment is 404", r.status == 404 && r.error() == "not_found", &r);
    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "x" }))?;
    s.ok("an operation before any code is 404 no_code", r.status == 404 && r.error() == "no_code", &r);
    Ok(())
}

fn ops(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("ops") {
        return Ok(());
    }
    let owner = Keys::generate();
    let other = Keys::generate();
    let name = s.name("ops");
    api.create(&owner, &name)?;
    let r = api.install(&owner, &name, &todo_install("v1"))?;
    s.ok("installing code succeeds", r.status == 200 && r.body["code_sha"] == "v1", &r);

    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "one" }))?;
    s.ok("a mutation runs", r.status == 200 && r.body["replayed"] == false && r.body["result"]["id"] == 1, &r);
    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "one" }))?;
    s.ok("a replay returns the stored result", r.status == 200 && r.body["replayed"] == true && r.body["result"]["id"] == 1, &r);
    s.ok("a replay does not write again", api.count(&owner, &name)? == 1, "count");
    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "two" }))?;
    s.ok("a conflicting body is 409", r.status == 409 && r.error() == "conflicting_body", &r);
    let r = api.op(&owner, &name, "add_todo", "bad id", json!({ "text": "x" }))?;
    s.ok("an invalid operation id is 400", r.status == 400, &r);
    let r = api.op(&owner, &name, "nope", "n1", json!({}))?;
    s.ok("an undeclared operation is 404", r.status == 404 && r.error() == "unknown_operation", &r);
    let r = api.op(&owner, &name, "add_todo", "e1", json!({ "text": "" }))?;
    s.ok("an author refusal is 422", r.status == 422 && r.error() == "app_failed", &r);
    let r = api.op(&owner, &name, "add_todo", "e1", json!({ "text": "now valid" }))?;
    s.ok("a refused id stays unused", r.status == 200 && r.body["replayed"] == false, &r);

    let before = api.count(&owner, &name)?;
    let r = api.op(&owner, &name, "add_then_throw", "t1", json!({ "text": "rolled back" }))?;
    s.ok("a throw after a write is 422", r.status == 422, &r);
    s.ok("a throw rolls its write back", api.count(&owner, &name)? == before, before);
    let r = api.op(&owner, &name, "add_async", "as1", json!({ "text": "async" }))?;
    s.ok(
        "an async mutation is refused",
        r.status == 422 && r.body["message"].as_str().unwrap_or("").contains("mutations are synchronous"),
        &r,
    );
    s.ok("an async mutation's write rolls back", api.count(&owner, &name)? == before, before);

    let big = "x".repeat(300 * 1024);
    let r = api.op(&owner, &name, "add_todo", "big", json!({ "text": big }))?;
    s.ok("an input over 256 KiB is 413", r.status == 413 && r.error() == "too_large", &r);
    let r = api.op(&owner, &name, "list", "q1", json!({}))?;
    s.ok("a query answers", r.status == 200 && r.body["result"]["todos"].as_array().map_or(0, |a| a.len()) == 2, &r);
    let r = api.op(&other, &name, "list", "q2", json!({}))?;
    s.ok("another key calling an operation is 403", r.status == 403, &r);

    let r = api.install(&owner, &name, &todo_install("v2"))?;
    s.ok("reinstalling keeps the app's data", r.status == 200 && api.count(&owner, &name)? == 2, &r);
    Ok(())
}

/// State survives a graceful restart and a crash of the node.
fn restart(s: &mut Suite, api: Api) -> Result<Api> {
    if !s.section("restart") {
        return Ok(api);
    }
    let owner = Keys::generate();
    let name = s.name("restart");
    api.create(&owner, &name)?;
    api.install(&owner, &name, &todo_install("v1"))?;
    let first = api.op(&owner, &name, "add_todo", "r1", json!({ "text": "survives" }))?;

    s.stop()?;
    let api = s.start(false)?;
    let r = api.op(&owner, &name, "add_todo", "r1", json!({ "text": "survives" }))?;
    s.ok("after a restart the replay returns the stored result", r.body["replayed"] == true && r.body["result"] == first.body["result"], &r);
    s.ok("after a restart the app's rows survive", api.count(&owner, &name)? == 1, "count");

    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("a mutation before the crash", r.status == 200, &r);
    s.crash()?;
    let api = s.start(false)?;
    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("after a crash an acknowledged mutation replays", r.body["replayed"] == true, &r);
    s.ok("after a crash no acknowledged write is lost", api.count(&owner, &name)? == 2, "count");
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/status"), None)?;
    s.ok("after a crash the owner and code survive", r.body["owner"] == owner.pubkey_hex() && r.body["code_sha"] == "v1", &r);
    Ok(api)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let only = match args.as_slice() {
        [] => None,
        [flag, section] if flag == "--only" => Some(section.clone()),
        _ => bail!("usage: fragment-e2e [--only <section>]"),
    };
    let tools = devstack::Tools::locate()?;
    devstack::write_dev_vars(&[])?;
    let run = format!("{:x}", now_s() % 0xffffff);
    let mut s = Suite { only, passed: 0, failed: vec![], tools, node: None, port: devstack::free_port()?, run };
    let api = s.start(true)?;
    auth(&mut s, &api)?;
    create(&mut s, &api)?;
    ops(&mut s, &api)?;
    restart(&mut s, api)?;
    s.stop()?;
    println!("\n{} passed, {} failed", s.passed, s.failed.len());
    if !s.failed.is_empty() {
        for f in &s.failed {
            println!("  FAIL {f}");
        }
        std::process::exit(1);
    }
    Ok(())
}
