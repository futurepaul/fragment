//! A job's text step with tools and drafts (docs/api.md, AI), in the `ai`
//! section, on the Workers AI fake: a tool loop of two calls with no
//! computer; a 429 on the model answered by the fallback, sent the same
//! tools; tools and drafts refused before any call; a draft a page sees
//! live (thinking, then its text growing, at most 4 frames a second, then
//! the record that replaces it); a step retried after its paid call,
//! charged once; and a stream the gateway holds open past its `[DONE]`:
//! a step reads it to there and drops it, and the model route settles its
//! call there, while the stream is still open.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_core::models::{AI_TEXT_MAX_BYTES, CHEAP_MODEL, FALLBACK_MODEL_DEFAULT, TOOLS_MAX};
use fragment_core::price::{PriceBook, Usage};
use fragment_fakes::workers_ai::{Pace, Reply, Used};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use super::ledger::entries;
use crate::api::{Api, Socket};
use crate::Suite;

const TOOLS_APP: &[u8] = include_bytes!("../../fixtures/tools.mjs");
const TOOLS_JSON: &[u8] = include_bytes!("../../fixtures/tools.json");

/// The fixture's tool, as the model is sent it.
pub(super) fn lookup() -> Value {
    json!({ "type": "function", "function": { "name": "lookup", "description": "Looks a word up in the dictionary.", "parameters": { "type": "object", "properties": { "word": { "type": "string" } }, "required": ["word"] } } })
}

/// The model asks for the fixture's tool on `word`.
pub(super) fn asks(word: &str) -> Reply {
    Reply::Tools(vec![("lookup".into(), json!({ "word": word }))])
}

fn charge(model: &str, u: Used) -> i64 {
    let usage = Usage::Tokens { model: model.into(), input: u.prompt - u.cached, cached_input: u.cached, cache_write: 0, output: u.completion };
    PriceBook::defaults().price(&usage).expect("the book prices the tiers' models").charge
}

/// A fragment running the fixture (crates/e2e/fixtures/tools.mjs).
pub(super) fn tools_fragment(s: &mut Suite, api: &Api, owner: &Keys, base: &str) -> Result<String> {
    let name = s.named(api, owner, base)?;
    let c = s.create(api, owner, &name)?;
    ship(s, &c, TOOLS_APP, TOOLS_JSON);
    Ok(name)
}

/// The fixture's tool loop on `question`, as `owner`, settled.
pub(super) fn look_up(api: &Api, owner: &Keys, name: &str, id: &str, input: Value, wait: Duration) -> Result<Value> {
    let r = api.op(owner, name, "look_up", id, input)?;
    Ok(settle(api, owner, name, started(&r), &["succeeded", "held"], wait))
}

