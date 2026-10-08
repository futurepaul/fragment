//! The test levers (`/api/test/*`, cell/src/levers.rs), as a preview has
//! them: they answer only a request that carries the fleet's test secret,
//! and to anyone else are the 404 any missing route is. Their e2e sign-in
//! makes `<name>@e2e.test` people under an issuer of their own (never
//! anyone WorkOS signs in), seats whose paid calls are capped at what the
//! sign-in asked for. On a branch deployment (a preview, or the hosted
//! lane's rehearsal on the local node) they reach the e2e's own things
//! alone. Valid, invalid and replay, here and on a preview.

use anyhow::{Context, Result};
use fragment_core::levers;
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use super::signin::who;
use crate::api::{Api, Call, Reply};
use crate::Suite;

/// A lever's request as anyone may send it: `secret` in its header, or
/// none (the client adds the run's own to no request of this kind).
fn unlocked(api: &Api, path: &str, secret: Option<String>) -> Result<Reply> {
    api.call_without_secret(Call {
        method: "POST",
        url: format!("{}{path}", api.base),
        body: Some(b"{}".to_vec()),
        content_type: Some("application/json"),
        extra: secret.map(|s| vec![(levers::SECRET_HEADER, s)]).unwrap_or_default(),
        ..Call::default()
    })
}

pub fn levers(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("levers", &[crate::Need::Levers]) {
        return Ok(());
    }
    // the gate: no secret, an empty one, or another is the 404 any missing route is
    let missing = unlocked(api, "/api/no-such-route", None)?;
    let tried = [
        ("/api/test/people", unlocked(api, "/api/test/people", None)?),
        ("/api/test/people", unlocked(api, "/api/test/people", Some(String::new()))?),
        ("/api/test/signin", unlocked(api, "/api/test/signin", Some("f".repeat(64)))?),
        ("/api/test/computer", unlocked(api, "/api/test/computer", None)?),
    ];
    // the router's 404 names the path it has no route for
    let as_missing = |path: &str, r: &Reply| r.status == 404 && r.text == missing.text.replace("/api/no-such-route", path);
    s.ok(
        "a lever without the fleet's secret, with an empty one, or with another, is the 404 any missing route is, word for word",
        missing.status == 404 && missing.text.contains("/api/no-such-route") && tried.iter().all(|(path, r)| as_missing(path, r)),
        format!("{} / {missing}", tried.iter().map(|(_, r)| r.to_string()).collect::<Vec<_>>().join(" / ")),
    );
    let people = api.unsigned("POST", "/api/test/people", Some(&json!({})))?;
    s.ok("with it, the lever answers", people.status == 200 && people.body["people"].is_array(), &people);

    // the e2e sign-in: valid
    let email = format!("levers-{}@e2e.test", &Keys::generate().pubkey_hex()[..12]);
    let signin = |email: &str, paid_calls: Value| api.unsigned("POST", "/api/test/signin", Some(&json!({ "email": email, "paidCalls": paid_calls })));
    let first = signin(&email, json!(0))?;
    let session = first.body["session"].as_str().unwrap_or("").to_string();
    let identity = first.body["identity"].as_str().unwrap_or("").to_string();
    let me = who(api, &session)?;
    s.ok(
        "an e2e person signs in by their @e2e.test email: a person made, a platform session that works, no paid calls unless asked",
        first.status == 200 && first.body["created"] == true && first.body["paidCalls"] == 0 && me["id"] == identity.as_str() && me["kind"] == "person",
        json!({ "signin": first.body, "me": me }),
    );
    let ledger = shell_get(api, &session, "/api/ledger")?;
    s.ok("they are a seat (the e2e makes fragments)", ledger.status == 200 && ledger.body["plan"] == "seat", &ledger);
    // replay: the same email is the same person, with a session of its own
    let again = signin(&email, json!(0))?;
    let again_session = again.body["session"].as_str().unwrap_or("").to_string();
    s.ok(
        "the same email again is the same person, not made again, in a new session; both work",
        again.status == 200 && again.body["identity"] == identity.as_str() && again.body["created"] == false && again_session != session && who(api, &session)?["id"] == identity.as_str() && who(api, &again_session)?["id"] == identity.as_str(),
        &again,
    );
    // a preview may hold many e2e people: their pages, until this one's
    let (mut found, mut after, mut pages) = (false, Value::Null, 0);
    // bounded: a hundred people a page, at most a hundred pages
    while !found && pages < 100 {
        let listed = api.unsigned("POST", "/api/test/people", Some(&json!({ "after": after })))?;
        anyhow::ensure!(listed.status == 200, "the e2e people: {listed}");
        found = listed.body["people"].as_array().into_iter().flatten().any(|p| p["identity"] == identity.as_str() && p["email"] == email.as_str());
        after = listed.body["next"].clone();
        pages += 1;
        if after.is_null() {
            break;
        }
    }
    s.ok("the sweep's list finds them, by identity and email", found, format!("{pages} pages"));
    // invalid
    let refused: Vec<(String, u16)> = [
        signin("paul@example.com", json!(0))?,
        signin("Levers@e2e.test", json!(0))?,
        signin("levers@e2e.test.example.com", json!(0))?,
        signin(&email, json!(levers::E2E_PAID_CALLS_MAX + 1))?,
        api.unsigned("POST", "/api/test/signin", Some(&json!({ "email": email, "plan": "seat_always_on" })))?,
    ]
    .iter()
    .map(|r| (r.error().to_string(), r.status))
    .collect();
    s.ok(
        "a sign-in for anyone not @e2e.test, more paid calls than an e2e person makes, or a field it does not take is refused (400)",
        refused.iter().all(|(code, status)| *status == 400 && code == "invalid_request"),
        format!("{refused:?}"),
    );

    if s.hosted() {
        scoped(s, api, &email, &identity)?;
    } else {
        not_a_workos_person(s, api, &email)?;
        paid_calls(s, api)?;
        mail(s, api)?;
    }
    Ok(())
}

