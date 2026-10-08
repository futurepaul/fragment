//! The operators' admin (docs/api.md, Operators; decision 59): people and
//! orgs a page at a time, searched by email; a person's page with their
//! ledger and computer; billing's health; a trial mailed; and the log of
//! what operators did. An operator key no person holds reaches it, as a
//! wipe's does; no one else. Valid and invalid.

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::Api;
use crate::{Need, Suite};

pub fn admin(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("admin", &[Need::Deployment, Need::Operator]) {
        return Ok(());
    }
    let op_session = api.sign_in("operator@e2e.test")?;
    let _ = api.approve(&op_session, &s.operator);
    let operator = s.operator.clone();
    let get = |keys: &Keys, path: &str| api.signed(keys, "GET", &format!("/api/admin{path}"), None);

    // ---- people, found by email
    let ivy = api.person()?;
    let ivy_id = api.identity(&ivy)?;
    let ivy_email = Api::email_of(&ivy);
    let r = api.signed(&operator, "POST", "/api/admin/seats", Some(&json!({ "email": ivy_email, "kind": "seat" })))?;
    let org = r.body["org"]["id"].as_str().unwrap_or("").to_string();
    let found = get(&operator, &format!("/people?q={}", ivy_email[..10].to_uppercase()))?;
    let row = found.body["people"].as_array().into_iter().flatten().find(|p| p["npub"] == ivy_id.as_str()).cloned().unwrap_or(Value::Null);
    s.ok(
        "an operator finds a person by the start of their email (any case): their npub, email, org, and comped seat",
        found.status == 200 && row["email"] == ivy_email.as_str() && row["org"]["id"] == org.as_str() && row["seat"]["comped"] == true && row["seat"]["good"] == true && row["admin"] == true,
        &found,
    );
    let none = get(&operator, "/people?q=nobody-signs-in-as-this")?;
    let wild = get(&operator, "/people?q=%25")?;
    s.ok("a search no one matches finds none; its wildcards are only themselves", none.body["people"] == json!([]) && wild.body["people"] == json!([]), json!([none.body, wild.body]));
    let page = get(&operator, "/people")?;
    let listed = page.body["people"].as_array().map(Vec::len).unwrap_or(0);
    s.ok("the list is a page at a time, by npub", page.status == 200 && (1..=100).contains(&listed) && (listed < 100) == page.body["next"].is_null(), format!("{listed} people, next {}", page.body["next"]));
    let theirs = get(&operator, &format!("/people/{ivy_id}"))?;
    s.ok(
        "a person's page: as listed, with their ledger, and no computer",
        theirs.status == 200 && theirs.body["person"] == row && theirs.body["ledger"]["plan"] == "seat" && theirs.body["computer"].is_null(),
        &theirs,
    );
    let missing = get(&operator, "/people/npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq")?;
    s.ok("no one by that npub is 404", missing.status == 404, &missing);

    // ---- orgs and billing's health
    let orgs = get(&operator, "/orgs")?;
    let listed = orgs.body["orgs"].as_array().into_iter().flatten().find(|o| o["id"] == org.as_str()).cloned().unwrap_or(Value::Null);
    s.ok("an operator lists orgs: each one's seats, admins and Stripe status", orgs.status == 200 && listed["comped_seats"].is_null() && listed["compedSeats"] == 1 && listed["admins"] == 1 && listed["status"].is_null(), &listed);
    let health = get(&operator, "/health")?;
    s.ok(
        "billing's health: orgs paying and seats, the queues, and nothing failing",
        health.status == 200 && health.body["seatsComped"].as_u64().is_some_and(|n| n >= 1) && health.body["failing"] == json!([]),
        &health,
    );

    // ---- a trial mailed
    let code = api.signed(&operator, "POST", "/api/admin/trials", Some(&json!({ "name": "Admin's", "kind": "seat", "days": 14, "capacity": 5 })))?;
    let code_id = code.body["id"].as_str().unwrap_or("").to_string();
    let to = format!("admin-trial-{}@example.com", &Keys::generate().pubkey_hex()[..8]);
    let sent = api.signed(&operator, "POST", &format!("/api/admin/trials/{code_id}/send"), Some(&json!({ "email": to })))?;
    if !s.hosted() {
        let mail = s.mail.sent_to(&to);
        s.ok(
            "an operator mails a trial: its days, its code, and where to start it",
            sent.status == 200 && mail.len() == 1 && mail[0].text.contains("14 days") && mail[0].text.contains(code.body["code"].as_str().unwrap_or("?")) && mail[0].text.contains("/settings?trial="),
            format!("{sent} {mail:?}"),
        );
    }
    let ended = api.signed(&operator, "PATCH", &format!("/api/admin/trials/{code_id}"), Some(&json!({ "revision": 1, "active": false })))?;
    let not_sent = api.signed(&operator, "POST", &format!("/api/admin/trials/{code_id}/send"), Some(&json!({ "email": to })))?;
    s.ok("an ended code is mailed to no one (400)", ended.status == 200 && not_sent.status == 400, &not_sent);

    // ---- the log
    let grant = api.signed(&operator, "POST", &format!("/api/ledger/{ivy_id}/grant"), Some(&json!({ "id": "admin-log-1", "micros": 1_000_000, "by": api.identity(&operator)?, "why": "a thank-you" })))?;
    let log = get(&operator, "/log")?;
    let actions: Vec<String> = log.body["entries"].as_array().into_iter().flatten().filter_map(|e| e["action"].as_str().map(str::to_string)).collect();
    let target_of = |action: &str| log.body["entries"].as_array().into_iter().flatten().find(|e| e["action"] == action).map(|e| e["target"].clone()).unwrap_or(Value::Null);
    s.ok(
        "the log holds what operators did, newest first: the grant, the trial's end and mailing, its making, the comp",
        grant.status == 200
            && actions.iter().position(|a| a == "ledger-grant") < actions.iter().position(|a| a == "trial-change")
            && ["ledger-grant", "trial-change", "trial-send", "trial-new", "comp"].iter().all(|a| actions.iter().any(|x| x == a))
            && target_of("ledger-grant") == ivy_id.as_str()
            && target_of("trial-new") == code_id.as_str(),
        format!("{actions:?}"),
    );

    // ---- who reaches it
    let wiper = s.wiper.clone().unwrap_or_else(Keys::generate);
    let by_key = get(&wiper, "/health")?;
    s.ok("an operator key no person holds reaches it (no registry asked of it)", by_key.status == 200, &by_key);
    // the CLI, with that key's file
    let dir = s.dir("admin-operator");
    let file = dir.join("operator.key");
    std::fs::write(&file, format!("{}\n", wiper.secret_hex()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    let home = s.dir("admin-cli");
    let path = file.to_string_lossy().to_string();
    let listed = s.cli_json(api, &home, &["operator", "people", "--q", &ivy_email, "--key-file", &path, "--json"]);
    s.ok(
        "`fragment operator people --q` with the operator key's file finds them too",
        listed.as_ref().is_ok_and(|v| v["people"].as_array().is_some_and(|p| p.iter().any(|p| p["npub"] == ivy_id.as_str()))),
        format!("{listed:?}"),
    );
    let stranger = api.person()?;
    let refused = [get(&stranger, "/people")?, get(&stranger, "/health")?, api.signed(&stranger, "POST", "/api/admin/trials", Some(&json!({ "name": "x", "kind": "seat", "days": 1, "capacity": 1 })))?];
    let unsigned = api.unsigned("GET", "/api/admin/people", None)?;
    s.ok(
        "anyone else is refused (403), unsigned is 401",
        refused.iter().all(|r| r.status == 403) && unsigned.status == 401,
        json!([refused.iter().map(|r| r.status).collect::<Vec<_>>(), unsigned.status]),
    );
    Ok(())
}

/// The admin's page (`/admin`) in Chrome, with an operator's own session:
/// a person who holds a key the deployment lists (decision 59). It lists
/// people and comps a seat, makes a trial code, shows the orgs, billing's
/// health and the log; to anyone else it says why it shows nothing.
pub fn admin_page(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("admin-page", &[Need::Chrome, Need::Deployment]) {
        return Ok(());
    }
    let Some(mut b) = s.browser()? else {
        s.ok("Chrome is installed for the admin's page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = std::time::Duration::from_secs(15);
    // the operator: a person who holds the deployment's operator key
    let session = api.sign_in("operator@e2e.test")?;
    let _ = api.approve(&session, &s.operator);
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = b.open(&format!("{}/admin", api.base))?;
    let listed = b.until(&page, "document.querySelectorAll('#people-rows tr').length > 0 && /people/.test(document.getElementById('status').textContent)", wait);
    let me = b.eval(&page, "document.getElementById('me').textContent")?;
    s.ok("an operator's session opens the admin's page: it lists people, and says who they are", listed && me == "operator@e2e.test", &me);

    let email = format!("admin-page-{}@e2e.test", &Keys::generate().pubkey_hex()[..10]);
    b.eval(&page, &format!("(() => {{ const f = document.getElementById('comp'); f.email.value = {email:?}; f.kind.value = 'seat_always_on'; f.requestSubmit(); }})()"))?;
    let comped = b.until(&page, &format!("document.getElementById('status').textContent.startsWith('comped: {email}')"), wait);
    s.ok("it comps a seat for an email", comped, b.eval(&page, "document.getElementById('status').textContent")?);
    b.eval(&page, "(() => { const f = document.getElementById('search'); f.q.value = 'operator@'; f.requestSubmit(); })()")?;
    let found = b.until(&page, "[...document.querySelectorAll('#people-rows tr')].length === 1 && document.querySelector('#people-rows tr').textContent.includes('operator@e2e.test')", wait);
    s.ok("it finds a person by the start of their email", found, b.eval(&page, "document.getElementById('people-rows').textContent")?);

    b.click(&page, "#tabs button[data-tab=trials]")?;
    b.eval(&page, "(() => { const f = document.getElementById('trial-new'); f.elements.name.value = 'From the page'; f.elements.days.value = '9'; f.requestSubmit(); })()")?;
    let made = b.until(&page, "document.getElementById('status').textContent.startsWith('made ') && [...document.querySelectorAll('#trial-rows tr')].some(r => r.textContent.includes('From the page'))", wait);
    s.ok("it makes a trial code, and lists it", made, b.eval(&page, "document.getElementById('status').textContent")?);

    b.click(&page, "#tabs button[data-tab=orgs]")?;
    let orgs = b.until(&page, &format!("[...document.querySelectorAll('#org-rows tr')].some(r => r.textContent.includes({email:?}))"), wait);
    s.ok("its orgs list holds the comp's new org of one", orgs, "");
    b.click(&page, "#tabs button[data-tab=health]")?;
    let health = b.until(&page, "document.querySelectorAll('#health-facts dt').length >= 8", wait);
    s.ok("it shows billing's health", health, "");
    b.click(&page, "#tabs button[data-tab=log]")?;
    let log = b.until(&page, &format!("[...document.querySelectorAll('#log-rows tr')].some(r => r.textContent.includes('comp') && r.textContent.includes({email:?}))"), wait);
    s.ok("its log holds the comp, by this operator", log, "");

    // anyone else
    let other = api.sign_in(&format!("admin-page-stranger-{}@e2e.test", crate::api::now_s()))?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &other)?;
    let theirs = b.open(&format!("{}/admin", api.base))?;
    let refused = b.until(&theirs, "document.getElementById('status').className === 'bad' && /operators/.test(document.getElementById('status').textContent)", wait);
    let rows = b.eval(&theirs, "document.querySelectorAll('#people-rows tr').length")?;
    s.ok("to anyone else it shows nothing, and says only operators may", refused && rows == 0, b.eval(&theirs, "document.getElementById('status').textContent")?);
    Ok(())
}
