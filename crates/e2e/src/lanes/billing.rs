//! Seats bought through Stripe (docs/billing.md; docs/api.md, Billing), on
//! the Stripe fake: a person's Checkout for their own seat, in an org of
//! their own; the seat theirs once Stripe says it is paid, by its webhook
//! or by the Checkout's return when no webhook comes; the subscription's
//! lapse and a second purchase; the portal. Every subscription the
//! platform writes is fetched from Stripe again, so a forged, stale,
//! foreign, repeated or out-of-order event changes nothing it should not.
//! Valid, invalid, replay, and a webhook lost.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::Api;
use crate::{Need, Suite};

/// How long a seat's push may take to reach a ledger (the registry's alarm).
const PUSHED: Duration = Duration::from_secs(20);

fn ledger(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/ledger", None).map(|r| r.body).unwrap_or(Value::Null)
}

fn seat(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/seat", None).map(|r| r.body).unwrap_or(Value::Null)
}

/// The org's live subscription's id (not one that ended), from what the
/// platform asked Stripe for.
fn subscription_of(s: &Suite, org: &str) -> Option<String> {
    s.stripe.requests().iter().rev().find_map(|r| {
        let id = r.path.strip_prefix("/v1/subscriptions/")?;
        let sub = s.stripe.subscription(id)?;
        (sub.metadata.get("fragment_org").map(String::as_str) == Some(org) && sub.status != "canceled").then(|| id.to_string())
    })
}

