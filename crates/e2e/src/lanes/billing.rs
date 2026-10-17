//! Seats bought through Stripe (docs/billing.md; docs/api.md, Billing), on
//! the Stripe fake: a person's Checkout for their own seat, which makes
//! nothing until it is paid, then an org of their own; the seat theirs
//! once Stripe says it is paid, by its webhook or by the Checkout's return
//! when no webhook comes; the subscription's lapse and a second purchase,
//! on the org's own customer; the portal. Every subscription the platform
//! writes is fetched from Stripe again, so a forged, stale, foreign,
//! repeated or out-of-order event changes nothing it should not. An org's
//! seats and admins are offers, no one's until they accept them; who pays
//! is no other admin's to remove. Valid, invalid, replay, and a webhook
//! lost. Then the security audit's H1, each of its steps refused
//! (`consent`), and its M4: a wipe cancels what the wiped would pay, its
//! customer kept, across a restart mid-step (`wiped`).

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::Api;
use crate::browser::{Lease, Page};
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
    let op_session = api.sign_in("operator@e2e.test")?;
    // the ledger section approves the operator's key when it runs first
    let _ = api.approve(&op_session, &s.operator);

    // ---- a person buys a $200 seat: nothing is made until they pay
    let ann = api.person()?;
    let ann_id = api.identity(&ann)?;
    let ann_email = Api::email_of(&ann);
    let customers = s.stripe.customers().len();
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
        "it takes a card always, its tax, promotion codes, one seat at its lookup key's price, names this deployment, the org it will make and its buyer, bills a customer Stripe makes for the buyer's email (none named), and returns to the welcome, paid or canceled",
        f("mode") == "subscription"
            && f("payment_method_collection") == "always"
            && f("automatic_tax[enabled]") == "true"
            && f("allow_promotion_codes") == "true"
            && f("customer_email") == ann_email
            && !asked.form.contains_key("customer")
            && !asked.form.keys().any(|k| k.starts_with("customer_update"))
            && f("line_items[0][quantity]") == "1"
            && f("metadata[fragment_deployment]") == api.base
            && f("subscription_data[metadata][fragment_deployment]") == api.base
            && f("subscription_data[metadata][fragment_org]") == org
            && f("metadata[fragment_person]") == ann_id
            && f("metadata[fragment_kind]") == "seat_always_on"
            && f("success_url").ends_with("/welcome?checkout={CHECKOUT_SESSION_ID}")
            && f("cancel_url").ends_with("/welcome?checkout=canceled")
            && fragment_core::org::valid_org_id(&org),
        format!("{:?}", asked.form),
    );
    let before = seat(api, &ann);
    let none = api.signed(&s.operator, "GET", &format!("/api/admin/orgs/{org}"), None)?;
    s.ok(
        "before they pay, nothing is made: they hold no seat and are in no org, the org their Checkout names is none (404), and Stripe made no customer",
        before["seat"].is_null() && before["org"].is_null() && before["admin"] == false && none.status == 404 && s.stripe.customers().len() == customers,
        json!({ "seat": before, "org": none.status, "customers": [customers, s.stripe.customers().len()] }),
    );
    s.stripe.complete(&session).map_err(anyhow::Error::msg)?;
    let pushed = s.eventually(PUSHED, || {
        let v = ledger(api, &ann);
        v["plan"] == "seat_always_on" && v["seat"] == "active"
    });
    let mine = seat(api, &ann);
    let view = api.signed(&ann, "GET", "/api/org", None)?;
    s.ok(
        "paid, Stripe's webhook makes the org their Checkout named: they admin it and hold its seat, paid not comped, they pay for it, and its plan reaches their ledger",
        pushed
            && mine["seat"]["kind"] == "seat_always_on"
            && mine["seat"]["comped"] == false
            && mine["seat"]["good"] == true
            && mine["admin"] == true
            && mine["org"]["id"] == org.as_str()
            && view.body["payer"] == ann_id.as_str()
            && view.body["members"].as_array().map(Vec::len) == Some(1),
        json!([mine, ledger(api, &ann), view.body]),
    );
    let delivered = s.stripe.delivered();
    s.ok("each event was answered 200", !delivered.is_empty() && delivered.iter().all(|(_, _, status)| *status == 200), format!("{delivered:?}"));
    let back = api.signed(&ann, "POST", &format!("/api/billing/sessions/{session}"), Some(&json!({})))?;
    s.ok("the Checkout's return again changes nothing, and answers their seat", back.status == 200 && back.body["seat"] == mine["seat"], &back);
    let again = api.signed(&ann, "POST", "/api/billing/checkout", Some(&json!({})))?;
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

    // ---- buying again: a new subscription replaces the one that ended,
    // on the org's own customer, for its paid seats
    let hundred = api.signed(&ann, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?;
    let r = api.signed(&ann, "POST", "/api/billing/checkout", Some(&json!({})))?;
    let session2 = r.body["session"].as_str().unwrap_or("").to_string();
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("a second Checkout was asked for")?;
    let own = s.stripe.subscription(&sub).map(|x| x.customer).context("the ended subscription's customer")?;
    let reused = asked.form.get("customer") == Some(&own) && !asked.form.contains_key("customer_email") && asked.form.get("line_items[0][quantity]").map(String::as_str) == Some("1");
    s.stripe.complete(&session2).map_err(anyhow::Error::msg)?;
    let restored = s.eventually(PUSHED, || {
        let v = ledger(api, &ann);
        v["seat"] == "active" && v["plan"] == "seat_always_on"
    });
    s.ok(
        "their org's admin buys again, naming no kind (the seat sold; a $100 one is refused, 400), on its own customer (none made) for its one paid seat: the new subscription replaces the ended one, and the seat is good",
        hundred.status == 400
            && hundred.message().contains("not sold")
            && r.status == 200
            && reused
            && restored
            && seat(api, &ann)["seat"]["kind"] == "seat_always_on"
            && seat(api, &ann)["org"]["id"] == org.as_str(),
        json!([hundred.body, seat(api, &ann), ledger(api, &ann), format!("{:?}", asked.form)]),
    );
    let old = s.stripe.subscription_json(&sub).context("the old subscription")?;
    let (b, sig) = s.stripe.signed_event("customer.subscription.updated", old, None);
    let late = s.stripe.deliver_raw(&b, Some(&sig)).map_err(anyhow::Error::msg)?;
    std::thread::sleep(Duration::from_secs(2));
    s.ok("the ended subscription's late event changes nothing", late == 200 && seat(api, &ann)["seat"]["good"] == true, seat(api, &ann));

    // ---- a webhook lost: the Checkout's return carries it
    let bo = api.person()?;
    let r = api.signed(&bo, "POST", "/api/billing/checkout", Some(&json!({})))?;
    let session3 = r.body["session"].as_str().unwrap_or("").to_string();
    s.stripe.hold_events(true);
    s.stripe.complete(&session3).map_err(anyhow::Error::msg)?;
    let none_yet = seat(api, &bo)["seat"].is_null();
    let back = api.signed(&bo, "POST", &format!("/api/billing/sessions/{session3}"), Some(&json!({})))?;
    s.ok("with no webhook, the Checkout's return gives them their seat at once", none_yet && back.status == 200 && back.body["seat"]["kind"] == "seat_always_on", &back);
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
    let seat_q = |q: u64| vec![("fragment_seat_always_on_month".to_string(), q)];
    let cy_email = format!("billing-cy-{}@e2e.test", &Keys::generate().pubkey_hex()[..10]);
    let hundred = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": cy_email, "kind": "seat" })))?;
    let r = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": cy_email })))?;
    let cy_seat = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let replay = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": cy_email, "kind": "seat_always_on" })))?;
    s.ok(
        "an org's admin offers a seat to an email, the always-on one sold (a $100 seat is refused, 400): an offer, no one's, mailed; again it is the same offer",
        hundred.status == 400
            && hundred.message().contains("not sold")
            && r.status == 200
            && r.body["created"] == true
            && r.body["seat"]["person"].is_null()
            && r.body["seat"]["seat"] == "seat_always_on"
            && r.body["seat"]["comped"] == false
            && replay.body["created"] == false
            && replay.body["seat"]["id"] == cy_seat.as_str(),
        json!([hundred.body, r.body, replay.body]),
    );
    if !s.hosted() {
        let sent = s.mail.sent_to(&cy_email);
        s.ok(
            "the email is mailed who offers it, and where to accept or decline it: nothing is theirs until they accept",
            r.body["mailed"] == true && sent.len() == 1 && sent[0].text.contains("offers you") && sent[0].text.contains(&ann_email) && sent[0].text.contains("accept or decline"),
            format!("{sent:?}"),
        );
    }
    let billed = s.eventually(PUSHED, || quantities(s) == seat_q(2));
    s.ok("Stripe bills it from the offer: the subscription's quantity follows the seats", billed, format!("{:?}", quantities(s)));
    let cy = Keys::generate();
    let cy_session = api.sign_in(&cy_email)?;
    api.approve(&cy_session, &cy)?;
    let waiting = seat(api, &cy);
    let offer = &waiting["offered"][0];
    s.ok(
        "signing in with the email attaches nothing: no seat, no org; they see the offer, its org's name, the seat, and who made it",
        waiting["seat"].is_null()
            && waiting["org"].is_null()
            && waiting["offered"].as_array().map(Vec::len) == Some(1)
            && offer["id"] == cy_seat.as_str()
            && offer["org"]["id"] == org.as_str()
            && offer["kind"] == "seat_always_on"
            && offer["admin"] == false
            && offer["from"] == ann_email.as_str()
            && ledger(api, &cy)["plan"] != "seat_always_on",
        &waiting,
    );
    let accepted = api.signed(&cy, "POST", &format!("/api/seat/offers/{cy_seat}/accept"), Some(&json!({})))?;
    let held = s.eventually(PUSHED, || ledger(api, &cy)["seat"] == "active" && ledger(api, &cy)["plan"] == "seat_always_on");
    s.ok(
        "they accept it: the seat is theirs, paid, in its org, and their ledger follows",
        accepted.status == 200 && accepted.body["seat"]["id"] == cy_seat.as_str() && accepted.body["seat"]["admin"] == false && accepted.body["offered"] == json!([]) && held,
        json!([accepted.body, ledger(api, &cy)]),
    );
    let again = api.signed(&cy, "POST", &format!("/api/seat/offers/{cy_seat}/accept"), Some(&json!({})))?;
    let declined = api.signed(&cy, "POST", &format!("/api/seat/offers/{cy_seat}/decline"), Some(&json!({})))?;
    let stranger_takes = api.signed(&api.person()?, "POST", &format!("/api/seat/offers/{cy_seat}/accept"), Some(&json!({})))?;
    s.ok(
        "accepting it again answers the same seat (a replay); declining it now, or anyone else accepting it, finds no offer (404)",
        again.status == 200 && again.body["seat"] == accepted.body["seat"] && declined.status == 404 && stranger_takes.status == 404 && seat(api, &cy)["seat"]["id"] == cy_seat.as_str(),
        json!([again.status, declined.status, stranger_takes.status]),
    );
    let r = api.signed(&ann, "PATCH", &format!("/api/org/seats/{cy_seat}"), Some(&json!({ "kind": "seat" })))?;
    s.ok("a paid seat's kind changes nowhere: one seat is sold, so there is no route (404)", r.status == 404 && seat(api, &cy)["seat"]["kind"] == "seat_always_on", &r);
    let prorated = s.stripe.requests().iter().filter(|a| a.path == format!("/v1/subscriptions/{sub2}") && a.method == "POST").all(|a| a.form.get("proration_behavior").map(String::as_str) == Some("create_prorations"));
    s.ok("every change is prorated onto the next invoice", prorated, "");

    // the seats are the truth: Stripe's count changed behind them is pushed back
    s.stripe.set_quantity(&sub2, "fragment_seat_always_on_month", 7).map_err(anyhow::Error::msg)?;
    s.stripe.set_status(&sub2, "active").map_err(anyhow::Error::msg)?;
    let repaired = s.eventually(PUSHED, || quantities(s) == seat_q(2));
    s.ok("a quantity changed in Stripe's dashboard is pushed back to the seats' count", repaired, format!("{:?}", quantities(s)));

    let not_admin = api.signed(&cy, "POST", "/api/org/seats", Some(&json!({ "email": "x@e2e.test" })))?;
    let not_admin_view = api.signed(&cy, "GET", "/api/org", None)?;
    s.ok("a seat's holder who is no admin changes nothing, nor sees the org (403)", not_admin.status == 403 && not_admin_view.status == 403, json!([not_admin.status, not_admin_view.status]));
    let r = api.signed(&ann, "DELETE", &format!("/api/org/seats/{cy_seat}"), None)?;
    let removed = s.eventually(PUSHED, || quantities(s) == seat_q(1));
    let canceled = s.eventually(PUSHED, || ledger(api, &cy)["seat"] == "canceled");
    s.ok("its admin removes it: the quantity goes down, and its holder's seat is canceled", r.status == 200 && removed && canceled, format!("{:?} {}", quantities(s), ledger(api, &cy)));
    let own = seat(api, &ann)["seat"]["id"].as_str().unwrap_or("").to_string();
    let last = api.signed(&ann, "DELETE", &format!("/api/org/seats/{own}"), None)?;
    s.ok("the org's last paid seat is not removed here: its subscription is canceled in the portal (400)", last.status == 400 && last.message().contains("cancel"), &last);

    // ---- admins: offered too, one at least, and who pays steps down only themselves
    let dee = api.person()?;
    let r = api.signed(&ann, "POST", "/api/org/admins", Some(&json!({ "email": Api::email_of(&dee) })))?;
    let dee_row = r.body["admin"]["id"].as_str().unwrap_or("").to_string();
    let offered = seat(api, &dee);
    let unseen = api.signed(&dee, "GET", "/api/org", None)?;
    s.ok(
        "an admin offers someone in no org an admin's place: an offer only; they are in no org, see it offered (an admin's place, no seat), and nothing of the org (403)",
        r.status == 200
            && r.body["created"] == true
            && r.body["admin"]["person"].is_null()
            && offered["org"].is_null()
            && offered["admin"] == false
            && offered["offered"][0]["id"] == dee_row.as_str()
            && offered["offered"][0]["admin"] == true
            && offered["offered"][0]["kind"].is_null()
            && unseen.status == 403,
        json!([r.body, offered, unseen.status]),
    );
    let accepted = api.signed(&dee, "POST", &format!("/api/seat/offers/{dee_row}/accept"), Some(&json!({})))?;
    let sees = api.signed(&dee, "GET", "/api/org", None)?;
    s.ok("they accept it: an admin, who sees the org and holds no seat", accepted.status == 200 && accepted.body["admin"] == true && sees.status == 200 && seat(api, &dee)["seat"].is_null(), &sees);
    let ann_row = sees.body["members"].as_array().into_iter().flatten().find(|m| m["person"] == ann_id.as_str()).and_then(|m| m["id"].as_str()).unwrap_or("").to_string();
    let removed = api.signed(&dee, "DELETE", &format!("/api/org/admins/{ann_row}"), None)?;
    let portal = api.signed(&dee, "POST", "/api/billing/portal", Some(&json!({})))?;
    s.ok(
        "who pays for the org is no other admin's to remove (403), and only they open its billing: another admin's portal is 403",
        removed.status == 403 && removed.message().contains("pays for this org") && seat(api, &ann)["admin"] == true && portal.status == 403 && seat(api, &ann)["pays"] == true && seat(api, &dee)["pays"] == false,
        json!([removed.body, portal.body]),
    );
    let r = api.signed(&ann, "DELETE", &format!("/api/org/admins/{dee_row}"), None)?;
    let gone = api.signed(&dee, "GET", "/api/org", None)?;
    s.ok("who pays removes another admin; one with no seat leaves the org with it", r.status == 200 && gone.status == 403 && seat(api, &dee)["org"].is_null(), &r);
    let last_admin = api.signed(&ann, "DELETE", &format!("/api/org/admins/{ann_row}"), None)?;
    s.ok("the last admin stays (400)", last_admin.status == 400, &last_admin);

    // ---- credit packs: $25, kept until spent, once by their Checkout
    let purchased = |k: &Keys| ledger(api, k)["purchasedMicros"].as_i64().unwrap_or(-1);
    let before = purchased(&ann);
    let r = api.signed(&ann, "POST", "/api/billing/packs", Some(&json!({})))?;
    let pack1 = r.body["session"].as_str().unwrap_or("").to_string();
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("a pack's Checkout")?;
    let f = |k: &str| asked.form.get(k).cloned().unwrap_or_default();
    s.ok(
        "a seat's holder opens a pack's Checkout: a payment at the pack's price, for their own ledger",
        r.status == 200 && f("mode") == "payment" && f("metadata[fragment_person]") == ann_id && f("metadata[fragment_buyer]") == ann_id && f("metadata[fragment_pack]") == "25",
        format!("{:?}", asked.form),
    );
    s.stripe.complete(&pack1).map_err(anyhow::Error::msg)?;
    let granted = s.eventually(PUSHED, || purchased(&ann) == before + 25_000_000);
    let back = api.signed(&ann, "POST", &format!("/api/billing/sessions/{pack1}"), Some(&json!({})))?;
    std::thread::sleep(Duration::from_secs(1));
    s.ok("paid, $25 of credit is theirs, once (its return again adds nothing)", granted && back.status == 200 && purchased(&ann) == before + 25_000_000, ledger(api, &ann));
    let fay = api.person()?;
    let r = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&fay) })))?;
    let fay_row = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let waits = api.signed(&ann, "POST", "/api/billing/packs", Some(&json!({ "member": fay_row })))?;
    let took = api.signed(&fay, "POST", &format!("/api/seat/offers/{fay_row}/accept"), Some(&json!({})))?;
    s.ok(
        "a seat offered to someone in no org is theirs once they accept it (no credit is bought for it before: 400)",
        r.status == 200 && r.body["seat"]["person"].is_null() && waits.status == 400 && took.status == 200 && took.body["seat"]["id"] == fay_row.as_str(),
        json!([r.body, waits.body, took.body]),
    );
    let fay_before = purchased(&fay);
    let r = api.signed(&ann, "POST", "/api/billing/packs", Some(&json!({ "member": fay_row })))?;
    let pack2 = r.body["session"].as_str().unwrap_or("").to_string();
    s.stripe.complete(&pack2).map_err(anyhow::Error::msg)?;
    let theirs = s.eventually(PUSHED, || purchased(&fay) == fay_before + 25_000_000);
    s.ok("an org's admin buys a pack for a member who holds a seat: the credit is the member's", r.status == 200 && theirs && purchased(&ann) == before + 25_000_000, ledger(api, &fay));
    let not_admin = api.signed(&fay, "POST", "/api/billing/packs", Some(&json!({ "member": fay_row })))?;
    let seatless = api.signed(&api.person()?, "POST", "/api/billing/packs", Some(&json!({})))?;
    s.ok("a member who is no admin buys none for others (403); someone with no seat buys none (400)", not_admin.status == 403 && seatless.status == 400, json!([not_admin.status, seatless.status]));

    // ---- an admin's place for one of its members is an offer too; its
    // accept merges it into their row, and replays
    let r = api.signed(&ann, "POST", "/api/org/admins", Some(&json!({ "email": Api::email_of(&fay) })))?;
    let promo = r.body["admin"]["id"].as_str().unwrap_or("").to_string();
    let unseen = api.signed(&fay, "GET", "/api/org", None)?;
    let waiting = seat(api, &fay);
    let took = answer(api, &fay, &promo, "accept")?;
    let again = answer(api, &fay, &promo, "accept")?;
    s.ok(
        "an admin's place for one of the org's members is an offer: they are no admin, and see nothing of the org (403), until they accept it; accepted, it joins their own row (their seat kept), and accepting it again answers the same (a replay)",
        r.status == 200
            && r.body["admin"]["person"].is_null()
            && promo != fay_row
            && unseen.status == 403
            && waiting["admin"] == false
            && waiting["offered"][0]["id"] == promo.as_str()
            && took.status == 200
            && took.body["admin"] == true
            && took.body["seat"]["id"] == fay_row.as_str()
            && took.body["offered"] == json!([])
            && again.status == 200
            && again.body == took.body,
        json!([r.body, took.body, again.body]),
    );

    // ---- invalid
    let refused = [
        ("the $100 seat, no longer sold (400)", api.signed(&stranger, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat" })))?, 400),
        ("a kind that is no seat's (400)", api.signed(&stranger, "POST", "/api/billing/checkout", Some(&json!({ "kind": "guest" })))?, 400),
        ("a field it does not take (400)", api.signed(&stranger, "POST", "/api/billing/checkout", Some(&json!({ "kind": "seat_always_on", "quantity": 3 })))?, 400),
        ("unsigned (401)", api.unsigned("POST", "/api/billing/checkout", Some(&json!({})))?, 401),
        ("an offer that is none (404)", api.signed(&stranger, "POST", "/api/seat/offers/mem-0000000000000000/accept", Some(&json!({})))?, 404),
        ("an offer's id that is no id (404)", api.signed(&stranger, "POST", "/api/seat/offers/nonesuch/decline", Some(&json!({})))?, 404),
        ("an answer with a body (400)", api.signed(&stranger, "POST", &format!("/api/seat/offers/{cy_seat}/accept"), Some(&json!({ "seat": "seat_always_on" })))?, 400),
        ("an answer unsigned (401)", api.unsigned("POST", &format!("/api/seat/offers/{cy_seat}/accept"), Some(&json!({})))?, 401),
        ("leaving with no org (400)", api.signed(&stranger, "POST", "/api/seat/leave", Some(&json!({})))?, 400),
    ];
    for (what, r, status) in refused {
        s.ok(&format!("refused: {what}"), r.status == status, &r);
    }
    let payer = consent(s, api)?;
    payer_goes(s, api)?;
    orphan(s, api)?;
    wiped(s, api, &payer)
}

/// The admins' view of `org`, as an operator lists it (its customer and
/// subscription the registry keeps), or null.
fn listed_org(s: &Suite, api: &Api, org: &str) -> Value {
    let all = api.signed(&s.operator, "GET", "/api/admin/orgs", None).map(|r| r.body).unwrap_or(Value::Null);
    all["orgs"].as_array().into_iter().flatten().find(|o| o["id"] == org).cloned().unwrap_or(Value::Null)
}

/// Who pays (the review of #367): their seat and admin's place are no
/// other admin's, and only they open its billing; stepping down, though
/// their subscription is set to end at its period's end (theirs to renew),
/// cancels it at once, deletes its pending prorations, and the org forgets
/// their customer; the lapsed org's offer is refused; and another admin
/// buying it again bills a customer of their own.
fn payer_goes(s: &mut Suite, api: &Api) -> Result<()> {
    let pia = api.person()?;
    let p_org = buys(s, api, &pia)?;
    let p_sub = subscription_of(s, &p_org).context("the payer's subscription")?;
    let pia_customer = s.stripe.subscription(&p_sub).map(|x| x.customer).unwrap_or_default();
    let quin = api.person()?;
    let r = api.signed(&pia, "POST", "/api/org/admins", Some(&json!({ "email": Api::email_of(&quin) })))?;
    answer(api, &quin, r.body["admin"]["id"].as_str().unwrap_or(""), "accept")?;
    let rho = api.person()?;
    let r = api.signed(&quin, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&rho) })))?;
    answer(api, &rho, r.body["seat"]["id"].as_str().unwrap_or(""), "accept")?;
    let prorated = s.eventually(PUSHED, || !s.stripe.pending_items(&p_sub).is_empty());
    let view = api.signed(&quin, "GET", "/api/org", None)?;
    let pia_id = api.identity(&pia)?;
    let pia_row = view.body["members"].as_array().into_iter().flatten().find(|m| m["person"] == pia_id.as_str()).and_then(|m| m["id"].as_str()).unwrap_or("").to_string();
    let seat_taken = api.signed(&quin, "DELETE", &format!("/api/org/seats/{pia_row}"), None)?;
    let place_taken = api.signed(&quin, "DELETE", &format!("/api/org/admins/{pia_row}"), None)?;
    let theirs = api.signed(&quin, "POST", "/api/billing/portal", Some(&json!({})))?;
    let own = api.signed(&pia, "POST", "/api/billing/portal", Some(&json!({})))?;
    s.ok(
        "who pays: another admin takes neither their seat nor their admin's place (403), nor opens its billing (403); they open it themselves",
        seat_taken.status == 403 && place_taken.status == 403 && theirs.status == 403 && own.status == 200 && view.body["payer"] == pia_id.as_str(),
        json!([seat_taken.body, place_taken.body, theirs.body, own.status]),
    );

    // an offer waiting, answered after the org lapses
    let sam = api.person()?;
    let r = api.signed(&quin, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&sam) })))?;
    let sam_offer = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    // set to end at its period's end (the portal's), still theirs to renew: they step down
    s.stripe.set_cancel_at_end(&p_sub, true).map_err(anyhow::Error::msg)?;
    let set = s.eventually(PUSHED, || listed_org(s, api, &p_org)["cancelAtEnd"] == true);
    let stepped = api.signed(&pia, "DELETE", &format!("/api/org/admins/{pia_row}"), None)?;
    let canceled = s.eventually(PUSHED, || s.stripe.subscription(&p_sub).is_some_and(|x| x.status == "canceled"));
    let forgotten = s.eventually(PUSHED, || {
        let o = listed_org(s, api, &p_org);
        o["customer"].is_null() && o["subscription"].is_null() && o["id"] == p_org.as_str()
    });
    let lapsed = s.eventually(PUSHED, || seat(api, &rho)["seat"]["good"] == false);
    s.ok(
        "who pays steps down, their subscription set to end at its period's end: it is canceled at once, its pending prorations deleted, the org forgets their customer (kept at Stripe), and its paid seats lapse",
        set && stepped.status == 200
            && prorated
            && canceled
            && s.stripe.pending_items(&p_sub).is_empty()
            && forgotten
            && s.stripe.customers().contains(&pia_customer)
            && lapsed
            && seat(api, &pia)["pays"] == false,
        json!({ "stepped": stepped.body, "org": listed_org(s, api, &p_org), "pending": s.stripe.pending_items(&p_sub) }),
    );
    let late = answer(api, &sam, &sam_offer, "accept")?;
    let portal = api.signed(&quin, "POST", "/api/billing/portal", Some(&json!({})))?;
    s.ok(
        "the lapsed org's offer is refused (400) until it pays again, and its billing is no one's (403)",
        late.status == 400 && late.message().contains("pays for nothing") && seat(api, &sam)["org"].is_null() && portal.status == 403,
        json!([late.body, portal.body]),
    );

    // another admin buys it again: on a customer of their own
    let paid_seats = api.signed(&quin, "GET", "/api/org", None)?.body["members"].as_array().map(|m| m.iter().filter(|m| m["seat"].is_string() && m["comped"] == false).count()).unwrap_or(0);
    let r = api.signed(&quin, "POST", "/api/billing/checkout", Some(&json!({})))?;
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("the co-admin's Checkout")?;
    s.stripe.complete(r.body["session"].as_str().unwrap_or("")).map_err(anyhow::Error::msg)?;
    let pays = s.eventually(PUSHED, || seat(api, &quin)["seat"]["good"] == true);
    let again = subscription_of(s, &p_org).and_then(|id| s.stripe.subscription(&id)).map(|x| x.customer).unwrap_or_default();
    s.ok(
        "another admin buys the org again on a customer of their own (its email theirs), never the one who went's, for its paid seats and theirs; now they pay",
        r.status == 200
            && !asked.form.contains_key("customer")
            && asked.form.get("customer_email") == Some(&Api::email_of(&quin))
            && asked.form.get("line_items[0][quantity]") == Some(&(paid_seats + 1).to_string())
            && pays
            && !again.is_empty()
            && again != pia_customer
            && seat(api, &quin)["pays"] == true,
        json!({ "form": format!("{:?}", asked.form), "customer": again }),
    );
    Ok(())
}

