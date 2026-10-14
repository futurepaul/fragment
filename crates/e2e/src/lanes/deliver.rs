//! Deliveries and AI (slice F): web push from mutations and jobs through
//! the delivery queue to a push service that checks VAPID and decrypts as
//! a browser would; subscriptions that are gone, retries, the dead-letter
//! report; and AI as a job's steps: text through the model
//! route and images on its transport (both the Workers AI fake, a lower
//! rung at the vendor boundary), generated images stored as files, video
//! steps refused, and the calories template's text step. What each paid
//! step costs is the ledger section's.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::workers_ai::{image_bytes, IMAGE_MODEL};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use crate::api::{Api, Call, Reply};
use crate::Suite;

const MEDIA_APP: &[u8] = include_bytes!("../../fixtures/media.mjs");
const MEDIA_JSON: &[u8] = include_bytes!("../../fixtures/media.json");
/// How long the slow push receiver takes to answer: well past the 2 s a
/// delivery beside it in the batch must land within.
const SLOW_RECEIVER: Duration = Duration::from_secs(4);

/// The image model's calls, in order.
fn images(s: &Suite) -> Vec<fragment_fakes::workers_ai::AiCall> {
    s.ai.calls().into_iter().filter(|c| c.model == IMAGE_MODEL).collect()
}

fn events(api: &Api, keys: &Keys, name: &str) -> String {
    api.signed(keys, "GET", &format!("/api/f/{name}/events?since=0"), None).map(|r| r.text).unwrap_or_default()
}

