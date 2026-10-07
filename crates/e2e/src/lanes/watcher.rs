//! The watch template, as a person uses it: they add a page to watch on
//! its page, see its value, and get a push when it changes; a page behind
//! a key is checked with a secret the app never holds; a page that does
//! not answer is retried, then its run is held until a replay; the cron
//! trigger's sweep checks every watch. The pages are a local upstream.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use fragment_fakes::http::{Handler, Response, Server};
use serde_json::json;

use super::jobs::{settle, started};
use super::signin::site_cookie;
use super::templates::person;
use crate::api::{Api, Call};
use crate::Suite;

const SECRET: &str = "sk-e2e-shop-41c7";

/// The shop the watches follow: a product page whose price and status
/// the lane sets, and a price API behind a key.
struct Shop {
    server: Server,
    price: Arc<Mutex<String>>,
    status: Arc<Mutex<u16>>,
    keys: Arc<Mutex<Vec<String>>>,
}

impl Shop {
    fn start() -> Result<Shop> {
        let price = Arc::new(Mutex::new("$49.00".to_string()));
        let status = Arc::new(Mutex::new(200u16));
        let keys: Arc<Mutex<Vec<String>>> = Arc::default();
        let (p, st, k) = (Arc::clone(&price), Arc::clone(&status), Arc::clone(&keys));
        let handler: Handler = Arc::new(move |req| match req.path.as_str() {
            "/kettle" => {
                let status = *st.lock().expect("status");
                let page = format!(
                    "<!doctype html><html><head><title>Kettle</title><script>var price = 'no';</script></head><body><h1>Kettle</h1><p class=\"price\">  {} <small>incl. tax</small></p></body></html>",
                    p.lock().expect("price")
                );
                Response::bytes(status, "text/html; charset=utf-8", page.into_bytes())
            }
            "/moved" => Response::bytes(301, "text/html", vec![]).with_header("location", "/kettle"),
            "/api/price" => {
                let auth = req.header("authorization").unwrap_or("").to_string();
                k.lock().expect("keys").push(auth.clone());
                match auth == format!("Bearer {SECRET}") {
                    true => Response::json(200, &json!({ "data": { "price": 12.5, "currency": "EUR" } })),
                    false => Response::json(401, &json!({ "error": "a key, please" })),
                }
            }
            _ => Response::json(404, &json!({})),
        });
        Ok(Shop { server: Server::start(0, handler)?, price, status, keys })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.url)
    }
}

