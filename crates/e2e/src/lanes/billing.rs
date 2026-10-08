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
    let r = api.signed(&ann, "POST", "/api/org/seats", Some(&json!({ "email": Api::email_of(&fay), "kind": "seat" })))?;
    let fay_row = r.body["seat"]["id"].as_str().unwrap_or("").to_string();
    let fay_before = purchased(&fay);
    let r = api.signed(&ann, "POST", "/api/billing/packs", Some(&json!({ "member": fay_row })))?;
    let pack2 = r.body["session"].as_str().unwrap_or("").to_string();
    s.stripe.complete(&pack2).map_err(anyhow::Error::msg)?;
    let theirs = s.eventually(PUSHED, || purchased(&fay) == fay_before + 25_000_000);
    s.ok("an org's admin buys a pack for a member who holds a seat: the credit is the member's", r.status == 200 && theirs && purchased(&ann) == before + 25_000_000, ledger(api, &fay));
    let not_admin = api.signed(&fay, "POST", "/api/billing/packs", Some(&json!({ "member": fay_row })))?;
    let seatless = api.signed(&api.person()?, "POST", "/api/billing/packs", Some(&json!({})))?;
    s.ok("a member who is no admin buys none for others (403); someone with no seat buys none (400)", not_admin.status == 403 && seatless.status == 400, json!([not_admin.status, seatless.status]));

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

