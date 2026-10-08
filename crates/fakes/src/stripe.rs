//! Stripe, for the surface the platform calls (docs/billing.md, "Stripe"):
//! customers, prices found by lookup key, Checkout Sessions (subscription
//! and payment), subscriptions read and changed, and portal sessions
//! (https://docs.stripe.com/api). Each request must carry the account's key
//! and the pinned `Stripe-Version`; a POST's `Idempotency-Key` answers the
//! same again. Its events go, signed as Stripe signs them, to the endpoint
//! the test sets (`set_endpoint`).
//!
//! A Checkout page stands where Stripe's would (`GET /pay/<session>`, its
//! "Pay" a POST to the same): `cargo xtask dev` people pay there by hand.
//! Levers: complete a session as a card would (`complete`), move a
//! subscription to a status (`set_status`: a trial's end, a decline, a
//! cancel), hold events back and let them go (`hold_events`,
//! `release_events`), send any body to the endpoint (`deliver_raw`), and
//! read what was asked of it (`requests`, `subscription`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fragment_core::stripe::{self, API_VERSION};
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

const DAY_S: i64 = 86_400;

fn now_s() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("after 1970").as_secs() as i64
}

/// A subscription as the fake keeps it.
#[derive(Clone, Debug)]
pub struct Sub {
    pub id: String,
    pub customer: String,
    pub status: String,
    /// (item id, lookup key, quantity)
    pub items: Vec<(String, String, u64)>,
    pub metadata: BTreeMap<String, String>,
    pub trial_end: Option<i64>,
    pub period_end: i64,
    pub cancel_at_period_end: bool,
}

#[derive(Clone, Debug)]
struct Session {
    id: String,
    mode: String,
    customer: String,
    /// (lookup key's price id, quantity)
    lines: Vec<(String, u64)>,
    metadata: BTreeMap<String, String>,
    sub_metadata: BTreeMap<String, String>,
    trial_days: Option<i64>,
    client_reference_id: Option<String>,
    success_url: String,
    status: String,
    subscription: Option<String>,
    amount: i64,
}

/// One request the platform made: its method, path and form.
#[derive(Clone, Debug)]
pub struct Asked {
    pub method: String,
    pub path: String,
    pub form: BTreeMap<String, String>,
}

#[derive(Default)]
struct State {
    counter: u64,
    customers: BTreeMap<String, BTreeMap<String, String>>,
    /// lookup key → (price id, unit amount)
    prices: BTreeMap<String, (String, i64)>,
    sessions: BTreeMap<String, Session>,
    subs: BTreeMap<String, Sub>,
    idempotent: HashMap<String, (u16, Value)>,
    asked: Vec<Asked>,
    endpoint: Option<(String, String)>,
    held: Option<Vec<Value>>,
    /// Events an API call made, delivered once its answer is on its way.
    pending: Vec<Value>,
    delivered: Vec<(String, String, u16)>,
}

