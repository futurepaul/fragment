//! The wall template: a page anyone with the link posts to. Its owner
//! names it and posts a photo; visitors with no account post notes, which
//! land live on every open page; a post an editor hides leaves them all;
//! and a wall that takes posts from people signed in refuses a visitor's,
//! its page offering them the sign-in.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use serde_json::{json, Value};

use super::templates::person;
use crate::api::Api;
use crate::Suite;

/// A photo as a phone's camera makes one: a JPEG, drawn by Chrome itself.
const PHOTO: &str = "(() => { const c = document.createElement('canvas'); c.width = 1600; c.height = 1200; const g = c.getContext('2d');
  g.fillStyle = '#e9d8c4'; g.fillRect(0, 0, 1600, 1200);
  [['#c8553d', 380, 420], ['#2d5b8a', 760, 330], ['#e0a03a', 1120, 460], ['#5b8a5f', 560, 700], ['#8a4b7a', 1000, 760]].forEach(([fill, x, y]) => {
    g.strokeStyle = '#6b5d4f'; g.lineWidth = 4; g.beginPath(); g.moveTo(x, y + 150); g.lineTo(x + 40, 1200); g.stroke();
    g.fillStyle = fill; g.beginPath(); g.ellipse(x, y, 120, 150, 0, 0, 7); g.fill(); });
  return c.toDataURL('image/jpeg', 0.9).split(',')[1]; })()";