/// A guest's first run and Billing in Chrome (cell/shell/shell.js,
/// billing.js): a new guest lands home, told what a guest may do and
/// offered a seat, with no button that only fails; they pay on Stripe's
/// page (the fake's) and come back to their seat, whose org's seats wait
/// until they ask for seats for others; they add one, buy credit, and are
/// offered their first agent. An invited guest lands on what was shared
/// with them, titled as its owner titled it. A trial mailed opens Billing
/// with its code. Stripe's pages are the fake's own.
pub fn billing_page(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("billing-page", &[Need::Chrome, Need::Fakes, Need::Deployment]) {
        return Ok(());
    }
    let Some(mut b) = s.browser()? else {
        s.ok("Chrome is installed for Billing's page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let shots = s.dir("billing-page");
    let wait = Duration::from_secs(20);
    let op_session = api.sign_in("operator@e2e.test")?;
    let _ = api.approve(&op_session, &s.operator);
    // a guest: the e2e's people are seats, so an operator makes one a guest
    let operator = s.operator.clone();
    let guest = |id: &str| -> Result<Keys> {
        let keys = api.person()?;
        let who = api.identity(&keys)?;
        let r = api.signed(&operator, "POST", &format!("/api/ledger/{who}/plan"), Some(&json!({ "id": id, "plan": "guest" })))?;
        anyhow::ensure!(r.status == 200, "making a guest: {r}");
        Ok(keys)
    };
    let gus = guest("billing-page-guest")?;
    let session = api.sign_in(&Api::email_of(&gus))?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = b.open(&format!("{}/", api.base))?;
    b.viewport(&page, 1280, 800, false)?;
    let text = |b: &mut crate::browser::Lease, p: &crate::browser::Page| b.eval(p, "document.getElementById('settings-page')?.innerText ?? ''").map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_default();
    // what a guest sees: the middle column, the sidebar, every button that
    // makes something (a guest's create is refused), and any warning
    let seen = "({ path: location.pathname, home: document.getElementById('notice')?.innerText ?? '', sidebar: document.getElementById('sidebar').innerText, makes: ['new-agent', 'new-agent-top', 'new-group', 'add-app'].filter((id) => !document.getElementById(id).hidden), warned: [...document.querySelectorAll('.settings-warning')].map((w) => w.textContent) })";
    let home = b.until(&page, "location.pathname === '/' && !document.getElementById('layout').hidden && /You're a guest/.test(document.getElementById('notice')?.innerText ?? '')", wait);
    let _ = b.screenshot(&page, &shots.join("guest-first-run.png"));
    let first = b.eval(&page, seen)?;
    s.ok(
        "a new guest's first run is home: what a guest may do, a seat and what it gives, and no button that can only fail",
        home && first["makes"] == json!([])
            && first["home"].as_str().is_some_and(|t| t.contains("share with you") && t.contains("Get a seat"))
            && first["sidebar"].as_str().is_some_and(|t| t.contains("shared with you")),
        &first,
    );
    b.eval(&page, "(document.querySelector('#notice .allow')?.click(), true)")?;
    let billing = b.until(&page, "location.pathname === '/settings' && document.querySelector('#settings-page h2')?.textContent === 'Billing' && /You are a guest/.test(document.getElementById('settings-page').innerText)", wait);
    let _ = b.screenshot(&page, &shots.join("guest-billing.png"));
    let headings = b.eval(&page, "[...document.querySelectorAll('#settings-page h2')].map((h) => h.textContent)")?;
    let first = b.eval(&page, seen)?;
    s.ok(
        "Get a seat opens Billing, first: the two seats, nothing in red, and nothing a guest has no use for (a computer, agents, skills, connections)",
        billing && first["warned"] == json!([]) && headings.as_array().is_some_and(|h| ["Computer", "Agents", "Skills", "Connections"].iter().all(|x| !h.contains(&json!(x)))),
        json!({ "headings": headings, "seen": first }),
    );

    b.eval(&page, "[...document.querySelectorAll('#settings-page button')].find(b => b.textContent === 'Get a $200 always-on seat').click()")?;
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    s.ok("its $200 seat goes to Stripe's Checkout", at_stripe, b.eval(&page, "location.href")?);
    b.click(&page, "form button")?;
    let back = b.until(&page, "location.pathname === '/settings' && /Paid: your seat is ready/.test(document.getElementById('settings-page')?.innerText ?? '')", wait);
    let _ = b.screenshot(&page, &shots.join("paid.png"));
    let shown = text(&mut b, &page);
    let buttons = b.eval(&page, "[...document.querySelectorAll('#settings-page button')].map((b) => b.textContent)")?;
    let has = |t: &str| buttons.as_array().is_some_and(|l| l.contains(&json!(t)));
    let alone = b.eval(&page, "!document.querySelector('.billing-seats') && ![...document.querySelectorAll('#settings-page h2')].some((h) => h.textContent.startsWith('Org'))")?;
    let warned = b.eval(&page, "[...document.querySelectorAll('.settings-warning')].map((w) => w.textContent)")?;
    s.ok(
        "paid, Checkout's return brings them back to their seat, a $200 always-on seat: an org of one shows no org's table, only its invoices and seats for others",
        back && shown.contains("$200 always-on seat") && alone == true && warned == json!([]) && has("Payment and invoices") && has("Add seats for others") && !location_has_query(&mut b, &page),
        json!({ "shown": shown, "buttons": buttons, "warned": warned }),
    );

    b.eval(&page, "[...document.querySelectorAll('#settings-page button')].find(b => b.textContent === 'Add seats for others').click()")?;
    let asked = b.until(&page, "!!document.querySelector('.billing-seats') && [...document.querySelectorAll('#settings-page form')].some(f => f.textContent.includes('Add a seat'))", wait);
    s.ok("asked, their org's seats show: theirs alone, and a seat to add by email", asked, text(&mut b, &page));
    let teammate = format!("billing-page-mate-{}@e2e.test", &Keys::generate().pubkey_hex()[..8]);
    b.eval(&page, &format!("(() => {{ const f = [...document.querySelectorAll('#settings-page form')].find(f => f.textContent.includes('Add a seat')); f.querySelector('input[type=email]').value = {teammate:?}; f.requestSubmit(); }})()"))?;
    let added = b.until(&page, &format!("[...document.querySelectorAll('.billing-seats tr')].some(r => r.textContent.includes({teammate:?}) && r.textContent.includes('invited'))"), wait);
    let unasked = b.eval(&page, "![...document.querySelectorAll('#settings-page button')].some((b) => b.textContent === 'Add seats for others')")?;
    s.ok("as its admin they add a seat by email: it waits, invited, and their org's seats now show unasked", added && unasked == true, text(&mut b, &page));

    let before = ledger(api, &gus)["purchasedMicros"].as_i64().unwrap_or(0);
    b.eval(&page, "[...document.querySelectorAll('#settings-page button')].find(b => b.textContent === 'Buy $25 of credit').click()")?;
    let at_stripe = b.until(&page, &format!("location.href.startsWith({:?})", s.stripe.url), wait);
    b.click(&page, "form button")?;
    let credited = b.until(&page, "/Paid: the credit is yours/.test(document.getElementById('settings-page')?.innerText ?? '')", wait);
    s.ok("they buy $25 of credit on Stripe's page, and it is theirs as they land", at_stripe && credited && ledger(api, &gus)["purchasedMicros"].as_i64() == Some(before + 25_000_000), ledger(api, &gus));

    // a seat and no chats yet: Billing offers the first run, which makes
    // their first agent (a guest's create would be refused)
    let offered = b.until(&page, "[...document.querySelectorAll('#settings-page button')].some(b => b.textContent === 'Make your first agent')", wait);
    s.ok("with their seat and no chats, Billing offers their first agent", offered, text(&mut b, &page));
    b.eval(&page, "[...document.querySelectorAll('#settings-page button')].find(b => b.textContent === 'Make your first agent').click()")?;
    let creating = b.until(&page, "location.pathname === '/' && /Creating your agent/.test(document.getElementById('first-run')?.innerText ?? '')", wait);
    let started = std::time::Instant::now();
    let mut theirs = Value::Null;
    // bounded: the wait
    while started.elapsed() < wait {
        theirs = api.signed(&gus, "GET", "/api/fragments", None)?.body;
        if theirs["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["kind"] == "agent" && f["role"] == "owner")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let made = theirs["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["kind"] == "agent" && f["role"] == "owner"));
    s.ok("and it runs: the shell's first run makes their agent, a seat's create", creating && made, &theirs);
    // its computer's start is the shell-ui section's; this page stops here
    b.close(page)?;

    // an invited guest: someone with a seat shares an app they titled with
    // the guest's email, and the guest lands on it
    let owner = api.person()?;
    let label = format!("garden-{}", &Keys::generate().pubkey_hex()[..6]);
    let made = api.signed(&owner, "POST", "/api/fragments", Some(&json!({ "label": label, "template": "todo", "title": "Garden plans" })))?;
    anyhow::ensure!(made.status == 200, "making the app to share: {made}");
    let app = made.body["name"].as_str().unwrap_or("").to_string();
    let bea = guest("billing-page-invited")?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{app}/invites"), Some(&json!({ "email": Api::email_of(&bea), "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "sharing it with the guest: {r}");
    let session = api.sign_in(&Api::email_of(&bea))?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = b.open(&format!("{}/", api.base))?;
    b.viewport(&page, 1280, 800, false)?;
    let landed = b.until(
        &page,
        &format!("location.pathname === '/' && !!document.querySelector('.viewer iframe[data-fragment={app:?}]') && /Get a seat/.test(document.getElementById('notice')?.innerText ?? '')"),
        wait,
    );
    let _ = b.screenshot(&page, &shots.join("invited-guest.png"));
    let row = b.eval(&page, &format!("document.querySelector('#apps .row[data-key=\"app:{app}\"] .label')?.textContent ?? null"))?;
    let first = b.eval(&page, seen)?;
    s.ok(
        "an invited guest lands home on what was shared with them, its window open and titled as its owner titled it, a seat offered beside it",
        landed && row == json!("Garden plans") && first["makes"] == json!([]),
        json!({ "row": row, "seen": first }),
    );
    b.close(page)?;

    // a trial mailed: its link opens Billing with its code
    let code = api.signed(&s.operator, "POST", "/api/admin/trials", Some(&json!({ "name": "Page", "kind": "seat", "days": 5, "capacity": 5 })))?;
    let code = code.body["code"].as_str().unwrap_or("").to_string();
    let ida = guest("billing-page-guest-2")?;
    let session = api.sign_in(&Api::email_of(&ida))?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page2 = b.open(&format!("{}/settings?trial={code}", api.base))?;
    let filled = b.until(&page2, &format!("document.querySelector('#settings-page input[placeholder=\"Trial code\"]')?.value === {code:?}"), wait);
    // evidence for a person: a guest's Billing as it looks (kept with a run's scratch)
    let _ = b.screenshot(&page2, &shots.join("guest.png"));
    s.ok("a trial's link opens Billing with its code filled in", filled, text(&mut b, &page2));
    println!("      (screenshots: {})", shots.display());
    Ok(())
}

/// Whether the page's address still carries a query (Checkout's return
/// takes its own off).
fn location_has_query(b: &mut crate::browser::Lease, page: &crate::browser::Page) -> bool {
    b.eval(page, "location.search").ok().and_then(|v| v.as_str().map(|s| !s.is_empty())).unwrap_or(true)
}