impl State {
    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}_{:024}", self.counter)
    }

    fn key_of_price(&self, price: &str) -> Option<String> {
        self.prices.iter().find(|(_, (id, _))| id == price).map(|(k, _)| k.clone())
    }

    fn price_json(&self, key: &str) -> Value {
        let (id, amount) = self.prices.get(key).cloned().unwrap_or_default();
        json!({ "id": id, "object": "price", "lookup_key": key, "unit_amount": amount, "currency": "usd", "active": true,
                "recurring": { "interval": "month", "interval_count": 1 }, "tax_behavior": "exclusive" })
    }

    fn sub_json(&self, s: &Sub) -> Value {
        let items: Vec<Value> = s
            .items
            .iter()
            .map(|(id, key, q)| json!({ "id": id, "object": "subscription_item", "quantity": q, "current_period_end": s.period_end, "price": self.price_json(key) }))
            .collect();
        json!({
            "id": s.id, "object": "subscription", "status": s.status, "customer": s.customer, "metadata": s.metadata,
            "items": { "object": "list", "data": items, "has_more": false },
            "trial_end": s.trial_end, "cancel_at_period_end": s.cancel_at_period_end,
        })
    }

    fn session_json(&self, s: &Session, base: &str) -> Value {
        json!({
            "id": s.id, "object": "checkout.session", "mode": s.mode, "status": s.status, "customer": s.customer,
            "payment_status": if s.status == "complete" { "paid" } else { "unpaid" },
            "subscription": s.subscription, "client_reference_id": s.client_reference_id, "metadata": s.metadata,
            "url": if s.status == "open" { Some(format!("{base}/pay/{}", s.id)) } else { None }, "amount_total": s.amount,
        })
    }

    fn event(&mut self, kind: &str, object: Value) -> Value {
        json!({ "id": self.next("evt"), "object": "event", "type": kind, "created": now_s(), "data": { "object": object }, "api_version": API_VERSION })
    }

    /// The session completes as a card would pay it: a subscription made
    /// (trialing for a trial's days), or a payment taken. Its events.
    fn complete(&mut self, session: &str, base: &str) -> Result<Vec<Value>, String> {
        let mut s = self.sessions.get(session).cloned().ok_or_else(|| format!("no session {session}"))?;
        if s.status != "open" {
            return Err(format!("{session} is {}", s.status));
        }
        let mut events = vec![];
        if s.mode == "subscription" {
            let id = self.next("sub");
            let mut items = vec![];
            for (price, q) in &s.lines {
                let key = self.key_of_price(price).ok_or_else(|| format!("no price {price}"))?;
                items.push((self.next("si"), key, *q));
            }
            let (status, trial_end) = match s.trial_days {
                Some(d) => ("trialing", Some(now_s() + d * DAY_S)),
                None => ("active", None),
            };
            let sub = Sub {
                id: id.clone(),
                customer: s.customer.clone(),
                status: status.into(),
                items,
                metadata: s.sub_metadata.clone(),
                trial_end,
                period_end: trial_end.unwrap_or(now_s() + 30 * DAY_S),
                cancel_at_period_end: false,
            };
            let object = self.sub_json(&sub);
            self.subs.insert(id.clone(), sub);
            s.subscription = Some(id);
            events.push(self.event("customer.subscription.created", object));
        }
        s.status = "complete".into();
        self.sessions.insert(s.id.clone(), s.clone());
        let object = self.session_json(&s, base);
        events.push(self.event("checkout.session.completed", object));
        Ok(events)
    }
}

