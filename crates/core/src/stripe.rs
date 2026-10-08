//! Stripe as fragment uses it (docs/billing.md, "Stripe"), the parts with
//! no network: a webhook's signature, the form bodies Stripe takes, what
//! fragment reads of a subscription and a Checkout Session, and the item
//! changes that bring a subscription's quantities to an org's seats. The
//! cell's client is `cell/src/stripe.rs`; the fake is `crates/fakes`'.
//!
//! The account is shared with finite-mono: everything fragment makes
//! carries `fragment_deployment` in its metadata, and anything without
//! this deployment's is not fragment's to touch (`ours`).

use std::collections::BTreeMap;

use fragment_proto::org::SeatKind;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::org::Status;

/// The API version every request pins (finite-mono's: items carry their
/// own `current_period_end`).
pub const API_VERSION: &str = "2026-04-22.dahlia";
/// How far a webhook's signed time may be from now.
pub const SIGNATURE_TOLERANCE_S: i64 = 300;
/// A webhook's body, at most.
pub const WEBHOOK_BODY_MAX_BYTES: usize = 256 * 1024;
/// The events a deployment's endpoint takes (and `xtask stripe check`
/// expects it to take exactly).
pub const EVENTS: [&str; 4] = ["checkout.session.completed", "customer.subscription.created", "customer.subscription.updated", "customer.subscription.deleted"];

/// Metadata keys on what fragment makes: the deployment (its platform's
/// origin), the org, and on a Checkout Session the person it seats and
/// their seat's kind.
pub const META_DEPLOYMENT: &str = "fragment_deployment";
pub const META_ORG: &str = "fragment_org";
pub const META_PERSON: &str = "fragment_person";
pub const META_KIND: &str = "fragment_kind";

/// A seat's monthly price, found by its lookup key: a price change is a
/// new Price with the key moved to it (`transfer_lookup_key`).
pub fn lookup_key(kind: SeatKind) -> &'static str {
    match kind {
        SeatKind::Seat => "fragment_seat_month",
        SeatKind::SeatAlwaysOn => "fragment_seat_always_on_month",
    }
}

pub fn kind_of_lookup_key(key: &str) -> Option<SeatKind> {
    [SeatKind::Seat, SeatKind::SeatAlwaysOn].into_iter().find(|k| lookup_key(*k) == key)
}

/// Each seat's price in cents, as `xtask stripe check` expects it (decision 25).
pub fn unit_amount(kind: SeatKind) -> i64 {
    match kind {
        SeatKind::Seat => 10_000,
        SeatKind::SeatAlwaysOn => 20_000,
    }
}

/// Whether metadata names this deployment: fragment's, and this one's.
pub fn ours(metadata: &BTreeMap<String, String>, deployment: &str) -> bool {
    metadata.get(META_DEPLOYMENT).is_some_and(|d| d == deployment)
}

// ------------------------------------------------------------- signatures

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// No `Stripe-Signature` header.
    Missing,
    /// A header with no time, or no `v1`.
    Malformed,
    /// Signed more than `SIGNATURE_TOLERANCE_S` from now.
    Stale,
    /// No `v1` matches the body under the secret.
    Mismatch,
}

impl SignatureError {
    pub fn message(self) -> &'static str {
        match self {
            SignatureError::Missing => "no Stripe-Signature header",
            SignatureError::Malformed => "a Stripe-Signature names its time (t=) and a v1 signature",
            SignatureError::Stale => "a Stripe-Signature older (or newer) than five minutes",
            SignatureError::Mismatch => "no Stripe-Signature matches the body",
        }
    }
}

fn mac(secret: &str, t: i64, body: &[u8]) -> Hmac<Sha256> {
    let mut h = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    h.update(t.to_string().as_bytes());
    h.update(b".");
    h.update(body);
    h
}

/// Checks a webhook's `Stripe-Signature` (`t=<s>,v1=<hex>[,v1=…]`): HMAC-
/// SHA256 of `<t>.<body>` under the endpoint's secret, compared in constant
/// time; any `v1` may match (a secret rolling over signs with both). Answers
/// the signed time.
pub fn verify(header: Option<&str>, body: &[u8], secret: &str, now_s: i64) -> Result<i64, SignatureError> {
    let header = header.ok_or(SignatureError::Missing)?;
    let mut t = None;
    let mut sigs = vec![];
    // bounded: a header's parts, each read once
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v),
            _ => {}
        }
    }
    let t = t.ok_or(SignatureError::Malformed)?;
    if sigs.is_empty() {
        return Err(SignatureError::Malformed);
    }
    if (now_s - t).abs() > SIGNATURE_TOLERANCE_S {
        return Err(SignatureError::Stale);
    }
    let matched = sigs.iter().filter_map(|s| hex::decode(s).ok()).any(|sig| mac(secret, t, body).verify_slice(&sig).is_ok());
    matched.then_some(t).ok_or(SignatureError::Mismatch)
}

