//! Finite Private (docs/cloudflare-v1.md, decision 61; docs/computers.md,
//! Models): the model route on a node shaped as Finite's own deployment
//! (deploy/example.jsonc), its tiers on Finite Private and Workers AI's
//! GLM-5.3 Flash behind them and for images. The vendors are the model
//! fake's two faces (a lower rung at the vendor boundary): its
//! OpenAI-compatible one answers as finite-mono's `finite-private-limiter`
//! does, letting in only the key the node binds for the tiers.
//!
//! What it proves: an agent's call on either tier reaches Finite Private
//! with the platform's key and nothing of the agent's, unstreamed, streamed
//! and calling a tool, and is settled on its payer's ledger at Finite
//! Private's price; one Finite Private fails before answering (its spent
//! window's 429, a 5xx, its model down) is made once more on Workers AI and
//! settled as Workers AI's; a refusal of the request, and a key it refuses,
//! are the call's; the route's `fallback` (Hermes' own switch) and `vision`
//! run on Workers AI; a call that hangs is the caller's to give up on (the
//! route never tries again on its own); and a job's `ai.text` runs there
//! too, falling back the same.

use std::time::Duration;

use anyhow::Result;
use fragment_core::price::{PriceBook, Usage};
use fragment_fakes::workers_ai::{Face, Used};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use super::ledger::{end_of, entries};
use crate::api::Api;
use crate::{Suite, FINITE_PRIVATE_KEY};

const LEDGER_APP: &[u8] = include_bytes!("../../fixtures/ledger.mjs");
const LEDGER_JSON: &[u8] = include_bytes!("../../fixtures/ledger.json");
/// The model route, as an agent calls it (its computer's model intercept).
const ROUTE: &str = "/api/models/v1/chat/completions";
/// Finite Private's GLM-5.3 Flash, the tiers' here.
const PRIVATE: &str = fragment_core::models::FINITE_PRIVATE_FLASH;
/// Workers AI's GLM-5.3 Flash: the fallback and the vision model here.
const FLASH: &str = fragment_core::models::GLM_FLASH;
/// How long a call the fake holds back hangs: past the impatient caller's
/// wait, as a stalled stream is past Hermes' 20 s.
const HANG: Duration = Duration::from_secs(8);
const PATIENCE: Duration = Duration::from_secs(3);

fn tokens(model: &str, input: u64, output: u64) -> Usage {
    Usage::Tokens { model: model.into(), input, cached_input: 0, cache_write: 0, output }
}

fn charge(usage: &Usage) -> i64 {
    PriceBook::defaults().price(usage).expect("the book prices both models").charge
}

fn chat(model: &str, stream: bool) -> Value {
    json!({ "model": model, "stream": stream, "messages": [{ "role": "user", "content": "hello" }] })
}

/// A streamed answer's data lines, `[DONE]` left out.
fn lines(text: &str) -> Vec<Value> {
    text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").filter_map(|d| serde_json::from_str(d).ok()).collect()
}