/// A Checkout whose completion never reaches the platform (its webhook
/// lost, its buyer's return never made): its subscription's event names an
/// org the registry lacks, and the platform finds the Checkout that made
/// it and seats its buyer.
fn orphan(s: &mut Suite, api: &Api) -> Result<()> {
    let ola = api.person()?;
    let r = api.signed(&ola, "POST", "/api/billing/checkout", Some(&json!({})))?;
    let session = r.body["session"].as_str().unwrap_or("").to_string();
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("the Checkout")?;
    let named = asked.form.get("metadata[fragment_org]").cloned().unwrap_or_default();
    s.stripe.hold_events(true);
    s.stripe.complete(&session).map_err(anyhow::Error::msg)?;
    s.stripe.drop_held("checkout.session.completed");
    s.stripe.release_events(false);
    let seated = s.eventually(PUSHED, || seat(api, &ola)["seat"]["good"] == true);
    s.ok(
        "a Checkout's completion lost, and no return: the subscription's own event names an org not made yet, and the platform finds the Checkout that made it and makes the org, its buyer seated",
        r.status == 200 && seated && org_of(api, &ola) == named,
        seat(api, &ola),
    );
    Ok(())
}

/// The id of the org `keys` is in (`GET /api/seat`'s), or "".
fn org_of(api: &Api, keys: &Keys) -> String {
    seat(api, keys)["org"]["id"].as_str().unwrap_or("").to_string()
}

