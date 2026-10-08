//! Orgs and seats (docs/billing.md; docs/api.md, Seats and orgs): an
//! operator comps a seat for an email, in a new org of one or in an org
//! named; a person who signs in with an email a seat waits on takes it; the
//! registry pushes each seat's plan to its holder's ledger (a comped seat is
//! active whatever its org pays; one that ends is canceled, the plan kept).
//! A seat's holder reads it and lets their always-on computer sleep; an
//! org's admins see it. Valid, invalid and replay. A seat's computer
//! staying awake is the computers section's (it has one).

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::Api;
use crate::{Need, Suite};

/// How long a seat's push may take to reach a ledger (the registry's alarm).
const PUSHED: Duration = Duration::from_secs(20);

fn ledger(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/ledger", None).map(|r| r.body).unwrap_or(Value::Null)
}

fn email(i: &str) -> String {
    format!("orgs-{i}-{}@e2e.test", &Keys::generate().pubkey_hex()[..10])
}

pub fn orgs(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("orgs", &[Need::Deployment]) {
        return Ok(());
    }
    let op_session = api.sign_in("operator@e2e.test")?;
    // the ledger section approves the operator's key when it runs first
    let _ = api.approve(&op_session, &s.operator);
    let operator = s.operator.clone();
    let comp = |body: Value| api.signed(&operator, "POST", "/api/admin/seats", Some(&body));

    // ---- a person who signed in already: their comp is theirs at once
    let alice = api.person()?;
    let alice_id = api.identity(&alice)?;
    let alice_email = Api::email_of(&alice);
    let r = comp(json!({ "email": alice_email.to_uppercase(), "kind": "seat_always_on" }))?;
    let alice_seat = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let alice_org = r.body["org"]["id"].as_str().unwrap_or("").to_string();
    s.ok(
        "an operator comps a seat for a person's email (any case): theirs at once, in a new org of one named by it, which they admin",
        r.status == 200
            && r.body["created"] == true
            && r.body["seat"]["person"] == alice_id.as_str()
            && r.body["seat"]["email"] == alice_email.as_str()
            && r.body["seat"]["seat"] == "seat_always_on"
            && r.body["seat"]["comped"] == true
            && r.body["seat"]["admin"] == true
            && r.body["org"]["name"] == alice_email.as_str()
            && fragment_core::org::valid_org_id(&alice_org),
        &r,
    );
    let again = comp(json!({ "email": alice_email, "kind": "seat_always_on" }))?;
    s.ok(
        "the same comp again is the same seat, not made again",
        again.status == 200 && again.body["created"] == false && again.body["seat"]["id"] == alice_seat.as_str() && again.body["org"]["id"] == alice_org.as_str(),
        &again,
    );
    if !s.hosted() {
        let sent = s.mail.sent_to(&alice_email);
        s.ok(
            "the platform mails them their seat, once (the replay mails nothing)",
            r.body["mailed"] == true && again.body["mailed"] == false && sent.len() == 1 && sent[0].subject == "You have a seat on fragment" && sent[0].text.contains("always-on computer") && sent[0].text.contains("It is yours now"),
            format!("{sent:?}"),
        );
    }
    let pushed = s.eventually(PUSHED, || {
        let v = ledger(api, &alice);
        v["plan"] == "seat_always_on" && v["seat"] == "active"
    });
    s.ok("its plan reaches their ledger: seat_always_on, active, with its month's credit", pushed, ledger(api, &alice));
    let mine = api.signed(&alice, "GET", "/api/seat", None)?;
    s.ok(
        "they read their seat: its kind, comped, good, their org, and that they admin it",
        mine.status == 200
            && mine.body["seat"]["id"] == alice_seat.as_str()
            && mine.body["seat"]["kind"] == "seat_always_on"
            && mine.body["seat"]["comped"] == true
            && mine.body["seat"]["good"] == true
            && mine.body["seat"]["sleeps"] == false
            && mine.body["admin"] == true
            && mine.body["org"]["id"] == alice_org.as_str(),
        &mine,
    );
    let slept = api.signed(&alice, "PUT", "/api/seat", Some(&json!({ "sleeps": true })))?;
    let woke = api.signed(&alice, "PUT", "/api/seat", Some(&json!({ "sleeps": false })))?;
    s.ok("they let its computer sleep, and take it back", slept.status == 200 && slept.body["seat"]["sleeps"] == true && woke.body["seat"]["sleeps"] == false, json!([slept.body, woke.body]));

    // ---- an email no one has signed in with: the seat waits on it
    let bob_email = email("bob");
    let r = comp(json!({ "email": bob_email, "kind": "seat_always_on" }))?;
    let bob_seat = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    s.ok("a comp for an email no one signs in with waits on it", r.status == 200 && r.body["seat"]["person"].is_null() && r.body["seat"]["admin"] == true, &r);
    let replay = comp(json!({ "email": bob_email, "kind": "seat_always_on" }))?;
    s.ok("and again is the same waiting seat", replay.status == 200 && replay.body["created"] == false && replay.body["seat"]["id"] == bob_seat.as_str(), &replay);
    if !s.hosted() {
        let sent = s.mail.sent_to(&bob_email);
        s.ok("the email is mailed where to sign in to take it", r.body["mailed"] == true && sent.len() == 1 && sent[0].text.contains("Sign in with this email address to take it"), format!("{sent:?}"));
        s.mail.fail_next("E_RATE_LIMIT_EXCEEDED");
        let failed = comp(json!({ "email": email("dave"), "kind": "seat" }))?;
        s.ok("a mail that fails leaves the seat made, and says it was not mailed", failed.status == 200 && failed.body["created"] == true && failed.body["mailed"] == false, &failed);
    }
    let bob = Keys::generate();
    let bob_session = api.sign_in(&bob_email)?;
    let me = api.approve(&bob_session, &bob)?;
    let bob_id = me.body["id"].as_str().unwrap_or("").to_string();
    let mine = api.signed(&bob, "GET", "/api/seat", None)?;
    s.ok(
        "whoever signs in with that email takes it, and admins its org",
        fragment_core::npub::is_identity(&bob_id) && mine.status == 200 && mine.body["seat"]["id"] == bob_seat.as_str() && mine.body["seat"]["kind"] == "seat_always_on" && mine.body["admin"] == true,
        &mine,
    );
    let pushed = s.eventually(PUSHED, || ledger(api, &bob)["plan"] == "seat_always_on");
    s.ok("its plan reaches their ledger too", pushed, ledger(api, &bob));

    // ---- an org named: a seat in someone's org, waiting
    let carol_email = email("carol");
    let r = comp(json!({ "email": carol_email, "kind": "seat", "org": alice_org }))?;
    let carol_seat = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    s.ok("an operator comps a seat in an org they name: not its admin", r.status == 200 && r.body["org"]["id"] == alice_org.as_str() && r.body["seat"]["admin"] == false, &r);
    let view = api.signed(&alice, "GET", "/api/org", None)?;
    let members = view.body["members"].as_array().cloned().unwrap_or_default();
    s.ok(
        "its admin sees the org: themselves, holding their seat, and the seat waiting on an email",
        view.status == 200
            && view.body["id"] == alice_org.as_str()
            && members.len() == 2
            && members[0]["person"] == alice_id.as_str()
            && members[1]["id"] == carol_seat.as_str()
            && members[1]["person"].is_null()
            && members[1]["email"] == carol_email.as_str(),
        &view,
    );
    let op_view = api.signed(&operator, "GET", &format!("/api/admin/orgs/{alice_org}"), None)?;
    s.ok("an operator sees any org", op_view.status == 200 && op_view.body == view.body, &op_view);
    let erin = api.person()?;
    let r = comp(json!({ "email": Api::email_of(&erin), "kind": "seat", "org": alice_org }))?;
    let not_admin = api.signed(&erin, "GET", "/api/org", None)?;
    let no_org = api.signed(&api.person()?, "GET", "/api/org", None)?;
    let own = api.signed(&bob, "GET", "/api/org", None)?;
    s.ok(
        "a seat's holder who is not its org's admin sees no org (403), nor does someone in none; an admin sees their own",
        r.status == 200 && not_admin.status == 403 && no_org.status == 403 && own.status == 200 && own.body["id"] != alice_org.as_str(),
        json!([r.status, not_admin.status, no_org.status, own.status]),
    );
    let erin_seat = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    api.signed(&operator, "DELETE", &format!("/api/admin/seats/{erin_seat}"), None)?;
    let offered = comp(json!({ "email": alice_email, "kind": "seat", "org": api.signed(&bob, "GET", "/api/seat", None)?.body["org"]["id"] }))?;
    s.ok("a person in an org is not comped into another (409)", offered.status == 409, &offered);

    // ---- changing and ending comps
    let r = api.signed(&operator, "PATCH", &format!("/api/admin/seats/{alice_seat}"), Some(&json!({ "kind": "seat" })))?;
    s.ok("an operator changes a comped seat's kind", r.status == 200 && r.body["seat"] == "seat", &r);
    let pushed = s.eventually(PUSHED, || ledger(api, &alice)["plan"] == "seat");
    s.ok("and the holder's ledger follows", pushed, ledger(api, &alice));
    let r = api.signed(&operator, "DELETE", &format!("/api/admin/seats/{bob_seat}"), None)?;
    s.ok("an operator ends a comp", r.status == 200 && r.body["id"] == bob_seat.as_str(), &r);
    let pushed = s.eventually(PUSHED, || {
        let v = ledger(api, &bob);
        v["seat"] == "canceled" && v["standing"] == json!({ "standing": "agents_stopped", "why": "seat_canceled" })
    });
    s.ok("its holder's seat is canceled: agents stop; the plan is kept", pushed && ledger(api, &bob)["plan"] == "seat_always_on", ledger(api, &bob));
    let mine = api.signed(&bob, "GET", "/api/seat", None)?;
    s.ok("an admin whose comp ended keeps their org, seatless", mine.status == 200 && mine.body["seat"].is_null() && mine.body["admin"] == true && mine.body["org"].is_object(), &mine);
    let r = api.signed(&operator, "DELETE", &format!("/api/admin/seats/{carol_seat}"), None)?;
    let view = api.signed(&alice, "GET", "/api/org", None)?;
    s.ok("a waiting seat that ends is gone from its org", r.status == 200 && view.body["members"].as_array().map(Vec::len) == Some(1), &view);
    let r = api.signed(&operator, "DELETE", &format!("/api/admin/seats/{carol_seat}"), None)?;
    s.ok("ending it again finds none (404)", r.status == 404, &r);
    let r = comp(json!({ "email": bob_email, "kind": "seat" }))?;
    let pushed = s.eventually(PUSHED, || {
        let v = ledger(api, &bob);
        v["seat"] == "active" && v["plan"] == "seat"
    });
    s.ok("a seatless admin comped again takes the seat in their own org, active again", r.status == 200 && r.body["seat"]["id"] == bob_seat.as_str() && pushed, ledger(api, &bob));

    // ---- invalid
    let stranger = api.person()?;
    let refused = [
        ("a person who is no operator comps (403)", api.signed(&stranger, "POST", "/api/admin/seats", Some(&json!({ "email": email("x"), "kind": "seat" })))?, 403),
        ("an email that is none (400)", comp(json!({ "email": "not-an-email", "kind": "seat" }))?, 400),
        ("a kind that is no seat's (400)", comp(json!({ "email": email("y"), "kind": "guest" }))?, 400),
        ("a field it does not take (400)", comp(json!({ "email": email("z"), "kind": "seat", "paid": true }))?, 400),
        ("an org that is none (404)", comp(json!({ "email": email("w"), "kind": "seat", "org": "org-0000000000000000" }))?, 404),
        ("a held seat comped again as another kind (409)", comp(json!({ "email": alice_email, "kind": "seat_always_on" }))?, 409),
        ("a seat id that is none (404)", api.signed(&operator, "PATCH", "/api/admin/seats/mem-0000000000000000", Some(&json!({ "kind": "seat" })))?, 404),
        ("someone with no seat lets it sleep (400)", api.signed(&stranger, "PUT", "/api/seat", Some(&json!({ "sleeps": true })))?, 400),
    ];
    for (what, r, status) in refused {
        s.ok(&format!("refused: {what}"), r.status == status, &r);
    }
    let none = api.signed(&stranger, "GET", "/api/seat", None)?;
    s.ok("someone with no seat reads none", none.status == 200 && none.body["seat"].is_null() && none.body["org"].is_null() && none.body["offered"] == json!([]), &none);
    Ok(())
}