/// A `Stripe-Signature` for `body` at `t` (the fake's, and tests').
pub fn sign(secret: &str, t: i64, body: &[u8]) -> String {
    format!("t={t},v1={}", hex::encode(mac(secret, t, body).finalize().into_bytes()))
}

// ------------------------------------------------------------- forms

/// A form body as Stripe takes it: `a[b][0][c]=v`, in the order pushed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form(Vec<(String, String)>);

impl Form {
    pub fn new() -> Form {
        Form::default()
    }

    pub fn push(mut self, key: impl Into<String>, value: impl Into<String>) -> Form {
        self.0.push((key.into(), value.into()));
        self
    }

    pub fn pairs(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn encode(&self) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &self.0 {
            s.append_pair(k, v);
        }
        s.finish()
    }
}

// ------------------------------------------------------------- what is read

/// A list as Stripe answers one.
#[derive(Debug, Clone, Deserialize)]
pub struct List<T> {
    pub data: Vec<T>,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Price {
    pub id: String,
    #[serde(default)]
    pub lookup_key: Option<String>,
    #[serde(default)]
    pub unit_amount: Option<i64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub id: String,
    #[serde(default)]
    pub quantity: Option<u64>,
    pub price: Price,
    #[serde(default)]
    pub current_period_end: Option<i64>,
}

/// What fragment reads of a subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub status: String,
    pub customer: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub items: List<Item>,
    #[serde(default)]
    pub trial_end: Option<i64>,
    #[serde(default)]
    pub cancel_at_period_end: bool,
}

/// What a Checkout Session says of itself.
#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutSession {
    pub id: String,
    pub mode: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub payment_status: Option<String>,
    #[serde(default)]
    pub customer: Option<String>,
    #[serde(default)]
    pub subscription: Option<String>,
    #[serde(default)]
    pub client_reference_id: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// An event: its object is read by its type.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub created: i64,
    pub data: EventData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventData {
    pub object: serde_json::Value,
}

/// Seats counted by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub seat: u64,
    pub seat_always_on: u64,
}

impl Counts {
    pub fn get(&self, kind: SeatKind) -> u64 {
        match kind {
            SeatKind::Seat => self.seat,
            SeatKind::SeatAlwaysOn => self.seat_always_on,
        }
    }

    pub fn total(&self) -> u64 {
        self.seat + self.seat_always_on
    }
}

/// A subscription as the registry keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub customer: String,
    pub status: Status,
    /// Each kind's item, and its quantity.
    pub items: Vec<(SeatKind, String, u64)>,
    /// When the paid period (or the trial) ends, in seconds.
    pub period_end: Option<i64>,
    pub trial_end: Option<i64>,
    pub cancel_at_period_end: bool,
}

impl Snapshot {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for (kind, _, q) in &self.items {
            match kind {
                SeatKind::Seat => c.seat += q,
                SeatKind::SeatAlwaysOn => c.seat_always_on += q,
            }
        }
        c
    }
}

impl Subscription {
    /// The subscription as the registry keeps it. A status Stripe does not
    /// name, an item on a price that is no seat's, a kind twice, or more
    /// items than a page are refused: not a subscription fragment made.
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let status = Status::parse(&self.status).ok_or_else(|| format!("{} has a status fragment does not know: {:?}", self.id, self.status))?;
        if self.items.has_more {
            return Err(format!("{} has more items than a page", self.id));
        }
        let mut items: Vec<(SeatKind, String, u64)> = vec![];
        for it in &self.items.data {
            let key = it.price.lookup_key.as_deref().unwrap_or("");
            let kind = kind_of_lookup_key(key).ok_or_else(|| format!("{}'s item {} is on a price that is no seat's ({key:?})", self.id, it.id))?;
            if items.iter().any(|(k, _, _)| *k == kind) {
                return Err(format!("{} has two items of {}", self.id, kind.as_str()));
            }
            items.push((kind, it.id.clone(), it.quantity.unwrap_or(0)));
        }
        let period_end = match status {
            Status::Trialing => self.trial_end,
            _ => self.items.data.iter().filter_map(|i| i.current_period_end).max(),
        };
        Ok(Snapshot { id: self.id.clone(), customer: self.customer.clone(), status, items, period_end, trial_end: self.trial_end, cancel_at_period_end: self.cancel_at_period_end })
    }
}

