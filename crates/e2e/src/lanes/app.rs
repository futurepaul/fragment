//! Operations in the app facet, with code from the live commit; then a
//! public fragment that anonymous visitors write to; then the effects a
//! mutation leaves, refused, delayed, and forged, across a restart.

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::Api;
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");
const GUESTBOOK_APP: &[u8] = include_bytes!("../../fixtures/guestbook.mjs");
const GUESTBOOK_JSON: &[u8] = include_bytes!("../../fixtures/guestbook.json");
const EFFECTS_APP: &[u8] = include_bytes!("../../fixtures/effects.mjs");
const EFFECTS_JSON: &[u8] = include_bytes!("../../fixtures/effects.json");

/// Commits an app and deploys it.
pub fn ship(s: &Suite, c: &Value, app: &[u8], manifest: &[u8]) -> String {
    s.commit(c, &[("app.mjs", Some(app)), ("fragment.json", Some(manifest))]);
    s.deploy(c)
}

fn count(api: &Api, keys: &Keys, name: &str) -> i64 {
    api.op(keys, name, "count", "q", json!({})).ok().and_then(|r| r.body["result"]["n"].as_i64()).unwrap_or(-1)
}

pub fn ops(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("ops") {
        return Ok(());
    }
    let owner = api.person()?;
    let editor = api.person()?;
    let other = api.person()?;
    let name = s.named(api, &owner, "ops")?;
    let c = s.create(api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", editor.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let live = ship(s, &c, TODO_APP, TODO_JSON);
    let r = api.status(&owner, &name)?;
    s.ok("deploying installs the app from live", r.body["code"]["sha"] == live.as_str() && r.body["code"]["error"].is_null(), &r);

    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "one" }))?;
    s.ok("a mutation runs", r.status == 200 && r.body["replayed"] == false && r.body["result"]["id"] == 1, &r);
    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "one" }))?;
    s.ok("a replay returns the stored result", r.status == 200 && r.body["replayed"] == true && r.body["result"]["id"] == 1, &r);
    s.ok("a replay does not write again", count(api, &owner, &name) == 1, "count");
    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "two" }))?;
    s.ok("a conflicting body is 409", r.status == 409 && r.error() == "conflicting_body", &r);
    let r = api.op(&editor, &name, "add_todo", "a1", json!({ "text": "two" }))?;
    s.ok("another caller's same id is their own operation", r.status == 200 && r.body["replayed"] == false, &r);
    let r = api.op(&owner, &name, "add_todo", "bad id", json!({ "text": "x" }))?;
    s.ok("an invalid operation id is 400", r.status == 400, &r);
    let r = api.op(&owner, &name, "nope", "n1", json!({}))?;
    s.ok("an undeclared operation is 404", r.status == 404 && r.error() == "unknown_operation", &r);
    let r = api.op(&owner, &name, "add_todo", "e1", json!({ "text": "" }))?;
    s.ok("an author refusal is 422", r.status == 422 && r.error() == "app_failed", &r);
    let r = api.op(&owner, &name, "add_todo", "e1", json!({ "text": "now valid" }))?;
    s.ok("a refused id stays unused", r.status == 200 && r.body["replayed"] == false, &r);
    let before = count(api, &owner, &name);
    let r = api.op(&owner, &name, "add_then_throw", "t1", json!({ "text": "rolled back" }))?;
    s.ok("a throw after a write is 422", r.status == 422, &r);
    s.ok("a throw rolls its write back", count(api, &owner, &name) == before, before);
    let r = api.op(&owner, &name, "add_async", "as1", json!({ "text": "async" }))?;
    s.ok("an async mutation is refused", r.status == 422 && r.message().contains("mutations are synchronous"), &r);
    s.ok("an async mutation's write rolls back", count(api, &owner, &name) == before, before);
    let r = api.op(&owner, &name, "add_todo", "big", json!({ "text": "x".repeat(300 * 1024) }))?;
    s.ok("an input over 256 KiB is 413", r.status == 413 && r.error() == "too_large", &r);
    let r = api.op(&owner, &name, "list", "q1", json!({}))?;
    s.ok("a query answers", r.status == 200 && r.body["result"]["todos"].as_array().map_or(0, |a| a.len()) == 3, &r);
    let r = api.op(&other, &name, "list", "q2", json!({}))?;
    s.ok("a stranger calling an operation is 403", r.status == 403, &r);

    // a bad manifest on a later deploy keeps the good code running
    s.commit(&c, &[("fragment.json", Some(br#"{"operations":{"x":{"kind":"task"}}}"#))]);
    let bad = s.deploy(&c);
    let r = api.status(&owner, &name)?;
    s.ok(
        "a deploy with an invalid fragment.json keeps the last good code",
        r.body["code"]["sha"] == live.as_str() && r.body["code"]["error"].as_str().is_some_and(|e| e.contains(&bad[..12])),
        &r,
    );
    s.ok("and it still answers", count(api, &owner, &name) == before, "count");
    let live2 = ship(s, &c, TODO_APP, TODO_JSON);
    let r = api.status(&owner, &name)?;
    s.ok("the next good deploy installs and clears the error", r.body["code"]["sha"] == live2.as_str() && r.body["code"]["error"].is_null(), &r);
    s.ok("redeploying keeps the app's data", count(api, &owner, &name) == before, "count");
    // a deploy during a code.storage outage installs once reads come back
    s.fake.fail_file_reads(true);
    let during = ship(s, &c, &[TODO_APP, b"\n// v3\n"].concat(), TODO_JSON);
    let r = api.status(&owner, &name)?;
    s.ok("during an outage live moves but the code stays", r.body["pins"]["live"] == during.as_str() && r.body["code"]["sha"] == live2.as_str(), &r);
    s.fake.fail_file_reads(false);
    api.signed(&owner, "POST", &format!("/api/f/{name}/refresh"), Some(&json!({})))?;
    let r = api.status(&owner, &name)?;
    s.ok("after the outage the next refresh installs it", r.body["code"]["sha"] == during.as_str(), &r);
    s.commit(&c, &[("app.mjs", None)]);
    s.deploy(&c);
    let r = api.op(&owner, &name, "list", "q3", json!({}))?;
    s.ok("a live commit without app.mjs has no operations", r.status == 404 && r.error() == "no_code", &r);
    Ok(())
}

pub fn public(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("public") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "guestbook")?;
    let c = s.create(api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    ship(s, &c, GUESTBOOK_APP, GUESTBOOK_JSON);

    let r = api.browser_op(&name, "sign", "s1", json!({ "text": "hello" }), None)?;
    let cookie = r.cookies().into_iter().find(|c| c.starts_with("fragment_anon=")).unwrap_or_default();
    s.ok("an anonymous visitor signs a public fragment", r.status == 200 && r.body["result"]["id"] == 1, &r);
    s.ok("the visitor gets an anonymous principal cookie", cookie.len() == "fragment_anon=".len() + 64, r.header("set-cookie"));
    s.ok("the cookie is HttpOnly and SameSite=Lax on the fragment's origin", r.header("set-cookie").contains("HttpOnly; SameSite=Lax") && r.header("set-cookie").contains("Path=/;"), r.header("set-cookie"));
    let r = api.browser_op(&name, "sign", "s1", json!({ "text": "hello" }), Some(&cookie))?;
    s.ok("the same visitor's retry replays", r.status == 200 && r.body["replayed"] == true, &r);
    let r = api.browser_op(&name, "sign", "s1", json!({ "text": "hello" }), None)?;
    s.ok("another visitor's same id is theirs, not a replay", r.status == 200 && r.body["replayed"] == false && r.body["result"]["id"] == 2, &r);
    let r = api.browser_op(&name, "entries", "q", json!({}), None)?;
    s.ok("anyone reads a public query", r.status == 200 && r.body["result"]["entries"].as_array().map_or(0, |a| a.len()) == 2, &r);
    let r = api.browser_op(&name, "peek", "q", json!({}), Some(&cookie))?;
    s.ok("a viewer query is 401 for a visitor", r.status == 401, &r);
    let r = api.browser_op(&name, "clear", "c1", json!({}), Some(&cookie))?;
    s.ok("an editor mutation is 401 for a visitor", r.status == 401, &r);
    let r = api.call(crate::api::Call {
        method: "POST",
        url: api.site_url(&name, "__op/sign"),
        body: Some(br#"{"id":"f1","input":{"text":"form"}}"#.to_vec()),
        content_type: Some("application/x-www-form-urlencoded"),
        ..Default::default()
    })?;
    s.ok("a call that is not JSON is refused (no cross-site forms)", r.status == 400, &r);
    let r = api.call(crate::api::Call { method: "GET", url: api.site_url(&name, "__op/sign"), ..Default::default() })?;
    s.ok("GET on an operation is refused", r.status == 400, &r);

    // the public floor is rate limited per visitor
    let mut statuses = vec![];
    for i in 0..125 {
        let r = api.browser_op(&name, "entries", &format!("r{i}"), json!({}), Some(&cookie))?;
        statuses.push(r.status);
        if r.status == 429 {
            break;
        }
    }
    let allowed = statuses.iter().filter(|s| **s == 200).count();
    s.ok(
        "a visitor is limited to 60 public calls a minute",
        statuses.last() == Some(&429) && (57..=120).contains(&allowed),
        format!("{allowed} allowed, last {:?}", statuses.last()),
    );
    let r = api.browser_op(&name, "entries", "fresh", json!({}), None)?;
    s.ok("another visitor still gets through", r.status == 200, &r);
    let r = api.op(&owner, &name, "clear", "c1", json!({}))?;
    s.ok("members are not limited by the public floor", r.status == 200, &r);

    // a link fragment's visitors hold the link; a members fragment's hold nothing
    let link = s.named(api, &owner, "guestbook-link")?;
    let lc = s.create(api, &owner, &link)?;
    ship(s, &lc, GUESTBOOK_APP, GUESTBOOK_JSON);
    let r = api.browser_op(&link, "sign", "l1", json!({ "text": "x" }), None)?;
    s.ok("without the link, a link fragment is 401", r.status == 401, &r);
    let view = format!("fragview={}", lc["viewToken"].as_str().unwrap_or(""));
    let r = api.browser_op(&link, "peek", "l2", json!({}), Some(&view))?;
    s.ok("the link holder is a viewer", r.status == 200, &r);
    let r = api.browser_op(&link, "clear", "l3", json!({}), Some(&view))?;
    s.ok("but not an editor", r.status == 401, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{link}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let r = api.browser_op(&link, "peek", "l4", json!({}), Some(&view))?;
    s.ok("on a members fragment the link counts for nothing", r.status == 401, &r);
    Ok(())
}

/// Effects the platform refuses (in the app, and after the commit), a
/// commit that keeps failing, and a ledger row the app writes itself: the
/// app answers through all of it, and after a restart of the node.
pub fn effects(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("effects") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.name("effects");
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, EFFECTS_APP, EFFECTS_JSON);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let call = |op: &str, id: &str, input: Value| api.op(&owner, &name, op, id, input);
    let notes = || count(api, &owner, &name);
    let channel = |ch: &str| -> Vec<Value> {
        api.signed(&owner, "GET", &format!("/api/f/{name}/channels/{ch}"), None)
            .map(|r| r.body["records"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    };
    let slugs = |slug: &str| channel("feed").iter().filter(|r| r["body"]["slug"] == slug).count();
    let applied = |id: &str| channel("ops").iter().filter(|r| r["body"]["id"] == id).count();
    let note = |s: &Suite, slug: &str| s.fake.file_at(&repo, "main", &format!("notes/{slug}.md"));

    let r = call("note", "n1", json!({ "slug": "one", "text": "one" }))?;
    s.ok("a note lands", r.status == 200 && note(s, "one").as_deref() == Some(&b"one"[..]) && slugs("one") == 1, &r);

    // refused in the app, while the mutation can still roll back
    let r = call("pointer", "p1", json!({}))?;
    s.ok("a mutation that writes pointer-shaped text is refused before it commits", r.status == 422 && r.message().contains("an app does not write blob pointers"), &r);
    let r = call("wide", "w1", json!({}))?;
    s.ok("a path within 300 characters but over 300 bytes is refused before it commits", r.status == 422 && r.message().contains("at most 300 bytes"), &r);
    let r = call("half", "h1", json!({}))?;
    s.ok("a record holding half a character is refused before it commits", r.status == 422 && r.message().contains("lone surrogate"), &r);
    s.ok("none of them wrote anything", notes() == 1, notes());

    // refused by the supervisor, after a commit that got past the in-app check
    let r = call("sneaky", "s1", json!({}))?;
    s.ok(
        "a pointer past a broken in-app check is refused after the commit",
        r.status == 422 && r.message().contains("refused its effects") && r.message().contains("blob pointers"),
        &r,
    );
    s.ok("the pointer never reaches main", s.fake.file_at(&repo, "main", "big.bin").is_none(), "");
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/events"), None)?;
    s.ok(
        "the refusal is in events, and the mutation counts as applied",
        r.body["events"].as_array().is_some_and(|a| a.iter().any(|e| e["kind"] == "effects.refused" && e["data"]["id"] == "s1")) && applied("s1") == 1,
        &r,
    );
    s.ok("the next query answers (the mutation's own write stands)", notes() == 2, notes());
    let r = call("note", "n2", json!({ "slug": "two", "text": "two" }))?;
    s.ok("the next mutation applies", r.status == 200 && note(s, "two").as_deref() == Some(&b"two"[..]), &r);

    // a ledger row the app writes itself, naming its caller
    let r = call("forge", "f1", json!({}))?;
    s.ok("(the app writes a ledger row of its own)", r.status == 200, &r);

    // a passing failure: every try of one commit loses to another writer
    s.fake.sabotage_commit_packs(5);
    let r = call("note", "n3", json!({ "slug": "three", "text": "three" }))?;
    s.ok("a commit that keeps failing answers a passing failure", r.status == 502 && r.error() == "upstream_failed", &r);
    s.ok("an unrelated call answers meanwhile", notes() == 4, notes());
    // The file is on main as soon as the commit lands; the `ops` record
    // follows once the cell has moved its pin.
    let landed = s.eventually(Duration::from_secs(45), || note(s, "three").is_some() && applied("n3") > 0);
    s.ok("the alarm applies it on a later try", landed && note(s, "three").as_deref() == Some(&b"three"[..]), "");
    s.ok("its record is published once, and it is applied once", slugs("three") == 1 && applied("n3") == 1, json!(channel("feed")));
    let packs = s.fake.commit_pack_count();
    let r = call("note", "n3", json!({ "slug": "three", "text": "three" }))?;
    s.ok("its replay applies nothing again", r.status == 200 && r.body["replayed"] == true && s.fake.commit_pack_count() == packs && slugs("three") == 1, &r);

    // a restart: the next activation settles what was pending, and only that
    s.stop()?;
    s.start(false, true)?;
    s.ok("after a restart the app answers", notes() == 4, notes());
    let r = call("note", "n4", json!({ "slug": "four", "text": "four" }))?;
    s.ok("and applies mutations", r.status == 200 && note(s, "four").as_deref() == Some(&b"four"[..]), &r);
    let forged = channel("feed").into_iter().filter(|r| r["kind"] == "forged").count();
    s.ok("the app's own ledger row is never applied", forged == 0 && applied("forged-1") == 0, json!(channel("feed")));
    Ok(())
}
