//! A guest buys a seat on Stripe's own Checkout and their first agent
//! answers (docs/billing.md), in Chrome on a branch preview with Stripe's
//! sandbox: a new person's path, as a person takes it. The `billing` and
//! `billing-page` sections pay on the Stripe fake; this one pays on
//! Stripe's hosted page with its test card. The hosted lane runs it by name
//! (`cargo xtask e2e --hosted … --only checkout --operator-key-file …`).
//!
//! An e2e person signs in as a seat, so the deployment's operator makes
//! them a guest. Home offers them a seat; Get a seat opens Billing; Get a
//! $100 seat goes to Stripe's Checkout, paid with 4242 4242 4242 4242;
//! Checkout's return brings them back to their seat, paid; Make your first
//! agent makes it as the shell's first run does; "hello" is answered, and
//! the answer is in the chat the shell shows. Then its computer sleeps and
//! what the shell made is deleted (its labels are the shell's, not the
//! run's, so `--sweep` would not find them). The subscription stays in the
//! sandbox: canceling it is the portal's, a person's.
//!
//! A check that depends on what the model chooses to do says so in its text.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::computers::{agent_replies, phase, turn_of, QUEUE_DRAIN};
use super::jobs::records;
use super::ledger::entries;
use crate::api::Api;
use crate::browser::{Lease, Page};
use crate::{Need, Suite};

pub const SECTION: &str = "checkout";

/// The paid calls the run lends the person: their agent's first turn (its
/// reply, Hermes' title and guardian).
const PAID_CALLS: u64 = 10;
/// Stripe's sandbox card that pays at once, without 3-D Secure, and what
/// else Checkout asks of it (each by its field's id): any future date, any
/// CVC, any ZIP.
const CARD: [(&str, &str); 5] = [("cardNumber", "4242424242424242"), ("cardExpiry", "1234"), ("cardCvc", "123"), ("billingName", "E2E Checkout"), ("billingPostalCode", "94110")];
/// Where Stripe's hosted Checkout is.
const STRIPE: &str = "https://checkout.stripe.com/";
/// The shell's pages, and Stripe's, to load and answer.
const PAGE: Duration = Duration::from_secs(60);
/// A payment, to Checkout's return to the platform.
const PAID: Duration = Duration::from_secs(90);
/// A new computer's first start on a preview, to its agent following its
/// chat (the shell's first run waits for it).
const FIRST_START: Duration = Duration::from_secs(10 * 60);
/// A turn that answers in words.
const REPLY: Duration = Duration::from_secs(5 * 60);
/// Its owner's sleep, asked until it is asleep.
const SLEEP: Duration = Duration::from_secs(3 * 60);
const POLL: Duration = Duration::from_secs(3);