pub fn push(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("push", &[crate::Need::Fakes, crate::Need::Levers]) {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "push")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, MEDIA_APP, MEDIA_JSON);
    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    let site = |path: &str, body: Option<Value>| -> Result<Reply> {
        api.call(Call {
            method: if body.is_some() { "POST" } else { "GET" },
            url: format!("{}?view={view}", api.site_url(&name, path)),
            body: body.map(|b| b.to_string().into_bytes()),
            content_type: Some("application/json"),
            ..Call::default()
        })
    };
    let wait = Duration::from_secs(30);

    let r = site("__push-key", None)?;
    s.ok("the fragment has a VAPID key for browsers", r.status == 200 && r.body["key"].as_str().is_some_and(|k| k.len() == 87), &r);
    let again = site("__push-key", None)?;
    s.ok("and keeps it", again.body["key"] == r.body["key"], &again);
    let r = site("__sw.js", None)?;
    s.ok("the service worker is served", r.status == 200 && r.header("content-type").starts_with("text/javascript") && r.text.contains("showNotification"), &r);
    let subscribe = |s: &Suite, id: &str, who: &str, seed: u8| {
        let sub = s.push.subscribe(id, seed);
        site("__push-sub", Some(json!({ "who": who, "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth })))
    };
    let r = subscribe(s, "a", "team", 3)?;
    s.ok("a page subscribes, tagged with a who", r.status == 200 && r.body["who"] == "team", &r);
    subscribe(s, "b", "solo", 5)?;
    let r = site("__push-sub", Some(json!({ "who": "x", "endpoint": format!("{}/push/z", s.push.url), "p256dh": "nope", "auth": "nope" })))?;
    s.ok("a subscription with bad keys is refused", r.status == 400, &r);
    let sub = s.push.subscribe(&s.name("push-for-someone"), 9);
    let r = site("__push-sub", Some(json!({ "who": "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6", "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth })))?;
    s.ok("a who that names an identity is that identity's own: a link holder subscribes for no one (403)", r.status == 403, &r);

    // a mutation pushes to everyone, once it commits, once
    let r = api.op(&owner, &name, "notify_all", "n1", json!({ "title": "hello" }))?;
    s.ok("a mutation's push is queued as it commits", r.status == 200, &r);
    let both = s.eventually(wait, || s.push.received("a").len() == 1 && s.push.received("b").len() == 1);
    s.ok(
        "both browsers get it, signed and decrypted",
        both && s.push.received("a")[0] == json!({ "title": "hello", "body": "from a mutation" }) && s.push.refused().is_empty(),
        format!("{:?} {:?} refused {:?}", s.push.received("a"), s.push.received("b"), s.push.refused()),
    );
    // the replay, then a sentinel push through the same queue: once the
    // sentinel has landed, a second push of the replay would have too
    let r = api.op(&owner, &name, "notify_all", "n1", json!({ "title": "hello" }))?;
    api.op(&owner, &name, "notify_all", "n1-sentinel", json!({ "title": "sentinel" }))?;
    let sentinel = |who: &str| s.push.received(who).iter().any(|p| p["title"] == "sentinel");
    let landed = s.eventually(wait, || sentinel("a") && sentinel("b"));
    let pushed = |title: &str| json!({ "title": title, "body": "from a mutation" });
    s.ok(
        "a replay pushes nothing again",
        r.body["replayed"] == true && landed && ["a", "b"].iter().all(|who| s.push.received(who) == [pushed("hello"), pushed("sentinel")]),
        format!("{:?} {:?}", s.push.received("a"), s.push.received("b")),
    );

    // a job pushes to one tag
    let (a0, b0) = (s.push.received("a").len(), s.push.received("b").len());
    let r = api.op(&owner, &name, "announce", "j1", json!({ "who": "team", "title": "team only" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok("a job's push step answers how many it queued", run["output"]["queued"] == 1, &run);
    s.ok("only that tag gets it", s.eventually(wait, || s.push.received("a").len() == a0 + 1) && s.push.received("b").len() == b0, "");

    // a subscription that is gone is dropped; a failing one is retried
    s.push.forget("b");
    api.op(&owner, &name, "notify_all", "n2", json!({ "title": "second" }))?;
    let dropped = s.eventually(wait, || events(api, &owner, &name).contains("push.gone"));
    s.ok("a push service's 410 drops the subscription", dropped, "");
    let r = api.op(&owner, &name, "announce", "j2", json!({ "who": "*", "title": "after the drop" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok("then only the live subscription is pushed to", run["output"]["queued"] == 1, &run);
    // that push lands first (its run may finish before it does): the next
    // count is the flaky push's alone
    s.eventually(wait, || s.push.received("a").iter().any(|p| p["title"] == "after the drop"));
    s.push.fail("a", 2);
    let before = s.push.received("a").len();
    api.op(&owner, &name, "notify_all", "n3", json!({ "title": "flaky" }))?;
    let landed = s.eventually(Duration::from_secs(60), || s.push.received("a").len() == before + 1);
    s.ok("a push service's 503 is retried until it lands", landed && s.push.received("a").last().is_some_and(|p| p["title"] == "flaky"), format!("{:?}", s.push.received("a")));

    // the outbox: a record or a push whose queue send fails is still
    // delivered, since it was written down with what caused it
    let fail_queue = |times: u32| api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "fail-deliveries", "times": times })));
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/subscriptions"), Some(&json!({ "channel": "news", "url": format!("{}/notify", s.push.url) })))?;
    s.ok("a member subscribes a URL to a channel", r.status == 200, &r);
    let r = fail_queue(1)?;
    s.ok("(the test fleet fails the next queue send)", r.status == 200, &r);
    api.op(&owner, &name, "headline", "h1", json!({ "text": "past a failed queue" }))?;
    let arrived = s.eventually(wait, || s.push.notified().iter().any(|f| f["type"] == "record" && f["record"]["body"]["text"] == "past a failed queue"));
    let deferred = events(api, &owner, &name).contains("delivery.deferred");
    s.ok("a record whose queue send failed still reaches its subscriber, from the outbox", arrived && deferred, format!("deferred: {deferred}"));
    let before = s.push.received("a").len();
    fail_queue(1)?;
    api.op(&owner, &name, "notify_all", "n4", json!({ "title": "past a failed queue" }))?;
    let pushed = s.eventually(wait, || s.push.received("a").len() == before + 1);
    s.ok("a push whose queue send failed still reaches the browser", pushed && s.push.received("a").last() == Some(&json!({ "title": "past a failed queue", "body": "from a mutation" })), format!("{:?}", s.push.received("a")));
    // an outage is one event: the second failed send finds the first row
    // still waiting (one lookup a drain), and says nothing more
    let deferred = || {
        api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=200"), None).map(|r| r.text.matches("\"delivery.deferred\"").count()).unwrap_or(0)
    };
    let before = deferred();
    fail_queue(2)?;
    api.op(&owner, &name, "headline", "o1", json!({ "text": "outage one" }))?;
    api.op(&owner, &name, "headline", "o2", json!({ "text": "outage two" }))?;
    let both = s.eventually(wait, || {
        let frames = s.push.notified();
        ["outage one", "outage two"].iter().all(|t| frames.iter().any(|f| f["record"]["body"]["text"] == *t))
    });
    let after = deferred();
    s.ok("two deliveries that wait through one outage say so once", both && after == before + 1, format!("arrived {both}; deferred events {before} then {after}"));

    // a batch goes out together: a receiver that takes 4 seconds to
    // answer holds up only its own delivery, and one queued behind it in
    // the same batch lands at once
    subscribe(s, "slow", "race", 11)?;
    subscribe(s, "fast", "race", 13)?;
    s.push.slow("slow", SLOW_RECEIVER);
    let r = api.op(&owner, &name, "notify_all", "race", json!({ "title": "side by side" }))?;
    let queued = Instant::now();
    let fast = s.eventually(Duration::from_secs(12), || !s.push.landed("fast").is_empty());
    let fast_after = s.push.landed("fast").first().map(|t| t.duration_since(queued));
    println!("      the fast delivery landed {fast_after:?} after the push was queued");
    s.ok(
        "a delivery in the same batch as one that takes 4 seconds still lands within 2 seconds",
        r.status == 200 && fast && fast_after.is_some_and(|d| d < Duration::from_secs(2)),
        format!("{r}; the fast one landed after {fast_after:?}"),
    );
    let slow = s.eventually(Duration::from_secs(30), || !s.push.landed("slow").is_empty());
    let slow_after = s.push.landed("slow").first().map(|t| t.duration_since(queued));
    s.ok(
        "and the slow one lands once it answers, once",
        slow && slow_after.is_some_and(|d| d >= SLOW_RECEIVER) && s.push.received("slow") == vec![json!({ "title": "side by side", "body": "from a mutation" })],
        format!("the slow one landed after {slow_after:?}: {:?}", s.push.received("slow")),
    );

    let r = site("__push-unsub", Some(json!({ "endpoint": format!("{}/push/a", s.push.url) })))?;
    s.ok("a page unsubscribes by its endpoint", r.status == 200 && r.body["removed"] == 1, &r);

    // out of retries: the dead-letter queue reports to the fragment
    subscribe(s, "c", "doomed", 7)?;
    s.push.fail("c", 1000);
    api.op(&owner, &name, "announce", "j3", json!({ "who": "doomed", "title": "never" }))?;
    let failed = s.eventually(Duration::from_secs(120), || events(api, &owner, &name).contains("delivery.failed"));
    s.ok("a delivery out of retries is reported in the event log", failed, "");
    Ok(())
}

pub fn ai(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("ai", &[crate::Need::Fakes]) {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "ai")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, MEDIA_APP, MEDIA_JSON);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let wait = Duration::from_secs(40);
    let run = |id: &str, op: &str, input: Value| -> Result<Value> {
        let r = api.op(&owner, &name, op, id, input)?;
        Ok(settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait))
    };
    // a run with no charge (none reserved, or one released) names no cost
    let cost = |r: &Value| r["costMicros"].as_i64().unwrap_or(0);
    // tools, drafts, and streams held open (ai_tools.rs)
    super::ai_tools::text_tools(s, api, wait)?;

    let r = run("t1", "summarize", json!({ "text": "the meeting notes" }))?;
    s.ok(
        "a job's text step answers the model's text, on the model its tier names",
        r["output"]["text"] == "echo: the meeting notes" && r["output"]["model"] == "@cf/zai-org/glm-5.3" && r["output"]["tier"] == "medium",
        &r,
    );
    let call = s.ai.calls().last().cloned();
    s.ok(
        "the model route sent its input bounded: the job's reasoning effort as given (high is one GLM takes), its tokens capped",
        call.as_ref().is_some_and(|c| c.model == "@cf/zai-org/glm-5.3" && c.body["reasoning_effort"] == "high" && c.body["max_tokens"] == 16_384 && c.body.get("model").is_none()),
        format!("{call:?}"),
    );
    let r = run("t-high", "summarize_high", json!({ "text": "the high tier" }))?;
    s.ok("a step on the high tier is refused, saying why (decision 23)", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("high tier is off")), &r);

    let r = run("i1", "draw", json!({ "prompt": "a lighthouse", "path": "art/lighthouse.jpg" }))?;
    s.ok(
        "an image step writes the model's JPEG to main",
        r["output"]["path"] == "art/lighthouse.jpg" && r["output"]["mediaType"] == "image/jpeg" && s.fake.file_at(&repo, "main", "art/lighthouse.jpg") == Some(image_bytes("a lighthouse")),
        &r,
    );
    let call = images(s).last().cloned();
    s.ok(
        "drawn by FLUX.1 [schnell] on the model route's transport: the catalog's input (4 steps unless named), the payer by an opaque id",
        call.as_ref().is_some_and(|c| c.body == json!({ "prompt": "a lighthouse", "steps": 4 }) && c.metadata["user_id"].as_str().is_some_and(|u| u.len() == 16)),
        format!("{call:?}"),
    );
    let mid = image_bytes("a mid harbour");
    let r = run("i-mid", "draw", json!({ "prompt": "a mid harbour", "path": "art/harbour.jpg", "steps": 8 }))?;
    s.ok(
        "a 300 KiB image, past an app's write limit and under a blob's size, is kept in git (bug 1), drawn at the steps the job named",
        r["status"] == "succeeded" && s.fake.file_at(&repo, "main", "art/harbour.jpg") == Some(mid) && images(s).last().is_some_and(|c| c.body["steps"] == 8),
        &r,
    );
    let big = image_bytes("a large mural");
    let r = run("i2", "draw", json!({ "prompt": "a large mural", "path": "art/mural.jpg" }))?;
    let pointer = fragment_core::blob::parse(&s.fake.file_at(&repo, "main", "art/mural.jpg").unwrap_or_default());
    s.ok(
        "a large image is a blob, its pointer in git",
        r["status"] == "succeeded" && pointer.as_ref().is_some_and(|p| p.sha256 == fragment_core::blob::sha256_hex(&big) && p.size == big.len() as u64),
        &r,
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file?path=art/mural.jpg"), None)?;
    s.ok("and its bytes are served", r.status == 200 && r.bytes == big, format!("{} {} bytes", r.status, r.bytes.len()));

    // refused before any call: nothing is reserved, nothing reaches the model
    let drawn = images(s).len();
    let png = run("i-png", "draw", json!({ "prompt": "a lighthouse", "path": "art/lighthouse.png" }))?;
    let model = run("i-model", "draw", json!({ "prompt": "a lighthouse", "path": "art/l.jpg", "model": "google/gemini-3.1-flash-lite-image" }))?;
    let steps = run("i-steps", "draw", json!({ "prompt": "a lighthouse", "path": "art/l.jpg", "steps": 9 }))?;
    s.ok(
        "an image to a path no JPEG's (bug 5), naming a model, or past 8 steps is refused, saying why, before any call",
        png["status"] == "held" && png["error"].as_str().is_some_and(|e| e.contains("ends in .jpg or .jpeg"))
            && model["status"] == "held" && model["error"].as_str().is_some_and(|e| e.contains("unknown field `model`"))
            && steps["status"] == "held" && steps["error"].as_str().is_some_and(|e| e.contains("steps is 1 to 8"))
            && images(s).len() == drawn && [&png, &model, &steps].iter().all(|r| cost(r) == 0),
        json!([png, model, steps]),
    );

    let calls = s.ai.calls().len();
    let r = run("v1", "film", json!({ "prompt": "waves", "path": "video/waves.mp4" }))?;
    s.ok(
        "a video step is refused, saying why, and calls and reserves nothing",
        r["status"] == "held"
            && r["error"].as_str().is_some_and(|e| e.contains("video steps are off until they run on Cloudflare"))
            && cost(&r) == 0
            && s.ai.calls().len() == calls
            && s.fake.file_at(&repo, "main", "video/waves.mp4").is_none(),
        &r,
    );

    s.ai.fail_next(&[503]);
    let calls = s.ai.calls().len();
    let r = run("t2", "summarize", json!({ "text": "again" }))?;
    let tried: Vec<String> = s.ai.calls()[calls..].iter().map(|c| c.model.clone()).collect();
    s.ok(
        "a text step whose model answers 503 is answered by the fallback model, once, and costs the fallback's call",
        r["output"]["text"] == "echo: again" && r["output"]["model"] == "@cf/deepseek-ai/deepseek-v4-flash-0731" && tried == ["@cf/zai-org/glm-5.3", "@cf/deepseek-ai/deepseek-v4-flash-0731"] && cost(&r) > 0,
        json!({ "run": r, "tried": tried }),
    );
    s.ai.fail_next(&[503]);
    let r = run("i3", "draw", json!({ "prompt": "a harbour at dusk", "path": "art/dusk.jpg" }))?;
    s.ok("the image model's 503 is retried", r["status"] == "succeeded" && s.fake.file_at(&repo, "main", "art/dusk.jpg") == Some(image_bytes("a harbour at dusk")), &r);
    s.ai.fail_next(&[400]);
    let r = run("i4", "draw", json!({ "prompt": "refused", "path": "art/none.jpg" }))?;
    s.ok("its refusal holds the run, saying so, and charges nothing", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("the model answered 400")) && cost(&r) == 0, &r);
    let r = run("i5", "draw", json!({ "prompt": "a png", "path": "art/junk.jpg" }))?;
    s.ok(
        "an answer that is no JPEG is refused, saying why, and nothing is written; the call is charged",
        r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("not a JPEG")) && s.fake.file_at(&repo, "main", "art/junk.jpg").is_none() && cost(&r) > 0,
        &r,
    );
    decisions(s, api, &owner, &name, wait)?;
    calories(s, api, wait)
}

/// Typed decisions (docs/api.md, AI): the Clef family by name, its media
/// the fragment's own blobs (as a page's `fragment.blob(file)` and a chat's
/// attachments name them) or small and inline, each refused before any
/// reservation or vendor call when it is outside the contract, and billed
/// for input tokens alone, once, whatever retries. Vendor-boundary
/// fixtures prove the transport, the media and the money, not a real
/// model's accuracy: that is the hosted lane's.
fn decisions(s: &mut Suite, api: &Api, owner: &fragment_nip98::Keys, name: &str, wait: Duration) -> Result<()> {
    use super::ledger::entries;
    use base64::Engine;
    use fragment_core::decide::{Kind, Model};
    use fragment_core::price::{PriceBook, Usage};
    use fragment_fakes::workers_ai::{spoken_wav, Used};
    s.ai.clear_script();
    let owner_id = api.identity(owner)?;
    let run = |id: &str, input: Value| -> Result<Value> {
        let r = api.op(owner, name, "decide", id, input)?;
        Ok(settle(api, owner, name, started(&r), &["succeeded", "held"], wait))
    };
    let price = |m: Model, input: u64| PriceBook::defaults().price(&Usage::Tokens { model: m.id().into(), input, cached_input: 0, cache_write: 0, output: 0 }).map_or(-1, |p| p.charge);
    let cost = |r: &Value| r["costMicros"].as_i64().unwrap_or(0);
    // the ledger's reservations of one run's steps, every attempt's
    let paid = |r: &Value| -> Vec<Value> {
        let marker = format!("/run/{}/attempt/", r["id"]);
        entries(api, &owner_id, &format!("step:{name}@")).into_iter().filter(|e| e["ref"].as_str().is_some_and(|r| r.contains(&marker))).collect()
    };
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let opts = json!({ "input": "tallow potato chips", "questions": {
        "aisle": { "type": "choice", "instructions": "Choose the aisle for the finished product", "options": { "snacks": "Chips and crackers", "produce": "Fresh fruit and vegetables", "grocery": "Shelf-stable foods", "frozen": "Frozen foods" } },
        "snack": { "type": "predicate", "instructions": "Is it a snack?" },
        "urgency": { "type": "score", "instructions": "How soon?", "levels": ["Later", "Now"] }
    } });
    let with = |extra: Value| {
        let mut v = opts.clone();
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        v
    };
    for (i, (item, aisle)) in [("tallow potato chips", "snacks"), ("kale chips", "snacks"), ("potato", "produce"), ("peanut butter", "grocery"), ("ice cream", "frozen")].iter().enumerate() {
        let mut ps = json!({ "snacks": 0.0, "produce": 0.0, "grocery": 0.0, "frozen": 0.0 });
        ps[*aisle] = json!(1.0);
        s.ai.decide_next(&[json!({ "model": "clef-flash", "answers": {
            "aisle": { "type": "choice", "choice": aisle, "probabilities": ps, "confidence": 1.0 },
            "snack": { "type": "noul", "noul": 0.9 },
            "urgency": { "type": "score", "score": 0.25, "legend": { "0": "Later", "1": "Now" }, "probabilities": { "0": 0.75, "1": 0.25 }, "confidence": 0.5 }
        }, "usage": { "input_tokens": 300, "output_tokens": 40 } })]);
        let r = run(&format!("decision-{i}"), with(json!({ "input": item })))?;
        s.ok(
            &format!("ai.decide passes the fixture's {item} → {aisle}, with typed probabilities, on clef-flash unless named, billed for its input alone"),
            r["status"] == "succeeded" && r["output"]["model"] == Model::ClefFlash.id() && r["output"]["answers"]["aisle"]["choice"] == *aisle
                && r["output"]["answers"]["snack"]["probability"] == 0.9 && r["output"]["answers"]["urgency"]["score"] == 0.25
                && cost(&r) == price(Model::ClefFlash, 300),
            &r,
        );
    }
    let call = s.ai.calls().last().cloned();
    s.ok(
        "a decision is System One's input on the model route's transport: the provider's names, the payer by an opaque id, no key",
        call.as_ref().is_some_and(|c| c.model == Model::ClefFlash.id() && c.body["model"] == "clef-flash" && c.body["state"] == "ice cream"
            && c.body["questions"]["snack"]["type"] == "noul" && c.body["questions"]["aisle"]["criteria"] == opts["questions"]["aisle"]["options"]
            && c.metadata["user_id"].as_str().is_some_and(|u| u.len() == 16) && c.authorization.is_none()),
        format!("{call:?}"),
    );
    s.ai.set_usage(&[Used { prompt: 500, cached: 0, completion: 0 }]);
    let r = run("decision-clef", with(json!({ "model": "clef" })))?;
    let call = s.ai.calls().last().cloned();
    s.ok(
        "`model: clef` runs Clef, its selector its own, at its price",
        r["status"] == "succeeded" && r["output"]["model"] == Model::Clef.id() && cost(&r) == price(Model::Clef, 500)
            && call.as_ref().is_some_and(|c| c.model == Model::Clef.id() && c.body["model"] == "clef"),
        json!({ "run": r, "call": call.map(|c| json!({ "model": c.model, "selector": c.body["model"] })) }),
    );

    // media: the fragment's own blobs, as a page uploads them
    let upload = |bytes: &[u8], kind: &str| -> Result<String> {
        let sha = fragment_core::blob::sha256_hex(bytes);
        let url = format!("{}/api/f/{name}/blobs/{sha}", api.base);
        let r = api.call(Call { method: "PUT", url, body: Some(bytes.to_vec()), content_type: Some(kind), keys: Some(owner), ..Call::default() })?;
        anyhow::ensure!(r.status == 200, "a blob's upload: {r}");
        Ok(sha)
    };
    let photo: &[u8] = b"\x89PNG\r\n\x1a\na-photo-of-tallow-chips";
    let memo = spoken_wav("buy more chips", 2);
    let clip: &[u8] = b"\x00\x00\x00\x18ftypmp42a-clip-of-the-snack-aisle";
    let (photo_sha, memo_sha, clip_sha) = (upload(photo, "image/png")?, upload(&memo, "audio/wav")?, upload(clip, "video/mp4")?);
    let big = upload(&vec![0x89; Kind::Image.max_bytes() as usize + 1], "image/png")?;
    // posted, as a chat's attachments are: a record naming a blob keeps it
    // (one nothing names goes after the e2e's grace of seconds)
    let attachments: Vec<Value> = [&photo_sha, &memo_sha, &clip_sha, &big].iter().map(|sha| json!({ "sha256": sha })).collect();
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/uploads"), Some(&json!({ "id": "uploads-1", "body": { "attachments": attachments } })))?;
    anyhow::ensure!(r.status == 200, "the uploads' record: {r}");
    s.ai.set_usage(&[Used { prompt: 1_300, cached: 0, completion: 0 }]);
    // a chat's attachment, as its record carries it
    let attachment = json!({ "sha256": photo_sha, "size": photo.len(), "type": "image/png", "name": "chips.png" });
    let r = run("decision-photo", with(json!({ "input": "a photo someone uploaded", "media": [attachment] })))?;
    let call = s.ai.calls().last().cloned();
    s.ok(
        "a photo people uploaded (a blob, named as a chat's attachment is) is decided about on clef-flash: its bytes reach the model as a data: URL, its tokens billed as input",
        r["status"] == "succeeded" && cost(&r) == price(Model::ClefFlash, 1_300)
            && call.as_ref().is_some_and(|c| c.model == Model::ClefFlash.id() && c.body["images"] == json!([format!("data:image/png;base64,{}", b64(photo))]) && c.body.get("audio").is_none()),
        json!({ "run": r, "images": call.map(|c| c.body["images"].clone()) }),
    );
    let inline = format!("data:image/png;base64,{}", b64(photo));
    let r = run("decision-inline", with(json!({ "model": "clef", "media": [{ "data": inline }] })))?;
    let call = s.ai.calls().last().cloned();
    s.ok(
        "a small image inline goes as it came, to Clef too",
        r["status"] == "succeeded" && call.as_ref().is_some_and(|c| c.model == Model::Clef.id() && c.body["images"] == json!([inline])),
        json!({ "run": r }),
    );
    s.ai.set_usage(&[Used { prompt: 2_000, cached: 0, completion: 0 }]);
    let media = json!([{ "sha256": memo_sha, "type": "audio/wav" }, { "sha256": clip_sha, "type": "video/mp4" }]);
    let r = run("decision-omni", with(json!({ "model": "clef-omni", "input": "a voice note and a clip", "media": media })))?;
    let call = s.ai.calls().last().cloned();
    s.ok(
        "a voice note and a video clip are heard and watched on clef-omni, each in its own field, billed at its price",
        r["status"] == "succeeded" && r["output"]["model"] == Model::ClefOmni.id() && cost(&r) == price(Model::ClefOmni, 2_000)
            && call.as_ref().is_some_and(|c| c.model == Model::ClefOmni.id() && c.body["model"] == "clef-omni"
                && c.body["audio"] == json!([format!("data:audio/wav;base64,{}", b64(&memo))]) && c.body["videos"] == json!([format!("data:video/mp4;base64,{}", b64(clip))])),
        json!({ "run": r }),
    );

    // refused before any reservation or vendor call, saying why
    let calls = s.ai.calls().len();
    let refusals = [
        ("audio on clef-flash", with(json!({ "media": [{ "sha256": memo_sha, "type": "audio/wav" }] })), "clef-omni alone"),
        ("a video on clef", with(json!({ "model": "clef", "media": [{ "sha256": clip_sha, "type": "video/mp4" }] })), "clef-omni alone"),
        ("an image past 4 MiB", with(json!({ "media": [{ "sha256": big, "type": "image/png" }] })), "image is at most 4 MiB"),
        ("five media items", with(json!({ "media": vec![json!({ "sha256": photo_sha, "type": "image/png" }); 5] })), "at most 4 media items"),
        ("a blob the fragment does not hold", with(json!({ "media": [{ "sha256": fragment_core::blob::sha256_hex(b"never uploaded"), "type": "image/png" }] })), "does not hold"),
        ("a browser's voice memo type, which Clef does not hear", with(json!({ "model": "clef-omni", "media": [{ "sha256": memo_sha, "type": "audio/webm" }] })), "media is one of"),
        ("a remote URL", with(json!({ "media": [{ "url": "https://example.com/chips.png" }] })), "media is a list of blobs"),
        ("a model no Clef's", with(json!({ "model": "cheap" })), "model is one of clef-flash, clef, clef-omni"),
        ("a question of no type it takes", json!({ "input": "chips", "questions": { "x": { "type": "rank", "instructions": "Rank it" } } }), "question is {type"),
        ("no questions", json!({ "input": "chips", "questions": {} }), "1 to 64 questions"),
        ("a choice of one", json!({ "input": "chips", "questions": { "x": { "type": "choice", "instructions": "Aisle?", "options": { "only": "One" } } } }), "2 to 255"),
        ("text past clef-flash's 16 KiB", with(json!({ "input": "x".repeat(Model::ClefFlash.request_max_bytes()) })), "at most 16384 bytes on clef-flash"),
    ];
    for (i, (what, input, why)) in refusals.into_iter().enumerate() {
        let r = run(&format!("decision-invalid-{i}"), input)?;
        s.ok(
            &format!("{what} is refused, saying why, before any reservation or vendor call"),
            r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains(why)) && cost(&r) == 0 && paid(&r).is_empty() && s.ai.calls().len() == calls,
            json!({ "run": r, "calls": s.ai.calls().len() - calls }),
        );
    }

    let r = api.op(owner, name, "decide_inline", "decision-inline-big", json!({ "bytes": 200 * 1024 }))?;
    let r = settle(api, owner, name, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "inline media past a job's 256 KiB (a larger file is a blob) is refused, saying why, before any reservation or vendor call",
        r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("inline media is at most 256 KiB")) && cost(&r) == 0 && paid(&r).is_empty() && s.ai.calls().len() == calls,
        &r,
    );

    // a step retried after its paid call answered is billed once: its kept answer, its media unread again
    api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "fail-after-paid", "times": 1 })))?;
    let calls = s.ai.calls().len();
    let r = run("decision-retry-paid", with(json!({ "media": [{ "sha256": photo_sha, "type": "image/png" }] })))?;
    let receipts = paid(&r);
    s.ok(
        "a decision about a photo that failed after its paid call retries from its kept answer: one vendor call, one charge",
        r["status"] == "succeeded" && s.ai.calls().len() == calls + 1 && receipts.len() == 1 && receipts[0]["entry"]["end"]["end"] == "settled" && receipts[0]["entry"]["end"]["charge"] == r["costMicros"],
        json!({ "run": r, "paid": receipts }),
    );
    s.ai.fail_next(&[503]);
    let calls = s.ai.calls().len();
    let r = run("decision-503", with(json!({ "model": "clef-omni" })))?;
    s.ok(
        "a decision model's 503 retries on the same model",
        r["status"] == "succeeded" && s.ai.calls().len() == calls + 2 && s.ai.calls()[calls..].iter().all(|c| c.model == Model::ClefOmni.id()),
        &r,
    );
    s.ai.fail_next(&[400]);
    let r = run("decision-400", opts.clone())?;
    s.ok("a decision model's permanent refusal releases its reservation", r["status"] == "held" && cost(&r) == 0, &r);

    // missing usage pays the reservation, the model's context; a malformed
    // answer is paid too, and kept as a rejection: a replay never buys it again
    s.ai.decide_next(&[json!({ "answers": {} })]);
    let calls = s.ai.calls().len();
    let held = run("decision-malformed", opts.clone())?;
    let receipts = paid(&held);
    s.ok(
        "a malformed paid decision is held, and missing usage is charged at the reservation: clef-flash's context window",
        held["status"] == "held" && cost(&held) == price(Model::ClefFlash, Model::ClefFlash.context_tokens()) && receipts.len() == 1 && receipts[0]["entry"]["end"]["basis"] == "reservation",
        json!({ "run": held, "entries": receipts }),
    );
    api.signed(owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": held["id"] })))?;
    let again = settle(api, owner, name, held["id"].as_i64().unwrap_or(0), &["held"], wait);
    s.ok(
        "replaying a malformed paid decision reuses its rejection and never calls again",
        again["status"] == "held" && again["error"] == held["error"] && s.ai.calls().len() == calls + 1,
        &again,
    );
    Ok(())
}