/// `keys` buys their own seat on the fake's Checkout and holds it, paid.
/// Answers the org it made.
fn buys(s: &Suite, api: &Api, keys: &Keys) -> Result<String> {
    let r = api.signed(keys, "POST", "/api/billing/checkout", Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "a Checkout: {r}");
    s.stripe.complete(r.body["session"].as_str().unwrap_or("")).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(s.eventually(PUSHED, || seat(api, keys)["seat"]["good"] == true), "the seat bought holds: {}", seat(api, keys));
    Ok(org_of(api, keys))
}

/// `who` answers the offer `id` (`accept` or `decline`).
fn answer(api: &Api, who: &Keys, id: &str, verb: &str) -> Result<crate::api::Reply> {
    api.signed(who, "POST", &format!("/api/seat/offers/{id}/{verb}"), Some(&json!({})))
}

/// The security audit's H1 (2026-10-17), each step refused: no org or
/// customer before a payment; an admin's place or a seat offered attaches
/// no one, at an offer or a sign-in, until they accept; someone offered
/// buys their own seat on a customer of their own, never the offering
/// org's; they decline; and their own yes elsewhere moves them unless it
/// leaves people with no admin (the last admin is refused), an org of one
/// ending with them, its subscription canceled, and one with only offers
/// waiting too, the offers withdrawn. A seat's holder leaves by their own
/// yes. Answers a person whose org pays, for `wiped`'s offers.
fn consent(s: &mut Suite, api: &Api) -> Result<Keys> {
    let mal = api.person()?;
    let mal_email = Api::email_of(&mal);
    let vic = api.person()?;
    let vic_email = Api::email_of(&vic);

    // 1. a Checkout begun and left makes nothing, so it offers no one anything
    let customers = s.stripe.customers().len();
    let r = api.signed(&mal, "POST", "/api/billing/checkout", Some(&json!({})))?;
    let session = r.body["session"].as_str().unwrap_or("").to_string();
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("a Checkout was asked for")?;
    let named = asked.form.get("metadata[fragment_org]").cloned().unwrap_or_default();
    let looked = api.signed(&s.operator, "GET", &format!("/api/admin/orgs/{named}"), None)?;
    let forced = api.signed(&mal, "POST", "/api/org/admins", Some(&json!({ "email": vic_email })))?;
    let seated = api.signed(&mal, "POST", "/api/org/seats", Some(&json!({ "email": vic_email })))?;
    s.ok(
        "H1: a Checkout begun and left makes no org (the one it names is none: 404) and no customer, so its buyer admins nothing and offers no one an admin's place or a seat (403)",
        r.status == 200 && seat(api, &mal)["org"].is_null() && looked.status == 404 && s.stripe.customers().len() == customers && forced.status == 403 && seated.status == 403,
        json!({ "org": looked.status, "admins": forced.body, "seats": seated.body }),
    );

    // 2. paid, its admin offers; an offer attaches no one, at once or at a sign-in
    s.stripe.complete(&session).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(s.eventually(PUSHED, || seat(api, &mal)["seat"]["good"] == true), "the attacker's seat holds");
    let a_org = org_of(api, &mal);
    let r = api.signed(&mal, "POST", "/api/org/admins", Some(&json!({ "email": vic_email })))?;
    let theirs = seat(api, &vic);
    let unseen = api.signed(&vic, "GET", "/api/org", None)?;
    let again = api.sign_in(&vic_email).map(|_| seat(api, &vic))?;
    s.ok(
        "H1: an admin's place offered to someone in no org attaches nothing: they are in none, and see the offer, its org and who made it, and nothing of the org (403); signing in again attaches nothing",
        r.status == 200
            && r.body["admin"]["person"].is_null()
            && theirs["org"].is_null()
            && theirs["admin"] == false
            && theirs["offered"][0]["org"]["id"] == a_org.as_str()
            && theirs["offered"][0]["admin"] == true
            && theirs["offered"][0]["from"] == mal_email.as_str()
            && unseen.status == 403
            && again["org"].is_null(),
        json!([r.body, theirs, unseen.status, again]),
    );
    let new_email = format!("billing-new-{}@e2e.test", &Keys::generate().pubkey_hex()[..10]);
    let r = api.signed(&mal, "POST", "/api/org/seats", Some(&json!({ "email": new_email })))?;
    let nu = Keys::generate();
    let nu_session = api.sign_in(&new_email)?;
    api.approve(&nu_session, &nu)?;
    let fresh = seat(api, &nu);
    s.ok(
        "H1: a seat offered to an email no one signs in with attaches no one at their first sign-in: they hold none, in no org, until they accept",
        r.status == 200 && fresh["seat"].is_null() && fresh["org"].is_null() && fresh["offered"][0]["kind"] == "seat_always_on" && ledger(api, &nu)["plan"] != "seat_always_on",
        &fresh,
    );

    // 3. offered, they buy their own seat: never on the offering org's customer
    let a_customer = subscription_of(s, &a_org).and_then(|id| s.stripe.subscription(&id)).map(|x| x.customer).context("the attacker's org's customer")?;
    let r = api.signed(&vic, "POST", "/api/billing/checkout", Some(&json!({})))?;
    let asked = s.stripe.requests().into_iter().rev().find(|a| a.path == "/v1/checkout/sessions").context("the victim's Checkout")?;
    let mal_id = api.identity(&mal)?;
    let none_other = s.stripe.requests().iter().filter(|q| q.path == "/v1/checkout/sessions").all(|q| q.form.get("customer") != Some(&a_customer) || q.form.get("metadata[fragment_person]") == Some(&mal_id));
    s.ok(
        "H1: with an offer waiting, they buy their own seat: a Checkout for a new org of theirs, on a customer Stripe makes for their email, and no Checkout but its buyer's own ever bills the offering org's customer",
        r.status == 200
            && !asked.form.contains_key("customer")
            && asked.form.get("customer_email") == Some(&vic_email)
            && asked.form.get("metadata[fragment_org]").is_some_and(|o| *o != a_org)
            && none_other,
        format!("{:?}", asked.form),
    );
    s.stripe.complete(r.body["session"].as_str().unwrap_or("")).map_err(anyhow::Error::msg)?;
    let paid = s.eventually(PUSHED, || seat(api, &vic)["seat"]["good"] == true);
    let b_org = org_of(api, &vic);
    let b_sub = subscription_of(s, &b_org).context("the victim's subscription")?;
    s.ok(
        "paid, they admin an org of their own, its customer their own, and the offer still waits on their answer",
        paid && b_org != a_org && seat(api, &vic)["admin"] == true && seat(api, &vic)["offered"].as_array().map(Vec::len) == Some(1) && s.stripe.subscription(&b_sub).is_some_and(|x| x.customer != a_customer),
        seat(api, &vic),
    );

    // 4. they decline it
    let offer = seat(api, &vic)["offered"][0]["id"].as_str().unwrap_or("").to_string();
    let declined = answer(api, &vic, &offer, "decline")?;
    let twice = answer(api, &vic, &offer, "decline")?;
    s.ok(
        "they decline it: it is gone; declining it again finds none (404); they stay in their own org",
        declined.status == 200 && declined.body["offered"] == json!([]) && twice.status == 404 && org_of(api, &vic) == b_org,
        json!([declined.body, twice.status]),
    );

    // 5. alone in their own org, a move ends it, its subscription canceled
    let r = api.signed(&mal, "POST", "/api/org/seats", Some(&json!({ "email": vic_email })))?;
    let to_vic = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let b_customer = s.stripe.subscription(&b_sub).map(|x| x.customer).unwrap_or_default();
    let moved = answer(api, &vic, &to_vic, "accept")?;
    let canceled = s.eventually(PUSHED, || s.stripe.subscription(&b_sub).is_some_and(|x| x.status == "canceled"));
    let ended = s.eventually(PUSHED, || api.signed(&s.operator, "GET", &format!("/api/admin/orgs/{b_org}"), None).is_ok_and(|r| r.status == 404));
    s.ok(
        "alone in their own org, they accept another's seat: their org ends with them, its subscription canceled at once, its Stripe customer kept, and they hold the seat offered",
        moved.status == 200 && moved.body["seat"]["id"] == to_vic.as_str() && moved.body["org"]["id"] == a_org.as_str() && canceled && ended && s.stripe.customers().contains(&b_customer),
        json!({ "moved": moved.body, "subscription": s.stripe.subscription(&b_sub).map(|x| x.status) }),
    );

    // 6. a seat's holder leaves by their own yes; an admin steps down first
    let left = api.signed(&vic, "POST", "/api/seat/leave", Some(&json!({})))?;
    let gone = s.eventually(PUSHED, || ledger(api, &vic)["seat"] == "canceled");
    let twice = api.signed(&vic, "POST", "/api/seat/leave", Some(&json!({})))?;
    let admin_leaves = api.signed(&mal, "POST", "/api/seat/leave", Some(&json!({})))?;
    s.ok(
        "a seat's holder who is no admin leaves: their seat ends and they are in no org; leaving again is 400; an admin leaves only by stepping down first (400)",
        left.status == 200 && left.body["org"].is_null() && gone && twice.status == 400 && admin_leaves.status == 400 && admin_leaves.message().contains("step down"),
        json!([left.body, twice.status, admin_leaves.body]),
    );

    // 7. the last admin of an org others are in is not moved from it, and
    // an org that pays for nothing (an operator's comp) offers nothing
    let quinn = api.person()?;
    let rae = api.person()?;
    let comp = api.signed(&s.operator, "POST", "/api/admin/seats", Some(&json!({ "email": Api::email_of(&quinn), "kind": "seat" })))?;
    let q_org = comp.body["org"]["id"].as_str().unwrap_or("").to_string();
    let rae_comp = api.signed(&s.operator, "POST", "/api/admin/seats", Some(&json!({ "email": Api::email_of(&rae), "kind": "seat", "org": q_org })))?;
    let r = api.signed(&mal, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&quinn) })))?;
    let moved = answer(api, &quinn, r.body["seat"]["id"].as_str().unwrap_or(""), "accept")?;
    let offers = api.signed(&quinn, "POST", "/api/org/admins", Some(&json!({ "email": vic_email })))?;
    s.ok(
        "the last admin of an org someone else is in is refused a move (409: no one would manage it); an org that pays for nothing offers no admin's place (400)",
        comp.status == 200 && rae_comp.status == 200 && rae_comp.body["seat"]["person"].is_string() && moved.status == 409 && moved.message().contains("last admin") && org_of(api, &quinn) == q_org && offers.status == 400,
        json!([moved.body, offers.body]),
    );

    // 8. offers are no one yet: the last admin of an org with only an offer
    // waiting moves, and their org ends, the offer withdrawn
    let uma = api.person()?;
    let u_org = buys(s, api, &uma)?;
    let u_sub = subscription_of(s, &u_org).context("the org's subscription")?;
    let vee = api.person()?;
    let r = api.signed(&uma, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&vee) })))?;
    let to_vee = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let offered = seat(api, &vee)["offered"].as_array().is_some_and(|o| o.iter().any(|o| o["id"] == to_vee.as_str()));
    let r = api.signed(&mal, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&uma) })))?;
    let moved = answer(api, &uma, r.body["seat"]["id"].as_str().unwrap_or(""), "accept")?;
    let canceled = s.eventually(PUSHED, || s.stripe.subscription(&u_sub).is_some_and(|x| x.status == "canceled"));
    let withdrawn = seat(api, &vee)["offered"].as_array().is_some_and(|o| o.iter().all(|o| o["id"] != to_vee.as_str()));
    s.ok(
        "offers are no one yet: the last admin of an org with only an offer waiting moves, their org ends (its subscription canceled), and the offer is withdrawn",
        offered && moved.status == 200 && org_of(api, &uma) == a_org && canceled && withdrawn,
        json!({ "moved": moved.body, "vee": seat(api, &vee) }),
    );
    Ok(mal)
}

