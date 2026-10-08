//! Buying seats through Stripe on the platform's API (docs/billing.md,
//! "Stripe"; docs/api.md, Billing): Checkout for a person's own seat, its
//! return, the portal for card and invoices, and Stripe's webhook. Every
//! subscription the platform writes is fetched from Stripe first, never
//! taken from what it was handed; the registry keeps the copy
//! (registry/billing.rs) and pushes what it means to each holder.

use fragment_core::stripe::{self as core_stripe, CheckoutSession, Event, Form, List, Price, Subscription, META_BUYER, META_DEPLOYMENT, META_KIND, META_ORG, META_PACK, META_PERSON};
use fragment_proto::ledger::GrantCredit;
use fragment_proto::org::{MySeat, SeatKind};
use fragment_proto::{limits, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::billing::{ApplyCheckout, ApplySubscription, CheckoutBegin, CustomerOf, PackBegin, SetCustomer, SubscriptionCopy};
use crate::registry::calls::By;
use crate::registry::orgs::Mine;
use crate::stripe::Stripe;
use crate::{acting_for, ask_registry, caller, js, json_answer, read_body, signer, Caller};
use fragment_nip98::Payload;

/// A Checkout's life, past Stripe's least (30 minutes) by a minute.
const CHECKOUT_TTL_S: i64 = 31 * 60;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Buy {
    /// The seat's kind (a trial code's own when one is named).
    #[serde(default)]
    kind: Option<SeatKind>,
    #[serde(default)]
    trial_code: Option<String>,
}

/// The Checkout's metadata key for the trial code it redeems.
const META_TRIAL: &str = "fragment_trial";

#[derive(Serialize)]
struct Checkout {
    url: String,
    session: String,
}

#[derive(Serialize)]
struct Portal {
    url: String,
}

fn by(env: &Env, req: &Request, url: &Url, bytes: &[u8]) -> CellResult<By> {
    if acting_for(url)?.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    Ok(match caller(env, req, url, Payload::Read(bytes))? {
        Caller::Key(k) => By::Key(k),
        Caller::Session(t) => By::Session(t),
    })
}

/// The seat's monthly price, found by its lookup key, as decision 25 has
/// it: active, USD, monthly, at its amount. Anything else is a deployment
/// whose Stripe is not set up as `xtask stripe check` wants it.
async fn price(stripe: &Stripe, kind: SeatKind) -> CellResult<String> {
    let key = core_stripe::lookup_key(kind);
    let found: List<Price> = stripe.get(&format!("/v1/prices?lookup_keys[]={key}&active=true")).await?;
    let p = found.data.into_iter().next().ok_or_else(|| CellError::host(format!("Stripe has no active price {key}: run `cargo xtask stripe setup`")))?;
    if p.unit_amount != Some(core_stripe::unit_amount(kind)) || p.currency.as_deref() != Some("usd") {
        return Err(CellError::host(format!("Stripe's price {key} is not ${} a month in USD", core_stripe::unit_amount(kind) / 100)));
    }
    Ok(p.id)
}

/// `POST /api/billing/checkout {kind?, trialCode?}`: a Checkout for the
/// asker's own seat, in their org (one made for them if they are in none);
/// with a trial code, Stripe's trial of its days, a card taken first.
async fn checkout(env: &Env, cfg: &Config, by: By, buy: Buy) -> CellResult<Checkout> {
    let stripe_cfg = cfg.stripe()?;
    let expires_at_s = js::now_ms() / 1000 + CHECKOUT_TTL_S;
    let plan = ask_registry(env, &CheckoutBegin { by, kind: buy.kind, trial: buy.trial_code, until_ms: expires_at_s * 1000 }).await?;
    let kind = plan.kind;
    let stripe = crate::stripe::client(env, cfg).await?;
    let deployment = cfg.stripe_deployment();
    let customer = customer(env, cfg, &stripe, &plan.org, &plan.org_name, &plan.email, plan.customer).await?;
    let price = price(&stripe, kind).await?;
    let platform = cfg.platform();
    let mut form = Form::new()
        .push("mode", "subscription")
        .push("customer", customer)
        .push("client_reference_id", plan.org.clone())
        .push("line_items[0][price]", price)
        .push("line_items[0][quantity]", "1")
        .push("payment_method_collection", "always")
        .push("success_url", format!("{platform}/settings?checkout={{CHECKOUT_SESSION_ID}}"))
        .push("cancel_url", format!("{platform}/settings?checkout=canceled"))
        .push("expires_at", expires_at_s.to_string())
        .push(format!("metadata[{META_DEPLOYMENT}]"), deployment)
        .push(format!("metadata[{META_ORG}]"), plan.org.clone())
        .push(format!("metadata[{META_PERSON}]"), plan.person.clone())
        .push(format!("metadata[{META_KIND}]"), kind.as_str())
        .push(format!("subscription_data[metadata][{META_DEPLOYMENT}]"), deployment)
        .push(format!("subscription_data[metadata][{META_ORG}]"), plan.org.clone());
    form = match &plan.trial {
        // a trial: Stripe's days, its card taken now and charged at the end
        // (none on file at the end cancels it); no discount on top
        Some(t) => form
            .push("subscription_data[trial_period_days]", t.days.to_string())
            .push("subscription_data[trial_settings][end_behavior][missing_payment_method]", "cancel")
            .push(format!("metadata[{META_TRIAL}]"), t.id.clone()),
        None => form.push("allow_promotion_codes", "true"),
    };
    if stripe_cfg.tax {
        form = form.push("automatic_tax[enabled]", "true").push("customer_update[address]", "auto");
    }
    let attempt = hex::encode(js::random_bytes::<8>());
    let session: CheckoutSession = stripe.post("/v1/checkout/sessions", &form, &format!("fragment-checkout-{}-{attempt}", plan.org)).await?;
    let url = session.url.ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, "Stripe made a Checkout with no URL"))?;
    Ok(Checkout { url, session: session.id })
}

