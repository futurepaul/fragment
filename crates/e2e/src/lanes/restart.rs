//! State survives a graceful restart and a crash of the node (a sleeping
//! job included); then the
//! node runs without hostnames and serves fragments from `/f/<name>/`.

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use fragment_proto::limits;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs;
use crate::api::{Api, Call};
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");
const BUDGET_APP: &[u8] = include_bytes!("../../fixtures/budget.mjs");
const BUDGET_JSON: &[u8] = include_bytes!("../../fixtures/budget.json");

fn count(api: &Api, keys: &Keys, name: &str) -> i64 {
    api.op(keys, name, "count", "q", json!({})).ok().and_then(|r| r.body["result"]["n"].as_i64()).unwrap_or(-1)
}

pub fn restart(s: &mut Suite, api: Api) -> Result<Api> {
    if !s.section("restart") {
        return Ok(api);
    }
    let owner = api.person()?;
    let member = api.person()?;
    let name = s.named(&api, &owner, "restart")?;
    let c = s.create(&api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/secrets/TOKEN"), None)?;
    let live = ship(s, &c, TODO_APP, TODO_JSON);
    let first = api.op(&owner, &name, "add_todo", "r1", json!({ "text": "survives" }))?;
    // the registry: a key added, and one revoked, before the restart
    let (added, revoked) = (Keys::generate(), api.person()?);
    let add = "/api/identities/me/keys";
    let r = api.signed(&revoked, "POST", add, Some(&json!({ "proof": api.proof(&added, "POST", add, &revoked) })))?;
    let r2 = api.signed(&added, "DELETE", &format!("/api/identities/me/keys/{}", revoked.pubkey_hex()), None)?;
    anyhow::ensure!(r.status == 200 && r2.status == 200, "registry setup: {r} {r2}");
    // a ledger: a month with something spent
    let paid = s.named(&api, &owner, "restart-paid")?;
    let pc = s.create(&api, &owner, &paid)?;
    ship(s, &pc, BUDGET_APP, BUDGET_JSON);
    let r = api.op(&owner, &paid, "summarize", "before", json!({ "text": "before the restart" }))?;
    jobs::settle(&api, &owner, &paid, jobs::started(&r), &["succeeded"], Duration::from_secs(40));
    let spent = api.signed(&owner, "GET", "/api/budget", None)?.body["spentMicros"].clone();
    // a paused operation: its pause is a row, not a cached list
    let r = api.signed(&owner, "POST", &format!("/api/f/{paid}/pause"), Some(&json!({ "op": "summarize", "paused": true })))?;
    anyhow::ensure!(r.status == 200, "pause setup: {r}");
    // code stored as the fleet stored it before the code tables: JSON in the code row
    let (stored, sc) = jobs::jobs_fragment(s, &api, &owner, "restart-code", |_| {})?;
    let code_before = api.status(&owner, &stored)?.body["code"].clone();
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": stored, "op": "code-before-tables" })))?;
    anyhow::ensure!(r.status == 200 && code_before["operations"]["save"]["kind"] == "mutation", "code-before-tables setup: {r} {code_before}");
    // and code stored with more operations than a manifest may declare
    let past = s.named(&api, &owner, "restart-past")?;
    let past_c = s.create(&api, &owner, &past)?;
    ship(s, &past_c, TODO_APP, TODO_JSON);
    let past_code = api.status(&owner, &past)?.body["code"].clone();
    let fill = limits::OPERATIONS_MAX + 1;
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": past, "op": "code-before-tables", "fill": fill })))?;
    anyhow::ensure!(r.status == 200 && past_code["operations"]["add_todo"]["kind"] == "mutation", "code-before-tables fill setup: {r} {past_code}");

    s.stop()?;
    let api = s.start(false, true)?;
    let r = api.op(&owner, &name, "add_todo", "r1", json!({ "text": "survives" }))?;
    s.ok("after a restart the replay returns the stored result", r.body["replayed"] == true && r.body["result"] == first.body["result"], &r);
    s.ok("after a restart the app's rows survive", count(&api, &owner, &name) == 1, "count");
    let r = api.status(&member, &name)?;
    s.ok("after a restart members survive", r.status == 200 && r.body["role"] == "editor", &r);
    let r = api.signed(&added, "GET", "/api/identities/me", None)?;
    s.ok("after a restart the registry still knows an added key", r.status == 200, &r);
    let r = api.signed(&revoked, "GET", "/api/fragments", None)?;
    s.ok("and a revoked key stays revoked", r.status == 401, &r);
    let r = api.signed(&owner, "GET", "/api/budget", None)?;
    s.ok("after a restart the month's spend is what it was", r.status == 200 && r.body["spentMicros"] == spent && spent.as_i64().unwrap_or(0) > 0, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{paid}/runs?limit=1"), None)?;
    s.ok("after a restart a paused operation is still paused", r.status == 200 && r.body["paused"] == json!(["summarize"]), &r);
    let r = api.status(&owner, &stored)?;
    s.ok("code stored before the code tables moves into them: its operations and schemas are the same", r.body["code"] == code_before, &r.body["code"]);
    let r = api.signed(&owner, "GET", &format!("/api/f/{stored}/triggers"), None)?;
    let on: Vec<Value> = r.body["triggers"].as_array().into_iter().flatten().map(|t| t["run"].clone()).collect();
    s.ok("and its triggers, in their order", on == vec![json!("ingest"), json!("ping"), json!("boom"), json!("tick")], &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{stored}/channels/feed"), None)?;
    s.ok("and its channels", r.status == 200, &r);
    let token = sc["inboxToken"].as_str().unwrap_or("");
    let r = jobs::inbox(&api, &stored, token, &json!({ "source": "migrated", "payload": { "items": ["after the move"] } }), None)?;
    let run = r.body["runs"][0].as_i64().unwrap_or(0);
    let ran = jobs::settle(&api, &owner, &stored, run, &["succeeded", "held"], Duration::from_secs(40));
    let items = api.op(&owner, &stored, "items", "q", json!({}))?;
    s.ok(
        "and its channel trigger starts its run",
        ran["status"] == "succeeded" && items.body["result"].as_array().is_some_and(|a| a.iter().any(|i| i["text"] == "after the move" && i["source"] == "migrated")),
        &items,
    );

    // Goal: stored data past a limit never reaches an assertion in the
    // constructor, which would fail every activation. Method: the fragment
    // whose stored code holds OPERATIONS_MAX + 1 operations answers, says
    // its code was dropped and why, and a refresh installs live again.
    let r = api.status(&owner, &past)?;
    s.ok("code stored past a manifest's limit does not stop its fragment: it answers", r.status == 200, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{past}/events?tail=20"), None)?;
    let dropped = r.body["events"].as_array().into_iter().flatten().find(|e| e["kind"] == "code.dropped").cloned().unwrap_or(Value::Null);
    let why = format!("operations: {fill}, past the limit of {}", limits::OPERATIONS_MAX);
    s.ok("and its code is dropped, with an event that says why", dropped["summary"].as_str().is_some_and(|m| m.contains(&why)), &r);
    let r = api.signed(&owner, "POST", &format!("/api/f/{past}/refresh"), Some(&json!({})))?;
    let code = api.status(&owner, &past)?.body["code"].clone();
    s.ok("and the next refresh installs live again, as it was", r.status == 200 && code == past_code, &code);

    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("a mutation before the crash", r.status == 200, &r);
    let (jobs, _) = jobs::jobs_fragment(s, &api, &owner, "restart-jobs", |_| {})?;
    let r = api.op(&owner, &jobs, "nap", "through-the-crash", json!({ "ms": 4000 }))?;
    let nap = jobs::started(&r);
    std::thread::sleep(Duration::from_millis(1000));
    s.crash()?;
    let api = s.start(false, true)?;
    let woke = jobs::settle(&api, &owner, &jobs, nap, &["succeeded", "held"], Duration::from_secs(60));
    let naps = jobs::records(&api, &owner, &jobs, "feed").iter().filter(|r| r["kind"] == "nap").count();
    s.ok("a job sleeping through a crash wakes and finishes, once", woke["status"] == "succeeded" && naps == 1, format!("{woke} ({naps} nap records)"));
    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("after a crash an acknowledged mutation replays", r.body["replayed"] == true, &r);
    s.ok("after a crash no acknowledged write is lost", count(&api, &owner, &name) == 2, "count");
    let r = api.status(&owner, &name)?;
    s.ok("after a crash pins and code survive", r.body["pins"]["live"] == live.as_str() && r.body["code"]["sha"] == live.as_str(), &r);
    let r = api.signed(&member, "GET", "/api/fragments", None)?;
    s.ok("after a crash the member's list survives", r.text.contains(&name), &r);
    s.commit(&c, &[("after.md", Some(b"webhooks still land"))]);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file?path=after.md"), None)?;
    s.ok("after a crash webhooks still move the pins", r.status == 200 && r.text == "webhooks still land", &r);
    Ok(api)
}

/// A fleet without a hostname suffix serves fragments under `/f/<name>/`.
pub fn pathmode(s: &mut Suite, api: Api) -> Result<()> {
    if !s.section("pathmode") {
        return Ok(());
    }
    drop(api);
    s.stop()?;
    let api = s.start(false, false)?;
    let owner = api.person()?;
    let name = s.named(&api, &owner, "paths")?;
    let c = s.create(&api, &owner, &name)?;
    s.ok("without a suffix the canonical URL is a path", c["canonical"] == format!("{}/f/{name}/", api.base), &c);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    s.commit(&c, &[("site/index.html", Some(b"<p>by path</p>")), ("app.mjs", Some(include_bytes!("../../fixtures/guestbook.mjs"))), ("fragment.json", Some(include_bytes!("../../fixtures/guestbook.json")))]);
    s.deploy(&c);
    let r = api.page(&name, "", None)?;
    s.ok("a page is served by path", r.status == 200 && r.text.contains("by path"), &r);
    let r = api.browser_op(&name, "sign", "p1", json!({ "text": "hi" }), None)?;
    s.ok("a browser call works by path", r.status == 200, &r);
    s.ok("its cookie is scoped to the fragment's path", r.header("set-cookie").contains(&format!("Path=/f/{name}/;")), r.header("set-cookie"));
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}", api.base), ..Call::default() })?;
    s.ok("the bare path redirects to the trailing slash", r.status == 308 && r.header("location") == format!("{}/f/{name}/", api.base), &r);
    s.stop()?;
    Ok(())
}
