//! Trials (docs/billing.md, "Trials"; decision 56), on the Stripe fake: an
//! operator makes a code; a person who never paid redeems it through a
//! Checkout that takes a card first, with Stripe's trial of its days; its
//! places are held by open Checkouts and subscriptions; one trial per
//! person. Operators list, raise, edit and end codes, compare-and-set.
//! Valid, invalid, replay.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::api::Api;
use crate::{Need, Suite};

const PUSHED: Duration = Duration::from_secs(20);

fn seat(api: &Api, keys: &fragment_nip98::Keys) -> Value {
    api.signed(keys, "GET", "/api/seat", None).map(|r| r.body).unwrap_or(Value::Null)
}

pub fn trials(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("trials", &[Need::Fakes, Need::Deployment]) {
        return Ok(());
    }
    let op_session = api.sign_in("operator@e2e.test")?;
    let _ = api.approve(&op_session, &s.operator);
    let operator = s.operator.clone();
    let admin = |method: &str, path: &str, body: Option<&Value>| api.signed(&operator, method, &format!("/api/admin{path}"), body);

    // ---- an operator makes a code
    let r = admin("POST", "/trials", Some(&json!({ "name": "Launch week", "kind": "seat_always_on", "days": 7, "capacity": 2 })))?;
    let id = r.body["id"].as_str().unwrap_or("").to_string();
    let code = r.body["code"].as_str().unwrap_or("").to_string();
    s.ok(
        "an operator makes a trial code: 16 unambiguous characters in fours, its kind, days and places",
        r.status == 200
            && code.len() == 19
            && fragment_core::org::trial_code(&code).is_some()
            && r.body["days"] == 7
            && r.body["capacity"] == 2
            && r.body["active"] == true
            && r.body["revision"] == 1
            && r.body["subscribed"] == 0,
        &r,
    );
    let stranger = api.person()?;
    let r = api.signed(&stranger, "POST", "/api/admin/trials", Some(&json!({ "name": "x", "kind": "seat", "days": 7, "capacity": 1 })))?;
    s.ok("someone who is no operator makes none (403)", r.status == 403, &r);

    // ---- a person redeems it, as typed
    let kim = api.person()?;
    let typed = code.to_lowercase().replace('-', " ");
    let r = api.signed(&kim, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": typed })))?;
    let session = r.body["session"].as_str().unwrap_or("").to_string();
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("a Checkout")?;
    let f = |k: &str| asked.form.get(k).cloned().unwrap_or_default();
    s.ok(
        "a code as typed (any case, spaces) opens a Checkout: Stripe's 7-day trial of its kind, a card first, no discount on top",
        r.status == 200
            && f("subscription_data[trial_period_days]") == "7"
            && f("subscription_data[trial_settings][end_behavior][missing_payment_method]") == "cancel"
            && f("payment_method_collection") == "always"
            && !asked.form.contains_key("allow_promotion_codes")
            && f("metadata[fragment_kind]") == "seat_always_on"
            && f("metadata[fragment_trial]") == id,
        format!("{:?}", asked.form),
    );
    s.stripe.complete(&session).map_err(anyhow::Error::msg)?;
    let trialing = s.eventually(PUSHED, || seat(api, &kim)["seat"]["good"] == true);
    let mine = seat(api, &kim);
    s.ok(
        "paid with a card, they hold a good $200 seat in its trial, which says when it ends",
        trialing && mine["seat"]["kind"] == "seat_always_on" && mine["seat"]["trialEnds"].as_i64().is_some_and(|t| t > crate::api::now_s() + 6 * 86_400),
        &mine,
    );

    // ---- its places: one bought, one held by an open Checkout, then none
    let lee = api.person()?;
    let held = api.signed(&lee, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code })))?;
    let again = api.signed(&lee, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code })))?;
    let max = api.person()?;
    let full = api.signed(&max, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code })))?;
    s.ok(
        "a second person's open Checkout holds the last place (they may open another); a third finds none (409)",
        held.status == 200 && again.status == 200 && full.status == 409 && full.message().contains("places"),
        json!([held.status, again.status, full.status, full.message()]),
    );
    let view = admin("GET", &format!("/trials/{id}"), None)?;
    s.ok(
        "an operator sees its uses: one subscribed (trialing), one open",
        view.status == 200 && view.body["subscribed"] == 1 && view.body["open"] == 1 && view.body["uses"].as_array().is_some_and(|u| u.iter().any(|u| u["status"] == "trialing")),
        &view,
    );
    let stale = admin("PATCH", &format!("/trials/{id}"), Some(&json!({ "revision": 0, "capacity": 3 })))?;
    let fewer = admin("PATCH", &format!("/trials/{id}"), Some(&json!({ "revision": 1, "capacity": 1 })))?;
    let raised = admin("PATCH", &format!("/trials/{id}"), Some(&json!({ "revision": 1, "capacity": 3 })))?;
    let now_room = api.signed(&max, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code })))?;
    s.ok(
        "a change on a stale revision is refused (409), places never shrink (400); raised, there is room again",
        stale.status == 409 && fewer.status == 400 && raised.status == 200 && raised.body["revision"] == 2 && now_room.status == 200,
        json!([stale.status, fewer.status, raised.body, now_room.status]),
    );

    // ---- once a person; for those who never paid
    let mine_again = api.signed(&kim, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code })))?;
    s.ok("someone with a seat redeems none (409)", mine_again.status == 409, &mine_again);
    let sub = s.stripe.requests().iter().rev().find_map(|a| a.path.strip_prefix("/v1/subscriptions/").map(str::to_string)).context("the trial's subscription")?;
    s.stripe.set_status(&sub, "canceled").map_err(anyhow::Error::msg)?;
    let lapsed = s.eventually(PUSHED, || seat(api, &kim)["seat"]["good"] == false);
    let twice = api.signed(&kim, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code })))?;
    let paid = api.signed(&kim, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?;
    s.ok(
        "a trial ended unpaid lapses its seat; a trial is once a person (409), but they may buy",
        lapsed && twice.status == 409 && paid.status == 200,
        json!([seat(api, &kim), twice.status, twice.message(), paid.status]),
    );

    // ---- codes that refuse
    let r = admin("POST", "/trials", Some(&json!({ "name": "Typed", "kind": "seat", "days": 30, "capacity": 5, "code": "fragment-launch" })))?;
    let typed_id = r.body["id"].as_str().unwrap_or("").to_string();
    let dup = admin("POST", "/trials", Some(&json!({ "name": "Dup", "kind": "seat", "days": 3, "capacity": 5, "code": "FRAGMENT LAUNCH" })))?;
    s.ok("an operator types a code; the same one again (any case) is refused (409)", r.status == 200 && r.body["code"] == "FRAG-MENT-LAUN-CH" && dup.status == 409, json!([r.body["code"], dup.status]));
    let off = admin("PATCH", &format!("/trials/{typed_id}"), Some(&json!({ "revision": 1, "active": false })))?;
    let nia = api.person()?;
    let ended = api.signed(&nia, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": "fragment-launch" })))?;
    let unknown = api.signed(&nia, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": "ZZZZ-ZZZZ-ZZZZ-ZZZZ" })))?;
    let wrong_kind = api.signed(&nia, "POST", "/api/billing/checkout", Some(&json!({ "trialCode": code, "kind": "seat" })))?;
    let neither = api.signed(&nia, "POST", "/api/billing/checkout", Some(&json!({})))?;
    s.ok(
        "an ended code (400), an unknown one (404), a kind the code is not (400), or neither kind nor code (400) is refused, and no org is made for it",
        off.status == 200 && ended.status == 400 && unknown.status == 404 && wrong_kind.status == 400 && neither.status == 400 && seat(api, &nia)["org"].is_null(),
        json!([ended.status, unknown.status, wrong_kind.status, neither.status, seat(api, &nia)]),
    );
    let bad = [
        admin("POST", "/trials", Some(&json!({ "name": "x", "kind": "seat", "days": 31, "capacity": 1 })))?,
        admin("POST", "/trials", Some(&json!({ "name": "x", "kind": "seat", "days": 7, "capacity": 0 })))?,
        admin("POST", "/trials", Some(&json!({ "name": "", "kind": "seat", "days": 7, "capacity": 1 })))?,
        admin("POST", "/trials", Some(&json!({ "name": "x", "kind": "seat", "days": 7, "capacity": 1, "expiresAt": 1 })))?,
        admin("POST", "/trials", Some(&json!({ "name": "x", "kind": "seat", "days": 7, "capacity": 1, "code": "OOOO-IIII" })))?,
    ];
    s.ok(
        "a trial past 30 days, no places, no name, an expiry past, or a code outside the alphabet is refused (400)",
        bad.iter().all(|r| r.status == 400),
        json!(bad.iter().map(|r| r.status).collect::<Vec<_>>()),
    );
    let list = admin("GET", "/trials", None)?;
    s.ok(
        "an operator lists the codes, newest first",
        list.status == 200 && list.body["codes"][0]["id"] == typed_id.as_str() && list.body["codes"].as_array().is_some_and(|c| c.iter().any(|c| c["id"] == id.as_str())),
        &list,
    );
    Ok(())
}