/// An org's Stripe customer: the one it has, or one made for it, once.
async fn customer(env: &Env, cfg: &Config, stripe: &Stripe, org: &str, name: &str, email: &str, has: Option<String>) -> CellResult<String> {
    if let Some(c) = has {
        return Ok(c);
    }
    #[derive(Deserialize)]
    struct Made {
        id: String,
    }
    let form = Form::new()
        .push("email", email)
        .push("name", name)
        .push(format!("metadata[{META_DEPLOYMENT}]"), cfg.stripe_deployment())
        .push(format!("metadata[{META_ORG}]"), org);
    let made: Made = stripe.post("/v1/customers", &form, &format!("fragment-customer-{org}")).await?;
    Ok(ask_registry(env, &SetCustomer { org: org.to_string(), customer: made.id }).await?.customer)
}

/// `POST /api/billing/packs {member?}`: a Checkout for a $25 credit pack
/// (decision 55), for the buyer's own ledger or an org member's.
async fn pack(env: &Env, cfg: &Config, by: By, member: Option<String>) -> CellResult<Checkout> {
    let stripe_cfg = cfg.stripe()?;
    let plan = ask_registry(env, &PackBegin { by, member }).await?;
    let stripe = crate::stripe::client(env, cfg).await?;
    let customer = customer(env, cfg, &stripe, &plan.org, &plan.org_name, &plan.email, plan.customer).await?;
    let found: List<Price> = stripe.get(&format!("/v1/prices?lookup_keys[]={}&active=true", core_stripe::PACK_LOOKUP_KEY)).await?;
    let price = found.data.into_iter().next().ok_or_else(|| CellError::host(format!("Stripe has no active price {}: run `cargo xtask stripe setup`", core_stripe::PACK_LOOKUP_KEY)))?;
    if price.unit_amount != Some(core_stripe::PACK_CENTS) || price.currency.as_deref() != Some("usd") {
        return Err(CellError::host(format!("Stripe's price {} is not ${} in USD", core_stripe::PACK_LOOKUP_KEY, core_stripe::PACK_CENTS / 100)));
    }
    let platform = cfg.platform();
    let deployment = cfg.stripe_deployment();
    let mut form = Form::new()
        .push("mode", "payment")
        .push("customer", customer)
        .push("client_reference_id", plan.org.clone())
        .push("line_items[0][price]", price.id)
        .push("line_items[0][quantity]", "1")
        .push("success_url", format!("{platform}/settings?pack={{CHECKOUT_SESSION_ID}}"))
        .push("cancel_url", format!("{platform}/settings?pack=canceled"))
        .push("expires_at", (js::now_ms() / 1000 + CHECKOUT_TTL_S).to_string())
        .push(format!("metadata[{META_DEPLOYMENT}]"), deployment)
        .push(format!("metadata[{META_ORG}]"), plan.org.clone())
        .push(format!("metadata[{META_PERSON}]"), plan.person.clone())
        .push(format!("metadata[{META_BUYER}]"), plan.buyer.clone())
        .push(format!("metadata[{META_PACK}]"), (core_stripe::PACK_CENTS / 100).to_string());
    if stripe_cfg.tax {
        form = form.push("automatic_tax[enabled]", "true").push("customer_update[address]", "auto");
    }
    let attempt = hex::encode(js::random_bytes::<8>());
    let session: CheckoutSession = stripe.post("/v1/checkout/sessions", &form, &format!("fragment-pack-{}-{attempt}", plan.org)).await?;
    let url = session.url.ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, "Stripe made a Checkout with no URL"))?;
    Ok(Checkout { url, session: session.id })
}