/// A GET as the shell makes it, with a platform session.
fn shell_get(api: &Api, session: &str, path: &str) -> Result<Reply> {
    api.call(Call {
        method: "GET",
        url: format!("{}{path}", api.base),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("x-fragment-shell", "1".into()), ("sec-fetch-site", "same-origin".into())],
        ..Call::default()
    })
}

/// On a branch deployment (a preview, or the rehearsal's node) the levers
/// reach the e2e's own things alone: no registry lever, no fragment not
/// labelled `e2e-`, no ledger of someone not an e2e person.
fn scoped(s: &mut Suite, api: &Api, email: &str, identity: &str) -> Result<()> {
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "calls": null })))?;
    s.ok("on a preview the registry's levers (the whole deployment's) are no route (404)", r.status == 404, &r);
    let r = api.unsigned("POST", "/api/test/mail", Some(&json!({ "to": "someone@e2e.test", "subject": "s", "text": "t" })))?;
    s.ok("nor the mail lever: a preview's mail is real (404)", r.status == 404, &r);
    let keys = Keys::generate();
    let session = api.sign_in(email)?;
    api.approve(&session, &keys)?;
    // a fragment the e2e did not label as its own (made and deleted here)
    let plain = format!("plain-{}", &keys.pubkey_hex()[..8]);
    let made = api.create(&keys, &plain)?;
    anyhow::ensure!(made.status == 200, "making {plain}: {made}");
    let name = made.body["name"].as_str().context("a create names the fragment")?.to_string();
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "alarm" })))?;
    let deleted = api.signed(&keys, "DELETE", &format!("/api/f/{name}"), None)?;
    s.ok("nor a fragment not labelled e2e- (403)", r.status == 403 && r.code() == Some(ErrorCode::Forbidden) && deleted.status == 200, &r);
    let own = s.named(api, &keys, "levers")?;
    let made = api.create(&keys, &own)?;
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": own, "op": "alarm" })))?;
    s.ok("while one labelled e2e- is the e2e's own", made.status == 200 && r.status == 200, &r);
    let stranger = fragment_core::npub::identity_of(&"0".repeat(64));
    let theirs = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": stranger, "op": "totals" })))?;
    let mine = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": identity, "op": "totals" })))?;
    s.ok("and a ledger lever reaches e2e people alone (403 for anyone else)", theirs.status == 403 && mine.status == 200, format!("{theirs} / {mine}"));
    Ok(())
}

