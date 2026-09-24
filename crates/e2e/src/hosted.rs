//! The e2e against a hosted fleet (`cargo xtask e2e --fleet <fleet>`): the
//! path a person takes, with nothing faked: the fleet's own nodes and
//! bucket, the real code.storage, OpenRouter, and the open internet. Its
//! fragments are named `e2e-<run>-…`, made by the fleet's e2e key (which the
//! fleet lists in `FRAGMENT_CREATORS`), and deleted with their repos after.
//!
//! Inputs come from xtask, as paths to files (secret values never pass
//! through arguments or output): `FRAGMENT_E2E_HOSTED` (the fleet's URL),
//! `FRAGMENT_E2E_SUFFIX`, `FRAGMENT_E2E_KEY_FILE`, and optionally
//! `FRAGMENT_E2E_OPENROUTER_KEY_FILE` (the live AI check) and
//! `FRAGMENT_E2E_CODESTORAGE_{ORG,API,KEY_FILE}` (removing the repos).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use fragment_core::codestorage::{Claims, OrgKey};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{now_s, Api, Call, Reply};
use crate::browser::Browser;

const EGRESS_APP: &[u8] = include_bytes!("../fixtures/hosted_egress.mjs");
const EGRESS_JSON: &[u8] = include_bytes!("../fixtures/hosted_egress.json");
const AI_APP: &[u8] = include_bytes!("../fixtures/hosted_ai.mjs");
const AI_JSON: &[u8] = include_bytes!("../fixtures/hosted_ai.json");
const RUN_WAIT: Duration = Duration::from_secs(120);

struct CodeStorage {
    org: String,
    api: String,
    key: OrgKey,
}

pub struct Hosted {
    passed: usize,
    failed: Vec<String>,
    only: Option<String>,
    run: String,
    cli: PathBuf,
    scratch: PathBuf,
    api: Api,
    key: Keys,
    home: PathBuf,
    openrouter: Option<String>,
    cs: Option<CodeStorage>,
    /// (name, repo) of each fragment this run made, removed at the end.
    made: Vec<(String, String)>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn read(path: &str) -> Result<String> {
    Ok(std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?.trim().to_string())
}

impl Hosted {
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

    fn note(&self, label: &str, value: impl std::fmt::Display) {
        println!("note  {label}: {value}");
    }

    fn name(&self, base: &str) -> String {
        format!("e2e-{}-{base}", self.run)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.scratch.join(format!("{name}-{}", self.run));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch dir");
        d
    }

    fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(&self.cli)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("FRAGMENT_HOST", &self.api.base)
            .env_remove("FRAGMENT_OUTPUT")
            .output()
            .expect("run the fragment CLI")
    }

