//! The split template: shared costs, live. Three people join in their
//! pages; one adds dinner and every page shows who owes whom; another
//! snaps a receipt, which a text step reads (the scripted model, sent the
//! photo as a JPEG) and adds as theirs; a payment settles part of it. A
//! visitor with the link reads it and joins nothing.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use fragment_core::npub;
use serde_json::{json, Value};

use super::templates::person;
use crate::api::Api;
use crate::Suite;

/// A receipt as a phone photographs one: a JPEG, drawn by Chrome itself.
const RECEIPT: &str = "(() => { const c = document.createElement('canvas'); c.width = 900; c.height = 1400; const g = c.getContext('2d');
  g.fillStyle = '#d9d2c5'; g.fillRect(0, 0, 900, 1400); g.fillStyle = '#fbfaf6'; g.fillRect(150, 80, 600, 1240);
  g.fillStyle = '#222'; g.font = 'bold 44px monospace'; g.fillText('TRATTORIA', 330, 200); g.font = '32px monospace';
  [['Pasta', '30.00'], ['Wine', '15.00'], ['', ''], ['TOTAL', '45.00']].forEach(([a, b], i) => { g.fillText(a, 200, 360 + i * 70); g.fillText(b, 560, 360 + i * 70); });
  return c.toDataURL('image/jpeg', 0.9).split(',')[1]; })()";

