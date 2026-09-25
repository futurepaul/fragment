//! Jobs and triggers (slice D): job runs as Workflows with durable steps,
//! a fetch that carries a secret it never shows, retries, held runs and
//! replays, sleeps; triggers from the inbox, a channel, files, and cron;
//! the hop budget, auto-pause, and the inbox cap.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::http::{Handler, Response, Server};
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{Api, Call, Reply};
use crate::Suite;

const JOBS_APP: &[u8] = include_bytes!("../../fixtures/jobs.mjs");
const JOBS_JSON: &[u8] = include_bytes!("../../fixtures/jobs.json");
const SECRET: &str = "sk-e2e-upstream-7f3a91";
const INBOX_FILES: [(&str, &[u8]); 4] = [
    ("fragment.json", include_bytes!("../../../../templates/inbox/fragment.json")),
    ("app.mjs", include_bytes!("../../../../templates/inbox/app.mjs")),
    ("site/index.html", include_bytes!("../../../../templates/inbox/site/index.html")),
    ("README.md", include_bytes!("../../../../templates/inbox/README.md")),
];

/// A request the upstream got.
#[derive(Debug, Clone)]
struct Hit {
    path: String,
    authorization: String,
    hops: String,
    /// JSON bodies (a delivery's), else null.
    body: Value,
}

/// What a job's fetch reaches: records what arrived, answers per path.
struct Upstream {
    server: Server,
    seen: Arc<Mutex<Vec<Hit>>>,
}

impl Upstream {
    fn start() -> Result<Upstream> {
        let seen: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let flaky = Arc::new(AtomicUsize::new(0));
        let log = Arc::clone(&seen);
        let handler: Handler = Arc::new(move |req| {
            let header = |k: &str| req.header(k).unwrap_or("").to_string();
            log.lock().expect("upstream log").push(Hit {
                path: req.path.clone(),
                authorization: header("authorization"),
                hops: header("x-fragment-hops"),
                body: serde_json::from_slice(&req.body).unwrap_or(Value::Null),
            });
            match req.path.as_str() {
                "/data" => Response::json(200, &json!({ "items": ["alpha", "beta"] })),
                "/once" => Response::json(200, &json!({ "items": ["once"] })),
                "/flaky" if flaky.fetch_add(1, Ordering::SeqCst) < 2 => Response::json(503, &json!({ "error": "busy" })),
                "/flaky" => Response::json(200, &json!({ "items": ["gamma"] })),
                "/down" => Response::json(503, &json!({ "error": "down" })),
                "/page" => Response::bytes(200, "text/html", b"<html><head><title> A page to title </title></head></html>".to_vec()),
                "/moved" => Response::json(302, &json!({})).with_header("location", "http://127.0.0.1:1/private"),
                _ => Response::json(404, &json!({ "error": "no" })),
            }
        });
        Ok(Upstream { server: Server::start(0, handler)?, seen })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.url)
    }

    fn hits(&self, path: &str) -> Vec<Hit> {
        self.seen.lock().expect("upstream log").iter().filter(|h| h.path == path).cloned().collect()
    }
}

/// A fragment running the jobs fixture, with `manifest` edits applied.
pub(super) fn jobs_fragment(s: &Suite, api: &Api, owner: &Keys, base: &str, edit: impl FnOnce(&mut Value)) -> Result<(String, Value)> {
    let name = s.named(api, owner, base)?;
    let c = s.create(api, owner, &name)?;
    let mut manifest: Value = serde_json::from_slice(JOBS_JSON)?;
    edit(&mut manifest);
    ship(s, &c, JOBS_APP, manifest.to_string().as_bytes());
    Ok((name, c))
}

fn run(api: &Api, keys: &Keys, name: &str, id: i64) -> Result<Reply> {
    api.signed(keys, "GET", &format!("/api/f/{name}/runs/{id}"), None)
}

