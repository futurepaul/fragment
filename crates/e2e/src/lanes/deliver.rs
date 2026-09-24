//! Deliveries and AI (slice F): web push from mutations and jobs through
//! the delivery queue to a push service that checks VAPID and decrypts as
//! a browser would; subscriptions that are gone, retries, the dead-letter
//! report; `notifyUrls`; and OpenRouter text, images, and video as a
//! job's steps, with generated media stored as files.

use std::time::Duration;

use anyhow::Result;
use fragment_fakes::openrouter::{image_bytes, video_bytes};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use crate::api::{Api, Call, Reply};
use crate::Suite;

const MEDIA_APP: &[u8] = include_bytes!("../../fixtures/media.mjs");
const MEDIA_JSON: &[u8] = include_bytes!("../../fixtures/media.json");

fn events(api: &Api, keys: &Keys, name: &str) -> String {
    api.signed(keys, "GET", &format!("/api/f/{name}/events?since=0"), None).map(|r| r.text).unwrap_or_default()
}

pub fn push(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("push") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.name("push");
    let c = s.create(api, &owner, &name)?;
    let mut manifest: Value = serde_json::from_slice(MEDIA_JSON)?;
    manifest["notifyUrls"] = json!([format!("{}/notify", s.push.url)]);
    ship(s, &c, MEDIA_APP, manifest.to_string().as_bytes());
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

    // a mutation pushes to everyone, once it commits, once
    let r = api.op(&owner, &name, "notify_all", "n1", json!({ "title": "hello" }))?;
    s.ok("a mutation's push is queued as it commits", r.status == 200, &r);
    let both = s.eventually(wait, || s.push.received("a").len() == 1 && s.push.received("b").len() == 1);
    s.ok(
        "both browsers get it, signed and decrypted",
        both && s.push.received("a")[0] == json!({ "title": "hello", "body": "from a mutation" }) && s.push.refused().is_empty(),
        format!("{:?} {:?} refused {:?}", s.push.received("a"), s.push.received("b"), s.push.refused()),
    );
    api.op(&owner, &name, "notify_all", "n1", json!({ "title": "hello" }))?;
    std::thread::sleep(Duration::from_secs(2));
    s.ok("a replay pushes nothing again", s.push.received("a").len() == 1, s.push.received("a").len());

    // a job pushes to one tag
    let r = api.op(&owner, &name, "announce", "j1", json!({ "who": "team", "title": "team only" }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok("a job's push step answers how many it queued", run["output"]["queued"] == 1, &run);
    s.ok("only that tag gets it", s.eventually(wait, || s.push.received("a").len() == 2) && s.push.received("b").len() == 1, "");

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
    let r = site("__push-unsub", Some(json!({ "endpoint": format!("{}/push/a", s.push.url) })))?;
    s.ok("a page unsubscribes by its endpoint", r.status == 200 && r.body["removed"] == 1, &r);

    // notifyUrls: a frame per move of main
    s.commit(&c, &[("notes/today.md", Some(b"# today"))]);
    let got = s.eventually(wait, || s.push.notified().iter().any(|f| f["paths"] == json!(["notes/today.md"])));
    let frame = s.push.notified().into_iter().find(|f| f["paths"] == json!(["notes/today.md"])).unwrap_or_default();
    s.ok("notifyUrls get a changed frame when main moves", got && frame["type"] == "changed" && frame["fragment"] == name.as_str(), &frame);

    // out of retries: the dead-letter queue reports to the fragment
    subscribe(s, "c", "doomed", 7)?;
    s.push.fail("c", 1000);
    api.op(&owner, &name, "announce", "j3", json!({ "who": "doomed", "title": "never" }))?;
    let failed = s.eventually(Duration::from_secs(120), || events(api, &owner, &name).contains("delivery.failed"));
    s.ok("a delivery out of retries is reported in the event log", failed, "");
    Ok(())
}

pub fn ai(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("ai") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.name("ai");
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, MEDIA_APP, MEDIA_JSON);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let wait = Duration::from_secs(40);
    let run = |id: &str, op: &str, input: Value| -> Result<Value> {
        let r = api.op(&owner, &name, op, id, input)?;
        Ok(settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait))
    };

    let r = run("t0", "summarize", json!({ "text": "hi" }))?;
    s.ok("without the fragment's OPENROUTER_API_KEY a call fails, saying so", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("OPENROUTER_API_KEY")), &r);
    api.call(Call {
        method: "PUT",
        url: format!("{}/api/f/{name}/secrets/OPENROUTER_API_KEY", api.base),
        body: Some(crate::OPENROUTER_KEY.as_bytes().to_vec()),
        keys: Some(&owner),
        ..Call::default()
    })?;
    let r = run("t1", "summarize", json!({ "text": "the meeting notes" }))?;
    s.ok("a job's text step answers the model's text", r["output"]["text"] == "echo: the meeting notes" && r["output"]["model"] == "openai/gpt-5-mini", &r);
    let calls = s.openrouter.calls();
    s.ok(
        "OpenRouter got the fragment's key, and the model",
        calls.iter().any(|c| c.1 == "/api/v1/chat/completions" && c.2 == "openai/gpt-5-mini" && c.3 == format!("Bearer {}", crate::OPENROUTER_KEY)),
        format!("{calls:?}"),
    );

    let r = run("i1", "draw", json!({ "prompt": "a lighthouse", "path": "art/lighthouse.png" }))?;
    s.ok(
        "an image step writes the image to main",
        r["output"]["path"] == "art/lighthouse.png" && s.fake.file_at(&repo, "main", "art/lighthouse.png") == Some(image_bytes("a lighthouse")),
        &r,
    );
    s.ok(
        "the job's reasoning option reaches OpenRouter as given",
        calls.iter().any(|c| c.1 == "/api/v1/chat/completions" && c.4 == r#"{"effort":"low"}"#),
        format!("{calls:?}"),
    );
    s.ok("with the image model the plan names", s.openrouter.calls().iter().any(|c| c.1 == "/api/v1/images" && c.2 == "google/gemini-3.1-flash-lite-image"), "");
    let big = image_bytes("a large mural");
    let r = run("i2", "draw", json!({ "prompt": "a large mural", "path": "art/mural.png" }))?;
    let pointer = fragment_core::blob::parse(&s.fake.file_at(&repo, "main", "art/mural.png").unwrap_or_default());
    s.ok(
        "a large image is a blob, its pointer in git",
        r["status"] == "succeeded" && pointer.as_ref().is_some_and(|p| p.sha256 == fragment_core::blob::sha256_hex(&big) && p.size == big.len() as u64),
        &r,
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file?path=art/mural.png"), None)?;
    s.ok("and its bytes are served", r.status == 200 && r.bytes == big, format!("{} {} bytes", r.status, r.bytes.len()));

    let r = run("v1", "film", json!({ "prompt": "waves", "path": "video/waves.mp4" }))?;
    let film = video_bytes(6);
    let pointer = fragment_core::blob::parse(&s.fake.file_at(&repo, "main", "video/waves.mp4").unwrap_or_default());
    s.ok(
        "a video step starts, polls, and saves the video as a blob",
        r["status"] == "succeeded" && pointer.as_ref().is_some_and(|p| p.sha256 == fragment_core::blob::sha256_hex(&film)),
        &r,
    );
    s.ok(
        "with the video model the plan names, asked as the job said",
        s.openrouter.calls().iter().any(|c| c.0 == "POST" && c.1 == "/api/v1/videos" && c.2 == "minimax/hailuo-3-max"),
        "",
    );

    s.openrouter.fail_next(&[503]);
    let r = run("t2", "summarize", json!({ "text": "again" }))?;
    s.ok("an OpenRouter 503 is retried", r["output"]["text"] == "echo: again", &r);
    s.openrouter.fail_next(&[402]);
    let r = run("t3", "summarize", json!({ "text": "no money" }))?;
    s.ok("out of credits holds the run, saying so", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("out of credits")), &r);
    let everything = format!("{}{}", events(api, &owner, &name), api.signed(&owner, "GET", &format!("/api/f/{name}/runs?limit=200"), None)?.text);
    s.ok("the key appears in no run or event", !everything.contains(crate::OPENROUTER_KEY), "the key leaked");
    Ok(())
}
