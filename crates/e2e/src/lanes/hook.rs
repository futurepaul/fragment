//! The hook template, as a person uses it: GitHub's and Stripe's
//! webhooks (and anything else's) post to its inbox, each delivery's
//! trigger keeps it as an event, and the board follows live in every
//! open page: a run's later status updates its event, a redelivery
//! changes nothing, and a push says when something starts failing and
//! when it recovers. No delivery is held for its shape.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use super::jobs::inbox;
use super::signin::site_cookie;
use super::templates::person;
use crate::api::{Api, Call};
use crate::Suite;

/// A GitHub Actions run's webhook (`workflow_run`), as GitHub sends it, trimmed.
fn run(id: u64, status: &str, conclusion: Option<&str>) -> Value {
    json!({
        "action": if conclusion.is_some() { "completed" } else { "requested" },
        "workflow_run": {
            "id": id, "name": "CI", "status": status, "conclusion": conclusion, "run_attempt": 1, "head_branch": "main",
            "head_commit": { "message": "Make it faster\n\nAnd smaller." },
            "html_url": format!("https://github.com/acme/app/actions/runs/{id}"),
        },
        "repository": { "full_name": "acme/app" },
    })
}

/// A Stripe event's webhook, trimmed.
fn stripe(id: &str, kind: &str, amount: u64, extra: Value) -> Value {
    let mut object = json!({ "amount": amount, "currency": "usd" });
    object.as_object_mut().expect("an object").extend(extra.as_object().cloned().unwrap_or_default());
    json!({ "id": id, "object": "event", "type": kind, "data": { "object": object } })
}