    fn cli_json(&self, cwd: &Path, args: &[&str]) -> Result<Value> {
        let out = self.cli(cwd, args);
        let v: Value = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("fragment {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))?;
        if v["ok"] != true {
            bail!("fragment {args:?}: {v}");
        }
        Ok(v["data"].clone())
    }

    fn signed(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
        self.api.signed(&self.key, method, path, body)
    }

    fn remember(&mut self, name: &str) {
        let repo = self.signed("GET", &format!("/api/f/{name}/status"), None).ok().and_then(|r| r.body["repo"].as_str().map(str::to_string));
        self.note(&format!("{name}'s repo"), repo.as_deref().unwrap_or("?"));
        self.made.push((name.to_string(), repo.unwrap_or_default()));
    }

    /// A job's run, once it has finished (or the last state seen).
    fn settle(&self, name: &str, started: &Reply) -> Value {
        let Some(run) = started.body["result"]["run"].as_u64() else { return started.body.clone() };
        let t0 = Instant::now();
        loop {
            let r = self.signed("GET", &format!("/api/f/{name}/runs/{run}"), None);
            if let Ok(r) = &r {
                let status = r.body["status"].as_str().unwrap_or("");
                if status == "succeeded" || status == "held" || t0.elapsed() > RUN_WAIT {
                    return r.body.clone();
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn eventually(&self, timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }
}

pub fn run(cli: PathBuf, scratch: PathBuf, only: Option<String>) -> Result<()> {
    let base = env("FRAGMENT_E2E_HOSTED").context("FRAGMENT_E2E_HOSTED is unset")?;
    let suffix = env("FRAGMENT_E2E_SUFFIX");
    let secret = read(&env("FRAGMENT_E2E_KEY_FILE").context("FRAGMENT_E2E_KEY_FILE is unset")?)?;
    let key = Keys::from_secret_hex(&secret).context("the e2e key file does not hold a 64-hex secret key")?;
    let run = format!("{:x}", now_s() % 0xffffff);
    let home = scratch.join(format!("hosted-home-{run}"));
    for dir in ["Library/Application Support/fragment", ".config/fragment"] {
        std::fs::create_dir_all(home.join(dir))?;
        let path = home.join(dir).join("config.json");
        std::fs::write(&path, json!({ "secret_key": secret, "host": base }).to_string())?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    }
    let openrouter = env("FRAGMENT_E2E_OPENROUTER_KEY_FILE").map(|p| read(&p)).transpose()?;
    let cs = match (env("FRAGMENT_E2E_CODESTORAGE_ORG"), env("FRAGMENT_E2E_CODESTORAGE_API"), env("FRAGMENT_E2E_CODESTORAGE_KEY_FILE")) {
        (Some(org), Some(api), Some(file)) => Some(CodeStorage { org, api, key: OrgKey::from_pem(&read(&file)?).map_err(anyhow::Error::msg)? }),
        _ => None,
    };
    let mut h = Hosted {
        passed: 0,
        failed: vec![],
        only,
        run,
        cli,
        scratch,
        api: Api::remote(&base, suffix.as_deref()),
        key,
        home: home.clone(),
        openrouter,
        cs,
        made: vec![],
    };
    println!("hosted e2e against {base} (run {})", h.run);
    let result = (|| -> Result<()> {
        health(&mut h)?;
        // the enrolled key is a person on the fleet (registering again answers the same one)
        let id = h.api.register(&h.key)?;
        h.note("identity", id);
        creators(&mut h)?;
        todo(&mut h)?;
        inbox(&mut h)?;
        blobs(&mut h)?;
        egress(&mut h)?;
        ai(&mut h)?;
        Ok(())
    })();
    cleanup(&mut h);
    let _ = std::fs::remove_dir_all(&home);
    result?;
    println!("\n{} passed, {} failed", h.passed, h.failed.len());
    if !h.failed.is_empty() {
        for f in &h.failed {
            println!("  FAIL {f}");
        }
        std::process::exit(1);
    }
    Ok(())
}

fn health(h: &mut Hosted) -> Result<()> {
    if !h.section("health") {
        return Ok(());
    }
    let r = h.api.unsigned("GET", "/healthz", None)?;
    let deploy = r.header("x-fragment-deploy");
    h.ok("the fleet answers, naming its deployment", r.status == 200 && !deploy.is_empty() && deploy != "dev", &r);
    h.note("deployment", deploy);
    let r = h.api.unsigned("GET", "/.well-known/celld/health", None)?;
    h.ok("celld reports the node healthy", r.status == 200, &r);
    Ok(())
}

fn creators(h: &mut Hosted) -> Result<()> {
    if !h.section("creators") {
        return Ok(());
    }
    let r = h.api.create(&Keys::generate(), &h.name("stranger"))?;
    h.ok("a key no one registered cannot create", r.status == 401, &r);
    let r = h.api.create(&h.api.person()?, &h.name("stranger"))?;
    h.ok("a person the fleet does not list cannot create", r.status == 403 && r.message().contains("by invitation"), &r);
    Ok(())
}

fn todo(h: &mut Hosted) -> Result<()> {
    if !h.section("todo") {
        return Ok(());
    }
    let name = h.name("todo");
    let dir = h.dir("todo");
    let t0 = Instant::now();
    let init = h.cli_json(&dir, &["init", &name, "--template", "todo", "--json"]);
    let took = t0.elapsed();
    h.remember(&name);
    let init = init?;
    let share = init["shareLink"].as_str().unwrap_or("").to_string();
    h.ok("fragment init makes, fills, and deploys a fragment (the real code.storage)", share.starts_with("https://"), &init);
    h.note("init (create + sync + deploy)", format!("{took:.1?}"));

    let r = h.api.call(Call { method: "GET", url: share.clone(), ..Call::default() })?;
    h.ok("its page is served over https", r.status == 200 && r.text.contains("<"), &r);
    let add = |h: &Hosted, id: &str, text: &str| h.api.op(&h.key, &name, "add", id, json!({ "text": text }));
    let first = add(h, "t1", "from the hosted e2e")?;
    h.ok("a mutation runs", first.status == 200 && first.body["result"]["id"].as_i64().is_some(), &first);
    let again = add(h, "t1", "from the hosted e2e")?;
    h.ok("the same id replays it", again.body["replayed"] == true && again.body["result"] == first.body["result"], &again);
    let other = add(h, "t1", "another body")?;
    h.ok("the same id with another body is 409", other.status == 409, &other);

    // durable writes on the fleet: sequential mutations, each answered once
    // replicated (two nodes: the fleet proof)
    let mut ms: Vec<u128> = vec![];
    for i in 0..20 {
        let t = Instant::now();
        let r = add(h, &format!("lat-{i}"), &format!("latency {i}"))?;
        if r.status == 200 {
            ms.push(t.elapsed().as_millis());
        }
    }
    ms.sort_unstable();
    let p = |q: f64| ms.get(((ms.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0);
    h.ok("twenty mutations in a row all land", ms.len() == 20, ms.len());
    h.note("mutation round trip from here (p50 / p95 / max)", format!("{} / {} / {} ms", p(0.5), p(0.95), p(1.0)));

    let Some(mut chrome) = Browser::launch(&h.scratch)? else {
        h.ok("Chrome is installed for the browser check (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(20);
    let a = chrome.open(&share)?;
    h.ok("the page connects over wss", chrome.until(&a, "document.getElementById('here')?.textContent === 'just you here'", wait), "");
    let b = chrome.open(&share)?;
    h.ok("a second page sees the first", chrome.until(&a, "document.getElementById('here').textContent === 'you and 1 other here'", wait), "");
    chrome.eval(&a, "document.getElementById('text').value = 'milk'; document.getElementById('add').requestSubmit(); true")?;
    h.ok("adding in one page shows it live in the other", chrome.until(&b, "[...document.querySelectorAll('#todos li span')].some(s => s.textContent === 'milk')", wait), "");
    Ok(())
}

fn inbox(h: &mut Hosted) -> Result<()> {
    if !h.section("inbox") {
        return Ok(());
    }
    let name = h.name("inbox");
    let dir = h.dir("inbox");
    let init = h.cli_json(&dir, &["init", &name, "--template", "inbox", "--json"]);
    h.remember(&name);
    let hook = init?["webhookUrl"].as_str().unwrap_or("").to_string();
    let r = h.api.call(Call {
        method: "POST",
        url: hook.clone(),
        body: Some(json!({ "source": "hosted-e2e", "payload": { "url": "https://example.com" } }).to_string().into_bytes()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    h.ok("a webhook delivery to the inbox is taken", r.status == 200 && r.body["ok"] == true, &r);
    let mut last = Value::Null;
    let landed = h.eventually(RUN_WAIT, || {
        last = h.api.op(&h.key, &name, "list", "q", json!({})).map(|r| r.body).unwrap_or_default();
        last["result"]["items"].as_array().is_some_and(|i| i.iter().any(|x| x["text"].as_str().is_some_and(|t| t.contains("Example Domain"))))
    });
    h.ok("its job fetched the page from the internet and recorded the title", landed, &last);
    let runs = h.signed("GET", &format!("/api/f/{name}/runs?op=ingest"), None)?;
    h.ok("the run succeeded", runs.body["runs"][0]["status"] == "succeeded", &runs);
    Ok(())
}

fn blobs(h: &mut Hosted) -> Result<()> {
    if !h.section("blobs") {
        return Ok(());
    }
    let name = h.name("blobs");
    let dir = h.dir("blobs");
    let big: Vec<u8> = (0..2 * 1024 * 1024 + 3).map(|i: usize| (i as u8).wrapping_mul(31).wrapping_add(7)).collect();
    std::fs::create_dir_all(dir.join("site"))?;
    std::fs::write(dir.join("site/index.html"), "<p>blobs</p>")?;
    std::fs::write(dir.join("site/big.bin"), &big)?;
    let created = h.cli_json(&dir, &["create", &name, "--json"]);
    h.remember(&name);
    let view = created?["viewToken"].as_str().unwrap_or("").to_string();
    let dir_s = dir.to_str().context("utf-8 path")?;
    let out = h.cli(&dir, &["deploy", &name, "--dir", dir_s]);
    h.ok("a folder with a 2 MiB file deploys", out.status.success(), String::from_utf8_lossy(&out.stderr));
    let files = h.signed("GET", &format!("/api/f/{name}/files"), None)?;
    let entry = files.body["files"].as_array().and_then(|a| a.iter().find(|f| f["path"] == "site/big.bin").cloned()).unwrap_or(Value::Null);
    h.ok("it is a blob, its pointer in git", entry["blob"] == true && entry["size"] == big.len(), &entry);
    let r = h.api.call(Call {
        method: "GET",
        url: format!("{}?view={view}", h.api.site_url(&name, "big.bin")),
        extra: vec![("range", "bytes=100-199".into())],
        ..Call::default()
    })?;
    h.ok("the site serves a range of its bytes from the fleet's bucket", r.status == 206 && r.bytes == big[100..200], format!("{} ({} bytes)", r.status, r.bytes.len()));
    let pulled = h.dir("blobs-pull");
    let out = h.cli(&pulled, &["sync", &name, "--dir", pulled.to_str().context("utf-8 path")?]);
    h.ok("another folder pulls the real bytes", out.status.success() && std::fs::read(pulled.join("site/big.bin")).is_ok_and(|b| b == big), String::from_utf8_lossy(&out.stderr));
    Ok(())
}

/// A job's fetch reaches the internet, never the fleet's private side, even
/// through a public name that resolves there (nip.io and sslip.io answer
/// the address a name spells).
fn egress(h: &mut Hosted) -> Result<()> {
    if !h.section("egress") {
        return Ok(());
    }
    let name = h.name("egress");
    let dir = h.dir("egress");
    std::fs::write(dir.join("app.mjs"), EGRESS_APP)?;
    std::fs::write(dir.join("fragment.json"), EGRESS_JSON)?;
    let created = h.cli_json(&dir, &["create", &name, "--json"]);
    h.remember(&name);
    created?;
    let out = h.cli(&dir, &["deploy", &name, "--dir", dir.to_str().context("utf-8 path")?]);
    h.ok("the probe app deploys", out.status.success(), String::from_utf8_lossy(&out.stderr));
    let probe = |h: &Hosted, id: &str, url: &str| -> Result<Value> {
        let started = h.api.op(&h.key, &name, "probe", id, json!({ "url": url }))?;
        Ok(h.settle(&name, &started))
    };
    let refused = |run: &Value| run["status"] == "succeeded" && run["output"]["error"].as_str().is_some_and(|e| e.contains("egress refused"));
    let run = probe(h, "loopback", "http://127.0.0.1.nip.io:8080/healthz")?;
    h.ok("a public name that resolves to loopback is refused, at once", refused(&run), &run);
    let run = probe(h, "6pn", "http://fdaa--3.sslip.io/")?;
    h.ok("a public name that resolves into Fly's private network is refused", refused(&run), &run);
    let run = probe(h, "public", "https://example.com/")?;
    h.ok("a public site is reached", run["output"]["status"] == 200, &run);
    Ok(())
}

fn ai(h: &mut Hosted) -> Result<()> {
    if !h.section("ai") {
        return Ok(());
    }
    let Some(key) = h.openrouter.clone() else {
        h.note("ai", "skipped: no FRAGMENT_E2E_OPENROUTER_KEY_FILE");
        return Ok(());
    };
    let name = h.name("ai");
    let dir = h.dir("ai");
    std::fs::write(dir.join("app.mjs"), AI_APP)?;
    std::fs::write(dir.join("fragment.json"), AI_JSON)?;
    let created = h.cli_json(&dir, &["create", &name, "--json"]);
    h.remember(&name);
    created?;
    let out = h.cli(&dir, &["deploy", &name, "--dir", dir.to_str().context("utf-8 path")?]);
    h.ok("the AI app deploys", out.status.success(), String::from_utf8_lossy(&out.stderr));
    let r = h.api.call(Call {
        method: "PUT",
        url: format!("{}/api/f/{name}/secrets/OPENROUTER_API_KEY", h.api.base),
        body: Some(key.into_bytes()),
        keys: Some(&h.key),
        ..Call::default()
    })?;
    h.ok("the fragment holds its own OpenRouter key", r.status == 200, r.status);

    let started = h.api.op(&h.key, &name, "summarize", "s1", json!({ "text": "In five words: what is a durable object?" }))?;
    let run = h.settle(&name, &started);
    h.ok("a live text call on the plan's model answers", run["status"] == "succeeded" && run["output"]["text"].as_str().is_some_and(|t| !t.trim().is_empty()), &run);
    h.note("text", format!("{} ({}; cost {})", run["output"]["text"], run["output"]["model"], run["output"]["usage"]["cost"]));

    let started = h.api.op(&h.key, &name, "draw", "d1", json!({ "prompt": "a small red circle on a plain white background", "path": "art/circle.png" }))?;
    let run = h.settle(&name, &started);
    h.ok("a live image is made and written to main", run["status"] == "succeeded" && run["output"]["size"].as_u64().is_some_and(|n| n > 1000), &run);
    let file = h.signed("GET", &format!("/api/f/{name}/file?path=art/circle.png"), None)?;
    h.ok("and its bytes are there", file.status == 200 && file.bytes.len() > 1000, format!("{} ({} bytes)", file.status, file.bytes.len()));
    h.note("image", format!("{} bytes, {}", run["output"]["size"], run["output"]["mediaType"]));
    Ok(())
}

/// Deletes this run's fragments, then their repos (the cell keeps a
/// deleted fragment's repo; a test run should leave nothing behind).
fn cleanup(h: &mut Hosted) {
    let made = std::mem::take(&mut h.made);
    let mut left = vec![];
    for (name, repo) in &made {
        match h.signed("DELETE", &format!("/api/f/{name}"), None) {
            Ok(r) if r.status == 200 || r.status == 404 => {}
            Ok(r) => left.push(format!("{name}: {}", r.status)),
            Err(e) => left.push(format!("{name}: {e}")),
        }
        let Some(cs) = &h.cs else { continue };
        if repo.is_empty() {
            continue;
        }
        let now = now_s();
        let token = cs.key.token(&Claims { iss: &cs.org, sub: "fragment-e2e", repo, scopes: &["repo:write"], iat: now, exp: now + 300 });
        let url = format!("{}/api/repos/{}", cs.api.trim_end_matches('/'), repo.replace('/', "%2F"));
        let deleted = reqwest::blocking::Client::new().delete(&url).bearer_auth(token).send().map(|r| r.status().as_u16());
        match deleted {
            Ok(200..=299) | Ok(404) => {}
            Ok(status) => left.push(format!("repo {repo}: {status}")),
            Err(e) => left.push(format!("repo {repo}: {e}")),
        }
    }
    if made.is_empty() {
        return;
    }
    if left.is_empty() {
        println!("\ncleaned up {} fragments{}", made.len(), if h.cs.is_some() { " and their repos" } else { "" });
    } else {
        println!("\ncleanup left: {}", left.join(", "));
    }
}