pub fn watcher(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("watcher", &[crate::Need::Fakes, crate::Need::Chrome]) {
        return Ok(());
    }
    let shop = Shop::start()?;
    let (owner, session) = person(api)?;
    let name = s.named(api, &owner, "watch")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "watch" }))?;
    anyhow::ensure!(made.status == 200, "watch from its template: {made}");
    let view = made.body["viewToken"].as_str().unwrap_or("").to_string();
    let wait = Duration::from_secs(30);
    let list = || api.op(&owner, &name, "list", "q", json!({})).map(|r| r.body["result"]["watches"].clone()).unwrap_or_default();
    let watch = |label: &str| list().as_array().and_then(|l| l.iter().find(|w| w["label"] == label).cloned()).unwrap_or_default();

    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/triggers"), None)?;
    let tick = &r.body["triggers"][0];
    s.ok(
        "its cron trigger sweeps every hour, on the hour",
        tick["cron"] == "0 * * * *" && tick["run"] == "sweep" && tick["nextAt"].as_i64().is_some_and(|at| at % 3_600_000 == 0),
        &r,
    );

    // someone who asked for pushes (a browser's subscription at the push fake)
    let sub = s.push.subscribe(&s.name("watch-changes"), 7);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}?view={view}", api.site_url(&name, "__push-sub")),
        body: Some(json!({ "who": "changes", "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth }).to_string().into_bytes()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    anyhow::ensure!(r.status == 200, "subscribing to its pushes: {r}");

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the watch template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&api.site_url(&name, ""), "fragment_site", &site_cookie(api, &session, &name)?)?;
    let page = chrome.open(&api.site_url(&name, ""))?;
    s.ok("its owner's page offers to watch a page", chrome.until(&page, "document.getElementById('add').hidden === false", wait), "");
    let add = format!(
        "document.getElementById('url').value = {:?}; document.getElementById('pick').value = '.price'; document.getElementById('label').value = 'Kettle'; document.getElementById('add').requestSubmit(); true",
        shop.url("/kettle")
    );
    chrome.eval(&page, &add)?;
    let card = |label: &str, has: &str| format!("[...document.querySelectorAll('.watch')].some((w) => w.querySelector('h3').textContent === {label:?} && w.textContent.includes({has:?}))");
    s.ok("a page added there is checked at once: the card shows the part picked", chrome.until(&page, &card("Kettle", "$49.00 incl. tax"), wait), watch("Kettle"));

    // someone with the link sees the same list, live, and cannot change it
    let other = chrome.another_context()?;
    let theirs = chrome.open_in(&other, &api.site_url(&name, &format!("?view={view}")))?;
    s.ok(
        "someone with the link sees the watch, and no form",
        chrome.until(&theirs, &card("Kettle", "$49.00"), wait) && chrome.eval(&theirs, "document.getElementById('add').hidden")? == true,
        "",
    );

    // the price drops: a check finds it, everyone sees it, and a push goes out
    *shop.price.lock().expect("price") = "$39.00".into();
    chrome.click(&page, ".watch .actions button")?;
    s.ok("a check now finds the new price", chrome.until(&page, &card("Kettle", "$39.00"), wait), watch("Kettle"));
    s.ok("the card says what it was", chrome.until(&page, &card("Kettle", "was $49.00 incl. tax"), wait), "");
    s.ok(
        "and the other page follows, its changes list too",
        chrome.until(&theirs, &format!("{} && document.getElementById('history').textContent.includes('$49.00 incl. tax → $39.00 incl. tax')", card("Kettle", "$39.00")), wait),
        "",
    );
    let pushed = s.eventually(wait, || !s.push.received(&s.name("watch-changes")).is_empty());
    let got = s.push.received(&s.name("watch-changes"));
    s.ok(
        "whoever asked gets a push saying what changed",
        pushed && got.len() == 1 && got[0]["title"] == "Kettle changed" && got[0]["body"] == "$49.00 incl. tax → $39.00 incl. tax" && got[0]["url"] == "./",
        json!(got),
    );
    let id = watch("Kettle")["id"].as_i64().unwrap_or(0);
    let r = api.op(&owner, &name, "check", "same", json!({ "id": id }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "a check that finds the same price changes nothing, and pushes nothing",
        run["status"] == "succeeded" && run["output"]["changed"] == false && s.push.received(&s.name("watch-changes")).len() == 1,
        &run,
    );

    // a price behind a key: the secret goes with each check, and nowhere else
    let r = api.call(Call { method: "PUT", url: format!("{}/api/f/{name}/secrets/SHOP_KEY", api.base), keys: Some(&owner), body: Some(SECRET.as_bytes().to_vec()), ..Call::default() })?;
    anyhow::ensure!(r.status == 200, "setting SHOP_KEY: {r}");
    let r = api.op(&owner, &name, "add", "api", json!({ "url": shop.url("/api/price"), "label": "Beans", "pick": "data.price", "header": "Authorization: Bearer {{SHOP_KEY}}" }))?;
    let beans = r.body["result"]["id"].as_i64().unwrap_or(0);
    let r = api.op(&owner, &name, "check", "api-1", json!({ "id": beans }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let sent = shop.keys.lock().expect("keys").clone();
    s.ok(
        "a JSON price behind a key: the secret named in its header reaches the shop, and the value at its path is kept",
        run["status"] == "succeeded" && watch("Beans")["value"] == "12.5" && sent == [format!("Bearer {SECRET}")],
        json!({ "run": run, "sent": sent }),
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/runs/{}", started(&r)), None)?;
    s.ok("the secret is in neither the run nor the page's list", !r.text.contains(SECRET) && !list().to_string().contains(SECRET) && watch("Beans")["header"] == true, &r);

    // what the page has not: said on the card, and the run succeeds
    let r = api.op(&owner, &name, "add", "nope", json!({ "url": shop.url("/kettle"), "label": "Nothing", "pick": ".sold-out" }))?;
    let r = api.op(&owner, &name, "check", "nope-1", json!({ "id": r.body["result"]["id"] }))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    s.ok(
        "a pick that matches nothing is said on its card; checking again would not help, so its run succeeds",
        run["status"] == "succeeded" && chrome.until(&page, &card("Nothing", "Nothing matched .sold-out"), wait),
        &run,
    );

    // the shop goes down: retried, then held; once it is back, a replay checks again
    *shop.status.lock().expect("status") = 503;
    let r = api.op(&owner, &name, "check", "down", json!({ "id": id }))?;
    let down = started(&r);
    let run = settle(api, &owner, &name, down, &["succeeded", "held"], Duration::from_secs(60));
    s.ok("a page that does not answer is retried, then its run is held, saying why", run["status"] == "held" && run["error"].as_str().is_some_and(|e| e.contains("503")), &run);
    s.ok("its card says it could not be reached, and which run is held", chrome.until(&page, &card("Kettle", &format!("run {down} held")), wait), "");
    *shop.status.lock().expect("status") = 200;
    *shop.price.lock().expect("price") = "$35.00".into();
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": down })))?;
    let run = settle(api, &owner, &name, down, &["succeeded", "held"], wait);
    s.ok(
        "replayed once it is back, the run checks again: the card shows the price, and the problem is gone",
        r.status == 200 && run["status"] == "succeeded" && run["output"]["changed"] == true && chrome.until(&page, &format!("{} && !{}", card("Kettle", "$35.00"), card("Kettle", "Couldn't reach it")), wait),
        &run,
    );

    // the cron trigger's sweep (called here, as the tick would): every watch checked
    let r = api.op(&owner, &name, "add", "moved", json!({ "url": shop.url("/moved"), "label": "Moved", "pick": ".price" }))?;
    let moved = r.body["result"]["id"].as_i64().unwrap_or(0);
    let before = watch("Kettle")["checked_at"].as_i64().unwrap_or(0);
    let r = api.op(&owner, &name, "sweep", "sweep-1", json!({}))?;
    let run = settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let checked = s.eventually(wait, || watch("Kettle")["checked_at"].as_i64().unwrap_or(0) > before && watch("Moved")["value"].is_string());
    s.ok("the sweep starts a check of every watch", run["status"] == "succeeded" && run["output"]["started"] == 4 && checked, json!({ "run": run, "list": list() }));
    s.ok("a page that redirects is followed to where it went", watch("Moved")["value"] == "$35.00 incl. tax", list());

    // as a person sees it, on a phone and on a laptop
    let shots = s.dir("watcher");
    for (file, w, h, mobile, scheme) in [("phone.png", 390, 844, true, "light"), ("laptop.png", 1280, 800, false, "light"), ("laptop-dark.png", 1280, 800, false, "dark")] {
        chrome.viewport(&page, w, h, mobile)?;
        chrome.color_scheme(&page, scheme)?;
        std::thread::sleep(Duration::from_millis(300));
        let _ = chrome.screenshot(&page, &shots.join(file));
    }
    println!("      (screenshots in {})", shots.display());
    let removed = api.op(&owner, &name, "remove", "rm", json!({ "id": moved }))?;
    s.ok("a watch removed is gone from every page", removed.status == 200 && chrome.until(&theirs, "![...document.querySelectorAll('.watch h3')].some((h) => h.textContent === 'Moved')", wait), &removed);
    let refused = api.op(&owner, &name, "add", "bad", json!({ "url": "ftp://shop.example/kettle" }))?;
    s.ok("a page that is not http(s) is refused, saying so", refused.status == 422 && refused.message().contains("http(s)"), &refused);
    Ok(())
}
