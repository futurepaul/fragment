//! The usage ledger (phase 3 of docs/cloudflare-v1.md; docs/ledger.md),
//! end to end on workerd: plans and guests (who make no fragments, and
//! still edit a seat's), a seat's included credit, operators' commands, AI
//! steps' reserve, settle and release (bugs 2 and 3), zero credit and the
//! overdraft (past it, no new fragments, and cron and triggers start no
//! runs until a top-up), a fragment's cap, the meters that reach the
//! ledger through the queue, and the model route, streamed and not. The model is the Workers AI fake behind the model route (a lower
//! rung at the vendor boundary, labeled so); a test sets the usage each
//! answer reports, so every charge is checked against the price book.

use std::time::Duration;

use anyhow::Result;
use fragment_core::price::{PriceBook, Usage};
use fragment_fakes::workers_ai::{image_bytes, Used, IMAGE_MODEL};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agents::settle as settle_agent;
use super::app::ship;
use super::jobs::{settle, started};
use crate::api::{url_enc, Api, Reply};

use crate::Suite;

const LEDGER_APP: &[u8] = include_bytes!("../../fixtures/ledger.mjs");
const LEDGER_JSON: &[u8] = include_bytes!("../../fixtures/ledger.json");
/// A fragment whose cron (due next new year, unless a test makes it due)
/// and file trigger each run a mutation, and a read `landed` asks.
const TRIGGERS_APP: &[u8] = br#"import { DurableObject } from "cloudflare:workers";
export class App extends DurableObject {
  tick() { return { ok: true }; }
  filed({ paths }) { return { paths }; }
  notes() { return { notes: [] }; }
}
"#;
const TRIGGERS_JSON: &[u8] = br#"{
  "operations": { "tick": { "kind": "mutation" }, "filed": { "kind": "mutation" }, "notes": { "kind": "query", "role": "viewer" } },
  "triggers": [{ "cron": "0 0 1 1 *", "run": "tick" }, { "files": "notes/**", "run": "filed" }]
}"#;
const FLASH: &str = "@cf/zai-org/glm-5.3-flash";
const GLM: &str = "@cf/zai-org/glm-5.3";
const USD: i64 = 1_000_000;

fn tokens(model: &str, input: u64, cached: u64, output: u64) -> Usage {
    Usage::Tokens { model: model.into(), input, cached_input: cached, cache_write: 0, output }
}

/// What the default book charges for `usage`.
fn charge(usage: &Usage) -> i64 {
    PriceBook::defaults().price(usage).expect("the book prices the tiers' models").charge
}