/// The audit's M4 (2026-10-17): a wipe cancels what the wiped would pay.
/// Its `billing` step, of the sole admin of an org someone else holds a
/// seat in: the dry run says it ends, its subscription canceled and the
/// other seat ending; Stripe failing the cancel, the step waits (an offer
/// to the person meanwhile refused: they are being wiped), the node
/// restarts mid-step, and the wipe run again finishes it: the subscription
/// canceled, its customer kept, the org gone, the other seat ended. A seat
/// holder wiped leaves their org going on, its count pushed down; who pays,
/// wiped while another admin keeps their org, has its subscription
/// canceled and their customer forgotten.
/// `payer`'s org pays.
fn wiped(s: &mut Suite, api: &Api, payer: &Keys) -> Result<()> {
    const ENDS: &str = "M4: the dry run of the sole admin of an org someone else holds a seat in says the wipe ends it: its subscription canceled, the other's seat ending";
    const WAITS: &str = "M4: Stripe failing its cancel, the billing step takes their place, says it waits on Stripe, and is not done; an offer to them meanwhile is refused (404: being wiped)";
    const RESUMED: &str = "M4: run again (after a restart mid-step, locally), the billing step goes on where it stopped and the wipe finishes: the subscription canceled, its Stripe customer kept, the org gone, and the other's seat ended";
    const LEAVES: &str = "M4: a seat's holder wiped leaves their org going on: the dry run says so, and its seats' count is pushed down";
    const SURVIVES: &str = "M4: who pays, wiped, while another admin keeps their org: the dry run says it goes on, its subscription canceled; it is, the org forgets their customer, and its admin opens no billing of theirs";
    if !s.runs("billing", &[Need::Operator]) {
        for label in [ENDS, WAITS, RESUMED, LEAVES, SURVIVES] {
            s.skip(label, "it needs an operator key no person holds (a wipe's)");
        }
        return Ok(());
    }
    let wiper = s.wiper.clone().context("Need::Operator lends an operator key")?;
    let wes = api.person()?;
    let wes_email = Api::email_of(&wes);
    let wes_id = api.identity(&wes)?;
    let w_org = buys(s, api, &wes)?;
    let xi = api.person()?;
    let xi_email = Api::email_of(&xi);
    let r = api.signed(&wes, "POST", "/api/org/seats", Some(&json!({ "email": xi_email })))?;
    answer(api, &xi, r.body["seat"]["id"].as_str().unwrap_or(""), "accept")?;
    anyhow::ensure!(s.eventually(PUSHED, || ledger(api, &xi)["seat"] == "active"), "the other's seat holds");
    let w_sub = subscription_of(s, &w_org).context("the org's subscription")?;
    let w_customer = s.stripe.subscription(&w_sub).map(|x| x.customer).unwrap_or_default();

    let dry = api.signed(&wiper, "GET", &format!("/api/people/{wes_email}/wipe"), None)?;
    let found = &dry.body["found"]["orgs"];
    s.ok(
        ENDS,
        dry.status == 200 && found.as_array().map(Vec::len) == Some(1) && found[0]["org"] == w_org.as_str() && found[0]["ends"] == true && found[0]["cancels"] == true && found[0]["lapses"] == json!([xi_email]),
        &dry,
    );

    // the cancel fails: the step waits on Stripe, the person locked
    s.stripe.fail("DELETE", "/v1/subscriptions/", true);
    let steps = fragment_core::wipe::Step::Billing.index() + 1;
    let r = api.signed(&wiper, "POST", &format!("/api/people/{wes_id}/wipe"), Some(&json!({ "confirm": wes_id, "steps": steps })))?;
    let billing = r.body["ran"].as_array().and_then(|l| l.iter().find(|x| x["step"] == "billing")).cloned().unwrap_or(Value::Null);
    let refused = api.signed(payer, "POST", "/api/org/seats", Some(&json!({ "email": wes_email })))?;
    s.ok(
        WAITS,
        r.status == 200
            && r.body["next"] == "billing"
            && billing["done"] == false
            && billing["note"].as_str().is_some_and(|n| n.contains("waiting on Stripe") && n.contains(&xi_email))
            && refused.status == 404
            && s.stripe.subscription(&w_sub).is_some_and(|x| x.status == "active"),
        json!({ "report": r.body, "offer": refused.body }),
    );

    // the node restarts mid-step (locally), then the wipe is run again
    let restarted;
    let api = if s.runs("billing", &[Need::Node]) {
        s.stop()?;
        restarted = s.start(false)?;
        &restarted
    } else {
        api
    };
    s.stripe.fail("DELETE", "/v1/subscriptions/", false);
    let r = api.signed(&wiper, "POST", &format!("/api/people/{wes_id}/wipe"), Some(&json!({ "confirm": wes_id })))?;
    let billing = r.body["ran"].as_array().and_then(|l| l.iter().find(|x| x["step"] == "billing")).cloned().unwrap_or(Value::Null);
    let org_gone = s.eventually(PUSHED, || api.signed(&s.operator, "GET", &format!("/api/admin/orgs/{w_org}"), None).is_ok_and(|r| r.status == 404));
    let lapsed = s.eventually(PUSHED, || ledger(api, &xi)["seat"] == "canceled");
    s.ok(
        RESUMED,
        r.status == 200
            && r.body["state"] == "wiped"
            && billing["done"] == true
            && s.stripe.subscription(&w_sub).is_some_and(|x| x.status == "canceled")
            && s.stripe.customers().contains(&w_customer)
            && org_gone
            && lapsed
            && seat(api, &xi)["org"].is_null(),
        json!({ "report": r.body, "subscription": s.stripe.subscription(&w_sub).map(|x| x.status), "xi": seat(api, &xi) }),
    );

    // a seat's holder wiped: their org goes on without them, its count pushed
    let p_org = org_of(api, payer);
    let p_sub = subscription_of(s, &p_org).context("the payer's org's subscription")?;
    let yuki = api.person()?;
    let yuki_id = api.identity(&yuki)?;
    let r = api.signed(payer, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&yuki) })))?;
    answer(api, &yuki, r.body["seat"]["id"].as_str().unwrap_or(""), "accept")?;
    let count = |s: &Suite| s.stripe.subscription(&p_sub).and_then(|x| x.items.first().map(|(_, _, q)| *q)).unwrap_or(0);
    let paid = api.signed(payer, "GET", "/api/org", None)?.body["members"].as_array().map(|m| m.iter().filter(|m| m["seat"].is_string() && m["comped"] == false).count() as u64).unwrap_or(0);
    let pushed = s.eventually(PUSHED, || count(s) == paid);
    let dry = api.signed(&wiper, "GET", &format!("/api/people/{yuki_id}/wipe"), None)?;
    let found = dry.body["found"]["orgs"][0].clone();
    let r = api.signed(&wiper, "POST", &format!("/api/people/{yuki_id}/wipe"), Some(&json!({ "confirm": yuki_id })))?;
    let down = s.eventually(PUSHED, || count(s) == paid - 1);
    s.ok(
        LEAVES,
        pushed && found["org"] == p_org.as_str() && found["ends"] == false && found["cancels"] == false && r.body["state"] == "wiped" && down && s.stripe.subscription(&p_sub).is_some_and(|x| x.status == "active"),
        json!({ "found": found, "count": count(s), "paid": paid }),
    );

    // who pays, wiped, while another admin keeps their org
    let zed = api.person()?;
    let zed_id = api.identity(&zed)?;
    let z_org = buys(s, api, &zed)?;
    let z_sub = subscription_of(s, &z_org).context("the payer's subscription")?;
    let z_customer = s.stripe.subscription(&z_sub).map(|x| x.customer).unwrap_or_default();
    let yan = api.person()?;
    let r = api.signed(&zed, "POST", "/api/org/admins", Some(&json!({ "email": Api::email_of(&yan) })))?;
    answer(api, &yan, r.body["admin"]["id"].as_str().unwrap_or(""), "accept")?;
    let dry = api.signed(&wiper, "GET", &format!("/api/people/{zed_id}/wipe"), None)?;
    let found = dry.body["found"]["orgs"][0].clone();
    let r = api.signed(&wiper, "POST", &format!("/api/people/{zed_id}/wipe"), Some(&json!({ "confirm": zed_id })))?;
    let canceled = s.eventually(PUSHED, || s.stripe.subscription(&z_sub).is_some_and(|x| x.status == "canceled"));
    let forgotten = s.eventually(PUSHED, || {
        let o = listed_org(s, api, &z_org);
        o["id"] == z_org.as_str() && o["customer"].is_null() && o["subscription"].is_null()
    });
    let portal = api.signed(&yan, "POST", "/api/billing/portal", Some(&json!({})))?;
    s.ok(
        SURVIVES,
        found["org"] == z_org.as_str()
            && found["ends"] == false
            && found["cancels"] == true
            && r.body["state"] == "wiped"
            && canceled
            && forgotten
            && s.stripe.customers().contains(&z_customer)
            && org_of(api, &yan) == z_org
            && seat(api, &yan)["admin"] == true
            && portal.status == 403,
        json!({ "found": found, "org": listed_org(s, api, &z_org), "portal": portal.body }),
    );
    Ok(())
}


