//! The when template: a poll anyone with the link votes in. Its owner sets
//! it up in the page; two visitors with no account vote in theirs, as
//! anonymous principals, and each page follows the other's ballot live,
//! with who is here. Closing it settles every page.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use super::templates::person;
use crate::api::Api;
use crate::Suite;

pub fn when(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("when", &[crate::Need::Chrome]) {
        return Ok(());
    }
    let (owner, session) = person(api)?;
    let name = s.named(api, &owner, "when")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "when" }))?;
    anyhow::ensure!(made.status == 200, "a poll from its template: {made}");
    let link = api.site_url(&name, &format!("?view={}", made.body["viewToken"].as_str().unwrap_or("")));
    let poll = || api.op(&owner, &name, "poll", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the when template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(20);

    // its owner, signed in, asks the question in the page
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let host = chrome.open(&api.site_url(&name, "__signin?return=/"))?;
    s.ok("its owner opens the new poll to its setup form", chrome.until(&host, "!document.getElementById('setup').hidden", wait), "");
    chrome.eval(
        &host,
        "(() => { const $ = (id) => document.getElementById(id); $('ask').value = 'Dinner next week?';
          for (const [day, hour] of [['2026-10-12', '19:00'], ['2026-10-13', '19:30'], ['2026-10-14', '']]) { $('day').value = day; $('hour').value = hour; $('addt').click(); }
          $('setup').requestSubmit(); return true; })()",
    )?;
    let set = chrome.until(&host, "document.querySelectorAll('#options .option').length === 3", wait);
    let p = poll();
    let ids: Vec<i64> = p["options"].as_array().map(|o| o.iter().filter_map(|o| o["id"].as_i64()).collect()).unwrap_or_default();
    s.ok("and saves it: three times to pick from, in the order given", set && p["question"] == "Dinner next week?" && p["mode"] == "times" && ids.len() == 3, &p);

    // two visitors with the link and no account vote, each seeing the other's
    let (a, b) = (chrome.another_context()?, chrome.another_context()?);
    let ana = chrome.open_in(&a, &link)?;
    let ben = chrome.open_in(&b, &link)?;
    for page in [&ana, &ben] {
        anyhow::ensure!(chrome.until(page, "document.querySelectorAll('#options .option').length === 3", wait), "a visitor's page shows the poll");
    }
    let name_as = |who: &str| format!("(() => {{ const n = document.getElementById('name'); n.value = '{who}'; n.dispatchEvent(new Event('change')); return true; }})()");
    let button = |at: usize, which: &str| format!("#options .option:nth-child({at}) button.{which}");
    chrome.eval(&ana, &name_as("Ana"))?;
    chrome.click(&ana, &button(1, "yes"))?;
    chrome.click(&ana, &button(2, "maybe"))?;
    chrome.eval(&ben, &name_as("Ben"))?;
    chrome.click(&ben, &button(1, "yes"))?;
    // who said yes to an option, as a page lists them (in the order they first voted)
    let who = |at: usize| format!("(document.querySelector('#options .option:nth-child({at}) .who')?.textContent ?? '').split(/, | · /).sort().join(',')");
    s.ok(
        "a visitor's vote shows live on another's page: Ana's on Ben's",
        chrome.until(&ben, &format!("{} === 'Ana,you' && {} === 'maybe Ana'", who(1), who(2)), wait),
        chrome.eval(&ben, "document.getElementById('options').innerText")?,
    );
    s.ok("and Ben's on Ana's", chrome.until(&ana, &format!("{} === 'Ben,you'", who(1)), wait), chrome.eval(&ana, "document.getElementById('options').innerText")?);
    s.ok(
        "the owner's page leads with the time both can make",
        chrome.until(&host, "document.querySelector('#options .option:nth-child(1) .tag')?.textContent === 'Best so far'", wait),
        chrome.eval(&host, "document.getElementById('options').innerText")?,
    );
    let here = "[...document.querySelectorAll('#here li span:last-child')].map((l) => l.textContent).sort().join(',')";
    s.ok("and shows who is here, by the names they gave", chrome.until(&host, &format!("{here} === 'Ana,Ben,you'"), wait), chrome.eval(&host, here)?);
    let p = poll();
    let ballots = p["ballots"].as_array().cloned().unwrap_or_default();
    let picks = |who: &str| ballots.iter().find(|b| b["name"] == who).map(|b| b["picks"].clone()).unwrap_or_default();
    s.ok(
        "each ballot is its voter's, by name: no principal leaves the app",
        ballots.len() == 2
            && picks("Ana") == json!({ ids[0].to_string(): "yes", ids[1].to_string(): "maybe" })
            && picks("Ben") == json!({ ids[0].to_string(): "yes" })
            && !p.to_string().contains("anon:"),
        &p,
    );
    chrome.reload(&ana)?;
    let pressed = "[...document.querySelectorAll('#options [aria-pressed=true]')].map((b) => b.className).join(',')";
    s.ok("a visitor's reload finds their own ballot", chrome.until(&ana, &format!("{pressed} === 'yes,maybe'"), wait), chrome.eval(&ana, pressed)?);

    // what a visitor may not do
    let call = |op: &str, input: Value| format!("import(new URL('./__fragment.js', location.href).href).then((f) => f.call('{op}', {input}).then(() => 'ok', (e) => e.status + ' ' + e.message))");
    let r = chrome.eval(&ana, &call("setup", json!({ "question": "mine now", "mode": "times", "options": ["x"] })))?;
    s.ok("a visitor cannot change the poll (only an editor sets it up)", r.as_str().is_some_and(|r| r.starts_with("401")), &r);
    let r = chrome.eval(&ana, &call("vote", json!({ "name": "Ana", "picks": [{ "option": 999, "answer": "yes" }] })))?;
    s.ok("a ballot naming no option is refused, saying so", r.as_str().is_some_and(|r| r.starts_with("422") && r.contains("no option 999")), &r);

    // closing settles every page
    chrome.click(&host, "#toggle")?;
    let settled = format!("!document.getElementById('settled').hidden && document.getElementById('settled').textContent.includes({:?}) && document.querySelector('#options button.yes').disabled", p["options"][0]["label"].as_str().unwrap_or("?"));
    s.ok("its owner closes the voting on the best time, and the visitors' pages say it is settled", chrome.until(&ana, &settled, wait) && chrome.until(&ben, &settled, wait), chrome.eval(&ben, "document.querySelector('header').innerText")?);
    let r = chrome.eval(&ben, &call("vote", json!({ "name": "Ben", "picks": [] })))?;
    s.ok("a vote after it closed is refused", r.as_str().is_some_and(|r| r.starts_with("422") && r.contains("voting is closed")), &r);
    chrome.close(ben)?;
    s.ok("a visitor who leaves is no longer here", chrome.until(&host, &format!("{here} === 'Ana,you'"), wait), chrome.eval(&host, here)?);

    // a quick poll takes one pick: a poll of times made one keeps each ballot's first yes
    api.op(&owner, &name, "close", "reopen", json!({ "closed": false }))?;
    let labels: Vec<Value> = p["options"].as_array().map(|o| o.iter().map(|o| o["label"].clone()).collect()).unwrap_or_default();
    let r = api.op(&owner, &name, "setup", "to-choice", json!({ "question": "Dinner next week?", "mode": "choice", "options": labels }))?;
    let p = poll();
    s.ok(
        "a poll of times made a quick poll keeps its options' votes, one yes a ballot",
        r.status == 200 && p["ballots"].as_array().is_some_and(|b| b.iter().all(|b| b["picks"] == json!({ ids[0].to_string(): "yes" }))),
        &p,
    );
    let r = chrome.eval(&ana, &call("vote", json!({ "name": "Ana", "picks": [{ "option": ids[0], "answer": "yes" }, { "option": ids[1], "answer": "yes" }] })))?;
    s.ok("and refuses a ballot of two picks", r.as_str().is_some_and(|r| r.starts_with("422") && r.contains("one answer")), &r);
    Ok(())
}