/// The fewest output tokens on `model`, after `prompt` in, that charge at
/// least `micros` (a call a test sizes to move a balance where it wants).
fn tokens_costing(model: &str, prompt: u64, micros: i64) -> u64 {
    let costs = |n: u64| charge(&tokens(model, prompt, 0, n));
    let mut hi = 1u64;
    // bounded: doubling past any charge a ledger takes ($100,000 a row)
    for _ in 0..64 {
        if costs(hi) >= micros {
            break;
        }
        hi *= 2;
    }
    let mut lo = 0u64;
    // bounded: a binary search over 64 bits
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if costs(mid) < micros {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn ledger(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/ledger", None).map(|r| r.body).unwrap_or(Value::Null)
}

fn m(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(i64::MIN)
}

/// A ledger's references under `prefix` (a test hook); one that does not
/// answer says why, and lists none.
pub(super) fn entries(api: &Api, identity: &str, prefix: &str) -> Vec<Value> {
    match api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": identity, "op": "entries", "prefix": prefix }))) {
        Ok(r) if r.status == 200 => r.body["entries"].as_array().cloned().unwrap_or_default(),
        Ok(r) => {
            println!("      (the ledger's entries under {prefix}: {r})");
            Vec::new()
        }
        Err(e) => {
            println!("      (the ledger's entries under {prefix}: {e:#})");
            Vec::new()
        }
    }
}

/// The reservations of one run's steps, every attempt's.
fn run_steps(api: &Api, identity: &str, name: &str, run: &Value) -> Vec<Value> {
    let marker = format!("/run/{}/attempt/", run["id"]);
    entries(api, identity, &format!("step:{name}@")).into_iter().filter(|e| e["ref"].as_str().is_some_and(|r| r.contains(&marker))).collect()
}

/// What a run's steps were charged on the ledger, together.
fn run_charged(api: &Api, identity: &str, name: &str, run: &Value) -> i64 {
    run_steps(api, identity, name, run).iter().filter_map(|e| e["entry"]["end"]["charge"].as_i64()).sum()
}


/// What moved a ledger's balance, ever (a test hook).
fn totals(api: &Api, identity: &str) -> Value {
    api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": identity, "op": "totals" }))).map(|r| r.body).unwrap_or(Value::Null)
}

fn lever(api: &Api, fragment: &str, op: &str, extra: Value) -> Result<Reply> {
    let mut body = json!({ "fragment": fragment, "op": op });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    api.unsigned("POST", "/api/test/fragment", Some(&body))
}

/// Waits for a deploy to land (by the webhook): a query answers once it has.
fn landed(s: &Suite, api: &Api, keys: &Keys, name: &str, wait: Duration) {
    s.eventually(wait, || api.op(keys, name, "notes", "q", json!({})).is_ok_and(|r| r.status == 200));
}

/// A reservation's end, as the ledger kept it: `settled` or `released` (none while held).
pub(super) fn end_of(entry: &Value) -> String {
    entry["entry"]["end"]["end"].as_str().unwrap_or("held").to_string()
}

pub fn ledger_lane(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("ledger") {
        return Ok(());
    }
    let wait = Duration::from_secs(40);
    s.ai.clear_script();
    let op_session = api.sign_in("operator@e2e.test")?;
    let op_id = api.approve(&op_session, &s.operator)?.body["id"].as_str().unwrap_or("").to_string();
    let operator = s.operator.clone();
    let command = |who: &str, what: &str, body: Value| api.signed(&operator, "POST", &format!("/api/ledger/{who}/{what}"), Some(&body));

    // ---- plans: a new person is the deployment's default (a seat here)
    let owner = api.person()?;
    let owner_id = api.identity(&owner)?;
    let v = ledger(api, &owner);
    s.ok(
        "a new person is a seat here (FRAGMENT_DEFAULT_PLAN), with the month's included credit",
        v["plan"] == "seat" && v["seat"] == "active" && m(&v, "includedGrantedMicros") == 50 * USD && m(&v, "balanceMicros") == 50 * USD && v["standing"]["standing"] == "ok",
        &v,
    );

    // ---- operators' commands: valid, refused, replayed, conflicting
    let grant = json!({ "id": "g1", "micros": USD, "by": op_id, "why": "the ledger lane" });
    let r = api.signed(&owner, "POST", &format!("/api/ledger/{owner_id}/grant"), Some(&grant))?;
    s.ok("only the deployment's operators grant credit", r.status == 403, &r);
    let r = command(&owner_id, "grant", grant.clone())?;
    let after = ledger(api, &owner);
    s.ok("an operator's grant is purchased credit", r.status == 200 && m(&after, "purchasedMicros") == USD && m(&after, "balanceMicros") == 51 * USD, &after);
    let r = command(&owner_id, "grant", grant.clone())?;
    s.ok("the same grant again changes nothing", r.status == 200 && ledger(api, &owner) == after, &r);
    let r = command(&owner_id, "grant", json!({ "id": "g1", "micros": 2 * USD, "by": op_id, "why": "the ledger lane" }))?;
    s.ok("its id with another body is refused (409)", r.status == 409 && r.error() == "conflicting_body", &r);
    let r = command(&owner_id, "grant", json!({ "id": "g0", "micros": 0, "by": op_id, "why": "nothing" }))?;
    s.ok("a grant of nothing is refused (400)", r.status == 400, &r);
    let r = command(&owner_id, "grant", json!({ "id": "g2", "micro": 5, "by": op_id, "why": "" }))?;
    s.ok("a misspelt field is refused, not read as nothing", r.status == 400, &r);
    let r = command(&owner_id, "grant", json!({ "id": "g3", "micros": 5, "by": owner_id, "why": "" }))?;
    s.ok("a grant names the operator who signs it", r.status == 400, &r);
    let username = api.username(&owner)?;
    let r = command(&username, "overdraft", json!({ "id": "o1", "micros": 2 * USD }))?;
    let again = command(&username, "overdraft", json!({ "id": "o1", "micros": 2 * USD }))?;
    let bad = command(&username, "overdraft", json!({ "id": "o2", "micros": 2_000 * USD }))?;
    s.ok("an operator names a person by username too; an overdraft past its limit is refused", r.status == 200 && again.status == 200 && bad.status == 400, json!([r.status, again.status, bad.status]));
    let r = command(&owner_id, "plan", json!({ "id": "p1", "plan": "emperor" }))?;
    s.ok("a plan that is none is refused", r.status == 400, &r);
    // a seat's state, as its payment hook will send it: ordered by `seq`
    let seated = api.person()?;
    let seated_id = api.identity(&seated)?;
    let canceled = command(&seated_id, "seat", json!({ "id": "s2", "seat": "canceled", "seq": 2 }))?;
    let late = command(&seated_id, "seat", json!({ "id": "s1", "seat": "active", "seq": 1 }))?;
    let again = command(&seated_id, "seat", json!({ "id": "s2", "seat": "canceled", "seq": 2 }))?;
    let zero = command(&seated_id, "seat", json!({ "id": "s0", "seat": "active", "seq": 0 }))?;
    let sv = ledger(api, &seated);
    s.ok(
        "a canceled seat stops agents; a change older than it (by seq) changes nothing; again is the same; seq 0 is refused",
        canceled.status == 200 && late.status == 200 && again.status == 200 && zero.status == 400
            && sv["seat"] == "canceled" && sv["standing"] == json!({ "standing": "agents_stopped", "why": "seat_canceled" }),
        &sv,
    );

    // ---- a guest: no agents, no AI steps, no fragments of their own
    // (decision 25; Paul, 2026-10-03). Their fragment was made while they
    // were a seat (the default here), so it is one a guest owns.
    let guest = api.person()?;
    let guest_id = api.identity(&guest)?;
    let guest_app = s.named(api, &guest, "ledger-guest")?;
    let c = s.create(api, &guest, &guest_app)?;
    ship(s, &c, LEDGER_APP, LEDGER_JSON);
    landed(s, api, &guest, &guest_app, wait);
    let r = command(&guest_id, "plan", json!({ "id": "to-guest", "plan": "guest" }))?;
    let gv = ledger(api, &guest);
    s.ok(
        "an operator sets a plan: a guest's standing stops agents, whatever its credit",
        r.status == 200 && gv["plan"] == "guest" && gv["standing"] == json!({ "standing": "agents_stopped", "why": "guest" }),
        &gv,
    );
    let refused = |r: &Reply| r.status == 403 && r.error() == "forbidden" && r.message().starts_with("guests can't create fragments");
    let made = s.named(api, &guest, "ledger-guest-new")?;
    let r = api.create(&guest, &made)?;
    s.ok("a guest's create is refused, 403, saying why", refused(&r), &r);
    let again = api.create(&guest, &made)?;
    let none = api.status(&guest, &made)?;
    s.ok("asked again it is refused again (a refusal is not remembered), and nothing was made", refused(&again) && none.status == 404, format!("{again} | {none}"));
    let r = api.signed(&guest, "POST", "/api/fragments", Some(&json!({ "name": s.name("ledger-guest-todo"), "template": "todo" })))?;
    s.ok("nor does a guest make one from a template", refused(&r), &r);
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&guest, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &guest) })))?;
    anyhow::ensure!(r.status == 200, "a guest's agent key: {r}");
    let r = api.signed(&hand, "POST", "/api/fragments", Some(&json!({ "name": s.name("ledger-guest-agent") })))?;
    s.ok("nor does an agent make one for a guest", refused(&r), &r);
    let calls = s.ai.calls().len();
    let r = api.op(&guest, &guest_app, "summarize", "g-1", json!({ "text": "a guest's step" }))?;
    let run = settle(api, &guest, &guest_app, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "a guest's AI step is refused, saying why, and never reaches the model",
        run["status"] == "held" && run["error"].as_str().is_some_and(|e| e.contains("a guest pays for nothing")) && s.ai.calls().len() == calls,
        &run,
    );
    let agents = s.agents()?;
    let bot = s.name("guest-bot");
    agents.signed(&guest, "POST", "/api/agents", Some(&json!({ "name": bot })))?;
    agents.signed(&guest, "POST", &format!("/api/a/{bot}/turns"), Some(&json!({ "text": "hello" })))?;
    let gv = settle_agent(s, &agents, &guest, &bot, wait);
    s.ok(
        "a guest's agent turn is refused, saying why, and never reaches the model",
        gv["outcome"] == "error" && gv["error"].as_str().is_some_and(|e| e.contains("a guest pays for nothing")) && s.ai.calls().len() == calls,
        json!({ "outcome": gv["outcome"], "error": gv["error"] }),
    );
    let r = api.op(&guest, &guest_app, "note", "gn-1", json!({ "text": "a guest writes" }))?;
    s.ok("a guest's fragment still takes writes (it is billed nothing)", r.status == 200, &r);

    // ---- an AI step: reserved, then settled from the usage it reported
    let name = s.named(api, &owner, "ledger")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, LEDGER_APP, LEDGER_JSON);
    landed(s, api, &owner, &name, wait);

    // a guest editor of a seat's fragment still writes there: its owner pays (decision 26)
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{guest_id}"), Some(&json!({ "role": "editor" })))?;
    let w = api.op(&guest, &name, "note", "gn-seat", json!({ "text": "a guest edits a seat's fragment" }))?;
    let post = api.signed(&guest, "POST", &format!("/api/f/{name}/channels/talk"), Some(&json!({ "id": "g-talk", "body": { "text": "a guest posts" } })))?;
    s.ok("a guest editor still writes to a seat's fragment, and posts there", w.status == 200 && post.status == 200, format!("{w} | {post}"));
    let r = command(&guest_id, "plan", json!({ "id": "to-seat", "plan": "seat" }))?;
    let made_now = api.create(&guest, &made)?;
    s.ok("an operator moves the guest to a seat, and the same create makes the fragment", r.status == 200 && made_now.status == 200 && made_now.body["name"] == made.as_str(), &made_now);
    let before = ledger(api, &owner);
    s.ai.set_usage(&[Used { prompt: 1000, cached: 200, completion: 500 }]);
    let r = api.op(&owner, &name, "summarize", "t-1", json!({ "text": "the notes", "tier": "cheap" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let paid = charge(&tokens(FLASH, 800, 200, 500));
    let after = ledger(api, &owner);
    s.ok(
        "an AI step runs on its tier's model through the model route",
        run["status"] == "succeeded" && run["output"]["text"] == "echo: the notes" && run["output"]["model"] == FLASH && run["output"]["tier"] == "cheap",
        &run,
    );
    let step = entries(api, &owner_id, &format!("step:{name}@"));
    s.ok(
        "it reserved, then settled from its usage (input less what was cached), once",
        step.len() == 1 && end_of(&step[0]) == "settled" && step[0]["entry"]["end"]["basis"] == "usage" && step[0]["entry"]["end"]["charge"] == paid,
        json!(step),
    );
    // (the fragment's own meters bill the owner too, a few micro-dollars at a time)
    s.ok(
        "the owner's balance moved by the charge, nothing is held, and the run shows its cost",
        m(&before, "balanceMicros") - m(&after, "balanceMicros") >= paid && m(&after, "reservedMicros") == 0 && run["costMicros"] == paid && run_charged(api, &owner_id, &name, &run) == paid,
        json!({ "before": before, "after": after, "charge": paid }),
    );
    let call = s.ai.calls().last().cloned();
    s.ok(
        "the gateway's metadata names the payer by an opaque id, never a name",
        call.as_ref().is_some_and(|c| c.metadata["user_id"].as_str().is_some_and(|u| u.len() == 16 && u.bytes().all(|b| b.is_ascii_hexdigit()) && !owner_id.contains(u)) && c.body["reasoning_effort"] == "low"),
        format!("{:?}", call.map(|c| c.metadata)),
    );

    // ---- bug 3: a call that fails for good gives its reservation back
    s.ai.fail_next(&[400]);
    let r = api.op(&owner, &name, "summarize", "t-400", json!({ "text": "refused", "tier": "cheap" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let released = entries(api, &owner_id, &format!("step:{name}@")).into_iter().filter(|e| end_of(e) == "released").count();
    let v = ledger(api, &owner);
    s.ok(
        "a step the model refuses for good is held, and its reservation goes back (bug 3)",
        run["status"] == "held" && released == 1 && m(&v, "reservedMicros") == 0 && run_charged(api, &owner_id, &name, &run) == 0,
        json!({ "run": run, "ledger": v }),
    );
    s.ai.fail_next(&[503, 503, 503, 503, 503]);
    let r = api.op(&owner, &name, "summarize", "t-503", json!({ "text": "flaky", "tier": "cheap" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], Duration::from_secs(90));
    let released = entries(api, &owner_id, &format!("step:{name}@")).into_iter().filter(|e| end_of(e) == "released").count();
    let v = ledger(api, &owner);
    s.ok(
        "a step whose retries run out gives its reservation back too (bug 3)",
        run["status"] == "held" && released == 2 && m(&v, "reservedMicros") == 0 && run_charged(api, &owner_id, &name, &run) == 0,
        json!({ "run": run, "ledger": v }),
    );

    // ---- bug 2: a step tried again after its call was paid never buys again
    lever(api, &name, "fail-after-paid", json!({ "times": 1 }))?;
    let calls = s.ai.calls().len();
    s.ai.set_usage(&[Used { prompt: 100, cached: 0, completion: 10 }]);
    let r = api.op(&owner, &name, "summarize", "t-retried", json!({ "text": "paid once", "tier": "cheap" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let once = charge(&tokens(FLASH, 100, 0, 10));
    s.ok(
        "a text step that failed after its paid call is tried again from what it kept: one call, one charge (bug 2)",
        run["status"] == "succeeded" && run["output"]["text"] == "echo: paid once" && s.ai.calls().len() == calls + 1 && run_charged(api, &owner_id, &name, &run) == once && run_steps(api, &owner_id, &name, &run).len() == 1,
        json!({ "run": run, "calls": s.ai.calls().len() - calls }),
    );
    lever(api, &name, "fail-after-paid", json!({ "times": 1 }))?;
    let images = || s.ai.calls().iter().filter(|c| c.model == IMAGE_MODEL).count();
    let drawn = images();
    let r = api.op(&owner, &name, "draw", "d-1", json!({ "prompt": "a lighthouse", "path": "art/lighthouse.jpg" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    // Workers AI's price: 4.80 neurons a 512×512 tile, 9.60 a step (fragment_core::media)
    let image = |tiles: u64, steps: u64| charge(&Usage::Neurons { milli: tiles * 4_800 + steps * 9_600 });
    s.ok(
        "an image step whose commit failed after it was paid commits from its kept bytes: one image bought (bug 2), charged its 4 tiles and 4 steps in neurons",
        run["status"] == "succeeded"
            && s.fake.file_at(c["repo"].as_str().unwrap_or(""), "main", "art/lighthouse.jpg") == Some(image_bytes("a lighthouse"))
            && images() == drawn + 1
            && run_charged(api, &owner_id, &name, &run) == image(4, 4),
        json!({ "run": run, "images": images() - drawn }),
    );
    let r = api.op(&owner, &name, "draw", "d-wide", json!({ "prompt": "a wide shore", "path": "art/shore.jpg", "steps": 8 }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let step = run_steps(api, &owner_id, &name, &run);
    s.ok(
        "an image is charged the tiles it covers (1536×1024: 6) and the steps it took, past its 1024×1024 reservation, from its usage",
        run["status"] == "succeeded"
            && step.len() == 1
            && step[0]["entry"]["end"]["basis"] == "usage"
            && run_charged(api, &owner_id, &name, &run) == image(6, 8)
            && m(&ledger(api, &owner), "reservedMicros") == 0,
        json!({ "run": run, "step": step }),
    );
    // a replay pays only for the step it had not paid for: the first call
    // answers, the second is refused for good
    let calls = s.ai.calls().len();
    s.ai.pass_next(1);
    s.ai.fail_next(&[400]);
    let r = api.op(&owner, &name, "twice", "w-1", json!({ "a": "first", "b": "second" }))?;
    let held = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": held["id"] })))?;
    let done = settle(api, &owner, &name, held["id"].as_i64().unwrap_or(0), &["succeeded"], wait);
    s.ok(
        "a replayed run reuses the step it paid for: the model is asked for the other alone",
        held["status"] == "held" && done["status"] == "succeeded" && done["output"] == json!({ "one": "echo: first", "two": "echo: second" }) && s.ai.calls().len() - calls == 3,
        json!({ "held": held, "done": done, "calls": s.ai.calls().len() - calls }),
    );

    // ---- zero credit: agents and AI steps stop; fragments keep taking writes
    let v = ledger(api, &owner);
    let n = tokens_costing(GLM, 10, m(&v, "balanceMicros") + USD / 2);
    s.ai.set_usage(&[Used { prompt: 10, cached: 0, completion: n }]);
    let r = api.op(&owner, &name, "summarize", "t-big", json!({ "text": "a long answer", "tier": "medium" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let v = ledger(api, &owner);
    s.ok(
        "usage that happened is charged in full, past zero: the balance is below it",
        run["status"] == "succeeded" && m(&v, "balanceMicros") < 0 && m(&v, "balanceMicros") > -2 * USD && v["standing"] == json!({ "standing": "agents_stopped", "why": "no_credit" }),
        &v,
    );
    let calls = s.ai.calls().len();
    let r = api.op(&owner, &name, "summarize", "t-zero", json!({ "text": "at zero", "tier": "cheap" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "at zero an AI step is refused, saying why",
        run["status"] == "held" && run["error"].as_str().is_some_and(|e| e.contains("agents are stopped: the credit is used up")) && s.ai.calls().len() == calls,
        &run,
    );
    let mine = s.name("ledger-bot");
    agents.signed(&owner, "POST", "/api/agents", Some(&json!({ "name": mine })))?;
    agents.signed(&owner, "POST", &format!("/api/a/{mine}/turns"), Some(&json!({ "text": "hello" })))?;
    let av = settle_agent(s, &agents, &owner, &mine, wait);
    s.ok(
        "and so is an agent's turn",
        av["outcome"] == "error" && av["error"].as_str().is_some_and(|e| e.contains("agents are stopped")) && s.ai.calls().len() == calls,
        json!({ "outcome": av["outcome"], "error": av["error"] }),
    );
    let w = api.op(&owner, &name, "note", "n-zero", json!({ "text": "at zero" }))?;
    let q = api.op(&owner, &name, "notes", "q", json!({}))?;
    s.ok("while its fragments keep taking writes and serving reads", w.status == 200 && q.status == 200, json!([w.status, q.status]));

    // ---- past the overdraft: read-only until a top-up brings it above zero
    // A fragment of the owner's whose cron and file triggers run mutations,
    // made at zero credit (a person makes fragments there, as they write).
    // Its cron is due next new year; the `cron-now` lever makes it due at once.
    let trig = s.named(api, &owner, "ledger-triggers")?;
    let tc = s.create(api, &owner, &trig)?;
    ship(s, &tc, TRIGGERS_APP, TRIGGERS_JSON);
    landed(s, api, &owner, &trig, wait);
    let runs = |op: &str| -> Vec<Value> {
        api.signed(&owner, "GET", &format!("/api/f/{trig}/runs?op={op}"), None).ok().and_then(|r| r.body["runs"].as_array().cloned()).unwrap_or_default()
    };
    let blocked = |op: &str, via: &str| runs(op).into_iter().find(|r| r["via"] == via && r["status"] == "blocked" && r["error"].as_str().is_some_and(|e| e.contains("read-only")));
    let succeeded = |op: &str, via: &str| runs(op).iter().filter(|r| r["via"] == via && r["status"] == "succeeded").count();
    lever(api, &trig, "cron-now", json!({}))?;
    let ran = s.eventually(wait, || succeeded("tick", "cron") == 1);
    s.ok("before, its cron runs (a mutation, as the fragment)", ran, json!(runs("tick")));
    let back = m(&ledger(api, &owner), "balanceMicros").unsigned_abs() as i64 + USD;
    command(&owner_id, "grant", json!({ "id": "g-back", "micros": back, "by": op_id, "why": "back above zero" }))?;
    let v = ledger(api, &owner);
    let n = tokens_costing(GLM, 10, m(&v, "balanceMicros") + 3 * USD);
    s.ai.set_usage(&[Used { prompt: 10, cached: 0, completion: n }]);
    let r = api.op(&owner, &name, "summarize", "t-over", json!({ "text": "past the overdraft", "tier": "medium" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let v = ledger(api, &owner);
    s.ok(
        "a charge past the $2 overdraft makes the person's fragments read-only",
        run["status"] == "succeeded" && m(&v, "balanceMicros") <= -2 * USD && v["standing"] == json!({ "standing": "read_only", "why": "overdrawn" }),
        &v,
    );
    // the fragment trusts what it heard of its owner's standing for up to a minute
    lever(api, &name, "forget-standing", json!({}))?;
    let w = api.op(&owner, &name, "note", "n-over", json!({ "text": "past the overdraft" }))?;
    let post = api.signed(&owner, "POST", &format!("/api/f/{name}/channels/talk"), Some(&json!({ "id": "p-over", "body": { "text": "hi" } })))?;
    let files = api.signed(&owner, "POST", &format!("/api/f/{name}/files"), Some(&json!({ "files": [{ "path": "a.txt", "text": "a" }] })))?;
    let deploy = api.signed(&owner, "POST", &format!("/api/f/{name}/deploy"), Some(&json!({})))?;
    let refused = [&w, &post, &files, &deploy].iter().all(|r| r.status == 402 && r.error() == "budget_used_up" && r.message().contains("read-only"));
    s.ok("then its mutations, posts, file writes and deploys are refused, 402, saying why", refused, format!("{w} | {post} | {files} | {deploy}"));
    let q = api.op(&owner, &name, "notes", "q", json!({}))?;
    let st = api.status(&owner, &name)?;
    s.ok(
        "while its reads still serve",
        q.status == 200 && q.body["result"]["notes"].as_array().is_some_and(|n| n.iter().any(|t| t == "at zero")) && st.status == 200,
        format!("{q} | {st}"),
    );
    let over = api.create(&owner, &s.named(api, &owner, "ledger-over")?)?;
    s.ok("nor do they make a new fragment, 402, saying why", over.status == 402 && over.error() == "budget_used_up" && over.message().contains("read-only"), &over);

    // its cron and its triggers start no runs: each is a blocked run, saying why (Paul, 2026-10-03)
    lever(api, &trig, "forget-standing", json!({}))?;
    lever(api, &trig, "cron-now", json!({}))?;
    let cron = s.eventually(wait, || blocked("tick", "cron").is_some());
    s.ok("past the overdraft its cron starts no run: the tick is a blocked run, saying why", cron && succeeded("tick", "cron") == 1, json!(runs("tick")));
    s.commit(&tc, &[("notes/past.md", Some(b"past the overdraft"))]);
    let filed = s.eventually(wait, || blocked("filed", "files").is_some());
    s.ok("nor does a file trigger, for a commit to main from outside", filed && succeeded("filed", "files") == 0, json!(runs("filed")));
    let held = blocked("tick", "cron").map(|r| r["id"].clone()).unwrap_or_default();
    let r = api.signed(&owner, "POST", &format!("/api/f/{trig}/replay"), Some(&json!({ "run": held })))?;
    s.ok("and a replay of the blocked run is refused, 402, saying why", r.status == 402 && r.error() == "budget_used_up" && r.message().contains("read-only"), &r);

    let top = m(&ledger(api, &owner), "balanceMicros").unsigned_abs() as i64 + USD;
    command(&owner_id, "grant", json!({ "id": "g-top", "micros": top, "by": op_id, "why": "a top-up" }))?;
    lever(api, &name, "forget-standing", json!({}))?;
    let w = api.op(&owner, &name, "note", "n-top", json!({ "text": "after the top-up" }))?;
    let v = ledger(api, &owner);
    s.ok("a top-up above zero restores writes", w.status == 200 && v["standing"]["standing"] == "ok", format!("{w} | {v}"));
    lever(api, &trig, "forget-standing", json!({}))?;
    lever(api, &trig, "cron-now", json!({}))?;
    let cron = s.eventually(wait, || succeeded("tick", "cron") == 2);
    s.commit(&tc, &[("notes/after.md", Some(b"after the top-up"))]);
    let filed = s.eventually(wait, || succeeded("filed", "files") == 1);
    s.ok("and its cron and file triggers start runs again", cron && filed, json!({ "tick": runs("tick"), "filed": runs("filed") }));
    let r = api.signed(&owner, "POST", &format!("/api/f/{trig}/replay"), Some(&json!({ "run": held })))?;
    let replayed = s.eventually(wait, || {
        api.signed(&owner, "GET", &format!("/api/f/{trig}/runs/{held}"), None).is_ok_and(|r| r.body["status"] == "succeeded" && r.body["attempt"] == 2)
    });
    s.ok("and the run blocked past the overdraft replays", r.status == 200 && replayed, &r);

    // ---- a fragment's cap stops everyone but its owner (decision 26)
    let visitor = api.person()?;
    let visitor_id = api.identity(&visitor)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{visitor_id}"), Some(&json!({ "role": "viewer" })))?;
    let r = api.signed(&visitor, "PUT", &format!("/api/f/{name}/cap"), Some(&json!({ "id": "c0", "micros": 1 })))?;
    s.ok("only a fragment's owner sets its cap", r.status == 403, &r);
    let cap = json!({ "id": "c1", "micros": 1_000 });
    let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/cap"), Some(&cap))?;
    let again = api.signed(&owner, "PUT", &format!("/api/f/{name}/cap"), Some(&cap))?;
    let conflict = api.signed(&owner, "PUT", &format!("/api/f/{name}/cap"), Some(&json!({ "id": "c1", "micros": 2_000 })))?;
    let bad = api.signed(&owner, "PUT", &format!("/api/f/{name}/cap"), Some(&json!({ "id": "c2", "micros": -1 })))?;
    s.ok(
        "the owner sets it ($0.001 here), once by its id; another body is 409, a negative cap 400",
        r.status == 200 && r.body["capMicros"] == 1_000 && again.status == 200 && conflict.status == 409 && bad.status == 400,
        format!("{r} | {again} | {conflict} | {bad}"),
    );
    let v = ledger(api, &owner);
    let spent = v["fragments"].as_array().and_then(|f| f.iter().find(|f| f["fragment"] == name.as_str()).cloned()).unwrap_or_default();
    s.ok("the owner's ledger shows the fragment's month against its cap", spent["capMicros"] == 1_000 && m(&spent, "spentMicros") > 1_000, &spent);
    let calls = s.ai.calls().len();
    let r = api.op(&visitor, &name, "ask", "v-1", json!({ "text": "from a visitor" }))?;
    let theirs = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "past its cap, a visitor's AI step is refused, saying why",
        theirs["status"] == "held" && theirs["error"].as_str().is_some_and(|e| e.contains("cap this month")) && s.ai.calls().len() == calls,
        &theirs,
    );
    let r = api.op(&owner, &name, "ask", "o-1", json!({ "text": "from its owner" }))?;
    let ours = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok("while its owner's goes on", ours["status"] == "succeeded" && ours["output"]["text"] == "echo: from its owner", &ours);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/cap"), Some(&json!({ "id": "c3", "micros": null })))?;
    s.ok("a cap goes back to the default ($5)", r.status == 200 && r.body["capMicros"] == 5 * USD && r.body["default"] == true, &r);

    // ---- the model route, called as an agent: metered from the final usage
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &owner) })))?;
    anyhow::ensure!(r.status == 200, "an agent of the owner's: {r}");
    let route = "/api/models/v1/chat/completions";
    let chat = |stream: bool| json!({ "model": "cheap", "stream": stream, "messages": [{ "role": "user", "content": "hello" }] });
    let aig = || entries(api, &owner_id, "aig:");
    let before = aig().len();
    s.ai.set_usage(&[Used { prompt: 50, cached: 0, completion: 20 }]);
    let r = api.signed(&hand, "POST", route, Some(&chat(false)))?;
    let settled = aig().into_iter().filter(|e| end_of(e) == "settled").count();
    s.ok(
        "an unstreamed call answers OpenAI's shape, and is settled from its usage",
        r.status == 200 && r.body["choices"][0]["message"]["content"] == "echo: hello" && aig().len() == before + 1 && settled == before + 1
            && aig().iter().any(|e| e["entry"]["end"]["charge"] == charge(&tokens(FLASH, 50, 0, 20))),
        json!({ "answer": r.body, "entries": aig() }),
    );
    s.ai.set_usage(&[Used { prompt: 40, cached: 10, completion: 30 }]);
    let r = api.signed(&hand, "POST", route, Some(&chat(true)))?;
    let lines: Vec<Value> = r.text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").filter_map(|d| serde_json::from_str(d).ok()).collect();
    let last = lines.last().cloned().unwrap_or_default();
    let shaped = lines.iter().filter(|l| l["choices"].as_array().is_some_and(|c| !c.is_empty())).all(|l| l.get("usage").is_none())
        && last["choices"] == json!([]) && last["usage"]["completion_tokens"] == 30;
    let streamed = charge(&tokens(FLASH, 30, 10, 30));
    let landed = s.eventually(wait, || aig().iter().any(|e| e["entry"]["end"]["charge"] == streamed && e["entry"]["end"]["basis"] == "usage"));
    s.ok("a streamed call reaches the client in OpenAI's shape (usage once, last)", r.status == 200 && shaped, &r.text);
    s.ok("and is settled from its last, cumulative usage only", landed, json!(aig()));
    s.ai.break_next();
    let r = api.signed(&hand, "POST", route, Some(&chat(true)))?;
    let worst = charge(&Usage::Tokens { model: FLASH.into(), input: chat(true).to_string().len() as u64 + 60, cached_input: 0, cache_write: 0, output: 16_384 });
    let broke = s.eventually(wait, || aig().iter().filter(|e| e["entry"]["end"]["basis"] == "reservation").count() == 1);
    s.ok("a stream that broke before its usage is settled at its worst case", r.status == 200 && broke, format!("about {worst} µ$: {}", json!(aig())));
    let r = api.signed(&hand, "POST", route, Some(&json!({ "model": "high", "messages": [{ "role": "user", "content": "hi" }] })))?;
    s.ok("the high tier is refused, saying why (decision 23)", r.status == 400 && r.message().contains("high tier is off"), &r);
    let r = api.signed(&hand, "POST", route, Some(&json!({ "model": "@cf/zai-org/glm-5.3", "messages": [{ "role": "user", "content": "hi" }] })))?;
    s.ok("a model id is never a tier", r.status == 400, &r);
    let r = api.signed(&owner, "POST", route, Some(&chat(false)))?;
    s.ok("a person does not call the model route (an agent does, for whom its owner pays)", r.status == 403, &r);
    let elsewhere = s.named(api, &visitor, "ledger-elsewhere")?;
    s.create(api, &visitor, &elsewhere)?;
    let r = api.signed(&hand, "POST", &format!("{route}?fragment={}", url_enc(&elsewhere)), Some(&chat(false)))?;
    s.ok("a call names only a fragment its agent is in", r.status == 403, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", fragment_core::npub::encode(hand.pubkey_hex())), Some(&json!({ "role": "editor" })))?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/cap"), Some(&json!({ "id": "c4", "micros": 1_000 })))?;
    let r = api.signed(&hand, "POST", &format!("{route}?fragment={}&for={}", url_enc(&name), url_enc(&visitor_id)), Some(&chat(false)))?;
    s.ok("an agent turn for someone else in a fragment past its cap is refused, 402", r.status == 402 && r.message().contains("cap this month"), &r);
    let r = api.signed(&hand, "POST", &format!("{route}?fragment={}&for={}", url_enc(&name), url_enc(&owner_id)), Some(&chat(false)))?;
    s.ok("and one for its owner goes on", r.status == 200, &r);

    // ---- the meters: requests, dynamic workers and storage, once each
    s.ai.clear_script();
    meters(s, api, wait)
}

/// A fragment's meters reach its owner's ledger through the queue, each
/// row once by its reference, a resent batch answered as before. Its owner
/// is a person of its own, so nothing else bills their ledger meanwhile.
fn meters(s: &mut Suite, api: &Api, wait: Duration) -> Result<()> {
    let owner = api.person()?;
    let owner_id = api.identity(&owner)?;
    let name = s.named(api, &owner, "ledger-meters")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, LEDGER_APP, LEDGER_JSON);
    landed(s, api, &owner, &name, wait);
    for _ in 0..5 {
        api.op(&owner, &name, "notes", "q", json!({}))?;
    }
    let sent = lever(api, &name, "meter-now", json!({}))?.body;
    let key = sent["key"].as_str().unwrap_or("").to_string();
    // every row the outbox held (one batch at a time: those after the first follow its acknowledgement)
    let carried: Vec<(String, Value)> = sent["rows"].as_array().into_iter().flatten().map(|r| (r["ref"].as_str().unwrap_or("").to_string(), r["usage"].clone())).collect();
    let acked = s.eventually(wait, || lever(api, &name, "meter", json!({})).is_ok_and(|r| r.body["batches"] == json!([]) && r.body["rows"] == json!([])));
    let rows = |prefix: &str| entries(api, &owner_id, &format!("{prefix}:{key}:"));
    let landed = |prefix: &str| -> bool {
        let ledger = rows(prefix);
        let sent: Vec<&(String, Value)> = carried.iter().filter(|(r, _)| r.starts_with(&format!("{prefix}:"))).collect();
        !sent.is_empty() && sent.iter().all(|(r, usage)| ledger.iter().filter(|e| e["ref"] == r.as_str()).map(|e| &e["entry"]["row"]["usage"]).collect::<Vec<_>>() == [usage])
    };
    let counted: i64 = carried.iter().filter(|(r, _)| r.starts_with("req:")).filter_map(|(_, u)| u["count"].as_i64()).sum();
    s.ok(
        "a fragment's requests reach its owner's ledger, counted per minute, each row once as it was sent",
        acked && counted >= 6 && landed("req") && rows("req").iter().all(|e| e["entry"]["row"]["fragment"] == name.as_str()),
        json!({ "sent": sent, "ledger": rows("req") }),
    );
    s.ok("its code version is one dynamic worker today, once however often it ran", rows("dw").len() == 1 && rows("dw")[0]["entry"]["charge"] == 3_000, json!(rows("dw")));
    s.ok(
        "a storage sample bills its SQLite as byte-hours",
        rows("store").iter().any(|e| e["entry"]["row"]["usage"]["class"] == "sqlite" && e["entry"]["row"]["usage"]["byte_hours"].as_u64().is_some_and(|b| b > 0)),
        json!(rows("store")),
    );
    // a batch applied whose acknowledgement is lost is sent again, and answered as before
    let before = totals(api, &owner_id);
    let had: Vec<Value> = rows("req");
    lever(api, &name, "fail-meter-acks", json!({ "times": 2 }))?;
    for _ in 0..3 {
        api.op(&owner, &name, "notes", "q", json!({}))?;
    }
    lever(api, &name, "meter-now", json!({ "sample": false }))?;
    lever(api, &name, "meter-now", json!({ "sample": false, "resend": true }))?;
    let acked = s.eventually(Duration::from_secs(60), || lever(api, &name, "meter", json!({})).is_ok_and(|r| r.body["batches"] == json!([]) && r.body["rows"] == json!([])));
    let after = totals(api, &owner_id);

    let new: Vec<Value> = rows("req").into_iter().filter(|e| !had.contains(e)).collect();
    let charged: i64 = new.iter().filter_map(|e| e["entry"]["charge"].as_i64()).sum();
    s.ok(
        "a batch sent again after its acknowledgement was lost charges its rows once",
        acked && new.len() == 1 && new[0]["entry"]["row"]["usage"]["count"].as_i64() >= Some(3) && m(&after, "charged") - m(&before, "charged") == charged,
        json!({ "before": before, "after": after, "new": new }),
    );
    let _ = c;
    Ok(())
}