/// A paid pack's credit on its person's ledger, once by its Checkout.
async fn pack_paid(env: &Env, session: &CheckoutSession) -> CellResult<bool> {
    let person = session.metadata.get(META_PERSON).ok_or_else(|| CellError::host(format!("{} has no {META_PERSON}", session.id)))?;
    if !fragment_core::npub::is_identity(person) {
        return Err(CellError::host(format!("{}'s {META_PERSON} is no identity", session.id)));
    }
    let grant = GrantCredit {
        id: format!("pack:{}", session.id),
        micros: core_stripe::PACK_MICROS,
        by: "stripe".into(),
        why: format!("a ${} credit pack (Stripe Checkout {})", core_stripe::PACK_CENTS / 100, session.id),
    };
    crate::ledger::ask(env, person, &grant).await.map_err(|e| CellError::new(e.code, format!("{person}'s ledger: {}", e.message)))?;
    Ok(true)
}

/// The subscription `id`, fetched from Stripe, as the registry keeps it.
async fn fetched(stripe: &Stripe, id: &str) -> CellResult<(Subscription, SubscriptionCopy)> {
    let sub: Subscription = stripe.get(&format!("/v1/subscriptions/{id}")).await?;
    let copy = sub.snapshot().map_err(CellError::host)?.into();
    Ok((sub, copy))
}

/// A completed Checkout of a seat (its return, or its webhook): the
/// subscription it made, fetched, and its buyer's seat. One of another
/// deployment's, or that is not yet complete, changes nothing.
async fn checked_out(env: &Env, cfg: &Config, stripe: &Stripe, session: &CheckoutSession) -> CellResult<bool> {
    if !core_stripe::ours(&session.metadata, cfg.stripe_deployment()) || session.status.as_deref() != Some("complete") {
        return Ok(false);
    }
    if session.mode == "payment" && session.metadata.contains_key(META_PACK) {
        return match session.payment_status.as_deref() {
            Some("paid") => pack_paid(env, session).await,
            _ => Ok(false),
        };
    }
    if session.mode != "subscription" {
        return Ok(false);
    }
    let meta = |k: &str| session.metadata.get(k).cloned().ok_or_else(|| CellError::host(format!("{} has no {k}", session.id)));
    let (org, person) = (meta(META_ORG)?, meta(META_PERSON)?);
    let kind = SeatKind::parse(&meta(META_KIND)?).ok_or_else(|| CellError::host(format!("{}'s kind is no seat's", session.id)))?;
    let sub_id = session.subscription.as_deref().ok_or_else(|| CellError::host(format!("{} completed with no subscription", session.id)))?;
    let (sub, copy) = fetched(stripe, sub_id).await?;
    if sub.metadata.get(META_ORG) != Some(&org) {
        return Err(CellError::host(format!("{sub_id} names another org than its Checkout {}", session.id)));
    }
    let trial = session.metadata.get(META_TRIAL).cloned();
    Ok(ask_registry(env, &ApplyCheckout { session: session.id.clone(), org, person, kind, subscription: copy, trial }).await?.applied)
}

/// `POST /api/billing/sessions/<id>`: the Checkout's return. The buyer
/// has their seat as they land, whatever the webhook's timing.
async fn returned(env: &Env, cfg: &Config, req: &Request, url: &Url, bytes: &[u8], id: &str) -> CellResult<MySeat> {
    let who = signer(env, req, url, bytes).await?;
    let stripe = crate::stripe::client(env, cfg).await?;
    if !id.starts_with("cs_") || id.len() > 255 {
        return Err(CellError::new(ErrorCode::NotFound, format!("no Checkout {id}")));
    }
    let session: CheckoutSession = stripe.get(&format!("/v1/checkout/sessions/{id}")).await?;
    // another's Checkout is none of the asker's (a pack's is its buyer's)
    let theirs = session.metadata.get(META_BUYER).or(session.metadata.get(META_PERSON));
    if theirs != Some(&who.id) || !core_stripe::ours(&session.metadata, cfg.stripe_deployment()) {
        return Err(CellError::new(ErrorCode::NotFound, format!("no Checkout {id}")));
    }
    checked_out(env, cfg, &stripe, &session).await?;
    let by = match &who.key {
        Some(k) => By::Key(k.clone()),
        None => By::Identity(who.id.clone()),
    };
    ask_registry(env, &Mine { by }).await
}

