//! The board template: a family's chores, live. Its owner adds a card and
//! gives it to a member, whose open page shows it as theirs and whose
//! browser gets a push (the push fake, subscribed as the page's "Notify
//! me" would); the member starts it and the owner's page follows. A
//! visitor with the link reads the board and changes nothing.

use std::time::Duration;

use anyhow::Result;
use fragment_core::npub;
use serde_json::{json, Value};

use super::signin::site_cookie;
use super::templates::person;
use crate::api::{Api, Call};
use crate::Suite;

pub fn board(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("board", &[crate::Need::Chrome, crate::Need::Fakes]) {
        return Ok(());
    }
    let (owner, owner_session) = person(api)?;
    let (ben, ben_session) = person(api)?;
    let (ben_id, ben_name) = (api.identity(&ben)?, api.username(&ben)?);
    let name = s.named(api, &owner, "board")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "board" }))?;
    anyhow::ensure!(made.status == 200, "a board from its template: {made}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", npub::encode(ben.pubkey_hex())), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "Ben joins the board: {r}");
    let link = api.site_url(&name, &format!("?view={}", made.body["viewToken"].as_str().unwrap_or("")));
    let board = || api.op(&owner, &name, "board", "q", json!({})).map(|r| r.body["result"]["cards"].clone()).unwrap_or_default();

    // Ben's phone takes pushes as Ben: what "Notify me" stores (headless
    // Chrome has no push service), tagged with his identity, from his session
    let sub = s.push.subscribe("board-ben", 7);
    let r = api.call(Call {
        method: "POST",
        url: api.site_url(&name, "__push-sub"),
        body: Some(json!({ "who": ben_id, "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth }).to_string().into_bytes()),
        content_type: Some("application/json"),
        cookie: Some(format!("fragment_site={}", site_cookie(api, &ben_session, &name)?)),
        ..Call::default()
    })?;
    anyhow::ensure!(r.status == 200, "Ben's browser subscribes to his pushes: {r}");

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the board template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(20);
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &owner_session)?;
    let host = chrome.open(&api.site_url(&name, "__signin?return=/"))?;
    let b = chrome.another_context()?;
    chrome.set_cookie_in(&b, &format!("{}/", api.base), "fragment_session", &ben_session)?;
    let benp = chrome.open_in(&b, &api.site_url(&name, "__signin?return=/"))?;
    for page in [&host, &benp] {
        anyhow::ensure!(chrome.until(page, "!document.getElementById('add').hidden && !!document.querySelector('section[data-col=todo] .empty')", wait), "a member's page opens the empty board");
    }
    s.ok("the owner's page shows who is here: the two of them", chrome.until(&host, "document.querySelectorAll('#here .face').length === 2", wait), chrome.eval(&host, "document.getElementById('here').outerHTML")?);

    // the owner adds a card and gives it to Ben
    let add = |title: &str| format!("(() => {{ document.getElementById('title').value = '{title}'; document.getElementById('add').requestSubmit(); return true; }})()");
    chrome.eval(&host, &add("Take out the bins"))?;
    let todo = |title: &str| format!("[...document.querySelectorAll('section[data-col=todo] .card p')].some((p) => p.textContent === '{title}')");
    s.ok("a card the owner adds shows live on Ben's page", chrome.until(&benp, &todo("Take out the bins"), wait), chrome.eval(&benp, "document.querySelector('.columns').innerText")?);
    let id = board()[0]["id"].as_i64().unwrap_or(0);
    let pick = |id: i64, who: &str| format!("(() => {{ const s = document.querySelector(\".card[data-id='{id}'] select\"); s.value = '{who}'; s.dispatchEvent(new Event('change')); return true; }})()");
    anyhow::ensure!(chrome.until(&host, &format!("[...document.querySelectorAll(\".card[data-id='{id}'] option\")].some((o) => o.value === '{ben_id}')"), wait), "the owner's page offers the card to Ben, a member");
    chrome.eval(&host, &pick(id, &ben_id))?;
    let yours = format!("document.querySelector(\".card[data-id='{id}']\")?.classList.contains('yours') && document.getElementById('status').textContent === '1 card is yours.'");
    s.ok("given to Ben, it shows on his page as his", chrome.until(&benp, &yours, wait), chrome.eval(&benp, "document.body.innerText")?);
    s.ok(
        "and his browser gets a push saying so",
        s.eventually(wait, || !s.push.received("board-ben").is_empty())
            && s.push.received("board-ben") == [json!({ "title": "Take out the bins", "body": "This one is yours now.", "tag": format!("card-{id}"), "url": "./" })],
        format!("{:?}", s.push.received("board-ben")),
    );
    let owner_sees = format!("[...document.querySelectorAll(\".card[data-id='{id}'] option\")].find((o) => o.selected)?.textContent === '{ben_name}'");
    s.ok("the owner's page names him on it", chrome.until(&host, &owner_sees, wait), "");

    // Ben starts it; the owner's page follows
    chrome.click(&benp, &format!(".card[data-id='{id}'] .next"))?;
    s.ok("Ben starts it, and the owner's page moves it to Doing", chrome.until(&host, &format!("!!document.querySelector(\"section[data-col=doing] .card[data-id='{id}']\")"), wait), chrome.eval(&host, "document.querySelector('.columns').innerText")?);

    // a card someone takes for themselves pushes nothing; one given to Ben does
    chrome.eval(&host, &add("Water the plants"))?;
    chrome.eval(&host, &add("Feed the cat"))?;
    anyhow::ensure!(chrome.until(&host, &todo("Feed the cat"), wait), "two more cards");
    let cards = board();
    let id_of = |title: &str| cards.as_array().and_then(|c| c.iter().find(|c| c["title"] == title)).and_then(|c| c["id"].as_i64()).unwrap_or(0);
    let owner_id = api.identity(&owner)?;
    chrome.eval(&host, &pick(id_of("Water the plants"), &owner_id))?;
    anyhow::ensure!(s.eventually(wait, || board().as_array().is_some_and(|c| c.iter().any(|c| c["title"] == "Water the plants" && c["assignee"] == owner_id.as_str()))), "the owner takes a card");
    chrome.eval(&host, &pick(id_of("Feed the cat"), &ben_id))?;
    let titles = || s.push.received("board-ben").iter().map(|p| p["title"].clone()).collect::<Vec<Value>>();
    s.eventually(wait, || titles().len() >= 2);
    s.ok("a card the owner takes pushes nothing; the next given to Ben does", titles() == [json!("Take out the bins"), json!("Feed the cat")], format!("{:?}", titles()));
    chrome.click(&benp, ".filter button[data-f=mine]")?;
    let mine = "[...document.querySelectorAll('.card p')].map((p) => p.textContent).sort().join(',')";
    s.ok("Ben's Mine shows his cards alone", chrome.until(&benp, &format!("{mine} === 'Feed the cat,Take out the bins'"), wait), chrome.eval(&benp, mine)?);

    // a visitor with the link reads it and changes nothing
    let v = chrome.another_context()?;
    let visitor = chrome.open_in(&v, &link)?;
    s.ok(
        "a visitor with the link sees the board, asked to sign in to change it",
        chrome.until(&visitor, &format!("{} && document.getElementById('add').hidden && !document.getElementById('signin').hidden && !document.querySelector('.card .next')", todo("Feed the cat")), wait),
        chrome.eval(&visitor, "document.body.innerText")?,
    );
    let call = |op: &str, input: Value| format!("import(new URL('./__fragment.js', location.href).href).then((f) => f.call('{op}', {input}).then(() => 'ok', (e) => e.status + ' ' + e.message))");
    let r = chrome.eval(&visitor, &call("add", json!({ "title": "sneaky" })))?;
    s.ok("and its call to add one is refused", r.as_str().is_some_and(|r| r.starts_with("422") && r.contains("sign in to change the board")), &r);
    let r = api.op(&owner, &name, "assign", "anon-to", json!({ "id": id, "to": "anon:00112233445566778899aabbccddeeff" }))?;
    s.ok("a card is given to a person, not to a visitor", r.status == 422 && r.message().contains("given to a person"), &r);
    Ok(())
}