pub(super) fn text_tools(s: &mut Suite, api: &Api, wait: Duration) -> Result<()> {
    let owner = api.person()?;
    let name = tools_fragment(s, api, &owner, "tools")?;
    let run = |id: &str, op: &str, input: Value| -> Result<Value> {
        let r = api.op(&owner, &name, op, id, input)?;
        Ok(settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait))
    };
    let cost = |r: &Value| r["costMicros"].as_i64().unwrap_or(0);

    // ---- valid: a tool loop of two calls, with no computer
    s.ai.reply_next(&[asks("shard"), Reply::Text("A shard is a small piece of something larger.".into())]);
    let calls = s.ai.calls().len();
    let r = run("loop", "look_up", json!({ "question": "What is a shard?" }))?;
    let made = s.ai.calls()[calls..].to_vec();
    let first = r["output"]["answers"][0].clone();
    let asked = first["message"]["tool_calls"][0].clone();
    s.ok(
        "a job's text step offers the model its tools, streamed, and answers the tool call it asked for as a message",
        r["status"] == "succeeded"
            && made.len() == 2
            && made[0].body["tools"] == json!([lookup()])
            && made[0].body["stream"] == true
            && first["finish_reason"] == "tool_calls"
            && first["text"] == ""
            && asked["type"] == "function"
            && asked["function"]["name"] == "lookup"
            && asked["function"]["arguments"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()) == Some(json!({ "word": "shard" })),
        json!({ "run": r, "calls": made.len() }),
    );
    let sent = made.get(1).map(|c| c.body["messages"].clone()).unwrap_or_default();
    s.ok(
        "the job runs the tool and calls again: the model reads the assistant's tool call and the tool's result",
        sent[1] == first["message"]
            && sent[2] == json!({ "role": "tool", "tool_call_id": asked["id"], "content": "a small piece of something larger" })
            && r["output"]["answers"][1]["finish_reason"] == "stop"
            && r["output"]["text"] == "A shard is a small piece of something larger.",
        json!({ "sent": sent, "answers": r["output"]["answers"] }),
    );
    let timing = first["timing"].clone();
    let (first_ms, ms) = (timing["first_ms"].as_i64().unwrap_or(-1), timing["ms"].as_i64().unwrap_or(-1));
    s.ok(
        "each answer says its timing: when the model began, how long it took, the model, its calls and its tries",
        timing["model"] == CHEAP_MODEL && timing["calls"] == 1 && timing["tries"] == 1 && first_ms >= 0 && ms >= first_ms,
        &timing,
    );
    s.ok("and the run is charged its two calls", cost(&r) > 0 && r["output"]["answers"].as_array().is_some_and(|a| a.iter().all(|a| a["usage"]["completion_tokens"].as_u64().is_some())), &r);

    // ---- the fallback: a 429 on the model; the fallback is sent the same tools, and asks for the tool
    s.ai.fail_next(&[429]);
    s.ai.reply_next(&[asks("fragment"), Reply::Text("A part broken off.".into())]);
    let calls = s.ai.calls().len();
    let r = run("fell", "look_up", json!({ "question": "What is a fragment?" }))?;
    let made = s.ai.calls()[calls..].to_vec();
    let models: Vec<&str> = made.iter().map(|c| c.model.as_str()).collect();
    let first = &r["output"]["answers"][0];
    s.ok(
        "a 429 on the model falls back: the fallback model is sent the same request, its tools too, and its tool call is the answer",
        r["status"] == "succeeded"
            && models == [CHEAP_MODEL, FALLBACK_MODEL_DEFAULT, CHEAP_MODEL]
            && made[0].body == made[1].body
            && made[1].body["tools"] == json!([lookup()])
            && first["model"] == FALLBACK_MODEL_DEFAULT
            && first["finish_reason"] == "tool_calls"
            && first["timing"]["calls"] == 2
            && first["timing"]["model"] == FALLBACK_MODEL_DEFAULT
            && r["output"]["answers"][1]["model"] == CHEAP_MODEL
            && r["output"]["text"] == "A part broken off.",
        json!({ "run": r, "models": models }),
    );

    // ---- invalid: refused, saying why, before anything is reserved or called
    let calls = s.ai.calls().len();
    let bad_schema = json!({ "type": "function", "function": { "name": "lookup", "parameters": { "type": "string" } } });
    let schema = run("bad-schema", "tooled", json!({ "prompt": "p", "tools": [bad_schema] }))?;
    let many: Vec<Value> = (0..=TOOLS_MAX).map(|i| json!({ "type": "function", "function": { "name": format!("t{i}") } })).collect();
    let too_many = run("too-many", "tooled", json!({ "prompt": "p", "tools": many }))?;
    let unknown = run("bad-key", "tooled", json!({ "prompt": "p", "tools": [{ "type": "function", "function": { "name": "f", "code": "rm -rf /" } }] }))?;
    let choice = run("bad-choice", "tooled", json!({ "prompt": "p", "tools": [lookup()], "tool_choice": { "type": "function", "function": { "name": "other" } } }))?;
    let undeclared = run("bad-draft", "tooled", json!({ "prompt": "p", "draft": { "channel": "nowhere", "turn": "t1" } }))?;
    let huge = run("too-big", "tooled", json!({ "prompt": "x", "repeat": AI_TEXT_MAX_BYTES }))?;
    let refused = |r: &Value, says: &str| r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains(says)) && cost(r) == 0;
    s.ok(
        "a tool whose parameters is no object schema, more than 64 tools, a key a tool does not take, a choice of none of them, a draft to a channel the app does not declare, and arguments past 512 KiB are refused, saying why, before any call",
        refused(&schema, "JSON Schema of an object")
            && refused(&too_many, "at most 64 tools")
            && refused(&unknown, "and no other key")
            && refused(&choice, "a function among the tools")
            && refused(&undeclared, "declares no \"nowhere\"")
            && refused(&huge, "arguments are at most 524288 bytes")
            && s.ai.calls().len() == calls,
        json!([schema["error"], too_many["error"], unknown["error"], choice["error"], undeclared["error"], huge["error"]]),
    );

    // ---- a streamed draft, seen live by a page
    let mut page = Socket::open(api, &name, "__live", Some(&owner), None)?;
    page.until("hello", 5)?;
    page.send(&json!({ "type": "subscribe", "channel": "answers", "after": 0 }))?;
    page.until("subscribed", 20)?;
    page.patience(Duration::from_secs(20))?;
    let said = "A shard is a small piece of something larger, broken off.";
    s.ai.pace_next(&[Pace { every_ms: 300, thinking: 4, hold_ms: 0 }]);
    s.ai.say_next(&[said]);
    let turn = "turn:live-1";
    let r = api.op(&owner, &name, "look_up", "live", json!({ "question": "What is a shard?", "draft": { "channel": "answers", "turn": turn } }))?;
    let (mut drafts, mut record) = (Vec::new(), Value::Null);
    // bounded: the drafts of one paced answer, then its record
    for _ in 0..100 {
        let Ok(f) = page.next() else { break };
        if f["type"] == "draft" && f["turn"] == turn {
            drafts.push(f);
        } else if f["type"] == "record" && f["body"]["turn"] == turn {
            record = f;
            break;
        }
    }
    page.close();
    let ran = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let thinking: Vec<i64> = drafts.iter().filter_map(|d| d["thinking"].as_i64()).collect();
    let texts: Vec<&str> = drafts.iter().filter(|d| d.get("thinking").is_none()).filter_map(|d| d["text"].as_str()).collect();
    let ats: Vec<i64> = drafts.iter().filter_map(|d| d["at"].as_i64()).collect();
    s.ok(
        "a page sees the step's draft live: thinking first (Thinking… 1s), as the run's principal",
        thinking.first() == Some(&0)
            && thinking.iter().any(|t| *t >= 1000)
            && drafts.iter().filter(|d| d.get("thinking").is_some()).all(|d| d["text"] == "")
            && drafts.iter().all(|d| d["principal"] == record["principal"] && d["channel"] == "answers"),
        json!(drafts),
    );
    s.ok(
        "then its text as it streams, each draft the whole text so far, growing, the last the whole answer",
        texts.len() >= 3 && texts.windows(2).all(|w| w[1].len() > w[0].len() && w[1].starts_with(w[0])) && texts.last() == Some(&said),
        json!(texts),
    );
    s.ok("at most 4 frames a second", ats.len() == drafts.len() && ats.windows(2).all(|w| w[1] - w[0] >= 250), json!(ats));
    let timing = &ran["output"]["answers"][0]["timing"];
    s.ok(
        "then the record the job posts under the draft's turn, which replaces it; the step's answer is the whole text, its first data long before its end",
        record["body"]["text"] == said
            && ran["status"] == "succeeded"
            && ran["output"]["text"] == said
            && matches!((timing["first_ms"].as_i64(), timing["ms"].as_i64()), (Some(first), Some(ms)) if first < 1000 && ms >= 2000),
        json!({ "record": record, "run": ran }),
    );

    // ---- a step retried after its paid call answers what it kept: one call, one charge
    api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "fail-after-paid", "times": 1 })))?;
    let (u1, u2) = (Used { prompt: 100, cached: 0, completion: 10 }, Used { prompt: 200, cached: 0, completion: 5 });
    s.ai.reply_next(&[asks("shard"), Reply::Text("Once.".into())]);
    s.ai.set_usage(&[u1, u2]);
    let calls = s.ai.calls().len();
    let r = run("paid-once", "look_up", json!({ "question": "What is a shard?" }))?;
    s.ok(
        "a tool step that failed after its paid call is tried again from what it kept: two calls in the loop, each charged once",
        r["status"] == "succeeded" && r["output"]["text"] == "Once." && s.ai.calls().len() == calls + 2 && cost(&r) == charge(CHEAP_MODEL, u1) + charge(CHEAP_MODEL, u2),
        json!({ "run": r, "calls": s.ai.calls().len() - calls, "expected": charge(CHEAP_MODEL, u1) + charge(CHEAP_MODEL, u2) }),
    );

    // ---- a stream held open past its [DONE]: read to there, and dropped
    let hold = Duration::from_secs(30);
    let hung = s.ai.hung_up();
    s.ai.pace_next(&[Pace { every_ms: 0, thinking: 0, hold_ms: hold.as_millis() as u64 }]);
    let t0 = Instant::now();
    let r = run("held", "tooled", json!({ "prompt": "quick" }))?;
    let took = t0.elapsed();
    let dropped = s.eventually(Duration::from_secs(5), || s.ai.hung_up() > hung);
    s.ok(
        "a step whose stream the gateway holds open after its [DONE] answers at its [DONE], and drops the connection",
        r["status"] == "succeeded" && r["output"]["text"] == "echo: quick" && took < hold / 2 && dropped,
        format!("{took:?}, dropped: {dropped}: {r}"),
    );
    route_held_open(s, api, &owner)
}

