//! Budgets (phase 4 slice C; ROADMAP decision 14), against the OpenRouter
//! fake: a fragment's owner pays for its paid steps from a monthly
//! allowance ($0.10 here), each step reserving its worst case ($0.05 for
//! text) and settling to the reported cost ($0.04 here). A step that does
//! not fit is held, and replays after a top-up; a settled step is never
//! paid again; the owner's own OpenRouter key carries the allowance as its
//! limit; the month resets.

use std::time::Duration;

use anyhow::Result;
use fragment_core::budget;
use fragment_fakes::openrouter::Costs;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use crate::api::Api;
use crate::Suite;

const BUDGET_APP: &[u8] = include_bytes!("../../fixtures/budget.mjs");
const BUDGET_JSON: &[u8] = include_bytes!("../../fixtures/budget.json");

fn month(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/budget", None).map(|r| r.body).unwrap_or(Value::Null)
}

fn m(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(-1)
}

pub fn budget(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("budget") {
        return Ok(());
    }
    let wait = Duration::from_secs(40);
    s.openrouter.set_costs(Costs { text: 0.04, image: 0.002, video_per_s: 0.01 });
    let email = "budget-owner@e2e.test";
    let session = api.sign_in(email)?;
    let owner = Keys::generate();
    let owner_id = api.approve(&session, &owner)?.body["id"].as_str().unwrap_or("").to_string();
    let org = format!("org:{}", owner_id.trim_start_matches("id:"));
    let name = s.named(api, &owner, "budget")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, BUDGET_APP, BUDGET_JSON);
    let run = |keys: &Keys, op: &str, id: &str, input: Value| -> Result<Value> {
        let r = api.op(keys, &name, op, id, input)?;
        Ok(settle(api, keys, &name, started(&r), &["succeeded", "held"], wait))
    };

    let v = month(api, &owner);
    s.ok(
        "a person starts the month with their budget, in their own billing org",
        m(&v, "allowanceMicros") == 100_000 && m(&v, "spentMicros") == 0 && v["billingOrg"] == org.as_str(),
        &v,
    );
    let r = run(&owner, "summarize", "t1", json!({ "text": "one" }))?;
    let key_of = |s: &Suite| s.openrouter.minted().into_iter().find(|k| k.name.contains(&org));
    let minted = key_of(s);
    s.ok("without its own key, a fragment's AI is paid by its owner", r["status"] == "succeeded" && r["output"]["text"] == "echo: one", &r);
    s.ok(
        "on the owner's own OpenRouter key, minted with their allowance as its monthly limit",
        minted.as_ref().is_some_and(|k| k.limit == Some(0.1) && k.limit_reset.as_deref() == Some("monthly")),
        format!("{minted:?}"),
    );
    let hash = minted.as_ref().map(|k| k.hash.clone()).unwrap_or_default();
    let patches_of = |s: &Suite| s.openrouter.patches().into_iter().filter(|(h, _)| *h == hash).map(|(_, b)| b).collect::<Vec<_>>();
    let chats = |s: &Suite| s.openrouter.calls().into_iter().filter(|c| c.1 == "/api/v1/chat/completions").collect::<Vec<_>>();
    let used_key = chats(s).last().map(|c| c.3.clone()).unwrap_or_default();
    s.ok("the call carried that key", minted.as_ref().is_some_and(|k| used_key == format!("Bearer {}", k.key)), &used_key);
    let v = month(api, &owner);
    s.ok("it settles to the reported cost", m(&v, "spentMicros") == 40_000 && m(&v, "reservedMicros") == 0 && m(&v, "remainingMicros") == 60_000, &v);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/runs/{}", r["id"]), None)?;
    s.ok("and the run shows what it cost", r.body["costMicros"] == 40_000, &r);

    // a visitor's call: the owner pays
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let r = api.browser_op(&name, "ask", "p1", json!({ "text": "from a visitor" }), None)?;
    let visitor = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let v = month(api, &owner);
    let row = v["usage"].as_array().and_then(|u| u.iter().find(|u| u["principal"].as_str().is_some_and(|p| p.starts_with("anon:")))).cloned().unwrap_or_default();
    s.ok("a public visitor's call bills the owner", visitor["status"] == "succeeded" && m(&v, "spentMicros") == 80_000 && row["billingOrg"] == org.as_str(), &v);
    s.ok("at 80% the month warns", v["warn"] == true, &v);

    // the month cannot cover the next step
    let before = chats(s).len();
    let r = run(&owner, "summarize", "t3", json!({ "text": "three" }))?;
    let held = r["id"].as_i64().unwrap_or(0);
    s.ok("a step the month cannot cover is held, saying so", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("budget used up")), &r);
    s.ok("and never reached OpenRouter", chats(s).len() == before, chats(s).len() - before);

    // a top-up, by the fleet's operators
    let top = format!("/api/budget/{owner_id}/top-up");
    let r = api.signed(&owner, "POST", &top, Some(&json!({ "usd": 0.1 })))?;
    s.ok("only the fleet's operators top up a budget", r.status == 403, &r);
    let op_session = api.sign_in("operator@e2e.test")?;
    api.approve(&op_session, &s.operator)?;
    let r = api.signed(&s.operator, "POST", &top, Some(&json!({ "usd": 0.1 })))?;
    s.ok("an operator's top-up raises the month's allowance", r.status == 200 && r.body["allowanceMicros"] == 200_000, &r);
    let patched = patches_of(s);
    s.ok("and the owner's key limit with it", patched.last().is_some_and(|b| b["limit"] == 0.2), format!("{patched:?}"));
    api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": held })))?;
    let r = settle(api, &owner, &name, held, &["succeeded", "held"], wait);
    s.ok("the held run succeeds on replay after the top-up", r["status"] == "succeeded" && r["output"]["text"] == "echo: three", &r);

    // a replayed run pays only for the steps it had not paid for
    let before = chats(s).len();
    let r = run(&owner, "twice", "w1", json!({ "a": "first", "b": "second" }))?;
    s.ok("a second step the month cannot cover holds the run after the first was paid", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("budget used up")), &r);
    let twice = r["id"].as_i64().unwrap_or(0);
    api.signed(&s.operator, "POST", &top, Some(&json!({ "usd": 0.05 })))?;
    api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": twice })))?;
    let r = settle(api, &owner, &name, twice, &["succeeded", "held"], wait);
    let after = chats(s).len();
    s.ok(
        "on replay the paid step answers what it answered, unpaid, and only the other runs",
        r["status"] == "succeeded" && r["output"] == json!({ "one": "echo: first", "two": "echo: second" }) && after - before == 2 && r["costMicros"] == 80_000,
        format!("{r} ({} calls)", after - before),
    );

    // two jobs racing for the last reservation
    let v = month(api, &owner);
    let short = 60_000 - m(&v, "remainingMicros");
    if short > 0 {
        api.signed(&s.operator, "POST", &top, Some(&json!({ "usd": short as f64 / 1e6 })))?;
    }
    let a = api.op(&owner, &name, "summarize", "race-a", json!({ "text": "a" }))?;
    let b = api.op(&owner, &name, "summarize", "race-b", json!({ "text": "b" }))?;
    let (ra, rb) = (settle(api, &owner, &name, started(&a), &["succeeded", "held"], wait), settle(api, &owner, &name, started(&b), &["succeeded", "held"], wait));
    let won = [&ra, &rb].iter().filter(|r| r["status"] == "succeeded").count();
    s.ok("two jobs racing for the last reservation cannot both run", won == 1, format!("{ra} {rb}"));
    let v = month(api, &owner);
    s.ok("and the month never goes past its allowance", m(&v, "spentMicros") + m(&v, "reservedMicros") <= m(&v, "allowanceMicros"), &v);
    let spent_at_openrouter = key_of(s).map(|k| (k.usage * 1e6).round() as i64).unwrap_or(-1);
    s.ok("what the month spent is what OpenRouter charged the key", m(&v, "spentMicros") == spent_at_openrouter, format!("{} vs {spent_at_openrouter}", m(&v, "spentMicros")));

    // the usage rows, in the shape finite.computer's Core takes
    let r = api.signed(&owner, "GET", "/api/budget/usage", None)?;
    let rows = r.body["usage"].as_array().cloned().unwrap_or_default();
    s.ok(
        "each paid step is one usage row: source reference, billing org, period, unit, quantity",
        !rows.is_empty()
            && rows.iter().all(|u| {
                u["sourceRef"].as_str().is_some_and(|s| s.starts_with(&format!("{name}@")) && s.contains("/run/") && s.contains("/step/"))
                    && u["billingOrg"] == org.as_str()
                    && u["unit"] == "usd_micro"
                    && u["period"] == v["period"]
                    && u["state"] == "settled"
                    && u["quantity"] == 40_000
            }),
        &r,
    );

    // a fragment with its own key pays for itself
    let own = s.named(api, &owner, "budget-own")?;
    let c = s.create(api, &owner, &own)?;
    ship(s, &c, BUDGET_APP, BUDGET_JSON);
    api.call(crate::api::Call {
        method: "PUT",
        url: format!("{}/api/f/{own}/secrets/OPENROUTER_API_KEY", api.base),
        body: Some(crate::OPENROUTER_KEY.as_bytes().to_vec()),
        keys: Some(&owner),
        ..crate::api::Call::default()
    })?;
    let spent = m(&month(api, &owner), "spentMicros");
    let r = api.op(&owner, &own, "summarize", "own1", json!({ "text": "mine" }))?;
    let r = settle(api, &owner, &own, started(&r), &["succeeded", "held"], wait);
    let last = chats(s).last().map(|c| c.3.clone()).unwrap_or_default();
    s.ok(
        "a fragment with its own OPENROUTER_API_KEY pays with it, unmetered",
        r["status"] == "succeeded" && last == format!("Bearer {}", crate::OPENROUTER_KEY) && m(&month(api, &owner), "spentMicros") == spent,
        &r,
    );

    // where people see it
    let r = api.call(crate::api::Call { method: "GET", url: format!("{}/", api.base), cookie: Some(format!("fragment_session={session}")), ..crate::api::Call::default() })?;
    s.ok("the platform page shows what is left this month", r.text.contains("left") && r.text.contains("AI this month"), &r);
    let home = s.dir("budget-home");
    s.login(api, &home);
    let out = s.cli(api, &home, &["budget"]);
    s.ok("fragment budget says what is left", out.status.success() && String::from_utf8_lossy(&out.stdout).contains("$0.10 left"), String::from_utf8_lossy(&out.stdout));

    // a new month
    let this_month = month(api, &owner)["period"].clone();
    let r = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": owner_id, "offsetMs": 32_i64 * 24 * 3600 * 1000 })))?;
    s.openrouter.reset_period();
    let v = month(api, &owner);
    s.ok(
        "the month resets on the 1st: nothing spent, the plain budget (top-ups were that month's)",
        r.status == 200 && v["period"] == r.body["period"] && v["period"] != this_month && m(&v, "spentMicros") == 0 && m(&v, "allowanceMicros") == 100_000,
        &v,
    );
    let r = run(&owner, "summarize", "next-month", json!({ "text": "again" }))?;
    let patched = patches_of(s);
    s.ok("and the key's limit goes back with it", r["status"] == "succeeded" && patched.last().is_some_and(|b| b["limit"] == 0.1), format!("{r} {patched:?}"));

    // an answer that names no cost is charged the step's reservation: the
    // money path fails closed, never at zero
    let before = month(api, &owner);
    s.openrouter.omit_costs(true);
    let r = run(&owner, "summarize", "unpriced", json!({ "text": "no price" }));
    s.openrouter.omit_costs(false);
    let r = r?;
    let v = month(api, &owner);
    s.ok(
        "a step whose answer names no cost is charged its reservation",
        r["status"] == "succeeded" && m(&v, "spentMicros") == m(&before, "spentMicros") + budget::TEXT_RESERVE && m(&v, "reservedMicros") == 0,
        format!("{before} then {v}"),
    );
    s.ok("and the event log says so", events(api, &owner, &name).contains("ai.cost-missing"), "no ai.cost-missing");

    // a video that ends undelivered is charged nothing, and its
    // reservation comes back; so does one whose run is held while it waits
    api.signed(&s.operator, "POST", &top, Some(&json!({ "usd": 1.0 })))?;
    let before = month(api, &owner);
    let r = run(&owner, "film", "expired", json!({ "prompt": "a video that will expire", "path": "v/expired.mp4" }))?;
    let v = month(api, &owner);
    s.ok("a video that expires holds its run, saying so", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("expired")), &r);
    s.ok(
        "and is charged nothing: its reservation comes back",
        m(&v, "spentMicros") == m(&before, "spentMicros") && m(&v, "reservedMicros") == 0 && r["costMicros"] == 0,
        format!("{before} then {v}"),
    );
    s.ok("the event log says the video was not delivered", events(api, &owner, &name).contains("ai.video-undelivered"), "no ai.video-undelivered");
    let r = run(&owner, "film", "vanished", json!({ "prompt": "a video that will vanish", "path": "v/vanished.mp4" }))?;
    let mut v = Value::Null;
    let released = s.eventually(wait, || {
        v = month(api, &owner);
        m(&v, "reservedMicros") == 0
    });
    s.ok(
        "a run held while its video waits for its cost gives the reservation back",
        r["status"] == "held" && released && m(&v, "spentMicros") == m(&before, "spentMicros"),
        format!("{r} {v}"),
    );
    s.openrouter.set_costs(Costs::default());
    Ok(())
}

fn events(api: &Api, keys: &Keys, name: &str) -> String {
    api.signed(keys, "GET", &format!("/api/f/{name}/events?since=0"), None).map(|r| r.text).unwrap_or_default()
}