/// One change to a subscription's items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemChange {
    Set { item: String, quantity: u64 },
    Delete { item: String },
    /// A kind it has no item for yet: its price is found by lookup key.
    Add { kind: SeatKind, quantity: u64 },
}

/// The changes that bring `snapshot`'s items to `want`, none when they
/// match. An org's last paid seat is never taken here (its subscription is
/// canceled in Stripe's portal), so `want` totals at least one.
pub fn item_changes(snapshot: &Snapshot, want: Counts) -> Vec<ItemChange> {
    assert!(want.total() >= 1, "a subscription keeps a seat: its last ends through the portal's cancel");
    let mut changes = vec![];
    for kind in [SeatKind::Seat, SeatKind::SeatAlwaysOn] {
        let n = want.get(kind);
        match (snapshot.items.iter().find(|(k, _, _)| *k == kind), n) {
            (Some((_, _, q)), n) if *q == n => {}
            (Some((_, id, _)), 0) => changes.push(ItemChange::Delete { item: id.clone() }),
            (Some((_, id, _)), n) => changes.push(ItemChange::Set { item: id.clone(), quantity: n }),
            (None, 0) => {}
            (None, n) => changes.push(ItemChange::Add { kind, quantity: n }),
        }
    }
    changes
}

/// The form for `POST /v1/subscriptions/<id>` that makes `changes`
/// (`price_of` names an added kind's price), prorated onto the next
/// invoice (decision 52).
pub fn items_form(changes: &[ItemChange], price_of: impl Fn(SeatKind) -> String) -> Form {
    let mut f = Form::new();
    for (i, c) in changes.iter().enumerate() {
        f = match c {
            ItemChange::Set { item, quantity } => f.push(format!("items[{i}][id]"), item.clone()).push(format!("items[{i}][quantity]"), quantity.to_string()),
            ItemChange::Delete { item } => f.push(format!("items[{i}][id]"), item.clone()).push(format!("items[{i}][deleted]"), "true"),
            ItemChange::Add { kind, quantity } => f.push(format!("items[{i}][price]"), price_of(*kind)).push(format!("items[{i}][quantity]"), quantity.to_string()),
        };
    }
    f.push("proration_behavior", "create_prorations")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "whsec_test";

    #[test]
    fn a_signature_verifies_once_fresh_and_untouched() {
        let body = br#"{"id":"evt_1"}"#;
        let h = sign(SECRET, 1_000, body);
        assert_eq!(verify(Some(&h), body, SECRET, 1_000), Ok(1_000));
        assert_eq!(verify(Some(&h), body, SECRET, 1_300), Ok(1_000), "at the tolerance's edge");
        assert_eq!(verify(Some(&h), body, SECRET, 1_301), Err(SignatureError::Stale));
        assert_eq!(verify(Some(&h), body, SECRET, 699), Err(SignatureError::Stale), "from the future too");
        assert_eq!(verify(Some(&h), br#"{"id":"evt_2"}"#, SECRET, 1_000), Err(SignatureError::Mismatch), "a tampered body");
        assert_eq!(verify(Some(&h), body, "whsec_other", 1_000), Err(SignatureError::Mismatch), "another endpoint's secret");
        assert_eq!(verify(None, body, SECRET, 1_000), Err(SignatureError::Missing));
        assert_eq!(verify(Some("v1=abc"), body, SECRET, 1_000), Err(SignatureError::Malformed));
        assert_eq!(verify(Some("t=1000"), body, SECRET, 1_000), Err(SignatureError::Malformed));
        assert_eq!(verify(Some("t=1000,v1=zz"), body, SECRET, 1_000), Err(SignatureError::Mismatch), "a v1 that is not hex");
    }

    #[test]
    fn any_v1_may_match_while_a_secret_rolls_over() {
        let body = b"{}";
        let old = sign("whsec_old", 5, body);
        let new = sign(SECRET, 5, body);
        let both = format!("{new},{},v0=ignored", old.split(',').nth(1).unwrap());
        assert_eq!(verify(Some(&both), body, "whsec_old", 5), Ok(5));
        assert_eq!(verify(Some(&both), body, SECRET, 5), Ok(5));
    }

    #[test]
    fn a_form_encodes_brackets_and_keeps_its_order() {
        let f = Form::new().push("items[0][price]", "price_1").push("metadata[fragment_org]", "org-1 &x");
        assert_eq!(f.encode(), "items%5B0%5D%5Bprice%5D=price_1&metadata%5Bfragment_org%5D=org-1+%26x");
    }

    fn subscription(status: &str, items: serde_json::Value) -> Subscription {
        serde_json::from_value(json!({
            "id": "sub_1", "object": "subscription", "status": status, "customer": "cus_1",
            "metadata": { "fragment_deployment": "https://p.example", "fragment_org": "org-0123456789abcdef" },
            "items": { "object": "list", "data": items, "has_more": false },
            "trial_end": 1_700_000_000, "cancel_at_period_end": false, "unknown_field": 1
        }))
        .unwrap()
    }

    fn item(id: &str, key: &str, q: u64, end: i64) -> serde_json::Value {
        json!({ "id": id, "quantity": q, "current_period_end": end, "price": { "id": format!("price_{key}"), "lookup_key": key } })
    }

    #[test]
    fn a_subscription_reads_as_its_seats() {
        let s = subscription("active", json!([item("si_a", "fragment_seat_month", 3, 10), item("si_b", "fragment_seat_always_on_month", 1, 20)]));
        let snap = s.snapshot().unwrap();
        assert_eq!(snap.status, Status::Active);
        assert_eq!(snap.counts(), Counts { seat: 3, seat_always_on: 1 });
        assert_eq!(snap.period_end, Some(20), "the latest item's period end");
        assert!(ours(&s.metadata, "https://p.example"));
        assert!(!ours(&s.metadata, "https://q.example"), "another deployment's");
        assert!(!ours(&BTreeMap::new(), "https://p.example"), "finite-mono's: no deployment of ours");
        let trialing = subscription("trialing", json!([item("si_a", "fragment_seat_month", 1, 10)])).snapshot().unwrap();
        assert_eq!(trialing.period_end, Some(1_700_000_000), "a trial ends at its trial's end");
    }

    #[test]
    fn a_subscription_fragment_did_not_make_is_refused() {
        assert!(subscription("active", json!([item("si_a", "finite_standard", 1, 10)])).snapshot().unwrap_err().contains("no seat's"));
        assert!(subscription("active", json!([item("si_a", "fragment_seat_month", 1, 10), item("si_b", "fragment_seat_month", 1, 10)])).snapshot().unwrap_err().contains("two items"));
        assert!(subscription("bogus", json!([])).snapshot().unwrap_err().contains("status"));
    }

    #[test]
    fn item_changes_bring_quantities_to_the_seats() {
        let snap = subscription("active", json!([item("si_a", "fragment_seat_month", 2, 10)])).snapshot().unwrap();
        assert_eq!(item_changes(&snap, Counts { seat: 2, seat_always_on: 0 }), vec![], "matching: nothing to change");
        assert_eq!(item_changes(&snap, Counts { seat: 3, seat_always_on: 0 }), vec![ItemChange::Set { item: "si_a".into(), quantity: 3 }]);
        let up = item_changes(&snap, Counts { seat: 1, seat_always_on: 1 });
        assert_eq!(up, vec![ItemChange::Set { item: "si_a".into(), quantity: 1 }, ItemChange::Add { kind: SeatKind::SeatAlwaysOn, quantity: 1 }], "an upgrade");
        let moved = item_changes(&snap, Counts { seat: 0, seat_always_on: 2 });
        assert_eq!(moved, vec![ItemChange::Delete { item: "si_a".into() }, ItemChange::Add { kind: SeatKind::SeatAlwaysOn, quantity: 2 }]);
        let f = items_form(&moved, |k| format!("price_{}", lookup_key(k)));
        assert_eq!(
            f.pairs().iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
            ["items[0][id]=si_a", "items[0][deleted]=true", "items[1][price]=price_fragment_seat_always_on_month", "items[1][quantity]=2", "proration_behavior=create_prorations"]
        );
    }

    #[test]
    #[should_panic(expected = "keeps a seat")]
    fn the_last_seat_never_goes_through_item_changes() {
        let snap = subscription("active", json!([item("si_a", "fragment_seat_month", 1, 10)])).snapshot().unwrap();
        item_changes(&snap, Counts::default());
    }

    #[test]
    fn lookup_keys_name_kinds_and_events_are_four() {
        for k in [SeatKind::Seat, SeatKind::SeatAlwaysOn] {
            assert_eq!(kind_of_lookup_key(lookup_key(k)), Some(k));
        }
        assert_eq!(unit_amount(SeatKind::Seat) * 2, unit_amount(SeatKind::SeatAlwaysOn));
        assert_eq!(EVENTS.len(), 4);
    }
}
