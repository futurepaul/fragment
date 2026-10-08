//! A tenant's reach: an app's database stops at 16 MiB, or at the cap its
//! fragment.json declares (at most 1 GiB), and the rest of the fragment
//! keeps working, and an app cannot run code from strings, block
//! its thread, or use the parts of its Durable Object the platform takes
//! away. Its CPU and memory limits are the runtime's, which local workerd
//! does not enforce (spike S1): the hosted lane checks those.

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
    if !s.section("facet-cap", &[]) {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "hoard")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, HOARD_APP, HOARD_JSON);
    let neighbor = s.named(api, &owner, "hoard-neighbor")?;
    let n = s.create(api, &owner, &neighbor)?;
    ship(s, &n, GUESTBOOK_APP, GUESTBOOK_JSON);
    let size = |api: &Api| -> Value { api.op(&owner, &name, "size", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or(Value::Null) };
    // 4 MiB at a time until the platform refuses
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
    let r = api.op(&owner, &neighbor, "sign", "n1", json!({ "text": "next door" }))?;
    s.ok("the other apps carry on", r.status == 200, &r);

    // a cap the app declares (fragment.json's storage.maxBytes): past 16 MiB, to its own
    let mut declared: Value = serde_json::from_slice(HOARD_JSON)?;
    declared["storage"] = json!({ "maxBytes": 32 * MIB });
    ship(s, &c, HOARD_APP, declared.to_string().as_bytes());
    let mut stored = 4;
    let mut refusal = None;
    for i in 0..10 {
        let r = api.op(&owner, &name, "add", &format!("d{i}"), json!({ "mib": 4 }))?;
        if r.status == 200 {
            stored = r.body["result"]["rows"].as_i64().unwrap_or(0);
        } else {
            refusal = Some(r);
            break;
        }
    }
    let refused = refusal.as_ref().is_some_and(|r| r.status == 507 && r.error() == "storage_full");
    s.ok(
        "an app that declares 32 MiB holds past 16 MiB, and stops at its own cap",
        refused && (28..32).contains(&stored),
        format!("{stored} MiB stored; {}", refusal.as_ref().map_or("never refused".into(), |r| r.to_string())),
    );
    declared["storage"] = json!({ "maxBytes": 2048 * MIB });
    ship(s, &c, HOARD_APP, declared.to_string().as_bytes());
    let code = api.status(&owner, &name).map(|r| r.body["code"].clone()).unwrap_or_default();
    s.ok("one past the most an app may declare (1 GiB) is refused at deploy", code["error"].as_str().is_some_and(|e| e.contains("storage.maxBytes")), &code);
    Ok(())
}

const PROBE_APP: &[u8] = include_bytes!("../../fixtures/probe.mjs");
const PROBE_JSON: &[u8] = include_bytes!("../../fixtures/probe.json");

/// What an app may not do: code from strings and `Atomics.wait` (the
/// runtime's), and an alarm, an async transaction, a KV write, or a facet
/// of its own (platform.mjs: S1 found an app's alarm wedges its facet).
pub fn lockdown(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("app-lockdown", &[]) {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "probe")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, PROBE_APP, PROBE_JSON);
    let neighbor = s.named(api, &owner, "probe-neighbor")?;
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
    let r = tried("try_alarm")?;
    s.ok("nor set an alarm (its triggers are its schedule)", refused(&r, "an app has no alarm"), &r.1);
    let r = tried("try_alarm_proto")?;
    s.ok("nor find the original on the prototype", refused(&r, "an app has no alarm"), &r.1);
    let r = tried("try_transaction")?;
    s.ok("nor open a transaction of its own", refused(&r, "a mutation is the app's transaction"), &r.1);
    let r = tried("try_put")?;
    s.ok("nor write outside its SQLite", refused(&r, "an app writes its SQLite"), &r.1);
    let r = tried("try_facets")?;
    s.ok("nor start a facet of its own", refused(&r, "it starts none of its own"), &r.1);
    let r = api.op(&owner, &neighbor, "sign", "n1", json!({ "text": "next door" }))?;
    s.ok("its neighbors carry on", r.status == 200, &r);
    let r = api.op(&owner, &name, "hello", "h", json!({}))?;
    s.ok("and the app itself answers again", r.status == 200 && r.body["result"]["hello"] == "still here", &r);
    Ok(())
}