pub fn split(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("split", &[crate::Need::Chrome, crate::Need::Fakes]) {
        return Ok(());
    }
    let (owner, owner_session) = person(api)?;
    let (ana, ana_session) = person(api)?;
    let (ben, ben_session) = person(api)?;
    let (sam_id, ana_id, ben_id) = (api.identity(&owner)?, api.identity(&ana)?, api.identity(&ben)?);
    let name = s.named(api, &owner, "split")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "split" }))?;
    anyhow::ensure!(made.status == 200, "a split from its template: {made}");
    for k in [&ana, &ben] {
        let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", npub::encode(k.pubkey_hex())), Some(&json!({ "role": "viewer" })))?;
        anyhow::ensure!(r.status == 200, "a member of the trip: {r}");
    }
    let r = api.op(&owner, &name, "about", "about", json!({ "title": "Lisbon", "currency": "eur" }))?;
    anyhow::ensure!(r.status == 200 && r.body["result"]["currency"] == "EUR", "the split's currency: {r}");
    let link = api.site_url(&name, &format!("?view={}", made.body["viewToken"].as_str().unwrap_or("")));
    let ledger = || api.op(&owner, &name, "ledger", "q", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the split template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(20);

    // three people open it signed in, and join
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &owner_session)?;
    let sam = chrome.open(&api.site_url(&name, "__signin?return=/"))?;
    let (a, b) = (chrome.another_context()?, chrome.another_context()?);
    chrome.set_cookie_in(&a, &format!("{}/", api.base), "fragment_session", &ana_session)?;
    chrome.set_cookie_in(&b, &format!("{}/", api.base), "fragment_session", &ben_session)?;
    let anap = chrome.open_in(&a, &api.site_url(&name, "__signin?return=/"))?;
    let benp = chrome.open_in(&b, &api.site_url(&name, "__signin?return=/"))?;
    for (page, who) in [(&sam, "Sam"), (&anap, "Ana"), (&benp, "Ben")] {
        anyhow::ensure!(chrome.until(page, "!document.getElementById('join').hidden", wait), "{who}'s page asks them to join");
        chrome.eval(page, &format!("(() => {{ document.getElementById('jname').value = '{who}'; document.getElementById('join').requestSubmit(); return true; }})()"))?;
    }
    let people = "[...document.querySelectorAll('#people li')].map((l) => l.children[1].textContent).sort().join(',')";
    s.ok("each joins in the page, and everyone's page lists the three", chrome.until(&sam, &format!("{people} === 'Ana,Ben,Sam (you)'"), wait), chrome.eval(&sam, people)?);

    // Sam adds dinner, shared by everyone
    chrome.eval(&sam, "(() => { document.getElementById('what').value = 'Dinner'; document.getElementById('amount').value = '60'; document.getElementById('add').requestSubmit(); return true; })()")?;
    s.ok(
        "Sam adds dinner, and Ana's page shows her share is hers to pay back, to Sam",
        chrome.until(&anap, "document.getElementById('mine').textContent === '€20.00' && document.getElementById('pays').textContent.includes('You pay Sam €20.00')", wait),
        chrome.eval(&anap, "document.querySelector('.side').innerText")?,
    );

    // Ana snaps a receipt: a text step reads it, and it is added as hers
    let photo = s.dir("split").join("receipt.jpg");
    let jpeg = chrome.eval(&anap, RECEIPT)?;
    std::fs::write(&photo, base64::engine::general_purpose::STANDARD.decode(jpeg.as_str().unwrap_or_default())?)?;
    s.ai.say_next(&[r#"{"what": "Trattoria", "total": 45.0, "lines": [{"name": "Pasta", "amount": 30}, {"name": "Wine", "amount": 15}]}"#]);
    chrome.choose_files(&anap, "#receipt", &[&photo])?;
    let scan_wait = Duration::from_secs(40);
    let trattoria = "[...document.querySelectorAll('#expenses li')].some((l) => l.querySelector('.what')?.textContent === 'Trattoria' && l.querySelector('.amt').textContent === '€45.00' && l.querySelector('.meta').textContent.startsWith('Ana paid'))";
    s.ok("Ana snaps a receipt, and Ben's page shows it as her expense, read from the photo", chrome.until(&benp, trattoria, scan_wait), chrome.eval(&benp, "document.getElementById('expenses').innerText")?);
    s.ok("and Ana's page says the scan added it", chrome.until(&anap, "document.getElementById('scan').textContent === 'Added Trattoria from your receipt.'", wait), chrome.eval(&anap, "document.getElementById('scan').textContent")?);
    let sent = s.ai.calls().into_iter().rev().find_map(|c| c.body["messages"][1]["content"][1]["image_url"]["url"].as_str().map(str::to_string)).unwrap_or_default();
    s.ok("the model was sent the photo as a JPEG", sent.starts_with("data:image/jpeg;base64,/9j/"), &sent[..sent.len().min(60)]);
    let l = ledger();
    let balance = |id: &str| l["people"].as_array().and_then(|p| p.iter().find(|p| p["id"] == id)).map(|p| p["balance"].clone()).unwrap_or_default();
    let lines = l["expenses"].as_array().and_then(|e| e.iter().find(|e| e["what"] == "Trattoria")).map(|e| e["lines"].clone()).unwrap_or_default();
    s.ok(
        "the ledger sums it in SQL: Sam is owed 25, Ana 10, Ben owes 35, settled by Ben paying each",
        balance(&sam_id) == 2500 && balance(&ana_id) == 1000 && balance(&ben_id) == -3500
            && l["settle"] == json!([{ "from": ben_id, "to": sam_id, "cents": 2500 }, { "from": ben_id, "to": ana_id, "cents": 1000 }])
            && lines == json!([{ "name": "Pasta", "cents": 3000 }, { "name": "Wine", "cents": 1500 }]),
        &l,
    );

    // Ben pays Sam back
    chrome.click(&benp, "#pays li button")?;
    s.ok(
        "Ben marks his payment to Sam paid, and Sam's page shows Sam square and Ben owing Ana alone",
        chrome.until(&sam, "document.getElementById('mine').textContent === 'Square' && document.getElementById('pays').textContent === 'Ben pays Ana €10.00'", wait),
        chrome.eval(&sam, "document.querySelector('.side').innerText")?,
    );

    // a photo the model cannot read adds nothing, and says so
    let before = ledger()["expenses"].as_array().map(Vec::len);
    s.ai.say_next(&["I see a cat."]);
    chrome.choose_files(&anap, "#receipt", &[&photo])?;
    s.ok(
        "a receipt the model cannot read adds nothing, and Ana's page says so",
        chrome.until(&anap, "document.getElementById('scan').textContent.startsWith(\"I couldn't read that receipt\")", scan_wait) && ledger()["expenses"].as_array().map(Vec::len) == before,
        chrome.eval(&anap, "document.getElementById('scan').textContent")?,
    );

    // what no one may do
    let v = chrome.another_context()?;
    let visitor = chrome.open_in(&v, &link)?;
    s.ok(
        "a visitor with the link sees the split, asked to sign in to join it",
        chrome.until(&visitor, &format!("{trattoria} && !document.getElementById('signin').hidden && document.getElementById('join').hidden && document.getElementById('add').hidden"), wait),
        chrome.eval(&visitor, "document.body.innerText")?,
    );
    let call = |op: &str, input: Value| format!("import(new URL('./__fragment.js', location.href).href).then((f) => f.call('{op}', {input}).then(() => 'ok', (e) => e.status + ' ' + e.message))");
    let r = chrome.eval(&visitor, &call("join", json!({ "name": "Eve" })))?;
    s.ok("and their join is refused", r.as_str().is_some_and(|r| r.starts_with("422") && r.contains("sign in to join")), &r);
    let r = api.op(&owner, &name, "add", "zero", json!({ "what": "Nothing", "cents": 0 }))?;
    s.ok("an expense of no money is refused by its schema", r.status == 400, &r);
    let stranger = api.identity(&api.person()?)?;
    let r = api.op(&owner, &name, "add", "stranger", json!({ "what": "Taxi", "cents": 1200, "among": [sam_id, stranger] }))?;
    s.ok("and one shared with someone not in the split, by the app", r.status == 422 && r.message().contains("not in the split"), &r);
    Ok(())
}