pub fn finite_private(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("finite-private", &[crate::Need::Fakes, crate::Need::Node, crate::Need::Levers]) {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_finite()?;
    s.ai.accept_key(FINITE_PRIVATE_KEY);
    s.ai.clear_script();
    let checked = checks(s, &api);
    // the node as the lanes after it expect it: every tier on Workers AI
    s.ai.clear_script();
    s.ai.down(&[]);
    s.ai.transcripts(false);
    s.stop()?;
    s.start(false)?;
    checked
}

fn checks(s: &mut Suite, api: &Api) -> Result<()> {
    let wait = Duration::from_secs(40);
    let owner = api.person()?;
    let owner_id = api.identity(&owner)?;
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &owner) })))?;
    anyhow::ensure!(r.status == 200, "an agent of the owner's: {r}");
    let aig = || entries(api, &owner_id, "aig:");
    // each call since `from`: the face it reached, and its model
    let tried = |s: &Suite, from: usize| s.ai.calls()[from..].iter().map(|c| (c.face, c.model.clone())).collect::<Vec<_>>();
    let private = |n: usize| vec![(Face::OpenAi, PRIVATE.to_string()); n];
    let fell = || vec![(Face::OpenAi, PRIVATE.to_string()), (Face::WorkersAi, FLASH.to_string())];
    let platform_key = format!("Bearer {FINITE_PRIVATE_KEY}");
    // the one new entry since `before`, settled at `charge` from its usage, on `model`
    let settled_as = |before: usize, model: &str, micros: i64| {
        let now = aig();
        now.len() == before + 1
            && now.iter().any(|e| end_of(e) == "settled" && e["entry"]["end"]["basis"] == "usage" && e["entry"]["end"]["charge"] == micros && e["entry"]["end"]["usage"]["model"] == model)
    };

    // ---- each tier on Finite Private, with the platform's key alone
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.set_usage(&[Used { prompt: 50, cached: 0, completion: 20 }]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("cheap", false)))?;
    let call = s.ai.calls().get(calls).cloned();
    let priced = charge(&tokens(PRIVATE, 50, 20));
    let settled = s.eventually(wait, || settled_as(before, PRIVATE, priced));
    s.ok(
        "a cheap call runs on Finite Private (its model by its id there), answered in OpenAI's shape, and is settled on its payer's ledger at its price",
        r.status == 200 && r.body["choices"][0]["message"]["content"] == "echo: hello" && tried(s, calls) == private(1) && call.as_ref().is_some_and(|c| c.body["model"] == PRIVATE) && settled,
        json!({ "status": r.status, "answer": r.body, "tried": format!("{:?}", tried(s, calls)), "entries": aig().len() - before }),
    );
    s.ok(
        "it carries the platform's Finite Private key and nothing of the agent's: no signature, no gateway metadata",
        call.as_ref().is_some_and(|c| c.authorization.as_deref() == Some(platform_key.as_str()) && c.metadata.is_null() && c.affinity.is_none() && c.body["reasoning_effort"] == "low" && c.body["max_tokens"] == 16_384),
        format!("{:?}", call.map(|c| (c.metadata, c.affinity, c.body["reasoning_effort"].clone(), c.authorization.map(|a| a.len())))),
    );
    let calls = s.ai.calls().len();
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("medium", false)))?;
    s.ok("and the medium tier too", r.status == 200 && tried(s, calls) == private(1), format!("{} {:?}", r.status, tried(s, calls)));

    // streamed, calling a tool
    s.ai.transcripts(true);
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.set_usage(&[Used { prompt: 40, cached: 0, completion: 12 }]);
    let tools = json!([{ "type": "function", "function": { "name": "terminal", "parameters": { "type": "object" } } }]);
    let run = json!({ "model": "cheap", "stream": true, "tools": tools, "messages": [{ "role": "user", "content": "[paul] run: echo private" }] });
    let r = api.signed(&hand, "POST", ROUTE, Some(&run))?;
    s.ai.transcripts(false);
    let streamed = lines(&r.text);
    let called = streamed.iter().find_map(|l| l["choices"][0]["delta"]["tool_calls"][0]["function"]["name"].as_str().map(str::to_string));
    let last = streamed.last().cloned().unwrap_or_default();
    let shaped = streamed.iter().filter(|l| l["choices"].as_array().is_some_and(|c| !c.is_empty())).all(|l| l.get("usage").is_none()) && last["choices"] == json!([]) && last["usage"]["completion_tokens"] == 12;
    let priced = charge(&tokens(PRIVATE, 40, 12));
    let settled = s.eventually(wait, || settled_as(before, PRIVATE, priced));
    s.ok(
        "a streamed call reaches the agent in OpenAI's shape, its tool call as Finite Private streamed it, and is settled from its last usage",
        r.status == 200 && called.as_deref() == Some("terminal") && shaped && tried(s, calls) == private(1) && settled,
        json!({ "status": r.status, "called": called, "last": last, "entries": aig().len() - before }),
    );

    // ---- failed before answering: once more on Workers AI's GLM-5.3 Flash
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.fail_next(&[429]);
    s.ai.set_usage(&[Used { prompt: 60, cached: 0, completion: 25 }]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("cheap", false)))?;
    let made = s.ai.calls()[calls..].to_vec();
    let priced = charge(&tokens(FLASH, 60, 25));
    let settled = s.eventually(wait, || settled_as(before, FLASH, priced));
    s.ok(
        "a call Finite Private answers 429 (its key's window spent) is made once more, the same, on Workers AI's GLM-5.3 Flash, which answers it",
        r.status == 200 && r.body["choices"][0]["message"]["content"] == "echo: hello" && tried(s, calls) == fell() && made.len() == 2 && made[0].body["messages"] == made[1].body["messages"] && made[1].authorization.is_none(),
        json!({ "status": r.status, "tried": format!("{:?}", tried(s, calls)) }),
    );
    s.ok("under one hold, settled once from Workers AI's usage, at its price", settled, json!({ "charge": priced, "entries": aig() }));
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.down(&[PRIVATE]);
    s.ai.set_usage(&[Used { prompt: 30, cached: 0, completion: 30 }]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("cheap", true)))?;
    s.ai.down(&[]);
    let last = lines(&r.text).last().cloned().unwrap_or_default();
    let priced = charge(&tokens(FLASH, 30, 30));
    let settled = s.eventually(wait, || settled_as(before, FLASH, priced));
    s.ok(
        "a streamed call whose Finite Private model is down (its limiter's 502) falls back the same, streamed in OpenAI's shape, settled as Workers AI's",
        r.status == 200 && last["choices"] == json!([]) && last["usage"]["completion_tokens"] == 30 && tried(s, calls) == fell() && settled,
        json!({ "status": r.status, "tried": format!("{:?}", tried(s, calls)), "last": last }),
    );
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.fail_next(&[503]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("medium", false)))?;
    s.ok(
        "a 503 (finite.computer not answering its limiter) falls back too, under one hold",
        r.status == 200 && tried(s, calls) == fell() && aig().len() == before + 1,
        format!("{} {:?}", r.status, tried(s, calls)),
    );
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.down(&[PRIVATE, FLASH]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("cheap", false)))?;
    s.ai.down(&[]);
    let released = s.eventually(wait, || aig().len() == before + 1 && aig().iter().filter(|e| end_of(e) == "released").count() >= 1);
    s.ok(
        "when Workers AI fails too, its failure is the call's, after two tries and no more, and the hold goes back",
        r.status == 502 && r.text.contains("could not route request to AI model") && tried(s, calls) == fell() && released,
        json!({ "status": r.status, "answer": r.text, "tried": format!("{:?}", tried(s, calls)) }),
    );
    let calls = s.ai.calls().len();
    s.ai.fail_next(&[400]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("cheap", false)))?;
    s.ok("a refusal of the request (400) is the call's: Workers AI is not asked", r.status == 400 && tried(s, calls) == private(1), format!("{} {:?}", r.status, tried(s, calls)));
    s.ai.accept_key("fpk_live_another");
    let calls = s.ai.calls().len();
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("cheap", false)))?;
    s.ai.accept_key(FINITE_PRIVATE_KEY);
    s.ok(
        "a key Finite Private refuses is its 401, passed through, and not fallen back from (a decision for Paul)",
        r.status == 401 && r.text.contains("invalid_api_key") && tried(s, calls) == private(1),
        format!("{} {:?} {}", r.status, tried(s, calls), r.text),
    );

    // ---- the route's other names: on Workers AI
    let (calls, before) = (s.ai.calls().len(), aig().len());
    s.ai.set_usage(&[Used { prompt: 30, cached: 0, completion: 10 }]);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("fallback", false)))?;
    let priced = charge(&tokens(FLASH, 30, 10));
    let settled = s.eventually(wait, || settled_as(before, FLASH, priced));
    s.ok(
        "`fallback` (Hermes' own switch when a call stalls) runs on Workers AI's GLM-5.3 Flash, and is settled at its price",
        r.status == 200 && tried(s, calls) == [(Face::WorkersAi, FLASH.to_string())] && settled,
        format!("{} {:?}", r.status, tried(s, calls)),
    );
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let look = json!({ "model": "vision", "messages": [{ "role": "user", "content": [{ "type": "text", "text": "what is this?" }, { "type": "image_url", "image_url": { "url": png } }] }] });
    let calls = s.ai.calls().len();
    let r = api.signed(&hand, "POST", ROUTE, Some(&look))?;
    s.ok(
        "`vision` runs on Workers AI's GLM-5.3 Flash: Finite Private's images are not qualified",
        r.status == 200 && tried(s, calls) == [(Face::WorkersAi, FLASH.to_string())],
        format!("{} {:?} {}", r.status, tried(s, calls), r.text),
    );

    // ---- a call that hangs: the caller's to give up on
    let calls = s.ai.calls().len();
    s.ai.delay_next(&[HANG.as_millis() as u64]);
    let gave_up = api.impatient(PATIENCE).signed(&hand, "POST", ROUTE, Some(&chat("cheap", true))).is_err();
    let during = tried(s, calls);
    let r = api.signed(&hand, "POST", ROUTE, Some(&chat("fallback", true)))?;
    s.ok(
        "a call Finite Private holds past its caller's patience is the caller's to give up on: the route makes no second try of its own, and the caller's `fallback` answers at once",
        gave_up && during == private(1) && r.status == 200 && tried(s, calls) == [(Face::OpenAi, PRIVATE.to_string()), (Face::WorkersAi, FLASH.to_string())],
        json!({ "gaveUp": gave_up, "during": format!("{during:?}"), "status": r.status, "tried": format!("{:?}", tried(s, calls)) }),
    );

    // ---- a job's text step on Finite Private, falling back the same
    let name = s.named(api, &owner, "private")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, LEDGER_APP, LEDGER_JSON);
    s.eventually(wait, || api.op(&owner, &name, "notes", "q", json!({})).is_ok_and(|r| r.status == 200));
    let calls = s.ai.calls().len();
    s.ai.set_usage(&[Used { prompt: 1_000, cached: 0, completion: 500 }]);
    let r = api.op(&owner, &name, "summarize", "fp-1", json!({ "text": "the notes", "tier": "cheap" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let call = s.ai.calls().get(calls).cloned();
    s.ok(
        "a job's `ai.text` runs on Finite Private with the platform's key, and costs its call at Finite Private's price",
        run["status"] == "succeeded"
            && run["output"]["text"] == "echo: the notes"
            && run["output"]["model"] == PRIVATE
            && run["output"]["tier"] == "cheap"
            && tried(s, calls) == private(1)
            && call.is_some_and(|c| c.authorization.as_deref() == Some(platform_key.as_str()))
            && run["costMicros"] == charge(&tokens(PRIVATE, 1_000, 500)),
        &run,
    );
    let calls = s.ai.calls().len();
    s.ai.fail_next(&[503]);
    let r = api.op(&owner, &name, "summarize", "fp-2", json!({ "text": "again", "tier": "medium" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "one whose Finite Private answers 503 is answered by Workers AI's GLM-5.3 Flash, once, and costs that call",
        run["status"] == "succeeded" && run["output"]["text"] == "echo: again" && run["output"]["model"] == FLASH && tried(s, calls) == fell() && run["costMicros"].as_i64().is_some_and(|c| c > 0),
        json!({ "run": run, "tried": format!("{:?}", tried(s, calls)) }),
    );
    Ok(())
}