/// The model route (models.rs `complete`) on a stream held open past its
/// `[DONE]`: the call is settled from its usage at its `[DONE]`, while the
/// stream is still open, not when (or if) it ends (the optchat survey's
/// late settle); the client reads it as it came.
fn route_held_open(s: &mut Suite, api: &Api, owner: &Keys) -> Result<()> {
    let hold = Duration::from_secs(12);
    let owner_id = api.identity(owner)?;
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, owner) })))?;
    anyhow::ensure!(r.status == 200, "an agent of the owner's: {r}");
    let aig = || entries(api, &owner_id, "aig:");
    let before = aig().len();
    let used = Used { prompt: 40, cached: 0, completion: 12 };
    s.ai.set_usage(&[used]);
    s.ai.pace_next(&[Pace { every_ms: 0, thinking: 0, hold_ms: hold.as_millis() as u64 }]);
    let body = json!({ "model": "cheap", "stream": true, "messages": [{ "role": "user", "content": "held" }] });
    let t0 = Instant::now();
    let (answer, settled, open_at_settle) = std::thread::scope(|scope| {
        let call = scope.spawn(|| api.signed(&hand, "POST", "/api/models/v1/chat/completions", Some(&body)).map(|r| (r, t0.elapsed())));
        let settled = s.eventually(hold / 2, || aig().iter().any(|e| e["entry"]["end"]["charge"] == charge(CHEAP_MODEL, used) && e["entry"]["end"]["basis"] == "usage"));
        let open = !call.is_finished();
        (call.join(), settled, open)
    });
    let (r, answered) = answer.map_err(|_| anyhow::anyhow!("the route call's thread panicked"))??;
    s.ok(
        "the model route settles a call from its usage at its [DONE], while the gateway still holds the stream open",
        settled && open_at_settle && r.status == 200 && aig().len() == before + 1,
        format!("settled: {settled}, the stream still open then: {open_at_settle}; the client's answer after {answered:?}: {}", json!(aig())),
    );
    // a stream whose events come apart in time reaches the client whole
    s.ai.pace_next(&[Pace { every_ms: 50, thinking: 2, hold_ms: 0 }]);
    let r = api.signed(&hand, "POST", "/api/models/v1/chat/completions", Some(&body))?;
    let said: String = r
        .text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|v| v["choices"][0]["delta"]["content"].as_str().map(str::to_string))
        .collect();
    s.ok(
        "a paced stream reaches the client whole, in OpenAI's shape, through to its [DONE]",
        r.status == 200 && said == "echo: held" && r.text.trim_end().ends_with("data: [DONE]"),
        &r.text,
    );
    Ok(())
}