pub struct Stripe {
    pub url: String,
    pub key: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn error(status: u16, kind: &str, message: &str) -> Response {
    Response::json(status, &json!({ "error": { "type": kind, "message": message } }))
}

/// A form body (`a[b][0]=v`), its keys as they are written.
fn form(body: &[u8]) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

/// The `prefix[...]` keys of a form, as a map by what is inside the brackets.
fn nested(f: &BTreeMap<String, String>, prefix: &str) -> BTreeMap<String, String> {
    f.iter().filter_map(|(k, v)| k.strip_prefix(&format!("{prefix}[")).and_then(|r| r.strip_suffix(']')).map(|inner| (inner.to_string(), v.clone()))).collect()
}

/// `items[i][field]` (or `line_items`) as rows by index.
fn rows(f: &BTreeMap<String, String>, prefix: &str) -> Vec<BTreeMap<String, String>> {
    let mut out: BTreeMap<usize, BTreeMap<String, String>> = BTreeMap::new();
    for (k, v) in f {
        let Some(rest) = k.strip_prefix(&format!("{prefix}[")) else { continue };
        let Some((i, field)) = rest.split_once("][") else { continue };
        let (Ok(i), Some(field)) = (i.parse::<usize>(), field.strip_suffix(']')) else { continue };
        out.entry(i).or_default().insert(field.to_string(), v.clone());
    }
    out.into_values().collect()
}

impl Stripe {
    /// Serves on `port` (0: any) for one account's secret key.
    pub fn start(port: u16, key: &str) -> std::io::Result<Stripe> {
        let state: Arc<Mutex<State>> = Arc::default();
        {
            let mut s = state.lock().expect("stripe state");
            for (key, amount) in [(stripe::lookup_key(fragment_proto::org::SeatKind::Seat), 10_000), (stripe::lookup_key(fragment_proto::org::SeatKind::SeatAlwaysOn), 20_000)] {
                let id = s.next("price");
                s.prices.insert(key.to_string(), (id, amount));
            }
            // finite-mono's, on the same account
            let id = s.next("price");
            s.prices.insert("finite_standard".into(), (id, 20_000));
        }
        let base: Arc<Mutex<String>> = Arc::default();
        let (st, at, secret) = (Arc::clone(&state), Arc::clone(&base), key.to_string());
        let handler: Handler = Arc::new(move |req: &Request| {
            let base = at.lock().expect("stripe base").clone();
            // the Checkout page a person pays on (no key: a browser's)
            if let Some(id) = req.path.strip_prefix("/pay/") {
                return pay_page(&st, req, id, &base);
            }
            if req.header("authorization") != Some(format!("Bearer {secret}").as_str()) {
                return error(401, "invalid_request_error", "Invalid API Key provided");
            }
            if req.header("stripe-version") != Some(API_VERSION) {
                return error(400, "invalid_request_error", &format!("the fake takes Stripe-Version {API_VERSION} only"));
            }
            let f = form(&req.body);
            let mut s = st.lock().expect("stripe state");
            s.asked.push(Asked { method: req.method.clone(), path: req.path.clone(), form: f.clone() });
            let idem = (req.method == "POST").then(|| req.header("idempotency-key").map(|k| format!("{}:{k}", req.path))).flatten();
            if let Some((status, v)) = idem.as_ref().and_then(|k| s.idempotent.get(k)).cloned() {
                return Response::json(status, &v);
            }
            let (status, v) = answer(&mut s, req, &f, &base);
            if let Some(k) = idem.filter(|_| status < 500) {
                s.idempotent.insert(k, (status, v.clone()));
            }
            let pending = std::mem::take(&mut s.pending);
            drop(s);
            if !pending.is_empty() {
                let st = Arc::clone(&st);
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    deliver(&st, pending);
                });
            }
            Response::json(status, &v)
        });
        let server = Server::start(port, handler)?;
        *base.lock().expect("stripe base") = server.url.clone();
        Ok(Stripe { url: server.url.clone(), key: key.to_string(), state, _server: server })
    }

    /// Where its events go, and the endpoint's signing secret.
    pub fn set_endpoint(&self, url: &str, secret: &str) {
        self.state.lock().expect("stripe state").endpoint = Some((url.to_string(), secret.to_string()));
    }

    /// Completes a session as a card would pay it, and delivers its events.
    pub fn complete(&self, session: &str) -> Result<(), String> {
        let events = {
            let mut s = self.state.lock().expect("stripe state");
            s.complete(session, &self.url)?
        };
        self.deliver(events);
        Ok(())
    }

    /// A subscription moves to `status` (a trial ending, a renewal declined,
    /// a cancel), and its event is delivered.
    pub fn set_status(&self, sub: &str, status: &str) -> Result<(), String> {
        let event = {
            let mut s = self.state.lock().expect("stripe state");
            let object = {
                let sb = s.subs.get_mut(sub).ok_or_else(|| format!("no subscription {sub}"))?;
                // a trial that ends into a paid month starts that month
                if sb.status == "trialing" && status == "active" {
                    sb.period_end = now_s() + 30 * DAY_S;
                }
                sb.status = status.to_string();
                sb.clone()
            };
            let object = s.sub_json(&object);
            let kind = if status == "canceled" { "customer.subscription.deleted" } else { "customer.subscription.updated" };
            s.event(kind, object)
        };
        self.deliver(vec![event]);
        Ok(())
    }

    /// A subscription's item at `lookup_key` comes to hold `quantity`, with
    /// no event: someone changed it in Stripe's dashboard.
    pub fn set_quantity(&self, sub: &str, lookup_key: &str, quantity: u64) -> Result<(), String> {
        let mut s = self.state.lock().expect("stripe state");
        let sb = s.subs.get_mut(sub).ok_or_else(|| format!("no subscription {sub}"))?;
        let it = sb.items.iter_mut().find(|(_, k, _)| k == lookup_key).ok_or_else(|| format!("{sub} has no item {lookup_key}"))?;
        it.2 = quantity;
        Ok(())
    }

    /// Events wait instead of going (`true`), as a lost webhook would; or go again.
    pub fn hold_events(&self, hold: bool) {
        let mut s = self.state.lock().expect("stripe state");
        s.held = if hold { Some(s.held.take().unwrap_or_default()) } else { None };
    }

    /// The events held back, delivered now, newest first when `reversed`
    /// (out of order); none are held after.
    pub fn release_events(&self, reversed: bool) {
        let mut held = self.state.lock().expect("stripe state").held.take().unwrap_or_default();
        if reversed {
            held.reverse();
        }
        self.deliver(held);
    }

    /// The events delivered: (event id, type, the endpoint's status).
    pub fn delivered(&self) -> Vec<(String, String, u16)> {
        self.state.lock().expect("stripe state").delivered.clone()
    }

    /// Posts `body` to the endpoint with `signature` as its header (none:
    /// none): a forged event, or another deployment's. The endpoint's status.
    pub fn deliver_raw(&self, body: &[u8], signature: Option<&str>) -> Result<u16, String> {
        let url = self.state.lock().expect("stripe state").endpoint.clone().map(|(u, _)| u).ok_or("no endpoint set")?;
        let mut headers = vec![("content-type", "application/json")];
        if let Some(sig) = signature {
            headers.push(("stripe-signature", sig));
        }
        crate::http::post(&url, &headers, body)
    }

    /// An event of `kind` about `object`, signed with the endpoint's secret
    /// at `t` (now: `None`): its body and header, for `deliver_raw`.
    pub fn signed_event(&self, kind: &str, object: Value, t: Option<i64>) -> (Vec<u8>, String) {
        let mut s = self.state.lock().expect("stripe state");
        let event = s.event(kind, object);
        let secret = s.endpoint.clone().map(|(_, k)| k).unwrap_or_default();
        let body = event.to_string().into_bytes();
        let sig = stripe::sign(&secret, t.unwrap_or_else(now_s), &body);
        (body, sig)
    }

    pub fn subscription(&self, id: &str) -> Option<Sub> {
        self.state.lock().expect("stripe state").subs.get(id).cloned()
    }

    /// A subscription's JSON, as the API answers it.
    pub fn subscription_json(&self, id: &str) -> Option<Value> {
        let s = self.state.lock().expect("stripe state");
        s.subs.get(id).map(|sb| s.sub_json(sb))
    }

    /// Every request the platform made, oldest first.
    pub fn requests(&self) -> Vec<Asked> {
        self.state.lock().expect("stripe state").asked.clone()
    }

    fn deliver(&self, events: Vec<Value>) {
        deliver(&self.state, events);
    }
}