/// The calories template, with no agent (Paul, 2026-10-07): a signed-in
/// person's plain words on `ask` start its `heard` job, whose text step
/// names the items; each is logged as theirs, and the answer is theirs on
/// `replies`. Words that name no food (the fake's echo is no JSON) log
/// nothing, and say so.
fn calories(s: &mut Suite, api: &Api, wait: Duration) -> Result<()> {
    let owner = api.person()?;
    let owner_id = api.identity(&owner)?;
    let name = s.named(api, &owner, "calories")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "calories" }))?;
    anyhow::ensure!(made.status == 200, "calories from its template: {made}");
    let replies = || {
        let r = api.signed(&owner, "GET", &format!("/api/f/{name}/channels/replies?after=0"), None);
        r.ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default()
    };
    let today = || api.op(&owner, &name, "today", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    s.ai.say_next(&[r#"{"items": [{"food": "2 eggs", "calories": 140}, {"food": "toast", "calories": 80}]}"#]);
    let post = |id: &str, text: &str| api.signed(&owner, "POST", &format!("/api/f/{name}/channels/ask"), Some(&json!({ "id": id, "body": { "text": text } })));
    post("a1", "2 eggs and toast")?;
    let landed = s.eventually(wait, || !replies().is_empty());
    let (day, said) = (today(), replies());
    s.ok(
        "calories: a signed-in person's plain words are read by a text step, each item logged as theirs, and answered for them on replies",
        landed && day["total"] == 220 && day["entries"].as_array().map(Vec::len) == Some(2) && said[0]["body"] == json!({ "for": owner_id, "text": "Logged 2 eggs (140 kcal), toast (80 kcal)." }),
        json!({ "today": day, "replies": said }),
    );
    post("a2", "hello there")?;
    let landed = s.eventually(wait, || replies().len() == 2);
    let (day, said) = (today(), replies());
    s.ok(
        "and words that name no food log nothing, saying so",
        landed && day["total"] == 220 && said[1]["body"]["text"].as_str().is_some_and(|t| t.starts_with("I couldn't tell")),
        json!({ "today": day, "replies": said }),
    );
    let r = api.op(&owner, &name, "summarize", "s1", json!({}))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let said = replies();
    s.ok(
        "its summary of the caller's day is a text step over their entries, answered for them",
        run["status"] == "succeeded" && said.len() == 3 && said[2]["body"]["for"] == owner_id.as_str()
            && said[2]["body"]["text"].as_str().is_some_and(|t| t.contains("2 eggs (140 kcal), toast (80 kcal); 220 kcal in all")),
        json!({ "run": run, "replies": said }),
    );
    Ok(())
}