/// Asks `f` every `POLL` until it answers, or `bound` passes.
#[track_caller]
fn within<T>(bound: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let at = std::panic::Location::caller();
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if t0.elapsed() >= bound {
            println!("      (a wait ran out its {:?} at {}:{})", bound, at.file(), at.line());
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// The person's own fragment of `kind`, by its list.
fn own(api: &Api, keys: &Keys, kind: &str) -> Option<String> {
    let listed = api.signed(keys, "GET", "/api/fragments", None).ok()?;
    listed.body["fragments"].as_array()?.iter().find(|f| f["kind"] == kind && f["role"] == "owner")?["name"].as_str().map(str::to_string)
}

/// Clicks the first button on the page whose text is `text`.
fn press(b: &mut Lease, page: &Page, text: &str) -> Result<bool> {
    Ok(b.eval(page, &format!("(() => {{ const b = [...document.querySelectorAll('button, a')].find((b) => b.textContent.trim() === {text:?}); b?.click(); return !!b; }})()"))? == json!(true))
}

/// Stripe's Checkout, paid with the test card: the card's form opened (an
/// accordion of payment methods shows it at a click), each field it asks
/// for focused and typed as a person types it, and read back (a click
/// could land on the field above as the form grows under it), Link's
/// opt-in turned off (on, it asks for a phone number), and Pay pressed.
/// The filled form is shot beside its Pay button.
fn pay(b: &mut Lease, page: &Page, shot: &std::path::Path) -> Result<Value> {
    let form = "(() => { if (!document.querySelector('#cardNumber')) document.querySelector('[data-testid=\"card-accordion-item-button\"]')?.click(); return !!document.querySelector('#cardNumber'); })()";
    anyhow::ensure!(b.until(page, form, PAGE), "Stripe's Checkout showed no card form");
    let mut filled = serde_json::Map::new();
    for (id, text) in CARD {
        let digits = |t: &str| t.chars().filter(char::is_ascii_digit).collect::<String>();
        // bounded: three tries a field
        for _ in 0..3 {
            let state = b.eval(page, &format!("(() => {{ const e = document.getElementById({id:?}); if (!e || e.offsetParent === null) return 'absent'; if (e.value) return 'filled'; e.scrollIntoView({{ block: 'center' }}); e.focus(); return document.activeElement === e ? 'focused' : 'unfocused'; }})()"))?;
            if state == json!("focused") {
                b.type_text(page, text)?;
            }
            std::thread::sleep(Duration::from_millis(300));
            let value = b.eval(page, &format!("document.getElementById({id:?})?.value ?? null"))?;
            let typed = value.as_str().is_some_and(|v| v == text || (!digits(v).is_empty() && digits(v) == digits(text)));
            filled.insert(id.into(), json!({ "state": state, "value": value }));
            if state == json!("absent") || typed {
                break;
            }
            // a wrong value is cleared and typed again
            b.eval(page, &format!("(() => {{ const e = document.getElementById({id:?}); if (e) {{ e.select(); document.execCommand('delete'); }} return true; }})()"))?;
        }
    }
    b.eval(page, "(() => { const link = document.getElementById('enableStripePass'); if (link?.checked) link.click(); return true; })()")?;
    std::thread::sleep(Duration::from_millis(800));
    let link = b.eval(page, "document.getElementById('enableStripePass')?.checked ?? null")?;
    b.eval(page, &format!("(document.querySelector({SUBMIT:?})?.scrollIntoView({{ block: 'center' }}), true)"))?;
    let _ = b.screenshot(page, shot);
    b.click(page, SUBMIT)?;
    Ok(json!({ "filled": filled, "linkOptIn": link }))
}

/// Checkout's Pay button.
const SUBMIT: &str = "[data-testid=\"hosted-payment-submit-button\"]";

/// What Stripe's page shows, when it does not pay: its errors, the frames
/// it opened (a challenge's), the end of its text, and who it was told
/// the browser is.
fn stripe_state(b: &mut Lease, page: &Page) -> Value {
    let state = "({ url: location.href, errors: [...document.querySelectorAll('.FieldError, [role=alert], .ConfirmPaymentButton--error, .Notice')].map((e) => e.textContent.trim()).filter(Boolean), frames: [...document.querySelectorAll('iframe')].map((f) => (f.src || f.name || '').slice(0, 100)), text: document.body.innerText.slice(-1200), agent: navigator.userAgent })";
    b.eval(page, state).unwrap_or(Value::Null)
}

pub fn checkout(s: &mut Suite, api: &Api) -> Result<()> {
    let why = format!("it buys a $100 seat on Stripe's sandbox Checkout in Chrome and starts a real agent for it, and spends up to {PAID_CALLS} of the run's paid calls");
    if !s.section_by_name(SECTION, &[Need::Levers, Need::Operator, Need::Chrome, Need::Computers, Need::Models, Need::RealAgent], &why) {
        return Ok(());
    }
    anyhow::ensure!(api.signs_in_by_levers(), "a real agent's person signs in through a preview's levers");
    let operator = s.wiper.clone().context("Need::Operator lends an operator key")?;
    let Some(mut b) = s.browser()? else {
        s.ok("Chrome is installed for Stripe's Checkout (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let shots = s.dir(SECTION);

    // ---- a guest: e2e people sign in as seats, so the operator makes one a guest
    let keys = Keys::generate();
    let email = Api::email_of(&keys);
    let (session, who) = api.e2e_sign_in(&email, PAID_CALLS)?;
    api.approve(&session, &keys)?;
    println!("      ({email}, lent {PAID_CALLS} paid calls)");
    let made_guest = api.signed(&operator, "POST", &format!("/api/ledger/{who}/plan"), Some(&json!({ "id": format!("checkout-{}", &keys.pubkey_hex()[..12]), "plan": "guest" })))?;
    anyhow::ensure!(made_guest.status == 200, "making a guest: {made_guest}");
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = b.open(&format!("{}/", api.base))?;
    b.viewport(&page, 1280, 800, false)?;
    let home = b.until(&page, "location.pathname === '/' && /You're a guest/.test(document.getElementById('notice')?.innerText ?? '')", PAGE);
    let _ = b.screenshot(&page, &shots.join("1-guest.png"));
    s.ok("a new guest lands home, a seat offered", home, b.eval(&page, "document.getElementById('notice')?.innerText ?? ''")?);
    b.eval(&page, "(document.querySelector('#notice .allow')?.click(), true)")?;
    let billing = b.until(&page, "location.pathname === '/settings' && [...document.querySelectorAll('#settings-page button')].some((b) => b.textContent === 'Get a $100 seat')", PAGE);
    s.ok("Get a seat opens Billing, its $100 seat offered", billing, b.eval(&page, "document.getElementById('settings-page')?.innerText ?? ''")?);

    // ---- Stripe's own Checkout, paid with the test card
    press(&mut b, &page, "Get a $100 seat")?;
    let at_stripe = b.until(&page, &format!("location.href.startsWith({STRIPE:?})"), PAGE);
    s.ok("Get a $100 seat goes to Stripe's hosted Checkout", at_stripe, b.eval(&page, "location.href")?);
    if !at_stripe {
        let _ = b.screenshot(&page, &shots.join("2-no-checkout.png"));
        return Ok(());
    }
    let t_pay = Instant::now();
    let paid = pay(&mut b, &page, &shots.join("2-checkout.png"));
    let back = paid.is_ok() && b.until(&page, &format!("location.href.startsWith({:?}) && /Paid: your seat is ready/.test(document.getElementById('settings-page')?.innerText ?? '')", api.base), PAID);
    let stripe = if back { Value::Null } else { stripe_state(&mut b, &page) };
    if !back {
        let _ = b.eval(&page, &format!("(document.querySelector({SUBMIT:?})?.scrollIntoView({{ block: 'center' }}), true)"));
    }
    let _ = b.screenshot(&page, &shots.join("3-paid.png"));
    println!("      (Pay pressed to their seat: {:.1?})", t_pay.elapsed());
    let seat = api.signed(&keys, "GET", "/api/seat", None).map(|r| r.body).unwrap_or(Value::Null);
    s.ok(
        "paid with 4242 4242 4242 4242, Checkout's return brings them back to their seat: a $100 seat, paid, in good standing",
        back && seat["seat"]["kind"] == "seat" && seat["seat"]["comped"] == false && seat["seat"]["good"] == true,
        json!({ "paid": paid.as_ref().map_err(|e| format!("{e:#}")), "stripe": stripe, "seat": seat }),
    );
    if !back {
        return Ok(());
    }

    // ---- their first agent, as the shell's first run makes it
    let offered = b.until(&page, "[...document.querySelectorAll('#settings-page button')].some((b) => b.textContent === 'Make your first agent')", PAGE);
    s.ok("with their seat and no chats, Billing offers their first agent", offered, b.eval(&page, "document.getElementById('settings-page')?.innerText ?? ''")?);
    let t_agent = Instant::now();
    press(&mut b, &page, "Make your first agent")?;
    let creating = b.until(&page, "location.pathname === '/' && /Creating your agent/.test(document.getElementById('first-run')?.innerText ?? '')", PAGE);
    let ready = creating && b.until(&page, "document.getElementById('first-run').hidden && !!document.querySelector('#frames iframe[data-fragment]')", FIRST_START);
    let _ = b.screenshot(&page, &shots.join("4-chat.png"));
    println!("      (Make your first agent to its chat open, its computer's first start: {:.1?})", t_agent.elapsed());
    let (agent, chat) = (own(api, &keys, "agent").unwrap_or_default(), own(api, &keys, "chat").unwrap_or_default());
    let computer = api.signed(&keys, "GET", "/api/computers", None).ok().and_then(|r| r.body["computers"][0].clone().as_object().cloned()).map(Value::Object).unwrap_or(Value::Null);
    let id = computer["computer"].as_str().unwrap_or("").to_string();
    let identity = computer["agents"].as_array().and_then(|l| l.iter().find(|a| a["fragment"] == agent.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    s.ok(
        &format!("Make your first agent makes it, its chat and its computer, and opens its chat once it is up, within {FIRST_START:?}"),
        ready && !agent.is_empty() && !chat.is_empty() && !identity.is_empty(),
        json!({ "creating": creating, "agent": agent, "chat": chat, "computer": computer, "firstRun": b.eval(&page, "document.getElementById('first-run')?.innerText ?? ''")? }),
    );

    if ready && !identity.is_empty() {
        // ---- "hello", answered, in the chat the shell shows
        let t_hello = Instant::now();
        let said = api.signed(&keys, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "checkout-hello", "body": { "text": "hello" } })))?;
        let turn = turn_of(&agent, &chat, "chat", said.body["record"]["seq"].as_i64().unwrap_or(0));
        let reply = within(REPLY, || {
            agent_replies(&records(api, &keys, &chat, "chat"), &identity).into_iter().filter(|r| r["body"]["turn"] == turn.as_str()).find_map(|r| r["body"]["text"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string))
        });
        println!("      (hello to its first reply: {:.1?})", t_hello.elapsed());
        s.ok(&format!("\"hello\" is answered within {REPLY:?} (model-dependent)"), said.status == 200 && reply.is_some(), json!({ "said": said.body, "reply": reply }));
        // a word of its answer, as the chat's page draws it (markdown aside)
        let word = reply.as_deref().unwrap_or("").split(|c: char| !c.is_alphabetic()).max_by_key(|w| w.len()).unwrap_or("").to_string();
        let shown = !word.is_empty() && b.until(&page, &format!("!!document.querySelector('#frames iframe[data-fragment={chat:?}]')"), PAGE) && {
            let t0 = Instant::now();
            // bounded: PAGE
            loop {
                let text = b.eval_in_frame(&page, &chat, "document.body.innerText").ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                if text.contains(&word) || t0.elapsed() > PAGE {
                    break text.contains(&word);
                }
                std::thread::sleep(POLL);
            }
        };
        let _ = b.screenshot(&page, &shots.join("5-reply.png"));
        s.ok("its answer shows in the chat the shell has open", shown, json!({ "word": word }));
    }

    // ---- what it spent; then asleep, and what the shell made deleted
    let used = entries(api, &who, "aig:").len() + entries(api, &who, "step:").len();
    println!("      ({used} of the {PAID_CALLS} paid calls lent)");
    s.ok(&format!("its paid calls stayed within the {PAID_CALLS} the run lent it"), used as u64 <= PAID_CALLS, used);
    b.close(page)?;
    if !id.is_empty() {
        std::thread::sleep(QUEUE_DRAIN);
        let slept = within(SLEEP, || {
            let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
            (phase(api, &keys, &id) == "asleep").then_some(())
        });
        s.ok("its computer sleeps at the end", slept.is_some(), phase(api, &keys, &id));
    }
    let names: Vec<String> = api.signed(&keys, "GET", "/api/fragments", None)?.body["fragments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["role"] == "owner")
        .filter_map(|f| f["name"].as_str().map(str::to_string))
        .collect();
    let deleted: Vec<u16> = names.iter().map(|n| api.signed(&keys, "DELETE", &format!("/api/f/{n}"), None).map(|r| r.status).unwrap_or(0)).collect();
    s.ok("what the shell made for them is deleted", deleted.iter().all(|s| *s == 200), json!({ "names": names, "deleted": deleted }));
    println!("      (screenshots: {})", shots.display());
    Ok(())
}