/// Each event to the endpoint, signed now, the lock never held across a
/// delivery (the endpoint asks the fake back); held back instead while
/// events are held.
fn deliver(st: &Arc<Mutex<State>>, events: Vec<Value>) {
    // bounded: the events one lever or call made
    for event in events {
        let (url, secret) = {
            let mut s = st.lock().expect("stripe state");
            if let Some(held) = s.held.as_mut() {
                held.push(event);
                continue;
            }
            match s.endpoint.clone() {
                Some(e) => e,
                None => continue,
            }
        };
        let body = event.to_string().into_bytes();
        let sig = stripe::sign(&secret, now_s(), &body);
        let status = crate::http::post(&url, &[("content-type", "application/json"), ("stripe-signature", &sig)], &body).unwrap_or(0);
        let (id, kind) = (event["id"].as_str().unwrap_or("").to_string(), event["type"].as_str().unwrap_or("").to_string());
        st.lock().expect("stripe state").delivered.push((id, kind, status));
    }
}

/// The API's answer to one request (the state locked).
fn answer(s: &mut State, req: &Request, f: &BTreeMap<String, String>, base: &str) -> (u16, Value) {
    let path = req.path.as_str();
    match (req.method.as_str(), path) {
        ("POST", "/v1/customers") => {
            let id = s.next("cus");
            let metadata = nested(f, "metadata");
            s.customers.insert(id.clone(), metadata.clone());
            (200, json!({ "id": id, "object": "customer", "email": f.get("email"), "name": f.get("name"), "metadata": metadata }))
        }
        ("GET", "/v1/prices") => {
            let keys: Vec<String> = req.pairs.iter().filter(|(k, _)| k == "lookup_keys[]").map(|(_, v)| v.clone()).collect();
            let data: Vec<Value> = keys.iter().filter(|k| s.prices.contains_key(*k)).map(|k| s.price_json(k)).collect();
            (200, json!({ "object": "list", "data": data, "has_more": false }))
        }
        ("POST", "/v1/checkout/sessions") => {
            let mode = f.get("mode").cloned().unwrap_or_default();
            let customer = f.get("customer").cloned().unwrap_or_default();
            if !s.customers.contains_key(&customer) {
                return (400, json!({ "error": { "type": "invalid_request_error", "message": format!("No such customer: '{customer}'") } }));
            }
            let lines: Vec<(String, u64)> = rows(f, "line_items").into_iter().map(|r| (r.get("price").cloned().unwrap_or_default(), r.get("quantity").and_then(|q| q.parse().ok()).unwrap_or(1))).collect();
            if mode == "subscription" && lines.iter().any(|(p, _)| s.key_of_price(p).is_none()) {
                return (400, json!({ "error": { "type": "invalid_request_error", "message": "No such price" } }));
            }
            let amount = lines.iter().map(|(p, q)| s.key_of_price(p).and_then(|k| s.prices.get(&k).map(|(_, a)| a * *q as i64)).unwrap_or(0)).sum::<i64>()
                + f.get("line_items[0][price_data][unit_amount]").and_then(|a| a.parse::<i64>().ok()).unwrap_or(0);
            let session = Session {
                id: s.next("cs"),
                mode,
                customer,
                lines,
                metadata: nested(f, "metadata"),
                sub_metadata: f.iter().filter_map(|(k, v)| k.strip_prefix("subscription_data[metadata][").and_then(|r| r.strip_suffix(']')).map(|m| (m.to_string(), v.clone()))).collect(),
                trial_days: f.get("subscription_data[trial_period_days]").and_then(|d| d.parse().ok()),
                client_reference_id: f.get("client_reference_id").cloned(),
                success_url: f.get("success_url").cloned().unwrap_or_default(),
                status: "open".into(),
                subscription: None,
                amount,
            };
            let v = s.session_json(&session, base);
            s.sessions.insert(session.id.clone(), session);
            (200, v)
        }
        ("GET", p) if p.starts_with("/v1/checkout/sessions/") => match s.sessions.get(&p["/v1/checkout/sessions/".len()..]) {
            Some(session) => (200, s.session_json(session, base)),
            None => (404, json!({ "error": { "type": "invalid_request_error", "message": "No such checkout.session" } })),
        },
        ("GET", p) if p.starts_with("/v1/subscriptions/") => match s.subs.get(&p["/v1/subscriptions/".len()..]) {
            Some(sub) => (200, s.sub_json(sub)),
            None => (404, json!({ "error": { "type": "invalid_request_error", "message": "No such subscription" } })),
        },
        ("POST", p) if p.starts_with("/v1/subscriptions/") => {
            let id = p["/v1/subscriptions/".len()..].to_string();
            let Some(mut sub) = s.subs.get(&id).cloned() else {
                return (404, json!({ "error": { "type": "invalid_request_error", "message": "No such subscription" } }));
            };
            if matches!(sub.status.as_str(), "canceled" | "incomplete_expired") {
                return (400, json!({ "error": { "type": "invalid_request_error", "message": "A canceled subscription can only update its cancellation_details and metadata." } }));
            }
            for row in rows(f, "items") {
                let q = row.get("quantity").and_then(|q| q.parse::<u64>().ok());
                match (row.get("id"), row.get("price")) {
                    (Some(item), _) if row.get("deleted").map(String::as_str) == Some("true") => sub.items.retain(|(i, _, _)| i != item),
                    (Some(item), _) => {
                        if let Some(it) = sub.items.iter_mut().find(|(i, _, _)| i == item) {
                            it.2 = q.unwrap_or(it.2);
                        }
                    }
                    (None, Some(price)) => {
                        let Some(key) = s.key_of_price(price) else {
                            return (400, json!({ "error": { "type": "invalid_request_error", "message": "No such price" } }));
                        };
                        let item = s.next("si");
                        sub.items.push((item, key, q.unwrap_or(1)));
                    }
                    _ => {}
                }
            }
            if let Some(c) = f.get("cancel_at_period_end") {
                sub.cancel_at_period_end = c == "true";
            }
            let v = s.sub_json(&sub);
            s.subs.insert(id, sub);
            let event = s.event("customer.subscription.updated", v.clone());
            s.pending.push(event);
            (200, v)
        }
        ("POST", "/v1/billing_portal/sessions") => {
            let id = s.next("bps");
            (200, json!({ "id": id, "object": "billing_portal.session", "url": format!("{base}/portal/{id}"), "return_url": f.get("return_url"), "configuration": f.get("configuration") }))
        }
        _ => (404, json!({ "error": { "type": "invalid_request_error", "message": format!("Unrecognized request URL ({} {path})", req.method) } })),
    }
}

