//! A tenant's reach over the node (docs/hardening.md H2): an app's database
//! stops at 16 MiB and the rest of the fragment keeps working; a node that
//! cannot load another app says so (503) while the apps it holds serve on.

use anyhow::Result;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::Api;
use crate::Suite;

const HOARD_APP: &[u8] = include_bytes!("../../fixtures/hoard.mjs");
const HOARD_JSON: &[u8] = include_bytes!("../../fixtures/hoard.json");
const GUESTBOOK_APP: &[u8] = include_bytes!("../../fixtures/guestbook.mjs");
const GUESTBOOK_JSON: &[u8] = include_bytes!("../../fixtures/guestbook.json");
const MIB: i64 = 1024 * 1024;

pub fn facet_cap(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("facet-cap") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.name("hoard");
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, HOARD_APP, HOARD_JSON);
    let neighbor = s.name("hoard-neighbor");
    let n = s.create(api, &owner, &neighbor)?;
    ship(s, &n, GUESTBOOK_APP, GUESTBOOK_JSON);
    let size = |api: &Api| -> Value { api.op(&owner, &name, "size", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or(Value::Null) };
    // 4 MiB at a time until the node refuses
    let mut stored = 0;
    let mut refusal = None;
    for i in 0..8 {
        let r = api.op(&owner, &name, "add", &format!("a{i}"), json!({ "mib": 4 }))?;
        if r.status == 200 {
            stored = r.body["result"]["rows"].as_i64().unwrap_or(0);
        } else {
            refusal = Some(r);
            break;
        }
    }
    let refused = refusal.as_ref().is_some_and(|r| r.status == 507 && r.error() == "storage_full");
    s.ok("an app's database stops at its 16 MiB cap (507 storage_full)", refused && (12..16).contains(&stored), format!("{stored} MiB stored; {}", refusal.as_ref().map_or("never refused".into(), |r| r.to_string())));
    let now = size(api);
    s.ok(
        "the refused mutation rolled back, and the app still answers",
        now["rows"].as_i64() == Some(stored) && now["bytes"].as_i64().is_some_and(|b| b <= 16 * MIB),
        &now,
    );
    let r = api.op(&owner, &name, "empty", "e1", json!({}))?;
    s.ok("emptying it works at the cap", r.status == 200 && r.body["result"]["rows"] == 0, &r);
    let r = api.op(&owner, &name, "add", "after", json!({ "mib": 4 }))?;
    s.ok("then it takes writes again", r.status == 200 && r.body["result"]["rows"] == 4, &r);
    // writes outside a mutation (which the model forbids) meet the node's own
    // stop, 4 MiB above the cap
    let r = api.op(&owner, &name, "sneak", "s1", json!({ "mib": 24 }))?;
    s.ok("a write from a query stops at the node's hard stop", r.status == 422 && r.message().contains("full"), &r);
    let r = api.op(&owner, &name, "empty", "e2", json!({}))?;
    s.ok("and the app still empties itself from there", r.status == 200 && r.body["result"]["rows"] == 0, &r);
    let r = api.op(&owner, &neighbor, "sign", "n1", json!({ "text": "next door" }))?;
    s.ok("the node and its other apps carry on", r.status == 200, &r);
    Ok(())
}

const PROBE_APP: &[u8] = include_bytes!("../../fixtures/probe.mjs");
const PROBE_JSON: &[u8] = include_bytes!("../../fixtures/probe.json");

/// What a loaded worker may not do (our celld fork, `CELLD_DYNAMIC_LOCKDOWN`),
/// and a heap that grows past twice its limit ends that app's turn only.
pub fn lockdown(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("app-lockdown") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.name("probe");
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, PROBE_APP, PROBE_JSON);
    let neighbor = s.name("probe-neighbor");
    let n = s.create(api, &owner, &neighbor)?;
    ship(s, &n, GUESTBOOK_APP, GUESTBOOK_JSON);
    let r = api.status(&owner, &name)?;
    s.ok("the probe app installs", r.body["code"]["error"].is_null() && r.body["code"]["sha"].is_string(), &r.body["code"]);
    let tried = |op: &str| -> Result<(u16, Value)> {
        let r = api.op(&owner, &name, op, op, json!({}))?;
        println!("      ({op}: {r})");
        Ok((r.status, r.body["result"].clone()))
    };
    let refused = |(status, v): &(u16, Value), says: &str| *status == 200 && v["ok"] == false && v["error"].as_str().is_some_and(|e| e.contains(says));
    let r = tried("try_eval")?;
    s.ok("an app cannot eval", refused(&r, "Code generation from strings disallowed"), &r.1);
    let r = tried("try_function")?;
    s.ok("nor build a Function from a string", refused(&r, "Code generation from strings disallowed"), &r.1);
    let r = tried("try_wait")?;
    s.ok("nor block its thread in Atomics.wait", refused(&r, "Atomics.wait cannot be called"), &r.1);
    let r = api.op(&owner, &name, "bomb", "b", json!({}))?;
    s.ok("a heap past twice its limit ends the app's turn (422)", r.status == 422 && r.message().contains("memory limit"), &r);
    let r = api.op(&owner, &neighbor, "sign", "n1", json!({ "text": "next door" }))?;
    s.ok("the node and its other apps carry on", r.status == 200, &r);
    let r = api.op(&owner, &name, "hello", "h", json!({}))?;
    s.ok("and the app itself answers again", r.status == 200 && r.body["result"]["hello"] == "still here", &r);
    Ok(())
}

/// Runs last: a node of its own, with room for two apps.
pub fn node_full(s: &mut Suite) -> Result<()> {
    if !s.section("node-full") {
        return Ok(());
    }
    if s.node.is_some() {
        s.stop()?;
    }
    s.node_env_extra = vec![("CELLD_LOADED_WORKERS_MAX".into(), "3".into())];
    let api = s.start(true, true)?;
    let owner = api.person()?;
    let names: Vec<String> = (0..3).map(|i| s.name(&format!("full{i}"))).collect();
    for name in &names {
        let c = s.create(&api, &owner, name)?;
        ship(s, &c, GUESTBOOK_APP, GUESTBOOK_JSON);
    }
    let sign = |name: &str, id: &str| api.op(&owner, name, "sign", id, json!({ "text": "hi" }));
    for name in &names[..2] {
        let r = sign(name, "first")?;
        s.ok("an app loads while the node has room", r.status == 200, &r);
    }
    let r = sign(&names[2], "first")?;
    s.ok("a node that cannot load another app says so (503 node_full)", r.status == 503 && r.error() == "node_full", &r);
    let r = sign(&names[0], "again")?;
    s.ok("the apps it holds keep serving", r.status == 200, &r);
    let r = api.status(&owner, &names[2])?;
    s.ok("and the refused fragment still answers without its app", r.status == 200, &r);
    s.stop()?;
    s.node_env_extra.clear();
    Ok(())
}

// the node's hard stop sits above the platform's cap
const _: () = assert!(fragment_devstack::FACET_MAX_BYTES == fragment_proto::limits::APP_DB_MAX_BYTES + 4 * 1024 * 1024);