pub fn wall(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("wall", &[crate::Need::Chrome]) {
        return Ok(());
    }
    let (owner, session) = person(api)?;
    let username = api.username(&owner)?;
    let name = s.named(api, &owner, "wall")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "wall" }))?;
    anyhow::ensure!(made.status == 200, "a wall from its template: {made}");
    let link = api.site_url(&name, &format!("?view={}", made.body["viewToken"].as_str().unwrap_or("")));
    let posts = || api.signed(&owner, "GET", &format!("/api/f/{name}/channels/posts?after=0"), None).map(|r| r.body["records"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the wall template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(20);

    // its owner, signed in, names the wall and posts a photo
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let host = chrome.open(&api.site_url(&name, "__signin?return=/"))?;
    s.ok("its owner opens the new wall, asked to give it a title", chrome.until(&host, "document.querySelector('#intro .link')?.textContent === 'Give the wall a title'", wait), "");
    chrome.click(&host, "#intro .link")?;
    chrome.eval(&host, "(() => { document.getElementById('atitle').value = \"Sam's 30th\"; document.getElementById('aintro').value = 'Leave a note for Sam.'; document.getElementById('about').requestSubmit(); return true; })()")?;
    s.ok("and names it", chrome.until(&host, "document.getElementById('title').textContent === \"Sam's 30th\" && document.getElementById('about').hidden", wait), "");
    let photo = s.dir("wall").join("balloons.jpg");
    let jpeg = chrome.eval(&host, PHOTO)?;
    std::fs::write(&photo, base64::engine::general_purpose::STANDARD.decode(jpeg.as_str().unwrap_or_default())?)?;
    chrome.choose_files(&host, "#file", &[&photo])?;
    anyhow::ensure!(chrome.until(&host, "!document.getElementById('preview').hidden", wait), "the picked photo shows before it is posted");
    chrome.eval(&host, "document.getElementById('text').value = 'Balloons are up!'; document.getElementById('compose').requestSubmit(); true")?;
    let photo_shown = "[...document.querySelectorAll('#wall .post img:not(.face)')].some((i) => i.complete && i.naturalWidth === 1600)";
    s.ok(
        "a photo it posts lands on the wall, shrunk to 1600 px, under its username",
        chrome.until(&host, &format!("{photo_shown} && document.querySelector('#wall .post strong')?.textContent === '{username}'"), wait),
        chrome.eval(&host, "document.getElementById('wall').innerText")?,
    );

    // two visitors with the link and no account
    let (a, b) = (chrome.another_context()?, chrome.another_context()?);
    let ana = chrome.open_in(&a, &link)?;
    let ben = chrome.open_in(&b, &link)?;
    for page in [&ana, &ben] {
        anyhow::ensure!(chrome.until(page, &format!("document.getElementById('title').textContent === \"Sam's 30th\" && {photo_shown}"), wait), "a visitor sees the wall and its photo");
    }
    s.ok("a visitor with the link sees the wall, its photo, and who posted it", chrome.eval(&ana, "document.querySelector('#wall .post strong').textContent")? == username.as_str(), "");
    s.ok("and is offered no photo of their own (only editors upload)", chrome.eval(&ana, "document.getElementById('photo').hidden")? == true, "");
    let note = |who: &str, text: &str| format!("(() => {{ document.getElementById('name').value = '{who}'; document.getElementById('text').value = '{text}'; document.getElementById('compose').requestSubmit(); return true; }})()");
    chrome.eval(&ana, &note("Ana", "See you at eight"))?;
    let landed = "[...document.querySelectorAll('#wall .post:not([hidden])')].some((p) => p.querySelector('p')?.textContent === 'See you at eight' && p.querySelector('strong').textContent === 'Ana')";
    s.ok("a visitor's note lands live on another visitor's page, by the name they gave", chrome.until(&ben, landed, wait), chrome.eval(&ben, "document.getElementById('wall').innerText")?);
    s.ok("and on the owner's", chrome.until(&host, landed, wait), "");
    let list = posts();
    let ana_post = list.iter().find(|r| r["body"]["text"] == "See you at eight").cloned().unwrap_or_default();
    s.ok(
        "the posts are records on the channel, each naming its poster: the owner, and the visitor's anonymous principal",
        list.len() == 2
            && list[0]["principal"] == api.identity(&owner)?.as_str()
            && list[0]["body"]["attachments"][0]["sha256"].as_str().is_some_and(|h| h.len() == 64)
            && ana_post["principal"].as_str().is_some_and(|p| p.starts_with("anon:")),
        Value::Array(list.clone()),
    );
    let upload = "fetch('./__blob/' + '0'.repeat(64), { method: 'PUT', body: 'x', headers: { 'content-type': 'image/jpeg' } }).then((r) => r.status)";
    let r = chrome.eval(&ana, upload)?;
    s.ok("a visitor's upload is refused (401)", r == 401, &r);

    // an editor takes a post down: off every page
    let seq = ana_post["seq"].as_i64().unwrap_or(0);
    chrome.click(&host, &format!("#wall .post[data-seq='{seq}'] .hide"))?;
    let gone = format!("document.querySelector(\"#wall .post[data-seq='{seq}']\")?.hidden === true");
    s.ok("its owner hides the visitor's note, and it leaves every page", chrome.until(&ana, &gone, wait) && chrome.until(&ben, &gone, wait), chrome.eval(&ben, "document.getElementById('wall').innerText")?);
    let w = api.op(&owner, &name, "wall", "q", json!({}))?;
    s.ok("the wall lists it hidden, and the channel still holds it", w.body["result"]["hidden"] == json!([seq]) && posts().len() == 2, &w);

    // a wall that takes posts from people signed in
    let mut manifest = api.signed(&owner, "GET", &format!("/api/f/{name}/manifest"), None)?.body;
    manifest["channels"]["posts"]["signedIn"] = json!(true);
    let w = api.signed(&owner, "POST", &format!("/api/f/{name}/files"), Some(&json!({ "files": [{ "path": "fragment.json", "text": manifest.to_string() }] })))?;
    let d = api.signed(&owner, "POST", &format!("/api/f/{name}/deploy"), Some(&json!({})))?;
    anyhow::ensure!(w.status == 200 && d.status == 200, "deploying signedIn: {w} / {d}");
    chrome.eval(&ben, &note("Ben", "Can I still post?"))?;
    s.ok(
        "a wall that says signedIn refuses a visitor's post, and its page offers the sign-in",
        chrome.until(&ben, "document.getElementById('note').textContent.includes('people signed in') && !!document.querySelector('#note a[href*=__signin]')", wait) && posts().len() == 2,
        chrome.eval(&ben, "document.getElementById('note').textContent")?,
    );
    Ok(())
}