pub fn hook(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("hook", &[crate::Need::Fakes, crate::Need::Chrome]) {
        return Ok(());
    }
    let (owner, session) = person(api)?;
    let name = s.named(api, &owner, "hook")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "hook" }))?;
    anyhow::ensure!(made.status == 200, "hook from its template: {made}");
    let (view, token) = (made.body["viewToken"].as_str().unwrap_or("").to_string(), made.body["inboxToken"].as_str().unwrap_or("").to_string());
    let wait = Duration::from_secs(30);
    let board = || api.op(&owner, &name, "board", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    let tile = |n: &str| board()["tiles"].as_array().and_then(|t| t.iter().find(|t| t["name"] == n).cloned()).unwrap_or_default();
    let deliver = |body: Value| -> Result<()> {
        let r = inbox(api, &name, &token, &body, None)?;
        anyhow::ensure!(r.status == 200, "a delivery: {r}");
        Ok(())
    };
    let alerts = s.name("hook-alerts");
    let sub = s.push.subscribe(&alerts, 13);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}?view={view}", api.site_url(&name, "__push-sub")),
        body: Some(json!({ "who": "alerts", "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth }).to_string().into_bytes()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    anyhow::ensure!(r.status == 200, "subscribing to its pushes: {r}");

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the hook template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&api.site_url(&name, ""), "fragment_site", &site_cookie(api, &session, &name)?)?;
    let page = chrome.open(&api.site_url(&name, ""))?;
    s.ok(
        "its owner's empty board says how to send it webhooks",
        chrome.until(&page, "document.getElementById('summary').textContent === 'waiting for the first webhook' && document.getElementById('setup').open && !document.getElementById('setup').hidden", wait),
        "",
    );
    let other = chrome.another_context()?;
    let theirs = chrome.open_in(&other, &api.site_url(&name, &format!("?view={view}")))?;
    let card = |n: &str, has: &str| format!("[...document.querySelectorAll('.tile')].some((t) => t.querySelector('.name').textContent === {n:?} && t.textContent.includes({has:?}))");

    // a CI run: requested, then completed, then GitHub's redelivery of it
    deliver(run(101, "queued", None))?;
    s.ok("a GitHub run that starts shows as running, on every open page", chrome.until(&theirs, &card("CI", "running · queued"), wait), tile("CI"));
    deliver(run(101, "completed", Some("success")))?;
    s.ok("its completion updates the same event: passing", chrome.until(&page, &card("CI", "passing"), wait) && chrome.until(&page, &card("CI", "main: Make it faster"), wait), board());
    deliver(run(101, "completed", Some("success")))?;
    let one = s.eventually(wait, || board()["recent"].as_array().map(|r| r.len()) == Some(1) && api.signed(&owner, "GET", &format!("/api/f/{name}/runs?status=succeeded&op=heard"), None).is_ok_and(|r| r.body["runs"].as_array().map(Vec::len) == Some(3)));
    s.ok("a redelivery of it changes nothing: one event", one, board());

    // a failure: the board says so first, and a push goes out; then it recovers
    deliver(run(102, "completed", Some("failure")))?;
    s.ok(
        "a failed run turns its tile red, first on the board, and the board says how many fail",
        chrome.until(&theirs, &format!("{} && document.getElementById('summary').textContent === '1 failing'", card("CI", "failing")), wait),
        board(),
    );
    let pushed = s.eventually(wait, || !s.push.received(&alerts).is_empty());
    s.ok(
        "whoever asked gets a push when it starts failing",
        pushed && s.push.received(&alerts) == [json!({ "title": "CI failed", "body": "acme/app: main: Make it faster", "tag": "acme/app/CI", "url": "./" })],
        json!(s.push.received(&alerts)),
    );
    deliver(run(103, "completed", Some("success")))?;
    s.ok("and a push when it recovers", s.eventually(wait, || s.push.received(&alerts).len() == 2) && s.push.received(&alerts)[1]["title"] == "CI is passing again", json!(s.push.received(&alerts)));
    s.ok("the board is all passing again", chrome.until(&page, "document.getElementById('summary').textContent === 'all passing'", wait), "");

    // payments, a deploy in the plain shape, a line of text, and junk
    deliver(stripe("evt_1", "payment_intent.succeeded", 2500, json!({ "description": "Pro plan" })))?;
    s.ok("a Stripe payment shows its amount", chrome.until(&page, &card("payment intent", "25.00 USD · Pro plan"), wait), tile("payment intent"));
    deliver(stripe("evt_2", "payment_intent.payment_failed", 900, json!({ "receipt_email": "ann@example.com" })))?;
    s.ok("a failed payment is a failure, and pushed", s.eventually(wait, || s.push.received(&alerts).len() == 3) && s.push.received(&alerts)[2]["title"] == "payment intent failed", json!(s.push.received(&alerts)));
    deliver(json!({ "source": "deploys", "payload": { "name": "web", "status": "ok", "detail": "v1.2.3", "id": "d1", "url": "https://web.example/" } }))?;
    s.ok("anything else's {name, status, detail} is a tile of its own", chrome.until(&page, &card("web", "passing"), wait), tile("web"));
    let r = api.call(Call { method: "POST", url: format!("{}/api/f/{name}/inbox?t={token}", api.base), body: Some(b"the backup finished".to_vec()), content_type: Some("text/plain"), ..Call::default() })?;
    anyhow::ensure!(r.status == 200, "a text delivery: {r}");
    deliver(json!([1, 2, 3]))?;
    s.ok(
        "a line of text, and a body of no known shape, are kept as messages",
        chrome.until(&page, "document.getElementById('recent').textContent.includes('the backup finished') && document.getElementById('recent').textContent.includes('[1,2,3]')", wait) && s.eventually(wait, || board()["recent"].as_array().map(Vec::len) == Some(8)),
        board(),
    );
    let held = api.signed(&owner, "GET", &format!("/api/f/{name}/runs?status=held"), None)?;
    s.ok("no delivery is held for its shape", held.body["runs"] == json!([]), &held);
    s.ok(
        "the last 14 days' chart counts today's: passed, failed, and the rest",
        board()["days"].as_array().is_some_and(|d| d.len() == 1 && d[0]["ok"] == 4 && d[0]["failed"] == 2 && d[0]["n"] == 8)
            && chrome.eval(&page, "document.querySelectorAll('#chart rect').length === 4")? == true,
        board(),
    );

    // as a person sees it, on a phone and on a laptop
    let shots = s.dir("hook");
    for (file, w, h, mobile, scheme) in [("phone.png", 390, 844, true, "light"), ("laptop.png", 1280, 800, false, "light"), ("laptop-dark.png", 1280, 800, false, "dark")] {
        chrome.viewport(&page, w, h, mobile)?;
        chrome.color_scheme(&page, scheme)?;
        std::thread::sleep(Duration::from_millis(300));
        let _ = chrome.screenshot(&page, &shots.join(file));
    }
    println!("      (screenshots in {})", shots.display());

    // an editor forgets a tile; someone with the link cannot
    s.ok("someone with the link has no forget", chrome.eval(&theirs, "document.querySelectorAll('.tile .link').length === 0 && document.getElementById('setup').hidden")? == true, "");
    chrome.eval(&page, "[...document.querySelectorAll('.tile')].find((t) => t.querySelector('.name').textContent === 'message').querySelector('.link').click(); true")?;
    s.ok("a tile forgotten is gone from every page", chrome.until(&theirs, &format!("!{}", card("message", "")), wait), board());
    Ok(())
}