/// Waits for a run to reach one of `want`, answering it as last read.
pub(super) fn settle(api: &Api, keys: &Keys, name: &str, id: i64, want: &[&str], timeout: Duration) -> Value {
    let t0 = Instant::now();
    loop {
        let r = run(api, keys, name, id).map(|r| r.body).unwrap_or(Value::Null);
        if want.iter().any(|w| r["status"] == *w) || t0.elapsed() > timeout {
            return r;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn runs(api: &Api, keys: &Keys, name: &str, query: &str) -> Vec<Value> {
    api.signed(keys, "GET", &format!("/api/f/{name}/runs?limit=200{query}"), None)
        .ok()
        .and_then(|r| r.body["runs"].as_array().cloned())
        .unwrap_or_default()
}

/// The newest 500 events.
fn events(api: &Api, keys: &Keys, name: &str) -> Vec<Value> {
    let last = api
        .signed(keys, "GET", &format!("/api/f/{name}/channels"), None)
        .ok()
        .and_then(|r| r.body["channels"].as_array().and_then(|a| a.iter().find(|c| c["name"] == "events").and_then(|c| c["seq"].as_i64())))
        .unwrap_or(0);
    api.signed(keys, "GET", &format!("/api/f/{name}/events?since={}", (last - 500).max(0)), None)
        .ok()
        .and_then(|r| r.body["events"].as_array().cloned())
        .unwrap_or_default()
}

pub(super) fn records(api: &Api, keys: &Keys, name: &str, channel: &str) -> Vec<Value> {
    api.signed(keys, "GET", &format!("/api/f/{name}/channels/{channel}"), None)
        .ok()
        .and_then(|r| r.body["records"].as_array().cloned())
        .unwrap_or_default()
}

pub(super) fn inbox(api: &Api, name: &str, token: &str, body: &Value, hops: Option<u32>) -> Result<Reply> {
    let mut extra = vec![("x-fragment-inbox-token", token.to_string())];
    if let Some(h) = hops {
        extra.push(("x-fragment-hops", h.to_string()));
    }
    api.call(Call {
        method: "POST",
        url: format!("{}/api/f/{name}/inbox", api.base),
        body: Some(body.to_string().into_bytes()),
        content_type: Some("application/json"),
        extra,
        ..Call::default()
    })
}

pub(super) fn started(r: &Reply) -> i64 {
    r.body["result"]["run"].as_i64().unwrap_or(0)
}

/// A fragment whose cron trigger fires every minute, for the triggers
/// section to check.
pub struct Cron {
    owner: Keys,
    name: String,
    deployed: Instant,
}

/// Deploys the cron fragment, when the triggers section will run. The
/// suite calls this after its last node restart before that section, so in
/// a full run a minute boundary has passed by the time it is checked, on
/// the real alarm; alone, the section waits for one.
pub fn cron(s: &Suite, api: &Api) -> Result<Option<Cron>> {
    if !s.runs("triggers") {
        return Ok(None);
    }
    let owner = api.person()?;
    let (name, _) = jobs_fragment(s, api, &owner, "cron", |m| m["triggers"] = json!([{ "cron": "* * * * *", "run": "tick" }]))?;
    Ok(Some(Cron { owner, name, deployed: Instant::now() }))
}

pub fn jobs(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("jobs") {
        return Ok(());
    }
    let upstream = Upstream::start()?;
    let owner = api.person()?;
    let viewer = api.person()?;
    let (name, _) = jobs_fragment(s, api, &owner, "jobs", |_| {})?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    api.call(Call {
        method: "PUT",
        url: format!("{}/api/f/{name}/secrets/API_KEY", api.base),
        body: Some(SECRET.as_bytes().to_vec()),
        keys: Some(&owner),
        ..Call::default()
    })?;
    let long = Duration::from_secs(40);

    // a job is started by a call, and a second call with its id is the same run
    let input = json!({ "url": upstream.url("/data") });
    let r = api.op(&owner, &name, "digest", "d1", input.clone())?;
    let id = started(&r);
    s.ok("calling a job answers its run", r.status == 200 && id > 0 && r.body["replayed"] == false, &r);
    let r = api.op(&owner, &name, "digest", "d1", input.clone())?;
    s.ok("calling again with the id is the same run (replayed)", started(&r) == id && r.body["replayed"] == true, &r);
    let r = api.op(&owner, &name, "digest", "d1", json!({ "url": upstream.url("/other") }))?;
    s.ok("the id with another input is 409", r.status == 409 && r.error() == "conflicting_body", &r);
    let r = api.op(&owner, &name, "digest", "d2", json!({}))?;
    s.ok("a job's input is checked against its schema before it runs", r.status == 400 && r.message().contains("/url"), &r);

    let done = settle(api, &owner, &name, id, &["succeeded", "held"], long);
    s.ok("the job runs to the end", done["status"] == "succeeded" && done["output"]["saved"] == 2 && done["output"]["status"] == 200, &done);
    s.ok("the run records who, how, and what", done["op"] == "digest" && done["via"] == "call" && done["input"] == input && done["attempt"] == 1, &done);
    let hits = upstream.hits("/data");
    s.ok("its fetch reached the upstream once", hits.len() == 1, format!("{hits:?}"));
    s.ok("the upstream got the secret in the header", hits.first().is_some_and(|h| h.authorization == format!("Bearer {SECRET}")), format!("{hits:?}"));
    s.ok("a fetch says how deep in a chain it is (x-fragment-hops)", hits.first().is_some_and(|h| h.hops == "1"), format!("{hits:?}"));
    let items = api.op(&owner, &name, "items", "q", json!({}))?;
    s.ok("the job's call step ran the mutation", items.body["result"] == json!([{ "text": "alpha", "source": "digest" }, { "text": "beta", "source": "digest" }]), &items);
    let feed = records(api, &owner, &name, "feed");
    let owner_id = api.identity(&owner)?;
    s.ok(
        "its mutation's records and its own publish land in the channel, as the caller",
        feed.len() == 3 && feed[2]["kind"] == "digest" && feed[2]["body"]["digest"] == 2 && feed[2]["principal"] == owner_id.as_str(),
        json!(feed),
    );
    let everything = format!("{done}{}{}", json!(feed), json!(events(api, &owner, &name)));
    s.ok("the secret appears in no run, record, or event", !everything.contains(SECRET), "the secret leaked");
    let ops = records(api, &owner, &name, "ops");
    s.ok("the step's mutation is ledgered under the run's step id", ops.iter().any(|o| o["body"] == json!({ "op": "save", "id": format!("job:{id}:1") })), json!(ops));

    // a step that fails for now is retried; one that keeps failing is the job's to catch
    let r = api.op(&owner, &name, "digest", "d3", json!({ "url": upstream.url("/flaky") }))?;
    let flaky = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok("an upstream 503 is retried until it answers", flaky["status"] == "succeeded" && upstream.hits("/flaky").len() == 3, &flaky);
    let r = api.op(&owner, &name, "careful", "c1", json!({ "url": upstream.url("/down") }))?;
    let careful_id = started(&r);
    let r = api.op(&owner, &name, "careful", "c2", json!({ "url": "ftp://example.com/x" }))?;
    let refused = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok(
        "a refused fetch fails its step at once, and the job may catch it",
        refused["output"]["caught"] == true && refused["output"]["name"] == "StepError" && refused["output"]["message"].as_str().is_some_and(|m| m.contains("http and https")),
        &refused,
    );

    let r = api.op(&owner, &name, "probe", "probe-1", json!({ "url": upstream.url("/moved") }))?;
    let moved = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok(
        "a redirect is answered to the job, not followed",
        moved["output"] == json!({ "status": 302, "location": "http://127.0.0.1:1/private" }) && upstream.hits("/moved").len() == 1,
        &moved,
    );

    // held, then replayed after the cause is fixed
    let r = api.op(&owner, &name, "fragile", "f1", json!({}))?;
    let fragile = started(&r);
    let held = settle(api, &owner, &name, fragile, &["succeeded", "held"], long);
    s.ok("a job that throws is held with its error", held["status"] == "held" && held["error"].as_str().is_some_and(|e| e.contains("the flag is not set")), &held);
    let r = api.signed(&viewer, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": fragile })))?;
    s.ok("a viewer cannot replay", r.status == 403, &r);
    let r = api.signed(&viewer, "GET", &format!("/api/f/{name}/runs?status=held"), None)?;
    s.ok("a viewer lists held runs", r.status == 200 && r.body["runs"].as_array().is_some_and(|a| a.iter().any(|x| x["id"] == fragile)), &r);
    let r = api.unsigned("GET", &format!("/api/f/{name}/runs"), None)?;
    s.ok("runs are not public (401 unsigned)", r.status == 401, &r);
    api.op(&owner, &name, "set_flag", "on", json!({ "on": true }))?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": fragile })))?;
    s.ok("an editor replays a held run as its next attempt", r.status == 200 && r.body["attempt"] == 2, &r);
    let replayed = settle(api, &owner, &name, fragile, &["succeeded", "held"], long);
    s.ok("the replay succeeds once the cause is fixed", replayed["status"] == "succeeded" && replayed["attempt"] == 2 && replayed["error"].is_null(), &replayed);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": fragile })))?;
    s.ok("a succeeded run is not replayed", r.status == 400, &r);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": 99999 })))?;
    s.ok("replaying an unknown run is 404", r.status == 404, &r);

    // sleeps, jobs starting jobs, and steps that cannot run
    let r = api.op(&owner, &name, "nap", "n1", json!({ "ms": 2500 }))?;
    let nap = started(&r);
    std::thread::sleep(Duration::from_millis(800));
    let mid = run(api, &owner, &name, nap)?.body;
    s.ok("a sleeping job is running", mid["status"] == "running", &mid);
    let woke = settle(api, &owner, &name, nap, &["succeeded", "held"], long);
    s.ok("it wakes and finishes", woke["status"] == "succeeded" && woke["output"]["slept"] == 2500, &woke);
    let r = api.op(&owner, &name, "parent", "p1", json!({}))?;
    let parent = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    let child = parent["output"]["run"].as_i64().unwrap_or(0);
    let child_run = settle(api, &owner, &name, child, &["succeeded", "held"], long);
    s.ok(
        "a job's call to a job starts it, one hop deeper",
        child > 0 && child_run["via"] == "job" && child_run["depth"] == 1 && child_run["status"] == "succeeded",
        format!("parent {parent} child {child_run}"),
    );
    let r = api.op(&owner, &name, "wrong", "w1", json!({}))?;
    let wrong = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok("a call step to an unknown operation fails the job", wrong["status"] == "held" && wrong["error"].as_str().is_some_and(|e| e.contains("no_such_op")), &wrong);
    let r = api.op(&owner, &name, "misfit", "m1", json!({}))?;
    let misfit = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok(
        "a step whose args do not decode fails for good, naming what is missing, and the job may catch it",
        misfit["status"] == "succeeded"
            && misfit["output"]["caught"] == true
            && misfit["output"]["name"] == "StepError"
            && misfit["output"]["message"].as_str().is_some_and(|m| m.contains("ai.text") && m.contains("missing field `model`")),
        &misfit,
    );

    // the step that kept failing: retried with backoff, then the job caught it
    let careful = settle(api, &owner, &name, careful_id, &["succeeded", "held"], Duration::from_secs(60));
    s.ok(
        "a step that keeps failing runs out of retries, and the job sees why",
        careful["output"]["caught"] == true && careful["output"]["message"].as_str().is_some_and(|m| m.contains("503")) && upstream.hits("/down").len() == 5,
        &careful,
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/runs?op=digest"), None)?;
    s.ok("runs filter by operation, newest first", r.body["runs"].as_array().is_some_and(|a| a.len() == 2 && a[0]["id"].as_i64() > a[1]["id"].as_i64()), &r);

    // a long job: each step's answer is kept by the cell, and the body
    // reads all of them back, in order, at every step
    let (steps, _) = jobs_fragment(s, api, &owner, "jobsteps", |_| {})?;
    let r = api.op(&owner, &steps, "count_up", "fifty", json!({ "n": 50 }))?;
    let fifty = settle(api, &owner, &steps, started(&r), &["succeeded", "held"], Duration::from_secs(120));
    let published: Vec<i64> = (1..=45).collect();
    s.ok(
        "a 50-step job (45 publishes, 5 sleeps) reads every earlier answer back, in order",
        fifty["status"] == "succeeded" && fifty["output"]["seqs"] == json!(published) && records(api, &owner, &steps, "steps").len() == 45,
        &fifty,
    );
    // a step whose answer is lost on its way back to the Workflow: the
    // retried step is answered from what the cell kept, not performed again
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": steps, "op": "drop-effects", "times": 1 })))?;
    s.ok("(the test fleet loses the fragment's next step answer)", r.status == 200, &r);
    let r = api.op(&owner, &steps, "probe", "once", json!({ "url": upstream.url("/once") }))?;
    let once = settle(api, &owner, &steps, started(&r), &["succeeded", "held"], long);
    s.ok(
        "a fetch whose answer was lost is answered again from what the cell kept: the upstream sees one request",
        once["status"] == "succeeded" && once["output"]["status"] == 200 && upstream.hits("/once").len() == 1,
        format!("{once}; the upstream saw {} requests", upstream.hits("/once").len()),
    );

    // A run in flight across the deploy that began keeping step answers
    // finds none: it starts again as its next attempt, once, by itself.
    // A run that loses its answers otherwise is held, as before.
    let forget = |deploy: bool| api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": steps, "op": "forget-steps", "deploy": deploy })));
    let r = api.op(&owner, &steps, "nap", "across", json!({ "ms": 2000 }))?;
    let across = started(&r);
    let napping = settle(api, &owner, &steps, across, &["running"], long);
    let r = forget(true)?;
    let resumed = settle(api, &owner, &steps, across, &["succeeded", "held"], long);
    let said = events(api, &owner, &steps).into_iter().any(|e| e["kind"] == "run.resumed" && e["data"]["run"] == across);
    s.ok(
        "a run in flight across the deploy that began keeping answers starts again by itself, once, and finishes",
        napping["status"] == "running" && r.status == 200 && resumed["status"] == "succeeded" && resumed["attempt"] == 2 && said,
        format!("{resumed}; run.resumed logged: {said}"),
    );
    let r = api.op(&owner, &steps, "nap", "lost", json!({ "ms": 2000 }))?;
    let lost = started(&r);
    settle(api, &owner, &steps, lost, &["running"], long);
    forget(false)?;
    let held = settle(api, &owner, &steps, lost, &["succeeded", "held"], long);
    s.ok(
        "a run launched since, whose answers are lost, is held (replay it), not started again",
        held["status"] == "held" && held["attempt"] == 1 && held["error"].as_str().is_some_and(|e| e.contains("no kept answer")),
        &held,
    );

    // the CLI
    let home = s.dir("jobs-cli");
    s.login(api, &home);
    let cli = s.cli_keys(&home).expect("the CLI logged in");
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", cli.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let wrong_id = wrong["id"].as_i64().unwrap_or(0).to_string();
    let r = s.cli_json(api, &home, &["runs", &name, "--status", "held", "--json"])?;
    s.ok("fragment runs --status held lists the held runs", r["runs"].as_array().is_some_and(|a| a.len() == 1 && a[0]["op"] == "wrong"), &r);
    let r = s.cli_json(api, &home, &["runs", &name, &wrong_id, "--json"])?;
    s.ok("fragment runs <id> shows one in full", r["input"] == json!({}) && r["error"].as_str().is_some_and(|e| e.contains("no_such_op")), &r);
    let r = s.cli_json(api, &home, &["triggers", &name, "--json"])?;
    s.ok("fragment triggers lists them", r["triggers"].as_array().map_or(0, |a| a.len()) == 4, &r);
    let r = s.cli_json(api, &home, &["pause", &name, "boom", "--json"])?;
    s.ok("fragment pause", r["paused"] == true, &r);
    let r = s.cli_json(api, &home, &["unpause", &name, "boom", "--json"])?;
    s.ok("fragment unpause", r["paused"] == false, &r);
    let r = s.cli_json(api, &home, &["replay", &name, &wrong_id, "--json"])?;
    s.ok("fragment replay", r["attempt"] == 2, &r);
    let r = s.cli_json(api, &home, &["call", &name, "nap", "--input", r#"{"ms":1}"#, "--json"])?;
    s.ok("fragment call on a job answers its run", r["result"]["run"].as_i64().is_some(), &r);

    // the inbox template: deliveries in, lines out
    let tpl = s.named(api, &cli, "inboxtpl")?;
    let c = s.create(api, &cli, &tpl)?;
    let files: Vec<(&str, Option<&[u8]>)> = INBOX_FILES.iter().map(|(p, b)| (*p, Some(*b))).collect();
    s.commit(&c, &files);
    s.deploy(&c);
    let token = c["inboxToken"].as_str().unwrap_or("").to_string();
    s.cli(api, &home, &["inbox", &tpl, "--token", &token, "--payload", r#"{"text":"hello from the cli"}"#]);
    inbox(api, &tpl, &token, &json!({ "source": "e2e", "payload": { "url": upstream.url("/page") } }), None)?;
    let mut list = Value::Null;
    let arrived = s.eventually(long, || {
        list = api.op(&cli, &tpl, "list", "q", json!({})).map(|r| r.body["result"]["items"].clone()).unwrap_or_default();
        list.as_array().is_some_and(|a| a.len() == 2)
    });
    let texts: Vec<&str> = list.as_array().map(|a| a.iter().filter_map(|i| i["text"].as_str()).collect()).unwrap_or_default();
    s.ok(
        "the inbox template turns each delivery into a line (a url fetched for its title)",
        arrived && texts.contains(&"hello from the cli") && texts.contains(&"A page to title (200)"),
        &list,
    );
    Ok(())
}

pub fn triggers(s: &mut Suite, api: &Api, cron: Option<Result<Option<Cron>, String>>) -> Result<()> {
    if !s.section("triggers") {
        return Ok(());
    }
    let cron = match cron {
        Some(Ok(Some(cron))) => cron,
        Some(Err(why)) => anyhow::bail!("the cron fragment was not deployed: {why}"),
        Some(Ok(None)) | None => anyhow::bail!("the cron fragment is deployed whenever the triggers section runs, and was not"),
    };
    let owner = api.person()?;
    let (name, c) = jobs_fragment(s, api, &owner, "triggers", |_| {})?;
    let token = c["inboxToken"].as_str().unwrap_or("").to_string();
    let long = Duration::from_secs(40);
    let own = api.status(&owner, &name)?.body["npub"].as_str().unwrap_or("").to_string();

    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/triggers"), None)?;
    let list = r.body["triggers"].as_array().cloned().unwrap_or_default();
    let r_cron = api.signed(&cron.owner, "GET", &format!("/api/f/{}/triggers", cron.name), None)?;
    let next_at = r_cron.body["triggers"][0]["nextAt"].as_i64();
    s.ok(
        "the triggers list names each one, a cron trigger with its next time (on a minute)",
        list.len() == 4 && list[0] == json!({ "channel": "inbox", "run": "ingest", "paused": false }) && next_at.is_some_and(|at| at % 60_000 == 0),
        format!("{r} {r_cron}"),
    );

    // the inbox: a delivery is a record, and its trigger runs with it
    let r = inbox(api, &name, &token, &json!({ "source": "github", "payload": { "items": ["from the inbox"] } }), None)?;
    let first = r.body["runs"][0].as_i64().unwrap_or(0);
    s.ok("an inbox post answers its record and the runs it started", r.status == 200 && r.body["seq"] == 1 && first > 0, &r);
    let done = settle(api, &owner, &name, first, &["succeeded", "held"], long);
    s.ok("the inbox's job runs as the fragment itself", done["status"] == "succeeded" && done["via"] == "channel" && done["principal"] == own.as_str(), &done);
    let items = api.op(&owner, &name, "items", "q", json!({}))?;
    s.ok("and saved what was delivered", items.body["result"] == json!([{ "text": "from the inbox", "source": "github" }]), &items);
    let recs = records(api, &owner, &name, "inbox");
    s.ok("the inbox channel keeps the delivery", recs.len() == 1 && recs[0]["body"]["source"] == "github" && recs[0]["principal"] == "inbox", json!(recs));
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/api/f/{name}/inbox?t={token}", api.base),
        body: Some(b"plain text".to_vec()),
        content_type: Some("text/plain"),
        ..Call::default()
    })?;
    s.ok("the token may come as ?t=, and a body that is not JSON is text", r.status == 200, &r);
    let r = inbox(api, &name, "wrong", &json!({ "payload": 1 }), None)?;
    s.ok("a bad inbox token is 403", r.status == 403, &r);
    // a delivery says how deep in a chain it already is: one at the hop
    // budget runs, one a hop past it is blocked
    let depth = limits::HOP_DEPTH_MAX;
    let r = inbox(api, &name, &token, &json!({ "payload": { "items": ["at the budget"] } }), Some(depth))?;
    let at_budget = settle(api, &owner, &name, r.body["runs"][0].as_i64().unwrap_or(0), &["succeeded", "held", "blocked"], long);
    let r = inbox(api, &name, &token, &json!({ "payload": { "items": ["past it"] } }), Some(depth + 1))?;
    let past = settle(api, &owner, &name, r.body["runs"][0].as_i64().unwrap_or(0), &["succeeded", "held", "blocked"], long);
    s.ok(
        "a delivery at the hop budget runs; one a hop past it is blocked",
        at_budget["status"] == "succeeded" && at_budget["depth"] == depth && past["status"] == "blocked" && past["depth"] == depth + 1,
        format!("{at_budget} {past}"),
    );

    // a channel trigger that feeds itself runs out the hop budget, the
    // ceiling for every chain (files and jobs count the same hops)
    api.op(&owner, &name, "ping", "loop", json!({}))?;
    let looped = s.eventually(long, || runs(api, &owner, &name, "&op=ping").iter().any(|r| r["status"] == "blocked"));
    // the blocked one past the budget is recorded while the last may still be finishing
    s.eventually(long, || !runs(api, &owner, &name, "&op=ping").iter().any(|r| r["status"] == "running" || r["status"] == "queued"));
    let pings = runs(api, &owner, &name, "&op=ping");
    s.ok(
        "a trigger loop runs to the hop budget, then is blocked",
        looped
            && pings.iter().filter(|r| r["status"] == "succeeded").count() == depth as usize
            && pings[0]["depth"] == depth + 1
            && pings[1]["depth"] == depth
            && pings[1]["status"] == "succeeded",
        json!(pings.iter().map(|r| (r["depth"].clone(), r["status"].clone())).collect::<Vec<_>>()),
    );
    s.ok("the event log says a loop was stopped", events(api, &owner, &name).iter().any(|e| e["kind"] == "cycle.detected"), "no cycle.detected");

    // held runs pause an operation's triggers at the count, not one
    // before; calls still work
    let held = |n: u64| s.eventually(long, || runs(api, &owner, &name, "&op=boom&status=held").len() as u64 == n);
    let paused_ops = || api.signed(&owner, "GET", &format!("/api/f/{name}/runs"), None).map(|r| r.body["paused"].clone()).unwrap_or_default();
    for i in 0..limits::AUTO_PAUSE_HELD - 1 {
        api.op(&owner, &name, "raise", &format!("r{i}"), json!({}))?;
    }
    let below = held(limits::AUTO_PAUSE_HELD - 1) && paused_ops() == json!([]);
    api.op(&owner, &name, "raise", &format!("r{}", limits::AUTO_PAUSE_HELD - 1), json!({}))?;
    let paused = held(limits::AUTO_PAUSE_HELD);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/runs"), None)?;
    s.ok(
        "held runs auto-pause the operation at the count, not one before",
        below && paused && r.body["paused"] == json!(["boom"]) && r.body["counts"]["held"].as_u64() >= Some(limits::AUTO_PAUSE_HELD),
        &r,
    );
    s.ok("the event log says why", events(api, &owner, &name).iter().any(|e| e["kind"] == "op.auto-paused"), "no op.auto-paused");
    api.op(&owner, &name, "raise", "past-the-count", json!({}))?;
    let blocked = runs(api, &owner, &name, "&op=boom");
    s.ok("a paused operation's trigger records a blocked run", blocked[0]["status"] == "blocked", json!(blocked[0]));
    let r = api.op(&owner, &name, "boom", "manual", json!({}))?;
    s.ok("a paused operation can still be called", r.status == 200 && r.body["result"]["status"] == "queued", &r);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/pause"), Some(&json!({ "op": "boom", "paused": false })))?;
    s.ok("unpausing starts triggered runs again", r.status == 200 && r.body["paused"] == false, &r);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/pause"), Some(&json!({ "op": "nope", "paused": true })))?;
    s.ok("pausing an unknown operation is 404", r.status == 404, &r);

    // a move of main that touches a matching file
    s.commit(&c, &[("notes/today.md", Some(b"# today")), ("other.txt", Some(b"x"))]);
    let filed = s.eventually(long, || runs(api, &owner, &name, "&op=tick").iter().any(|r| r["via"] == "files" && r["status"] == "succeeded"));
    let ticked_paths = || -> Vec<Value> {
        api.op(&owner, &name, "ticks", "q", json!({}))
            .map(|r| r.body["result"].as_array().into_iter().flatten().filter(|t| t["ref"] == "main").map(|t| t["paths"].clone()).collect())
            .unwrap_or_default()
    };
    s.ok("a file trigger runs with the matching paths", filed && ticked_paths() == [json!(["notes/today.md"])], json!(ticked_paths()));
    // a move that touches no matching file, then a sentinel move that does:
    // moves are read in order, so once the sentinel's run has ticked the
    // first move's would have too
    s.commit(&c, &[("other.txt", Some(b"y"))]);
    s.commit(&c, &[("notes/sentinel.md", Some(b"# sentinel"))]);
    let sentinel = s.eventually(long, || ticked_paths().contains(&json!(["notes/sentinel.md"])));
    let filed = runs(api, &owner, &name, "&op=tick").iter().filter(|r| r["via"] == "files").count();
    s.ok(
        "a move that touches no matching file starts nothing",
        sentinel && filed == 2 && ticked_paths() == [json!(["notes/today.md"]), json!(["notes/sentinel.md"])],
        format!("{filed} runs from files, ticked {}", json!(ticked_paths())),
    );

    // a trigger step that fails after its record is appended (here after
    // its first run started, before its last): the call fails visibly, the
    // record's delivery goes out, and the same call again starts the rest
    let (fails, _) = jobs_fragment(s, api, &owner, "trigfail", |m| {
        m["triggers"] = json!([{ "channel": "alarms", "run": "tick" }, { "channel": "alarms", "run": "boom" }])
    })?;
    let hook = Upstream::start()?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{fails}/subscriptions"), Some(&json!({ "channel": "alarms", "url": hook.url("/data") })))?;
    s.ok("(the owner subscribes to alarms)", r.status == 200, &r);
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": fails, "op": "fail-triggers", "times": 1 })))?;
    s.ok("(the test fleet fails the fragment's next trigger step before its last run)", r.status == 200, &r);
    let counts = || (records(api, &owner, &fails, "alarms").len(), runs(api, &owner, &fails, "&op=tick").len(), runs(api, &owner, &fails, "&op=boom").len());
    let r = api.op(&owner, &fails, "raise", "once", json!({}))?;
    let after = counts();
    s.ok(
        "a trigger step that fails after the append fails the call: the record stands, its first run started, its last not",
        r.status >= 500 && after == (1, 1, 0),
        format!("{r}; records, tick runs, boom runs: {after:?}"),
    );
    let delivered = s.eventually(Duration::from_secs(10), || !hook.hits("/data").is_empty());
    s.ok("the record's delivery was written before its triggers: it goes out all the same", delivered, hook.hits("/data").len());
    let r = api.op(&owner, &fails, "raise", "once", json!({}))?;
    let after = counts();
    s.ok(
        "the same call again starts the run that did not start, and nothing twice",
        r.status == 200 && r.body["replayed"] == true && after == (1, 1, 1),
        format!("{r}; records, tick runs, boom runs: {after:?}"),
    );

    // an outbox write that fails after its record's append: the call fails
    // visibly and nothing is sent; the same call again writes the record's
    // deliveries (the record was never marked outboxed), and sends it once
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": fails, "op": "fail-outbox", "times": 1 })))?;
    s.ok("(the test fleet fails the fragment's next outbox write)", r.status == 200, &r);
    let sent_before = hook.hits("/data").len();
    let r = api.op(&owner, &fails, "raise", "outbox", json!({}))?;
    let appended = records(api, &owner, &fails, "alarms").len();
    s.ok(
        "an outbox write that fails after the append fails the call, and the record stands",
        r.status >= 500 && appended == 2,
        format!("{r}; records: {appended}"),
    );
    let r = api.op(&owner, &fails, "raise", "outbox", json!({}))?;
    let delivered = s.eventually(Duration::from_secs(10), || hook.hits("/data").len() > sent_before);
    s.ok(
        "a retry writes the record's delivery (it was never marked outboxed) and sends it once",
        r.status == 200 && delivered && records(api, &owner, &fails, "alarms").len() == 2,
        format!("{r}; deliveries since: {}", hook.hits("/data").len() - sent_before),
    );
    // a third call, then a sentinel record through the same subscription:
    // once the sentinel's delivery is in, one the third call wrote would be
    let r = api.op(&owner, &fails, "raise", "outbox", json!({}))?;
    api.op(&owner, &fails, "raise", "sentinel", json!({}))?;
    let (retried, last) = (appended as i64, records(api, &owner, &fails, "alarms").len() as i64);
    let seq_of = |h: &Hit| h.body["record"]["seq"].as_i64();
    let sentinel = s.eventually(Duration::from_secs(10), || hook.hits("/data").iter().any(|h| seq_of(h) == Some(last)));
    let sent = hook.hits("/data").iter().filter(|h| seq_of(h) == Some(retried)).count();
    s.ok(
        "and a third call writes nothing again",
        r.status == 200 && last == retried + 1 && sentinel && sent == 1 && hook.hits("/data").len() == sent_before + 2,
        format!("{r}; the retried record was delivered {sent} times; deliveries since: {}", hook.hits("/data").len() - sent_before),
    );

    // the rate ceiling: an operation's triggers start runs up to their
    // hourly count, and the next one is blocked
    let (busy, busy_c) = jobs_fragment(s, api, &owner, "ceiling", |m| m["triggers"] = json!([{ "channel": "alarms", "run": "tick" }]))?;
    for i in 0..=limits::TRIGGERED_RUNS_PER_HOUR {
        api.op(&owner, &busy, "raise", &format!("b{i}"), json!({}))?;
    }
    let ticks = runs(api, &owner, &busy, "&op=tick");
    let r = api.signed(&owner, "GET", &format!("/api/f/{busy}/runs?limit=1"), None)?;
    s.ok(
        "the triggered run one past the hourly count is blocked and pauses the operation; the last within it ran",
        ticks.len() as u64 == limits::TRIGGERED_RUNS_PER_HOUR + 1 && ticks[0]["status"] == "blocked" && ticks[1]["status"] != "blocked" && r.body["paused"] == json!(["tick"]),
        &r,
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{busy}/triggers"), None)?;
    s.ok("the triggers list shows the pause on the trigger", r.body["paused"] == json!(["tick"]) && r.body["triggers"][0]["paused"] == true, &r);

    // a live commit with no app says nothing about the next code: the
    // pause outlasts it, and the next good deploy comes back paused
    s.commit(&busy_c, &[("app.mjs", None)]);
    s.deploy(&busy_c);
    let r = api.status(&owner, &busy)?;
    s.ok("(a live commit without app.mjs installs no app)", r.body["code"]["sha"].is_null(), &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{busy}/runs?limit=1"), None)?;
    s.ok("an operation stays paused while live has no app", r.body["paused"] == json!(["tick"]), &r);
    let mut same: Value = serde_json::from_slice(JOBS_JSON)?;
    same["triggers"] = json!([{ "channel": "alarms", "run": "tick" }]);
    ship(s, &busy_c, JOBS_APP, same.to_string().as_bytes());
    let r = api.signed(&owner, "GET", &format!("/api/f/{busy}/triggers"), None)?;
    s.ok("and the app deployed again comes back paused", r.body["paused"] == json!(["tick"]) && r.body["triggers"][0]["paused"] == true, &r);

    // an operation the installed code no longer has loses its pause; one
    // of that name later starts clean
    let mut without: Value = serde_json::from_slice(JOBS_JSON)?;
    without["operations"].as_object_mut().expect("operations").remove("tick");
    without["triggers"] = json!([]);
    ship(s, &busy_c, JOBS_APP, without.to_string().as_bytes());
    let r = api.signed(&owner, "GET", &format!("/api/f/{busy}/runs?limit=1"), None)?;
    s.ok("an operation removed at install is no longer paused", r.status == 200 && r.body["paused"] == json!([]), &r);
    let mut again: Value = serde_json::from_slice(JOBS_JSON)?;
    again["triggers"] = json!([{ "channel": "alarms", "run": "tick" }]);
    ship(s, &busy_c, JOBS_APP, again.to_string().as_bytes());
    let r = api.signed(&owner, "GET", &format!("/api/f/{busy}/triggers"), None)?;
    s.ok("and put back, it starts unpaused", r.body["paused"] == json!([]) && r.body["triggers"][0]["paused"] == false, &r);

    // the inbox cap: records whose runs have not succeeded
    let full = s.named(api, &owner, "inboxcap")?;
    let c2 = s.create(api, &owner, &full)?;
    ship(s, &c2, JOBS_APP, JOBS_JSON);
    api.signed(&owner, "POST", &format!("/api/f/{full}/pause"), Some(&json!({ "op": "ingest", "paused": true })))?;
    let token2 = c2["inboxToken"].as_str().unwrap_or("").to_string();
    let mut refused_at = None;
    for i in 0..=limits::INBOX_PENDING_MAX {
        let r = inbox(api, &full, &token2, &json!({ "payload": i }), None)?;
        if r.status != 200 {
            refused_at = Some((i, r));
            break;
        }
    }
    s.ok(
        "the inbox takes its count of pending records, then answers 429",
        refused_at.as_ref().is_some_and(|(i, r)| *i == limits::INBOX_PENDING_MAX && r.code() == Some(ErrorCode::RateLimited)),
        refused_at.map(|(i, r)| format!("{i}: {r}")).unwrap_or_default(),
    );
    s.ok("the event log says the inbox was full", events(api, &owner, &full).iter().any(|e| e["kind"] == "inbox.rejected"), "no inbox.rejected");

    // cron, last: its fragment was deployed before the sections ahead of
    // this one, so a full run has passed a minute boundary by now; alone,
    // this waits for the one after the deploy
    let waited = Instant::now();
    let remaining = Duration::from_secs(70).saturating_sub(cron.deployed.elapsed()).max(Duration::from_secs(10));
    let ticks = || runs(api, &cron.owner, &cron.name, "&op=tick");
    let ticked = s.eventually(remaining, || ticks().iter().any(|r| r["via"] == "cron" && r["status"] == "succeeded"));
    println!("      the cron check waited {:?}, {:?} after the deploy", waited.elapsed(), cron.deployed.elapsed());
    let tick = ticks().into_iter().find(|r| r["via"] == "cron").unwrap_or(Value::Null);
    s.ok("a cron trigger runs on its minute", ticked && tick["trigger"] == "* * * * *", &tick);
    Ok(())
}
