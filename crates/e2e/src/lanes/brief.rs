//! The brief template, as a person uses it: they add their feeds on its
//! page (a site's page that names its feed is swapped for it), brief
//! themselves now, and read the model's summary above the new items, live,
//! with a push; a feed that does not answer is named in the brief; the
//! hourly cron's tick makes a brief only at the brief's hour. The feeds are
//! a local upstream, the model the Workers AI fake.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fragment_core::cron::civil;
use fragment_fakes::http::{Handler, Response, Server};
use serde_json::json;

use super::jobs::{settle, started};
use super::signin::site_cookie;
use super::templates::person;
use crate::api::{now_ms, Api, Call};
use crate::Suite;

const SUMMARY: &str = "- Kettles are cheaper this week (E2E Daily)\n- **Rain** all Tuesday, then sun (A Blog)";
const HOUR_MS: i64 = 3_600_000;

/// `ms` as RSS dates it (RFC 2822, in GMT).
fn rfc2822(ms: i64) -> String {
    let (days, secs) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000) / 1000);
    let (y, m, d) = civil(days);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][m as usize - 1];
    format!("{weekday}, {d:02} {month} {y} {:02}:{:02}:{:02} GMT", secs / 3600, secs / 60 % 60, secs % 60)
}

/// `ms` as Atom dates it (RFC 3339, in UTC).
fn rfc3339(ms: i64) -> String {
    let (days, secs) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000) / 1000);
    let (y, m, d) = civil(days);
    format!("{y}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

/// The feeds: an RSS feed (two items from today, one from two days ago),
/// a blog whose page names its Atom feed, and a feed that is down.
fn feeds() -> Result<Server> {
    let now = now_ms();
    let rss = format!(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>E2E Daily</title><link>https://daily.example/</link>
<item><title>Kettles are cheaper this week</title><link>https://daily.example/kettles?a=1&amp;b=2</link><pubDate>{}</pubDate><description><![CDATA[<p>Prices <b>fell</b> again.</p>]]></description></item>
<item><title>Tea &amp; biscuits</title><link>javascript:alert(1)</link><pubDate>{}</pubDate><description>&lt;p&gt;A study.&lt;/p&gt;</description></item>
<item><title>Old news</title><link>https://daily.example/old</link><pubDate>{}</pubDate></item>
</channel></rss>"#,
        rfc2822(now - HOUR_MS),
        rfc2822(now - 2 * HOUR_MS),
        rfc2822(now - 48 * HOUR_MS)
    );
    let atom = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><feed xmlns="http://www.w3.org/2005/Atom"><title>A Blog</title><link rel="self" href="/blog.atom"/>
<entry><title>Rain all Tuesday</title><link rel="alternate" href="/posts/rain"/><updated>{}</updated><summary>Then sun.</summary></entry>
<entry><title>Notes on gardens</title><link href="https://blog.example/gardens"/><published>{}</published></entry>
</feed>"#,
        rfc3339(now - 3 * HOUR_MS),
        rfc3339(now - 4 * HOUR_MS)
    );
    let handler: Handler = Arc::new(move |req| match req.path.as_str() {
        "/daily.rss" => Response::bytes(200, "application/rss+xml", rss.clone().into_bytes()),
        "/blog" => Response::bytes(200, "text/html", br#"<html><head><title>A Blog</title><link rel="alternate" type="application/atom+xml" href="/blog.atom"></head><body>Hello</body></html>"#.to_vec()),
        "/blog.atom" => Response::bytes(200, "application/atom+xml", atom.clone().into_bytes()),
        "/gone" => Response::json(503, &json!({ "error": "down" })),
        _ => Response::json(404, &json!({})),
    });
    Ok(Server::start(0, handler)?)
}

pub fn brief(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("brief", &[crate::Need::Fakes, crate::Need::Chrome]) {
        return Ok(());
    }
    let upstream = feeds()?;
    let url = |path: &str| format!("{}{path}", upstream.url);
    let (owner, session) = person(api)?;
    let name = s.named(api, &owner, "brief")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "brief" }))?;
    anyhow::ensure!(made.status == 200, "brief from its template: {made}");
    let view = made.body["viewToken"].as_str().unwrap_or("").to_string();
    let wait = Duration::from_secs(40);
    let state = || api.op(&owner, &name, "state", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    let briefs = || api.signed(&owner, "GET", &format!("/api/f/{name}/channels/briefs"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();

    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/triggers"), None)?;
    s.ok("its cron trigger ticks every hour", r.body["triggers"][0]["cron"] == "0 * * * *" && r.body["triggers"][0]["run"] == "brief", &r);
    let sub = s.push.subscribe(&s.name("briefs"), 11);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}?view={view}", api.site_url(&name, "__push-sub")),
        body: Some(json!({ "who": "briefs", "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth }).to_string().into_bytes()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    anyhow::ensure!(r.status == 200, "subscribing to its pushes: {r}");

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the brief template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&api.site_url(&name, ""), "fragment_site", &site_cookie(api, &session, &name)?)?;
    let page = chrome.open(&api.site_url(&name, ""))?;
    let tz = chrome.eval(&page, "Intl.DateTimeFormat().resolvedOptions().timeZone")?;
    s.ok(
        "its owner's page sets the brief's time zone to theirs, at 7",
        s.eventually(wait, || state()["tz"] == tz) && state()["hour"] == 7 && chrome.until(&page, "document.getElementById('when').textContent.includes('7:00')", wait),
        json!({ "state": state(), "browser": tz }),
    );

    // feeds added on the page are read once for their titles
    chrome.eval(&page, &format!("document.getElementById('url').value = {:?}; document.getElementById('add').requestSubmit(); true", url("/daily.rss")))?;
    s.ok("a feed added there is read for its title", chrome.until(&page, "document.getElementById('feeds').textContent.includes('E2E Daily')", wait), state());
    let r = api.op(&owner, &name, "add_feed", "blog", json!({ "url": url("/blog") }))?;
    let r = api.op(&owner, &name, "probe", "blog-1", json!({ "id": r.body["result"]["id"] }))?;
    settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait);
    let blog = state()["feeds"].as_array().and_then(|f| f.iter().find(|f| f["title"] == "A Blog").cloned()).unwrap_or_default();
    s.ok("a site's page that names its feed is swapped for that feed", blog["url"] == url("/blog.atom").as_str(), state());
    api.op(&owner, &name, "add_feed", "gone", json!({ "url": url("/gone") }))?;
    let r = api.op(&owner, &name, "add_feed", "again", json!({ "url": url("/daily.rss") }))?;
    s.ok("a feed already there is refused, saying so", r.status == 422 && r.message().contains("already"), &r);

    // brief me now: the new items, summed up by a text step, live and pushed
    s.ai.say_next(&[SUMMARY]);
    let calls = s.ai.chats().len();
    chrome.click(&page, "#now")?;
    let written = chrome.until(&page, "document.getElementById('brief').textContent.includes('Kettles are cheaper this week (E2E Daily)')", Duration::from_secs(90));
    let shown = chrome.eval(&page, "[...document.querySelectorAll('#items li a, #items li span')].map((a) => a.textContent + ' ' + (a.href || ''))")?;
    s.ok(
        "brief me now: the model's summary leads the page, then the new items, newest first, linked when they link to the web",
        written
            && shown == json!([
                "Kettles are cheaper this week https://daily.example/kettles?a=1&b=2",
                "Tea & biscuits ",
                "Rain all Tuesday http://127.0.0.1:{port}/posts/rain".replace("{port}", &upstream.port.to_string()),
                "Notes on gardens https://blog.example/gardens",
            ]),
        &shown,
    );
    s.ok("its points are points, its bold is bold", chrome.eval(&page, "document.querySelectorAll('#brief ul li').length === 2 && document.querySelector('#brief strong')?.textContent === 'Rain'")? == true, "");
    s.ok("a feed that does not answer is named, and the brief comes anyway", chrome.eval(&page, "document.querySelector('.failed')?.textContent.includes('503')")? == true, chrome.eval(&page, "document.getElementById('brief').textContent")?);
    let asked = s.ai.chats().get(calls).cloned().unwrap_or_default();
    let prompt = asked["messages"][1]["content"].as_str().unwrap_or("");
    s.ok(
        "the model is asked about what is new since yesterday, and nothing older",
        s.ai.chats().len() == calls + 1 && prompt.contains("E2E Daily: Kettles are cheaper this week. Prices fell again.") && prompt.contains("A Blog: Rain all Tuesday. Then sun.") && !prompt.contains("Old news"),
        prompt,
    );
    let pushed = s.eventually(wait, || !s.push.received(&s.name("briefs")).is_empty());
    let got = s.push.received(&s.name("briefs"));
    s.ok(
        "whoever asked gets a push when it is ready",
        pushed && got.len() == 1 && got[0]["title"].as_str().is_some_and(|t| t.starts_with("Your brief for ")) && got[0]["body"].as_str().is_some_and(|b| b.starts_with("Kettles are cheaper")),
        json!(got),
    );
    let other = chrome.another_context()?;
    let theirs = chrome.open_in(&other, &api.site_url(&name, &format!("?view={view}")))?;
    s.ok(
        "someone with the link reads the same brief, and cannot brief anyone",
        chrome.until(&theirs, "document.getElementById('brief').textContent.includes('Kettles are cheaper')", wait) && chrome.eval(&theirs, "document.getElementById('now').hidden && document.getElementById('settings').hidden")? == true,
        "",
    );

    // as a person sees it, on a phone and on a laptop
    let shots = s.dir("brief");
    for (file, w, h, mobile, scheme) in [("phone.png", 390, 844, true, "light"), ("laptop.png", 1280, 800, false, "light"), ("laptop-dark.png", 1280, 800, false, "dark")] {
        chrome.viewport(&page, w, h, mobile)?;
        chrome.color_scheme(&page, scheme)?;
        std::thread::sleep(Duration::from_millis(300));
        let _ = chrome.screenshot(&page, &shots.join(file));
    }
    println!("      (screenshots in {})", shots.display());

    // the cron's tick: a brief only at the brief's hour, and with nothing
    // new, no model call and no push
    let next = (now_ms() / HOUR_MS + 1) * HOUR_MS;
    let hour = (next / HOUR_MS).rem_euclid(24);
    api.op(&owner, &name, "set_time", "utc", json!({ "hour": hour, "tz": "UTC" }))?;
    let tick = |at: i64, id: &str| -> Result<serde_json::Value> {
        let r = api.op(&owner, &name, "brief", id, json!({ "cron": "0 * * * *", "at": at }))?;
        Ok(settle(api, &owner, &name, started(&r), &["succeeded", "held"], wait))
    };
    let before = briefs().len();
    let off = tick(next + HOUR_MS, "tick-off")?;
    s.ok("a tick at another hour makes nothing", off["output"]["due"] == false && briefs().len() == before, &off);
    let calls = s.ai.chats().len();
    let on = tick(next, "tick-on")?;
    let last = briefs().last().cloned().unwrap_or_default();
    s.ok(
        "the tick at the brief's hour makes one; nothing new since the last, so no model call and no push",
        on["status"] == "succeeded" && briefs().len() == before + 1 && last["body"]["text"] == "Nothing new in your feeds since the last brief." && s.ai.chats().len() == calls && s.push.received(&s.name("briefs")).len() == 1,
        json!({ "run": on, "last": last }),
    );
    s.ok(
        "the page lists both, and an earlier one opens",
        chrome.until(&page, "document.querySelectorAll('#archive button').length === 2", wait)
            && chrome.eval(&page, "document.querySelectorAll('#archive button')[1].click(); document.getElementById('brief').textContent.includes('Kettles are cheaper')")? == true,
        "",
    );
    let r = api.op(&owner, &name, "set_time", "nowhere", json!({ "hour": 7, "tz": "Mars/Olympus" }))?;
    s.ok("a time zone that is none is refused, saying so", r.status == 422 && r.message().contains("not a time zone"), &r);
    Ok(())
}