/// Locally: the platform's mail (cell/src/mail.rs) reaches the mail fake,
/// from the deployment's address, as the Email Sending binding would take
/// it; a message it cannot send, or a service that refuses one, says why.
fn mail(s: &mut Suite, api: &Api) -> Result<()> {
    let to = format!("{}@example.com", s.name("mail"));
    let send = |to: &str, subject: &str| api.unsigned("POST", "/api/test/mail", Some(&json!({ "to": to, "subject": subject, "text": "Open it: https://example.com/x" })));
    let r = send(&to, "Paul shared a thing with you")?;
    let sent = s.mail.sent_to(&to);
    s.ok(
        "the platform's mail reaches Email Sending (the fake) from the deployment's address, as it was asked",
        r.status == 200 && r.body["messageId"].as_str().is_some_and(|id| !id.is_empty()) && sent.len() == 1 && sent[0].from == crate::MAIL_FROM && sent[0].subject == "Paul shared a thing with you",
        &r,
    );
    let refused = [send("not an address", "s")?, send(&format!("{to}, eve@example.com"), "s")?, send(&to, "two\nlines")?];
    s.ok(
        "a message to anything but one plain address, or with a subject of two lines, is refused (400), and nothing is sent",
        refused.iter().all(|r| r.status == 400) && s.mail.sent().iter().all(|m| m.to == to) && s.mail.sent_to(&to).len() == 1,
        refused.iter().map(ToString::to_string).collect::<Vec<_>>().join(" / "),
    );
    s.mail.fail_next("E_RATE_LIMIT_EXCEEDED");
    let r = send(&to, "s")?;
    s.ok("the service's refusal says so: too many sent is 429", r.status == 429 && r.message().contains("E_RATE_LIMIT_EXCEEDED"), &r);
    Ok(())
}

/// Locally: an e2e person is never anyone a real sign-in reaches. The same
/// email through WorkOS (the fake) is refused: an e2e person is under an
/// issuer of their own, and an email names one person (decision 45).
fn not_a_workos_person(s: &mut Suite, api: &Api, email: &str) -> Result<()> {
    let r = api.workos_callback(email)?;
    s.ok(
        "the same email through WorkOS reaches no one: it is the e2e person's, and an email names one person (409, on a page)",
        r.status == 409 && r.header("content-type").starts_with("text/html") && String::from_utf8_lossy(&r.bytes).contains("another account"),
        &r,
    );
    Ok(())
}

/// Locally, on the Workers AI fake: an e2e person lent one paid call makes
/// one model call (their agent's, through the model route), and the ledger
/// refuses the next before it is made.
fn paid_calls(s: &mut Suite, api: &Api) -> Result<()> {
    let keys = Keys::generate();
    let email = Api::email_of(&keys);
    let r = api.unsigned("POST", "/api/test/signin", Some(&json!({ "email": email, "paidCalls": 1 })))?;
    anyhow::ensure!(r.status == 200 && r.body["paidCalls"] == 1, "an e2e person lent one paid call: {r}");
    let session = r.body["session"].as_str().unwrap_or("").to_string();
    let identity = r.body["identity"].as_str().unwrap_or("").to_string();
    api.approve(&session, &keys)?;
    let hand = Keys::generate();
    let reg = "/api/identities";
    let made = api.signed(&keys, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &keys) })))?;
    anyhow::ensure!(made.status == 200, "an agent of theirs: {made}");
    let call = |text: &str| api.signed(&hand, "POST", "/api/models/v1/chat/completions", Some(&json!({ "model": "cheap", "messages": [{ "role": "user", "content": text }] })));
    let calls = |api: &Api| -> usize {
        let r = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": identity, "op": "entries", "prefix": "aig:" })));
        r.map(|r| r.body["entries"].as_array().map_or(0, Vec::len)).unwrap_or(0)
    };
    let one = call("one")?;
    s.ok("their first model call is made", one.status == 200 && one.body["choices"][0]["message"]["content"] == "echo: one" && calls(api) == 1, &one);
    let asked = s.ai.chats().len();
    let two = call("two")?;
    s.ok(
        "the next is refused by their ledger before it is made (402), and the model is not asked",
        two.status == 402 && two.message().contains("paid calls") && calls(api) == 1 && s.ai.chats().len() == asked,
        &two,
    );
    let cap = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": identity, "op": "paid-calls", "max": 1 })))?;
    s.ok("(the ledger counts the call it allowed)", cap.status == 200 && cap.body == json!({ "max": 1, "used": 1 }), &cap);
    Ok(())
}