pub fn billing(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("billing", &[Need::Fakes]) {
        return Ok(());
    }

    // ---- a person buys a $200 seat
    let ann = api.person()?;
    let ann_id = api.identity(&ann)?;
    let r = api.signed(&ann, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat_always_on" })))?;
    let session = r.body["session"].as_str().unwrap_or("").to_string();
    s.ok(
        "a person's Checkout for a $200 seat: Stripe's page, to pay on",
        r.status == 200 && session.starts_with("cs_") && r.body["url"].as_str().is_some_and(|u| u.starts_with(&s.stripe.url)),
        &r,
    );
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("a Checkout was asked for")?;
    let f = |k: &str| asked.form.get(k).cloned().unwrap_or_default();
    let org = f("metadata[fragment_org]");
    s.ok(
        "it takes a card always, its tax, promotion codes, one seat at its lookup key's price, and names this deployment, its org and its buyer",
        f("mode") == "subscription"
            && f("payment_method_collection") == "always"
            && f("automatic_tax[enabled]") == "true"
            && f("allow_promotion_codes") == "true"
            && f("line_items[0][quantity]") == "1"
            && f("metadata[fragment_deployment]") == api.base
            && f("subscription_data[metadata][fragment_deployment]") == api.base
            && f("subscription_data[metadata][fragment_org]") == org
            && f("metadata[fragment_person]") == ann_id
            && f("metadata[fragment_kind]") == "seat_always_on"
            && f("success_url").ends_with("/settings?checkout={CHECKOUT_SESSION_ID}")
            && fragment_core::org::valid_org_id(&org),
        format!("{:?}", asked.form),
    );
    let before = seat(api, &ann);
    s.ok("before they pay, they hold no seat; they admin their new org", before["seat"].is_null() && before["admin"] == true && before["org"]["id"] == org.as_str(), &before);
    s.stripe.complete(&session).map_err(anyhow::Error::msg)?;
    let pushed = s.eventually(PUSHED, || {
        let v = ledger(api, &ann);
        v["plan"] == "seat_always_on" && v["seat"] == "active"
    });
    let mine = seat(api, &ann);
    s.ok(
        "paid, Stripe's webhook gives them the seat, paid not comped, and its plan reaches their ledger",
        pushed && mine["seat"]["kind"] == "seat_always_on" && mine["seat"]["comped"] == false && mine["seat"]["good"] == true,
        json!([mine, ledger(api, &ann)]),
    );
    let delivered = s.stripe.delivered();
    s.ok("each event was answered 200", !delivered.is_empty() && delivered.iter().all(|(_, _, status)| *status == 200), format!("{delivered:?}"));
    let back = api.signed(&ann, "POST", &format!("/api/billing/sessions/{session}"), Some(&json!({})))?;
    s.ok("the Checkout's return again changes nothing, and answers their seat", back.status == 200 && back.body["seat"] == mine["seat"], &back);
    let again = api.signed(&ann, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?;
    s.ok("someone who holds a seat buys none (409)", again.status == 409, &again);
    let sub = subscription_of(s, &org).context("the org's subscription was fetched")?;

    // ---- webhooks that are not Stripe's, or not this deployment's
    let object = s.stripe.subscription_json(&sub).context("the subscription")?;
    let (body, sig) = s.stripe.signed_event("customer.subscription.updated", object.clone(), None);
    let forged = s.stripe.deliver_raw(&body, Some(&sig.replace("v1=", "v1=00"))).map_err(anyhow::Error::msg)?;
    let unsigned = s.stripe.deliver_raw(&body, None).map_err(anyhow::Error::msg)?;
    let (stale_body, stale_sig) = s.stripe.signed_event("customer.subscription.updated", object.clone(), Some(crate::api::now_s() - 600));
    let stale = s.stripe.deliver_raw(&stale_body, Some(&stale_sig)).map_err(anyhow::Error::msg)?;
    s.ok("an event with a forged signature, none, or one ten minutes old is refused (401)", [forged, unsigned, stale] == [401, 401, 401], json!([forged, unsigned, stale]));
    let mut finite = object.clone();
    finite["metadata"] = json!({ "finite_customer_org_id": "x" });
    let mut other = object.clone();
    other["metadata"]["fragment_deployment"] = json!("https://another.example");
    let foreign: Vec<u16> = [finite, other]
        .into_iter()
        .map(|o| {
            let (b, sig) = s.stripe.signed_event("customer.subscription.deleted", o, None);
            s.stripe.deliver_raw(&b, Some(&sig)).unwrap_or(0)
        })
        .collect();
    let replayed = s.stripe.deliver_raw(&body, Some(&sig)).map_err(anyhow::Error::msg)?;
    s.ok(
        "finite-mono's event and another deployment's are answered and dropped; a real one again is no change",
        foreign == [200, 200] && replayed == 200 && seat(api, &ann)["seat"]["good"] == true,
        json!([foreign, replayed]),
    );

    // ---- a lapse: past due is still good; canceled stops agents
    s.stripe.set_status(&sub, "past_due").map_err(anyhow::Error::msg)?;
    std::thread::sleep(Duration::from_secs(2));
    s.ok("past due is good standing: Stripe's retries are the grace", seat(api, &ann)["seat"]["good"] == true && ledger(api, &ann)["seat"] == "active", ledger(api, &ann));
    // out of order: the events of active, then canceled, arrive reversed
    s.stripe.hold_events(true);
    s.stripe.set_status(&sub, "active").map_err(anyhow::Error::msg)?;
    s.stripe.set_status(&sub, "canceled").map_err(anyhow::Error::msg)?;
    s.stripe.release_events(true);
    let lapsed = s.eventually(PUSHED, || {
        let v = ledger(api, &ann);
        v["seat"] == "canceled" && v["standing"] == json!({ "standing": "agents_stopped", "why": "seat_canceled" })
    });
    s.ok(
        "canceled, whatever order its events came in, their seat lapses: agents stop, the plan is kept",
        lapsed && seat(api, &ann)["seat"]["good"] == false && ledger(api, &ann)["plan"] == "seat_always_on",
        json!([seat(api, &ann), ledger(api, &ann)]),
    );

    // ---- buying again: a new subscription replaces the one that ended
    let r = api.signed(&ann, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?;
    let session2 = r.body["session"].as_str().unwrap_or("").to_string();
    s.stripe.complete(&session2).map_err(anyhow::Error::msg)?;
    let restored = s.eventually(PUSHED, || {
        let v = ledger(api, &ann);
        v["seat"] == "active" && v["plan"] == "seat"
    });
    s.ok(
        "their org's admin buys again, as a $100 seat: the new subscription replaces the ended one, and the seat is good",
        r.status == 200 && restored && seat(api, &ann)["seat"]["kind"] == "seat" && seat(api, &ann)["org"]["id"] == org.as_str(),
        json!([seat(api, &ann), ledger(api, &ann)]),
    );
    let old = s.stripe.subscription_json(&sub).context("the old subscription")?;
    let (b, sig) = s.stripe.signed_event("customer.subscription.updated", old, None);
    let late = s.stripe.deliver_raw(&b, Some(&sig)).map_err(anyhow::Error::msg)?;
    std::thread::sleep(Duration::from_secs(2));
    s.ok("the ended subscription's late event changes nothing", late == 200 && seat(api, &ann)["seat"]["good"] == true, seat(api, &ann));

    // ---- a webhook lost: the Checkout's return carries it
    let bo = api.person()?;
    let r = api.signed(&bo, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?;
    let session3 = r.body["session"].as_str().unwrap_or("").to_string();
    s.stripe.hold_events(true);
    s.stripe.complete(&session3).map_err(anyhow::Error::msg)?;
    let none_yet = seat(api, &bo)["seat"].is_null();
    let back = api.signed(&bo, "POST", &format!("/api/billing/sessions/{session3}"), Some(&json!({})))?;
    s.ok("with no webhook, the Checkout's return gives them their seat at once", none_yet && back.status == 200 && back.body["seat"]["kind"] == "seat", &back);
    s.stripe.release_events(false);
    std::thread::sleep(Duration::from_secs(1));
    s.ok("its events arriving late change nothing", seat(api, &bo)["seat"] == back.body["seat"], seat(api, &bo));
    let theirs = api.signed(&ann, "POST", &format!("/api/billing/sessions/{session3}"), Some(&json!({})))?;
    let none = api.signed(&bo, "POST", "/api/billing/sessions/cs_nonesuch", Some(&json!({})))?;
    s.ok("another person's Checkout is none of theirs (404), nor one Stripe never made", theirs.status == 404 && none.status == 404, json!([theirs.status, none.status]));

    // ---- the portal: card and invoices, the org's admins only
    let r = api.signed(&ann, "POST", "/api/billing/portal", Some(&json!({})))?;
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/billing_portal/sessions").context("a portal session was asked for")?;
    s.ok(
        "an org's admin opens Stripe's portal, on this deployment's own configuration",
        r.status == 200 && r.body["url"].as_str().is_some_and(|u| u.contains("/portal/")) && asked.form.get("configuration").map(String::as_str) == Some(crate::STRIPE_PORTAL),
        &r,
    );
    let stranger = api.person()?;
    let r = api.signed(&stranger, "POST", "/api/billing/portal", Some(&json!({})))?;
    s.ok("someone in no org opens none (403)", r.status == 403, &r);

    // ---- an org's admin gives seats: each billed from its invite
    let sub2 = subscription_of(s, &org).context("the org's new subscription")?;
    let quantities = |s: &Suite| s.stripe.subscription(&sub2).map(|x| x.items.iter().map(|(_, k, q)| (k.clone(), *q)).collect::<Vec<_>>()).unwrap_or_default();
    let seat_q = |q: u64| vec![("fragment_seat_month".to_string(), q)];
    let cy_email = format!("billing-cy-{}@e2e.test", &Keys::generate().pubkey_hex()[..10]);
    let r = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": cy_email, "kind": "seat" })))?;
    let cy_seat = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let replay = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": cy_email, "kind": "seat" })))?;
    s.ok(
        "an org's admin adds a seat for an email: it waits on it, mailed, and again is the same seat",
        r.status == 200 && r.body["created"] == true && r.body["seat"]["person"].is_null() && r.body["seat"]["comped"] == false && replay.body["created"] == false && replay.body["seat"]["id"] == cy_seat.as_str(),
        json!([r.body, replay.body]),
    );
    if !s.hosted() {
        let sent = s.mail.sent_to(&cy_email);
        s.ok("the email is mailed where to sign in to take it", r.body["mailed"] == true && sent.len() == 1 && sent[0].text.contains("Sign in with this email address to take it"), format!("{sent:?}"));
    }
    let billed = s.eventually(PUSHED, || quantities(s) == seat_q(2));
    s.ok("Stripe bills it from the invite: the subscription's quantity follows the seats", billed, format!("{:?}", quantities(s)));
    let cy = Keys::generate();
    let cy_session = api.sign_in(&cy_email)?;
    api.approve(&cy_session, &cy)?;
    let held = s.eventually(PUSHED, || ledger(api, &cy)["seat"] == "active" && ledger(api, &cy)["plan"] == "seat");
    s.ok("whoever signs in with it holds it, paid, and their ledger follows", held && seat(api, &cy)["seat"]["id"] == cy_seat.as_str() && seat(api, &cy)["seat"]["admin"] == false, seat(api, &cy));
    let r = api.signed(&ann, "PATCH", &format!("/api/org/seats/{cy_seat}"), Some(&json!({ "kind": "seat_always_on" })))?;
    let moved = s.eventually(PUSHED, || {
        let mut q = quantities(s);
        q.sort();
        q == vec![("fragment_seat_always_on_month".to_string(), 1), ("fragment_seat_month".to_string(), 1)]
    });
    let upgraded = s.eventually(PUSHED, || ledger(api, &cy)["plan"] == "seat_always_on");
    s.ok("its admin upgrades it: an item of each kind, and the holder's plan follows", r.status == 200 && moved && upgraded, format!("{:?} {}", quantities(s), ledger(api, &cy)));
    let prorated = s.stripe.requests().iter().filter(|a| a.path == format!("/v1/subscriptions/{sub2}") && a.method == "POST").all(|a| a.form.get("proration_behavior").map(String::as_str) == Some("create_prorations"));
    s.ok("every change is prorated onto the next invoice", prorated, "");

    // the seats are the truth: Stripe's counts changed behind them are pushed back
    s.stripe.set_quantity(&sub2, "fragment_seat_month", 7).map_err(anyhow::Error::msg)?;
    s.stripe.set_status(&sub2, "active").map_err(anyhow::Error::msg)?;
    let repaired = s.eventually(PUSHED, || {
        let mut q = quantities(s);
        q.sort();
        q == vec![("fragment_seat_always_on_month".to_string(), 1), ("fragment_seat_month".to_string(), 1)]
    });
    s.ok("a quantity changed in Stripe's dashboard is pushed back to the seats' count", repaired, format!("{:?}", quantities(s)));

    let not_admin = api.signed(&cy, "POST", "/api/org/seats", Some(&json!({ "email": "x@e2e.test", "kind": "seat" })))?;
    let not_admin_view = api.signed(&cy, "GET", "/api/org", None)?;
    s.ok("a seat's holder who is no admin changes nothing, nor sees the org (403)", not_admin.status == 403 && not_admin_view.status == 403, json!([not_admin.status, not_admin_view.status]));
    let r = api.signed(&ann, "DELETE", &format!("/api/org/seats/{cy_seat}"), None)?;
    let removed = s.eventually(PUSHED, || quantities(s) == seat_q(1));
    let canceled = s.eventually(PUSHED, || ledger(api, &cy)["seat"] == "canceled");
    s.ok("its admin removes it: the quantity goes down, and its holder's seat is canceled", r.status == 200 && removed && canceled, format!("{:?} {}", quantities(s), ledger(api, &cy)));
    let own = seat(api, &ann)["seat"]["id"].as_str().unwrap_or("").to_string();
    let last = api.signed(&ann, "DELETE", &format!("/api/org/seats/{own}"), None)?;
    s.ok("the org's last paid seat is not removed here: its subscription is canceled in the portal (400)", last.status == 400 && last.message().contains("cancel"), &last);

    // admins: one at least
    let dee = api.person()?;
    let r = api.signed(&ann, "POST", "/api/org/admins", Some(&json!({ "email": Api::email_of(&dee) })))?;
    let dee_row = r.body["admin"]["id"].as_str().unwrap_or("").to_string();
    let sees = api.signed(&dee, "GET", "/api/org", None)?;
    s.ok("an admin adds another, who sees the org (and holds no seat)", r.status == 200 && r.body["created"] == true && sees.status == 200 && seat(api, &dee)["seat"].is_null(), &sees);
    let ann_row = sees.body["members"].as_array().into_iter().flatten().find(|m| m["person"] == ann_id.as_str()).and_then(|m| m["id"].as_str()).unwrap_or("").to_string();
    let r = api.signed(&dee, "DELETE", &format!("/api/org/admins/{ann_row}"), None)?;
    let kept = seat(api, &ann);
    s.ok("an admin removes another; one with a seat keeps it", r.status == 200 && kept["admin"] == false && kept["seat"]["good"] == true, &kept);
    let last_admin = api.signed(&dee, "DELETE", &format!("/api/org/admins/{dee_row}"), None)?;
    s.ok("the last admin stays (400)", last_admin.status == 400, &last_admin);
    let r = api.signed(&dee, "POST", "/api/org/admins", Some(&json!({ "email": Api::email_of(&ann) })))?;
    s.ok("and makes them admin again", r.status == 200 && seat(api, &ann)["admin"] == true, &r);

    // ---- invalid
    let refused = [
        ("a kind that is no seat's (400)", api.signed(&stranger, "POST", "/api/billing/checkout", Some(&json!({ "kind": "guest" })))?, 400),
        ("a field it does not take (400)", api.signed(&stranger, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat", "quantity": 3 })))?, 400),
        ("unsigned (401)", api.unsigned("POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?, 401),
    ];
    for (what, r, status) in refused {
        s.ok(&format!("refused: {what}"), r.status == status, &r);
    }
    Ok(())
}
