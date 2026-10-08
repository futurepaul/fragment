//! Deliveries and AI (slice F): web push from mutations and jobs through
//! the delivery queue to a push service that checks VAPID and decrypts as
//! a browser would; subscriptions that are gone, retries, the dead-letter
//! report; and AI as a job's steps: text through the model
//! route (its tools, its drafts as it streams), decisions (Clef) and images
//! on its transport (all the Workers AI fake, a lower rung at the vendor
//! boundary), generated images stored as files, video steps refused, and
//! the calories template's text step. What each paid step costs is the
//! ledger section's.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::workers_ai::{image_bytes, IMAGE_MODEL};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use crate::api::{Api, Call, Reply, Socket};
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
    let r = site("__push-sub", Some(json!({ "who": "id:00112233445566778899aabbccddeeff", "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth })))?;
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
    s.push.fail("a", 2);
    let before = s.push.received("a").len();
    api.op(&owner, &name, "notify_all", "n3", json!({ "title": "flaky" }))?;
    s.ok("a push service's 503 is retried until it lands", s.eventually(Duration::from_secs(60), || s.push.received("a").len() == before + 2), "");

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
    if !s.section("ai", &[crate::Need::Fakes, crate::Need::Levers]) {
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
    tools_and_drafts(s, api, &owner, &c, &run)?;
    decisions(s, api, &name, &run)?;

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
    let r = run("t2", "summarize", json!({ "text": "again" }))?;
    s.ok("the model's 503 is retried", r["output"]["text"] == "echo: again", &r);
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
    calories(s, api, wait)
}

/// A text step's tools, as a turn uses them: the model's tool call is the
/// step's message, and the conversation it is passed back in (the call, the
/// tool's result) reaches the model as it came. A step with a draft streams,
/// and a socket on the channel sees its text so far as the fragment's drafts.
fn tools_and_drafts(s: &mut Suite, api: &Api, owner: &Keys, c: &Value, run: &dyn Fn(&str, &str, Value) -> Result<Value>) -> Result<()> {
    let name = c["name"].as_str().unwrap_or_default();
    let cost = |r: &Value| r["costMicros"].as_i64().unwrap_or(0);
    let calls = s.ai.calls().len();
    let r = run("tool1", "tool_turn", json!({ "ask": "what is a shard? [[call lookup {\"word\": \"shard\"}]]" }))?;
    let (first, second) = (&r["output"]["first"], &r["output"]["second"]);
    let call = &first["message"]["tool_calls"][0];
    s.ok(
        "a text step offering tools answers the model's tool call as its message (OpenAI's shape), saying why it stopped",
        r["status"] == "succeeded"
            && first["finish_reason"] == "tool_calls"
            && first["message"]["role"] == "assistant"
            && call["type"] == "function"
            && call["function"]["name"] == "lookup"
            && call["id"].as_str().is_some_and(|i| !i.is_empty())
            && call["function"]["arguments"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()) == Some(json!({ "word": "shard" })),
        &r,
    );
    let sent: Vec<Value> = s.ai.chats().into_iter().skip(calls).collect();
    s.ok(
        "the model is offered the tools as the job gave them, and the next call carries the assistant's tool call and the tool's result as they came",
        sent.len() == 2
            && sent[0]["tools"][0]["function"]["name"] == "lookup"
            && sent[0]["tool_choice"] == "auto"
            && sent[1]["messages"][1]["tool_calls"][0]["id"] == call["id"]
            && sent[1]["messages"][2] == json!({ "role": "tool", "tool_call_id": call["id"], "content": "shard: a small piece broken off" }),
        json!(sent),
    );
    s.ok(
        "and the model's answer from the tool's result is the next step's text, with no tool call",
        second["text"].as_str().is_some_and(|t| t.contains("shard: a small piece broken off")) && second["finish_reason"] == "stop" && second["message"].get("tool_calls").is_none(),
        &r,
    );
    let tools = |n: usize| -> Vec<Value> { (0..n).map(|i| json!({ "type": "function", "function": { "name": format!("t{i}") } })).collect() };
    let calls = s.ai.calls().len();
    let many = run("tool-many", "ask_text", json!({ "prompt": "p", "tools": tools(65) }))?;
    let choice = run("tool-choice", "ask_text", json!({ "prompt": "p", "tool_choice": "required" }))?;
    let nowhere = run("draft-nowhere", "ask_text", json!({ "prompt": "p", "draft": { "channel": "elsewhere", "turn": "t" } }))?;
    let held = |r: &Value, says: &str| r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains(says)) && cost(r) == 0;
    s.ok(
        "more than 64 tools, a tool choice with no tools, or a draft to a channel the app does not declare is refused, saying why, before any call",
        held(&many, "at most 64 tools") && held(&choice, "tool_choice") && held(&nowhere, "declares no \"elsewhere\"") && s.ai.calls().len() == calls,
        json!([many, choice, nowhere]),
    );

    // a draft: the step streams, its text so far the channel's draft
    let mut page = Socket::open(api, name, "__live", Some(owner), None)?;
    page.until("hello", 5)?;
    page.send(&json!({ "type": "subscribe", "channel": "thinking", "after": 0 }))?;
    page.until("subscribed", 5)?;
    let said = "a long thought about gardens and the light on them";
    let whole = format!("echo: {said}");
    let calls = s.ai.calls().len();
    let r = api.op(owner, name, "ask_text", "draft1", json!({ "prompt": said, "draft": { "channel": "thinking", "turn": "turn:t_1" } }))?;
    let mut drafts = vec![];
    // bounded: each frame within the socket's wait, until the whole answer
    while let Ok(frame) = page.next() {
        if frame["type"] == "draft" {
            drafts.push(frame.clone());
            if frame["text"] == whole.as_str() {
                break;
            }
        }
    }
    page.close();
    let done = settle(api, owner, name, started(&r), &["succeeded", "held"], Duration::from_secs(40));
    let streamed = s.ai.calls().get(calls).is_some_and(|c| c.body["stream"] == true && c.body["stream_options"]["include_usage"] == true);
    s.ok(
        "a text step with a draft streams: a socket on the channel sees its text so far as the fragment's drafts under the step's turn, the last its whole answer",
        streamed
            && !drafts.is_empty()
            && drafts.iter().all(|d| d["channel"] == "thinking" && d["turn"] == "turn:t_1" && d["principal"] == c["npub"] && d["text"].as_str().is_some_and(|t| whole.starts_with(t)))
            && drafts.last().is_some_and(|d| d["text"] == whole.as_str()),
        json!({ "drafts": drafts }),
    );
    s.ok(
        "and the step answers the streamed text and message, metered from the stream's last usage",
        done["status"] == "succeeded" && done["output"]["text"] == whole.as_str() && done["output"]["message"] == json!({ "role": "assistant", "content": whole }) && done["output"]["finish_reason"] == "stop" && cost(&done) > 0,
        &done,
    );
    s.ai.break_next();
    let calls = s.ai.calls().len();
    let r = run("draft-broken", "ask_text", json!({ "prompt": "after a break", "draft": { "channel": "thinking", "turn": "turn:t_2" } }))?;
    s.ok(
        "a stream that breaks before its answer ends is called again, and answers",
        r["status"] == "succeeded" && r["output"]["text"] == "echo: after a break" && s.ai.calls().len() == calls + 2,
        &r,
    );
    Ok(())
}

/// A decision step: Clef's answers, one per question, charged its input
/// tokens, kept (a step tried again after its call never calls again), and
/// refused before any call outside its bounds.
fn decisions(s: &mut Suite, api: &Api, name: &str, run: &dyn Fn(&str, &str, Value) -> Result<Value>) -> Result<()> {
    let cost = |r: &Value| r["costMicros"].as_i64().unwrap_or(0);
    let questions = json!({
        "garden": { "type": "noul", "instructions": "Plants or garden?" },
        "taxes": { "type": "noul", "instructions": "Money or taxes?" },
        "room": { "type": "choice", "instructions": "Which room?", "criteria": { "kitchen": "cooking", "garden": "outside" } },
        "size": { "type": "score", "instructions": "How big?", "criteria": ["none", "small", "large"] },
    });
    let asked = json!({ "model": "clef-flash", "state": "We planned the small garden: tomatoes and basil.", "questions": questions });
    let calls = s.ai.calls().len();
    let r = run("dec1", "sort", asked.clone())?;
    let a = &r["output"]["answers"];
    s.ok(
        "a decision step answers Clef's answers, one per question of each type, charged its input tokens",
        r["status"] == "succeeded"
            && r["output"]["model"] == "@cf/cloudflare/clef-flash"
            && a["garden"] == json!({ "type": "noul", "noul": 0.9 })
            && a["taxes"]["noul"] == 0.1
            && a["room"]["choice"] == "garden"
            && a["size"]["score"] == 1.0
            && r["output"]["usage"]["input_tokens"].as_u64().is_some_and(|n| n > 0)
            && cost(&r) > 0,
        &r,
    );
    let call = s.ai.calls().get(calls).cloned();
    s.ok(
        "Clef is called with the catalog's input, on the model route's transport (the payer by an opaque id)",
        call.as_ref().is_some_and(|c| c.model == "@cf/cloudflare/clef-flash" && c.body == asked && c.metadata["user_id"].as_str().is_some_and(|u| u.len() == 16)),
        format!("{call:?}"),
    );
    let lever = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "fail-after-paid", "times": 1 })))?;
    let calls = s.ai.calls().len();
    let r = run("dec-kept", "sort", json!({ "model": "clef", "state": "a kept garden", "questions": { "garden": { "type": "noul", "instructions": "A garden?" } } }))?;
    s.ok(
        "a decision step that failed after its paid call answers from what it kept: one call",
        lever.status == 200 && r["status"] == "succeeded" && r["output"]["answers"]["garden"]["noul"] == 0.9 && s.ai.calls().len() == calls + 1,
        &r,
    );
    let none = run("dec-none", "sort", json!({ "model": "clef", "state": "s", "questions": {} }))?;
    let pro = run("dec-pro", "sort", json!({ "model": "clef-pro", "state": "s", "questions": { "q": { "type": "noul", "instructions": "?" } } }))?;
    let two = run("dec-two", "sort", json!({ "model": "clef", "state": "s", "questions": { "q": { "type": "choice", "instructions": "?", "criteria": { "only": null } } } }))?;
    let held = |r: &Value, says: &str| r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains(says)) && cost(r) == 0;
    s.ok(
        "a decision with no questions, on a model that is no Clef, or a choice of one option is refused, saying why, before any call",
        held(&none, "1 to 64 questions") && held(&pro, "unknown variant `clef-pro`") && held(&two, "2 to 255 options") && s.ai.calls().len() == calls + 1,
        json!([none, pro, two]),
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