/// An amount of core's as the shell shows it: whole dollars (`$200`).
fn dollars(micros: i64) -> String {
    assert_eq!(micros % 1_000_000, 0, "core's prices are whole dollars");
    format!("${}", micros / 1_000_000)
}

/// The person's own fragments of `kind`, counted.
fn owned(api: &Api, keys: &Keys, kind: &str) -> usize {
    api.signed(keys, "GET", "/api/fragments", None)
        .ok()
        .and_then(|r| r.body["fragments"].as_array().map(|l| l.iter().filter(|f| f["kind"] == kind && f["role"] == "owner").count()))
        .unwrap_or(0)
}

/// The welcome is shown: its card, at its address, with its one seat.
const WELCOMED: &str = "location.pathname === '/welcome' && !document.getElementById('first-run').hidden && document.querySelectorAll('#first-run-card [data-plan]').length === 1";

/// What the first run's card shows: its address, its text, its seats and
/// whether they can be pressed, its trial code, the import's place, what
/// it says went wrong, and the first run's steps.
const SEEN: &str = "(() => { const card = document.getElementById('first-run-card'); const slot = card.querySelector('[data-slot=import]'); return { path: location.pathname, search: location.search, layout: !document.getElementById('layout').hidden, card: document.getElementById('first-run').hidden ? '' : card.innerText, plans: [...card.querySelectorAll('[data-plan]')].map((b) => b.dataset.plan), enabled: [...card.querySelectorAll('[data-plan], form button')].every((b) => !b.disabled), trial: card.querySelector('input[placeholder=\"Trial code\"]')?.value ?? null, importing: slot ? (slot.hidden ? 'hidden' : 'shown') : 'absent', errors: [...card.querySelectorAll('.form-error')].filter((e) => !e.hidden).map((e) => e.textContent), steps: [...card.querySelectorAll('.creating-steps li')].map((li) => `${li.className}: ${li.textContent}`) }; })()";

/// The first run's seat step is done (Checkout's return applied, its query
/// off), and the first agent is being made in its chat, at home's address
/// (the in-chat first run: a guest's creates are refused, so only a seat
/// gets there).
const SEATED: &str = "location.pathname === '/' && location.search === '' && document.getElementById('first-run').hidden && !document.getElementById('layout').hidden && !!document.querySelector('#frames iframe[data-fragment]')";

/// Clicks the first run's button (or link) whose text is `text`; answers
/// whether there was one.
fn press(text: &str) -> String {
    format!("(() => {{ const b = [...document.querySelectorAll('#first-run-card button, #first-run-card a')].find((b) => !b.hidden && b.textContent === {text:?}); b?.click(); return !!b; }})()")
}

/// Clicks the first element `selector` matches; answers whether there was one.
fn click(selector: &str) -> String {
    format!("(() => {{ const e = document.querySelector({selector:?}); e?.click(); return !!e; }})()")
}

/// Clicks the first of the elements `selector` matches whose text is
/// `text`; answers whether there was one.
fn click_text(selector: &str, text: &str) -> String {
    format!("(() => {{ const e = [...document.querySelectorAll({selector:?})].find((e) => e.textContent === {text:?}); e?.click(); return !!e; }})()")
}

/// Where a page is, what it shows, and which of the shell's files it has.
/// Resource Timing lists a file once its answer is in, so one the page
/// lacks is a request that never came back: its modules then never run,
/// and the page stays blank (CI, 2026-10-13: a page's billing.js and
/// vendor/split-grid.js never reached the node).
const PAGE: &str = "({ at: location.href, ready: document.readyState, firstRun: document.getElementById('first-run')?.hidden === false, layout: document.getElementById('layout')?.hidden === false, text: document.body?.innerText.slice(0, 300) ?? '', shell: performance.getEntriesByType('resource').filter((e) => e.name.includes('/__shell/')).map((e) => `${e.name.split('/__shell/')[1]} ${e.responseStatus}`) })";