/// `GET /pay/<id>`: a page with a Pay button; its POST completes the
/// session and sends the browser to the session's success URL.
fn pay_page(st: &Arc<Mutex<State>>, req: &Request, id: &str, base: &str) -> Response {
    if req.method == "GET" {
        let s = st.lock().expect("stripe state");
        let Some(session) = s.sessions.get(id) else { return error(404, "invalid_request_error", "no such session") };
        let page = format!(
            r#"<!doctype html><title>Stripe Checkout (fake)</title><form method="post"><p>The Stripe fake: pay {} (test card)</p><button>Pay</button></form>"#,
            match session.trial_days {
                Some(d) => format!("nothing today, a {d}-day trial"),
                None => format!("${:.2}", session.amount as f64 / 100.0),
            }
        );
        return Response::bytes(200, "text/html; charset=utf-8", page.into_bytes());
    }
    let (events, success) = {
        let mut s = st.lock().expect("stripe state");
        match s.complete(id, base) {
            Ok(events) => (events, s.sessions.get(id).map(|x| x.success_url.replace("{CHECKOUT_SESSION_ID}", id)).unwrap_or_default()),
            Err(why) => return error(400, "invalid_request_error", &why),
        }
    };
    deliver(st, events);
    Response::bytes(303, "text/plain", vec![]).with_header("location", &success)
}