/// `POST /api/billing/portal`: Stripe's portal for the org the asker
/// admins: card, invoices, billing address, and cancel at period end.
async fn portal(env: &Env, cfg: &Config, by: By) -> CellResult<Portal> {
    let customer = ask_registry(env, &CustomerOf { by }).await?.customer;
    let stripe = crate::stripe::client(env, cfg).await?;
    let mut form = Form::new().push("customer", customer).push("return_url", format!("{}/settings", cfg.platform()));
    if let Some(p) = &cfg.stripe()?.portal {
        form = form.push("configuration", p.clone());
    }
    #[derive(Deserialize)]
    struct Session {
        url: String,
    }
    let s: Session = stripe.post("/v1/billing_portal/sessions", &form, &format!("fragment-portal-{}", hex::encode(js::random_bytes::<8>()))).await?;
    Ok(Portal { url: s.url })
}

/// `/api/billing/…`: a person's.
pub(crate) async fn route(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    match (req.method(), rest) {
        (Method::Post, ["checkout"]) => {
            let buy: Buy = serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let by = by(env, &req, url, &bytes)?;
            json_answer(&checkout(env, cfg, by, buy).await?)
        }
        (Method::Post, ["sessions", id]) => json_answer(&returned(env, cfg, &req, url, &bytes, id).await?),
        (Method::Post, ["packs"]) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Buying {
                #[serde(default)]
                member: Option<String>,
            }
            let b: Buying = serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let by = by(env, &req, url, &bytes)?;
            json_answer(&pack(env, cfg, by, b.member).await?)
        }
        (Method::Post, ["portal"]) => {
            let by = by(env, &req, url, &bytes)?;
            json_answer(&portal(env, cfg, by).await?)
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/billing/{}", m.as_ref(), rest.join("/")))),
    }
}

/// `POST /api/stripe/webhook`: Stripe's events, signed with this
/// deployment's endpoint's secret. Anything not this deployment's is
/// answered and dropped (finite-mono's, another preview's); what is, is
/// fetched again from Stripe and written. A failure answers 5xx, and
/// Stripe sends it again.
pub(crate) async fn webhook(mut req: Request, env: &Env, cfg: &Config) -> CellResult<Response> {
    cfg.stripe()?;
    let bytes = read_body(&mut req, core_stripe::WEBHOOK_BODY_MAX_BYTES).await?;
    let secret = crate::keys::stripe_webhook_secret(env).await?;
    let signature = req.headers().get("stripe-signature")?;
    core_stripe::verify(signature.as_deref(), &bytes, &secret, js::now_ms() / 1000).map_err(|e| CellError::new(ErrorCode::Unauthenticated, e.message()))?;
    let event: Event = serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("an event: {e}")))?;
    let deployment = cfg.stripe_deployment();
    let handled = match event.kind.as_str() {
        "checkout.session.completed" => {
            let session: CheckoutSession = serde_json::from_value(event.data.object).map_err(|e| CellError::invalid(format!("a session: {e}")))?;
            match core_stripe::ours(&session.metadata, deployment) {
                true => {
                    let stripe = crate::stripe::client(env, cfg).await?;
                    checked_out(env, cfg, &stripe, &session).await?
                }
                false => false,
            }
        }
        "customer.subscription.created" | "customer.subscription.updated" | "customer.subscription.deleted" => {
            #[derive(Deserialize)]
            struct Named {
                id: String,
                #[serde(default)]
                metadata: std::collections::BTreeMap<String, String>,
            }
            let named: Named = serde_json::from_value(event.data.object).map_err(|e| CellError::invalid(format!("a subscription: {e}")))?;
            match (core_stripe::ours(&named.metadata, deployment), named.metadata.get(META_ORG)) {
                (true, Some(org)) => {
                    let stripe = crate::stripe::client(env, cfg).await?;
                    let (_, copy) = fetched(&stripe, &named.id).await?;
                    ask_registry(env, &ApplySubscription { org: org.clone(), subscription: copy, at: Some(event.created) }).await?.applied
                }
                _ => false,
            }
        }
        _ => false,
    };
    console_log!("{}", json!({ "stripe-event": event.id, "type": event.kind, "applied": handled }));
    json_answer(&json!({ "received": true, "applied": handled }))
}