/// `PAGE` on `page`, or why it could not be read.
fn page_state(b: &mut Lease, page: &Page) -> Value {
    b.eval(page, PAGE).unwrap_or_else(|e| json!(format!("{e:#}")))
}

/// Does `step` on `page`: an action that answers whether it found what it
/// acts on. When it did not, the step is a FAIL that says where the page is
/// and what it has, and the caller stops there: each step after it acts on
/// what this one makes.
fn act(s: &mut Suite, b: &mut Lease, page: &Page, what: &str, step: &str) -> Result<bool> {
    if b.eval(page, step)? == json!(true) {
        return Ok(true);
    }
    s.ok(&format!("{what}: it is on the page"), false, page_state(b, page));
    Ok(false)
}

/// Clicks `selector` as a person does (Stripe's pages), as `act` does an action.
fn act_at(s: &mut Suite, b: &mut Lease, page: &Page, what: &str, selector: &str) -> bool {
    match b.click(page, selector) {
        Ok(()) => true,
        Err(e) => {
            s.ok(&format!("{what}: {e:#}"), false, page_state(b, page));
            false
        }
    }
}

/// Waits until `expr` holds on `page`, a page of the shell just loaded or
/// navigated to. `wrangler dev`'s proxy now and then never answers a
/// request in a page's burst of module loads (CI, 2026-10-13: billing.js
/// and vendor/split-grid.js, twice, which never reached the node), and the
/// page's modules then never run. A page whose shell never ran (neither its
/// first run nor its layout shows) when the wait runs out is said, with
/// the files it has, and loaded again, once; anything else is the wait's
/// own answer.
#[track_caller]
fn view(b: &mut Lease, page: &Page, expr: &str, wait: Duration) -> bool {
    if b.until(page, expr, wait) {
        return true;
    }
    let state = page_state(b, page);
    if state["firstRun"] != false || state["layout"] != false || !state["at"].as_str().is_some_and(|at| at.starts_with("http")) {
        return false;
    }
    println!("      (the shell never ran, a request of its page never answered: {state}; loaded again)");
    b.reload(page).is_ok() && b.until(page, expr, wait)
}

/// A page of the person signed in with `session`, open at `path`.
fn page_of(b: &mut Lease, api: &Api, session: &str, path: &str) -> Result<Page> {
    b.set_cookie(&format!("{}/", api.base), "fragment_session", session)?;
    let page = b.open(&format!("{}{path}", api.base))?;
    b.viewport(&page, 1280, 800, false)?;
    Ok(page)
}

/// A new person's first run, and Billing, in Chrome (cell/shell/shell.js
/// `welcome`, billing.js): a new guest lands on the welcome, never
/// settings: what this is, the one seat and a trial code. A guest's
/// Billing in settings links to it. Checkout left by its Back comes back
/// to it, saying so; a trial code that is none is said there. Paid on
/// Stripe's page (the fake's), Checkout's return comes back to it, applies
/// the seat and makes their first agent; that return again goes home.
/// Billing then manages the seat: its org's seats wait until they ask for
/// seats for others, they add one, and buy credit. A guest an org offers a
/// seat sees the offer first on the welcome, accepts it, and goes on to
/// their first run. An invited guest lands on what was shared with them,
/// titled as its owner titled it, a seat offered beside it. Another's
/// Checkout returned is refused, and leads back to the seats; the node
/// restarts while a buyer is at Checkout, no webhook comes, and the return
/// alone seats them; a reload mid-wait makes nothing twice, and the webhook
/// come late changes nothing. A trial mailed brings a signed-out person
/// through sign-in and back with its code. A guest given a seat goes
/// straight to the first run. Stripe's pages are the fake's own. A step
/// that acts on the page does so only on what is there: one that finds
/// nothing is a FAIL saying where the page is and what it has, not an
/// error thrown in the page.
pub fn billing_page(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("billing-page", &[Need::Chrome, Need::Fakes, Need::Deployment, Need::Node]) {
        return Ok(());
    }
    let Some(mut b) = s.browser()? else {
        s.ok("Chrome is installed for the welcome and Billing's page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let shots = s.dir("billing-page");
    let wait = Duration::from_secs(30);
    let op_session = api.sign_in("operator@e2e.test")?;
    let _ = api.approve(&op_session, &s.operator);
    // a guest: the e2e's people are seats, so an operator makes one a guest
    let operator = s.operator.clone();
    let guest = |api: &Api, id: &str| -> Result<Keys> {
        let keys = api.person()?;
        let who = api.identity(&keys)?;
        let r = api.signed(&operator, "POST", &format!("/api/ledger/{who}/plan"), Some(&json!({ "id": id, "plan": "guest" })))?;
        anyhow::ensure!(r.status == 200, "making a guest: {r}");
        Ok(keys)
    };
    let text = |b: &mut crate::browser::Lease, p: &crate::browser::Page| b.eval(p, "document.getElementById('settings-page')?.innerText ?? ''").map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_default();
    // what is sold, as core has it: every price a page shows is these
    let offer = fragment_core::stripe::offer();
    let (seat_price, included, pack) = (dollars(offer.seat_micros), dollars(offer.included_micros), dollars(offer.pack_micros));

    // ---- a new guest: the welcome, never settings
    let gus = guest(api, "billing-page-guest")?;
    let mine = seat(api, &gus);
    s.ok(
        "GET /api/seat says who is a guest (a guest's plan, no seat) and what is sold, at core's prices",
        mine["guest"] == true
            && mine["seat"].is_null()
            && mine["offer"] == json!({ "kind": "seat_always_on", "seatMicros": offer.seat_micros, "includedMicros": offer.included_micros, "packMicros": offer.pack_micros }),
        &mine,
    );
    let session = api.sign_in(&Api::email_of(&gus))?;
    let page = page_of(&mut b, api, &session, "/")?;
    let welcomed = view(&mut b, &page, WELCOMED, wait);
    let _ = b.screenshot(&page, &shots.join("1-welcome.png"));
    let seen = b.eval(&page, SEEN)?;
    s.ok(
        &format!("a new guest's first run is the welcome, never settings: what this is, the one seat sold ({seat_price} a month, always on, {included} of credit: core's prices), a trial code, what more credit costs, the import's place kept hidden, and who is signed in"),
        welcomed
            && seen["layout"] == false
            && seen["plans"] == json!(["seat_always_on"])
            && seen["trial"] == ""
            && seen["importing"] == "hidden"
            && seen["errors"] == json!([])
            && seen["card"].as_str().is_some_and(|t| {
                t.contains("Agents that work for you")
                    && t.contains(&format!("{seat_price} a month, always on"))
                    && t.contains(&format!("{included} of credit included each month"))
                    && t.contains(&format!("{pack} a pack"))
                    && t.contains(&Api::email_of(&gus))
                    && t.contains("Sign out")
                    && !t.contains("Not now")
            }),
        &seen,
    );
    // settings' Billing manages a seat; a guest's links to the welcome
    b.eval(&page, "(location.assign('/settings'), true)")?;
    let billing = view(&mut b, &page, "location.pathname === '/settings' && document.querySelector('#settings-page h2')?.textContent === 'Billing' && /You are a guest/.test(document.getElementById('settings-page').innerText)", wait);
    let offered = b.eval(
        &page,
        "({ links: [...document.querySelectorAll('#settings-page a')].filter((a) => a.textContent === 'Get a seat').map((a) => a.getAttribute('href')), buys: [...document.querySelectorAll('#settings-page button, #settings-page input')].map((b) => b.textContent || b.placeholder).filter((t) => /seat|trial/i.test(t)), headings: [...document.querySelectorAll('#settings-page h2')].map((h) => h.textContent), warned: [...document.querySelectorAll('.settings-warning')].map((w) => w.textContent) })",
    )?;
    s.ok(
        "a guest's Billing in settings sells nothing itself: it names the one seat's price, its Get a seat is the welcome's, and nothing a guest has no use for shows (a computer, agents, skills, connections)",
        billing
            && text(&mut b, &page).contains(&format!("A seat, {seat_price} a month"))
            && offered["links"] == json!(["/welcome"])
            && offered["buys"] == json!([])
            && offered["warned"] == json!([])
            && offered["headings"].as_array().is_some_and(|h| ["Computer", "Agents", "Skills", "Connections"].iter().all(|x| !h.contains(&json!(x)))),
        &offered,
    );
    if !act(s, &mut b, &page, "settings' Get a seat", &click_text("#settings-page a", "Get a seat"))? {
        return Ok(());
    }
    let back = view(&mut b, &page, WELCOMED, wait);

    // ---- invalid: Checkout left by its Back; a trial code that is none
    if !act(s, &mut b, &page, "the welcome's seat", &click("#first-run-card [data-plan=seat_always_on]"))? {
        return Ok(());
    }
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    if !act_at(s, &mut b, &page, "Stripe's Back", "#back") {
        return Ok(());
    }
    let canceled = view(&mut b, &page, &format!("{WELCOMED} && location.search === '' && /Checkout was canceled: nothing was bought/.test(document.getElementById('first-run-card').innerText)"), wait);
    let _ = b.screenshot(&page, &shots.join("2-canceled.png"));
    s.ok(
        "Get a seat there opens the welcome; its seat goes to Stripe's Checkout, whose Back comes back to the welcome, saying nothing was bought, the seat offered again",
        back && at_stripe && canceled && seat(api, &gus)["seat"].is_null(),
        json!({ "seen": b.eval(&page, SEEN)?, "seat": seat(api, &gus) }),
    );
    let typed = "(() => { const f = document.querySelector('#first-run-card form.trial-code'); if (!f) return false; f.querySelector('input').value = 'ZZZZ-ZZZZ-ZZZZ-ZZZZ'; f.requestSubmit(); return true; })()";
    if !act(s, &mut b, &page, "the welcome's trial code", typed)? {
        return Ok(());
    }
    let refused = b.until(&page, "[...document.querySelectorAll('#first-run-card .form-error')].some((e) => !e.hidden && /no such trial code/.test(e.textContent))", wait);
    let seen = b.eval(&page, SEEN)?;
    let _ = b.screenshot(&page, &shots.join("3-no-such-code.png"));
    s.ok(
        "a trial code that is none is said on the welcome, which stays, its choices open again; nothing is bought",
        refused && seen["path"] == "/welcome" && seen["enabled"] == true && seat(api, &gus)["seat"].is_null(),
        &seen,
    );

    // ---- valid: the $200 seat paid; its return is the welcome's, then the first run
    if !act(s, &mut b, &page, "the welcome's $200 seat", &click("#first-run-card [data-plan=seat_always_on]"))? {
        return Ok(());
    }
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    let paid = b.eval(&page, "location.pathname.split('/').pop()")?.as_str().unwrap_or("").to_string();
    if !act_at(s, &mut b, &page, "Stripe's Pay", "form button") {
        return Ok(());
    }
    let seated = view(&mut b, &page, SEATED, wait);
    let _ = b.screenshot(&page, &shots.join("4-paid-creating.png"));
    let made = s.eventually(wait, || owned(api, &gus, "agent") == 1 && owned(api, &gus, "chat") == 1);
    let mine = seat(api, &gus);
    s.ok(
        "paid, Checkout's return comes back to the welcome: it applies the seat (a $200 always-on seat), takes its query off, and the first run goes on to their agent and its chat, a seat's creates",
        at_stripe && seated && made && mine["seat"]["kind"] == "seat_always_on" && mine["seat"]["good"] == true,
        json!({ "seen": b.eval(&page, SEEN)?, "seat": mine }),
    );
    // ---- replay: that return again, its first run made, goes home
    let purchased = ledger(api, &gus)["purchasedMicros"].clone();
    b.eval(&page, &format!("(location.assign({:?}), true)", format!("/welcome?checkout={paid}")))?;
    let home = view(&mut b, &page, "location.pathname === '/' && location.search === '' && document.getElementById('first-run').hidden && !document.getElementById('layout').hidden && !!document.querySelector('#frames iframe[data-fragment]')", wait);
    let again = seat(api, &gus);
    s.ok(
        "Checkout's return again goes home to their chat: the seat as it was, nothing made or credited twice",
        home && again["seat"] == mine["seat"] && owned(api, &gus, "agent") == 1 && owned(api, &gus, "chat") == 1 && ledger(api, &gus)["purchasedMicros"] == purchased,
        json!({ "seat": again, "at": b.eval(&page, "location.pathname + location.search")? }),
    );

    // ---- Billing manages the seat from here
    b.eval(&page, "(location.assign('/settings'), true)")?;
    let held = format!("Always-on seat, {seat_price} a month");
    let managed = view(
        &mut b,
        &page,
        &format!("location.pathname === '/settings' && (document.getElementById('settings-page')?.innerText ?? '').includes({held:?}) && [...document.querySelectorAll('#settings-page button')].some((b) => b.textContent === 'Add seats for others')"),
        wait,
    );
    let _ = b.screenshot(&page, &shots.join("5-billing.png"));
    let shown = text(&mut b, &page);
    let buttons = b.eval(&page, "[...document.querySelectorAll('#settings-page button')].map((b) => b.textContent)")?;
    let has = |t: &str| buttons.as_array().is_some_and(|l| l.contains(&json!(t)));
    let alone = b.eval(&page, "!document.querySelector('.billing-seats') && ![...document.querySelectorAll('#settings-page h2')].some((h) => h.textContent.startsWith('Org'))")?;
    let warned = b.eval(&page, "[...document.querySelectorAll('.settings-warning')].map((w) => w.textContent)")?;
    s.ok(
        &format!("their Billing shows their seat, an always-on seat at {seat_price} a month: an org of one shows no org's table, only its invoices and seats for others, and no first run of its own"),
        managed && alone == true && warned == json!([]) && has("Payment and invoices") && has("Add seats for others") && !has("Make your first agent"),
        json!({ "shown": shown, "buttons": buttons, "warned": warned }),
    );
    if !act(s, &mut b, &page, "Billing's Add seats for others", &click_text("#settings-page button", "Add seats for others"))? {
        return Ok(());
    }
    let asked = b.until(&page, "!!document.querySelector('.billing-seats') && [...document.querySelectorAll('#settings-page form')].some(f => f.textContent.includes('Add a seat'))", wait);
    s.ok("asked, their org's seats show: theirs alone, and a seat to add by email", asked, text(&mut b, &page));
    let teammate = format!("billing-page-mate-{}@e2e.test", &Keys::generate().pubkey_hex()[..8]);
    let typed = format!("(() => {{ const f = [...document.querySelectorAll('#settings-page form')].find(f => f.textContent.includes('Add a seat')); if (!f) return false; f.querySelector('input[type=email]').value = {teammate:?}; f.requestSubmit(); return true; }})()");
    if !act(s, &mut b, &page, "Billing's Add a seat", &typed)? {
        return Ok(());
    }
    let added = b.until(&page, &format!("[...document.querySelectorAll('.billing-seats tr')].some(r => r.textContent.includes({teammate:?}) && r.textContent.includes('invited'))"), wait);
    let unasked = b.eval(&page, "![...document.querySelectorAll('#settings-page button')].some((b) => b.textContent === 'Add seats for others')")?;
    s.ok("as its admin they add a seat by email: it waits, invited, and their org's seats now show unasked", added && unasked == true, text(&mut b, &page));
    let before = ledger(api, &gus)["purchasedMicros"].as_i64().unwrap_or(0);
    let buy = format!("Buy {pack} of credit");
    if !act(s, &mut b, &page, &format!("Billing's {buy}"), &click_text("#settings-page button", &buy))? {
        return Ok(());
    }
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    if !act_at(s, &mut b, &page, "Stripe's Pay", "form button") {
        return Ok(());
    }
    let credited = view(&mut b, &page, "location.pathname === '/settings' && /Paid: the credit is yours/.test(document.getElementById('settings-page')?.innerText ?? '')", wait);
    s.ok(
        &format!("they buy {pack} of credit on Stripe's page, and it is theirs as they land back in settings"),
        at_stripe && credited && ledger(api, &gus)["purchasedMicros"].as_i64() == Some(before + offer.pack_micros),
        ledger(api, &gus),
    );
    b.close(page)?;

    // ---- a guest offered a seat: the welcome shows the offer first (its
    // org, who made it, Accept and Decline); accepted, on to their first run
    let kai = guest(api, "billing-page-offered")?;
    let r = api.signed(&gus, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&kai) })))?;
    anyhow::ensure!(r.status == 200, "offering the guest a seat: {r}");
    let session = api.sign_in(&Api::email_of(&kai))?;
    let page = page_of(&mut b, api, &session, "/")?;
    let offered = format!(
        "{WELCOMED} && (() => {{ const o = document.querySelector('#first-run-card .billing-offer'); return !!o && o.innerText.includes({:?}) && o.innerText.includes('offers you an always-on seat') && [...o.querySelectorAll('button')].map((b) => b.textContent).join() === 'Accept,Decline'; }})()",
        Api::email_of(&gus)
    );
    let shown = view(&mut b, &page, &offered, wait);
    let _ = b.screenshot(&page, &shots.join("5b-offered.png"));
    let seen = b.eval(&page, SEEN)?;
    if !act(s, &mut b, &page, "the welcome's Accept", &press("Accept"))? {
        return Ok(());
    }
    let seated = view(&mut b, &page, SEATED, wait);
    let made = s.eventually(wait, || owned(api, &kai, "agent") == 1);
    s.ok(
        "a guest offered a seat lands on the welcome, the offer first: its org, who made it, Accept and Decline; accepted, the seat is theirs and their first run makes their agent",
        shown && seated && made && seat(api, &kai)["seat"]["good"] == true && seat(api, &kai)["offered"] == json!([]),
        json!({ "seen": seen, "seat": seat(api, &kai) }),
    );
    b.close(page)?;

    // ---- an invited guest: someone with a seat shares an app they titled
    // with the guest's email, and the guest lands on it, not the welcome
    let owner = api.person()?;
    let label = format!("garden-{}", &Keys::generate().pubkey_hex()[..6]);
    let made = api.signed(&owner, "POST", "/api/fragments", Some(&json!({ "label": label, "template": "todo", "title": "Garden plans" })))?;
    anyhow::ensure!(made.status == 200, "making the app to share: {made}");
    let app = made.body["name"].as_str().unwrap_or("").to_string();
    let bea = guest(api, "billing-page-invited")?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{app}/invites"), Some(&json!({ "email": Api::email_of(&bea), "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "sharing it with the guest: {r}");
    let session = api.sign_in(&Api::email_of(&bea))?;
    let page = page_of(&mut b, api, &session, "/")?;
    let landed = view(
        &mut b,
        &page,
        &format!("location.pathname === '/' && !!document.querySelector('.viewer iframe[data-fragment={app:?}]') && /Get a seat/.test(document.getElementById('notice')?.innerText ?? '')"),
        wait,
    );
    let _ = b.screenshot(&page, &shots.join("6-invited-guest.png"));
    let row = b.eval(&page, &format!("document.querySelector('#apps [data-key=\"app:{app}\"] .label')?.textContent ?? null"))?;
    // every control that makes something (a guest's create is refused: the
    // dock's Add, the top bar's new chat, the launcher's start and send, the
    // desktop's requests), the launcher open with words typed: it only finds
    if !act(s, &mut b, &page, "the sidebar's new chat", &click("#sidebar-new-chat"))? {
        return Ok(());
    }
    let typed = "(() => { const i = document.getElementById('workspace-search'); if (!i) return false; i.value = 'garden'; i.dispatchEvent(new Event('input')); return true; })()";
    if !act(s, &mut b, &page, "the launcher's search", typed)? {
        return Ok(());
    }
    let found = b.until(&page, &format!("!!document.querySelector('#search-results [data-fragment={app:?}]')"), wait);
    let makes = b.eval(
        &page,
        "[document.getElementById('add-app') && 'add-app', !document.getElementById('new-chat').hidden && 'new-chat', !document.getElementById('search-send').disabled && 'search-send', document.querySelector('#search-results .search-start') && 'search-start', document.querySelector('#desktop-empty .desktop-suggestion') && 'suggestion'].filter(Boolean)",
    )?;
    b.eval(&page, "(document.getElementById('search-dialog')?.dispatchEvent(new Event('cancel')), true)")?;
    b.until(&page, "!document.getElementById('search-dialog')?.open", wait);
    s.ok(
        "an invited guest lands home on what was shared with them, not the welcome: its window open and titled as its owner titled it, a seat offered beside it, and no control that only fails (their launcher only finds)",
        landed && row == json!("Garden plans") && found && makes == json!([]),
        json!({ "row": row, "makes": makes }),
    );
    if !act(s, &mut b, &page, "the guest's Get a seat", &click("#notice .allow"))? {
        return Ok(());
    }
    let offered = b.until(&page, &format!("{WELCOMED} && [...document.querySelectorAll('#first-run-card a')].some((a) => a.textContent === 'Not now' && a.getAttribute('href') === '/')"), wait);
    s.ok("their Get a seat opens the welcome, whose Not now goes back to what is shared with them", offered, b.eval(&page, SEEN)?);
    b.close(page)?;

    // ---- another's Checkout returned; the node restarts while a buyer is
    // at Checkout, and no webhook comes; a reload mid-wait; the webhook late
    let ida = guest(api, "billing-page-restart")?;
    let session = api.sign_in(&Api::email_of(&ida))?;
    let page = page_of(&mut b, api, &session, &format!("/welcome?checkout={paid}"))?;
    let failed = view(
        &mut b,
        &page,
        "[...document.querySelectorAll('#first-run-card .creating-steps li')].some((li) => li.className === 'failed' && li.textContent === 'Setting up your seat') && [...document.querySelectorAll('#first-run-card button')].some((b) => !b.hidden && b.textContent === 'Choose a seat')",
        wait,
    );
    let seen = b.eval(&page, SEEN)?;
    let _ = b.screenshot(&page, &shots.join("7-not-theirs.png"));
    let chose = b.eval(&page, &press("Choose a seat"))? == json!(true) && b.until(&page, &format!("{WELCOMED} && location.search === ''"), wait);
    s.ok(
        "another's Checkout returned is refused and said, and applies nothing, its address kept until then; Choose a seat goes back to the welcome's seats",
        failed
            && seen["search"].as_str().is_some_and(|q| q.contains("checkout="))
            && seen["errors"].as_array().is_some_and(|e| e.iter().any(|t| t.as_str().is_some_and(|t| t.contains("no Checkout"))))
            && chose
            && seat(api, &ida)["seat"].is_null(),
        &seen,
    );
    if !act(s, &mut b, &page, "the welcome's seat", &click("#first-run-card [data-plan=seat_always_on]"))? {
        return Ok(());
    }
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    // the webhook lost, and the node restarted, while they are on Stripe's page
    s.stripe.hold_events(true);
    let delivered = s.stripe.delivered().len();
    s.stop()?;
    let restarted = s.start(false);
    let seated = restarted.is_ok() && b.click(&page, "form button").is_ok() && view(&mut b, &page, SEATED, wait);
    let held = s.stripe.delivered().len() == delivered;
    let api = &restarted?;
    let mine = seat(api, &ida);
    s.ok(
        "the node restarted while they were at Checkout, and no webhook came: its return alone, after the restart, seats them (the always-on seat) and goes on to their first agent",
        at_stripe && seated && held && mine["seat"]["kind"] == "seat_always_on" && mine["seat"]["good"] == true,
        json!({ "seen": b.eval(&page, SEEN)?, "seat": mine, "held": held }),
    );
    // a reload while its computer starts (the first run's longest wait)
    let midway = b.until(&page, "[...document.querySelectorAll('#first-run-card .creating-steps li')].some((li) => li.textContent === 'Starting its computer' && li.className === 'doing') || !document.getElementById('layout').hidden", wait);
    b.reload(&page)?;
    let resumed = view(
        &mut b,
        &page,
        "location.pathname === '/' && (document.getElementById('first-run').hidden ? !!document.querySelector('#frames iframe[data-fragment]') : !!document.querySelector('#first-run-card .creating-steps li.doing'))",
        wait,
    );
    let _ = b.screenshot(&page, &shots.join("8-reloaded.png"));
    // the in-chat first run makes the chat before the agent: the agent may
    // still be on its way as the reloaded page shows the chat
    let once = s.eventually(wait, || owned(api, &ida, "agent") == 1 && owned(api, &ida, "chat") == 1);
    s.ok(
        "a reload mid-wait goes on where it stopped, at home's address: one agent, one chat, nothing made twice",
        midway && resumed && once,
        json!({ "seen": b.eval(&page, SEEN)?, "agents": owned(api, &ida, "agent"), "chats": owned(api, &ida, "chat") }),
    );
    s.stripe.release_events(false);
    let late = s.eventually(wait, || s.stripe.delivered().len() > delivered);
    let after = seat(api, &ida);
    s.ok(
        "the webhook, come late, changes nothing: the seat as the return made it",
        late && after["seat"] == mine["seat"] && s.stripe.delivered()[delivered..].iter().all(|(_, _, status)| *status == 200),
        json!({ "seat": after, "delivered": s.stripe.delivered()[delivered..].to_vec() }),
    );
    b.close(page)?;

    // ---- a trial mailed: signed out, its link signs in and comes back with
    // its code, filled in; its trial's Checkout returns to the welcome too
    let code = api.signed(&s.operator, "POST", "/api/admin/trials", Some(&json!({ "name": "Page", "days": 30, "capacity": 5 })))?;
    let code = code.body["code"].as_str().unwrap_or("").to_string();
    let nobody = b.another_context()?;
    let out = b.open_in(&nobody, &format!("{}/welcome?trial={code}", api.base))?;
    let href = format!("/auth/login?return=%2Fwelcome%3Ftrial%3D{code}");
    let asked = view(&mut b, &out, &format!("document.querySelector('#first-run-card a.primary')?.getAttribute('href') === {href:?}"), wait);
    s.ok("signed out, a trial's link asks them to sign in, and to come back to the welcome with its code", asked, b.eval(&out, "document.querySelector('#first-run-card a.primary')?.getAttribute('href') ?? null")?);
    b.close(out)?;
    let jo = guest(api, "billing-page-trial")?;
    let session = api.sign_in(&Api::email_of(&jo))?;
    let page = page_of(&mut b, api, &session, &format!("/welcome?trial={code}"))?;
    let filled = view(&mut b, &page, &format!("{WELCOMED} && location.search === '' && document.querySelector('#first-run-card input[placeholder=\"Trial code\"]')?.value === {code:?}"), wait);
    // evidence for a person: the welcome as a mailed trial opens it (kept with a run's scratch)
    let _ = b.screenshot(&page, &shots.join("9-trial.png"));
    let started = "(() => { const f = document.querySelector('#first-run-card form.trial-code'); f?.requestSubmit(); return !!f; })()";
    if !act(s, &mut b, &page, "the welcome's Start a trial", started)? {
        return Ok(());
    }
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?}) && /30-day trial/.test(document.body.innerText)", s.stripe.url), wait);
    if !act_at(s, &mut b, &page, "Stripe's Pay", "form button") {
        return Ok(());
    }
    let seated = view(&mut b, &page, SEATED, wait);
    let mine = seat(api, &jo);
    s.ok(
        "signed in, the link's welcome has its code filled in; its 30-day trial takes a card on Stripe's page and comes back to the welcome, which seats them in their trial of the always-on seat and goes on to their first agent",
        filled && at_stripe && seated && mine["seat"]["good"] == true && mine["seat"]["kind"] == "seat_always_on" && mine["seat"]["trialEnds"].as_i64().is_some(),
        json!({ "seen": b.eval(&page, SEEN)?, "seat": mine }),
    );
    b.close(page)?;

    // ---- a seat given (an operator's comp; an org's admin gives one the
    // same way): its plan may not have reached their ledger as they land,
    // and either way they go straight to the first run, never the seats
    let max = guest(api, "billing-page-given")?;
    let given = api.signed(&s.operator, "POST", "/api/admin/seats", Some(&json!({ "email": Api::email_of(&max), "kind": "seat" })))?;
    let session = api.sign_in(&Api::email_of(&max))?;
    let page = page_of(&mut b, api, &session, "/")?;
    // its seat's own step while its plan is on its way, else the chat at once
    let straight = view(
        &mut b,
        &page,
        "location.pathname === '/' && !document.querySelector('#first-run-card [data-plan]') && (!!document.querySelector('#first-run-card .creating-steps') || (!document.getElementById('layout').hidden && !!document.querySelector('#frames iframe[data-fragment]')))",
        wait,
    );
    let made = s.eventually(wait, || owned(api, &max, "agent") == 1);
    s.ok(
        "a guest given a seat goes straight to their first run, never the welcome's seats, and it makes their agent",
        given.status == 200 && straight && made,
        json!({ "given": given.body, "seen": b.eval(&page, SEEN)? }),
    );

    // ---- their comp ends: their ledger keeps its plan, canceled (decision
    // 53), and they keep their chat; to the shell, its Billing and its
    // welcome alike they are a guest, and Get a seat reaches the seat
    let comp = given.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let ended = api.signed(&s.operator, "DELETE", &format!("/api/admin/seats/{comp}"), None)?;
    let guested = s.eventually(PUSHED, || seat(api, &max)["guest"] == true);
    let (kept, mine) = (ledger(api, &max), seat(api, &max));
    s.ok(
        "their comp ended, GET /api/seat says they are a guest (no seat), though their ledger keeps the seat's plan, canceled",
        ended.status == 200 && guested && mine["seat"].is_null() && kept["plan"] == "seat" && kept["seat"] == "canceled" && owned(api, &max, "chat") == 1,
        json!({ "ended": ended.body, "seat": mine, "ledger": { "plan": kept["plan"], "seat": kept["seat"] } }),
    );
    b.reload(&page)?;
    let home = view(&mut b, &page, "location.pathname === '/' && !document.getElementById('layout').hidden && !!document.querySelector('#frames iframe[data-fragment]') && document.getElementById('sidebar-new-chat')?.getAttribute('aria-label') === 'Search chats and apps'", wait);
    let makes = b.eval(&page, "[!document.getElementById('new-chat').hidden && 'new-chat', document.getElementById('add-app') && 'add-app'].filter(Boolean)")?;
    b.eval(&page, "(location.assign('/settings'), true)")?;
    let billed = view(&mut b, &page, "location.pathname === '/settings' && /You are a guest/.test(document.getElementById('settings-page')?.innerText ?? '')", wait);
    let headings = b.eval(&page, "[...document.querySelectorAll('#settings-page h2')].map((h) => h.textContent)")?;
    s.ok(
        "the shell agrees: home, their chat open, makes nothing (its launcher only finds), and their Billing says they are a guest, with settings a guest's (no computer or agents)",
        home && makes == json!([]) && billed && headings.as_array().is_some_and(|h| ["Computer", "Agents"].iter().all(|x| !h.contains(&json!(x)))),
        json!({ "makes": makes, "headings": headings, "billing": text(&mut b, &page) }),
    );
    if !act(s, &mut b, &page, "their Billing's Get a seat", &click_text("#settings-page a", "Get a seat"))? {
        return Ok(());
    }
    let offered = view(
        &mut b,
        &page,
        &format!("{WELCOMED} && [...document.querySelectorAll('#first-run-card a')].some((a) => a.textContent === 'Not now') && document.getElementById('first-run-card').innerText.includes({:?})", format!("{seat_price} a month, always on")),
        wait,
    );
    let _ = b.screenshot(&page, &shots.join("10-comp-ended.png"));
    let seen = b.eval(&page, SEEN)?;
    if !act(s, &mut b, &page, "the welcome's seat", &click("#first-run-card [data-plan=seat_always_on]"))? {
        return Ok(());
    }
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    s.ok(
        "its Get a seat reaches the seat offer, though they own a chat (not home, nor a first run): the one seat at core's price, Not now back to their chat, and its seat goes to Stripe's Checkout",
        offered && at_stripe,
        json!({ "seen": seen, "at": b.eval(&page, "location.href")? }),
    );
    b.close(page)?;
    println!("      (screenshots: {})", shots.display());
    Ok(())
}
