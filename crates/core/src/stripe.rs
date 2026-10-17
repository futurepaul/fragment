//! Stripe as fragment uses it (docs/billing.md, "Stripe"), the parts with
//! no network: what it sells (one seat, `seat_always_on`, and the credit
//! pack), a webhook's signature, the form bodies Stripe takes, what
//! fragment reads of a subscription and a Checkout Session, and the
//! quantity that brings a subscription to an org's paid seats. The cell's
//! client is `cell/src/stripe.rs`; the fake is `crates/fakes`'.
//!
//! The account is shared with finite-mono: everything fragment makes
//! carries `fragment_deployment` in its metadata, and anything without
//! this deployment's is not fragment's to touch (`ours`).

use std::collections::BTreeMap;

use fragment_proto::ledger::SeatState;
use fragment_proto::org::{Offer, SeatKind};
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
/// A pack's Checkout: who bought it (whose return it is), and the pack;
/// `META_PERSON` is whose ledger it grants.
pub const META_BUYER: &str = "fragment_buyer";
pub const META_PACK: &str = "fragment_pack";

/// The one seat Stripe sells (`SeatKind::SOLD`; Paul, 2026-10-15: "the
/// $200 plan and the $25 credit pack"): its monthly price, found by its
/// lookup key, and its amount in cents, the price's truth: Checkout refuses
/// a Price of another amount (cell/src/billing.rs), and `xtask stripe
/// check` expects this one (decision 25). A price change is a change here,
/// and a new Price with the key moved to it (`transfer_lookup_key`). A
/// `seat` is an operator's comp only.
pub const SEAT_LOOKUP_KEY: &str = "fragment_seat_always_on_month";
pub const SEAT_CENTS: i64 = 20_000;
pub const SEAT_MICROS: i64 = SEAT_CENTS * CENT_MICROS;
/// A cent, in the ledger's micro-dollars.
const CENT_MICROS: i64 = 10_000;

/// A seat of a kind Stripe does not sell, asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotSold(pub SeatKind);

impl NotSold {
    pub fn message(self) -> String {
        format!("a {} seat is not sold: the one seat is seat_always_on, ${} a month, always on", self.0.as_str(), SEAT_CENTS / 100)
    }
}

/// `kind`, when it is the seat Stripe sells.
pub fn for_sale(kind: SeatKind) -> Result<SeatKind, NotSold> {
    match kind == SeatKind::SOLD {
        true => Ok(kind),
        false => Err(NotSold(kind)),
    }
}

/// The credit pack (decision 55): one, $25, a one-time price by its
/// lookup key, granted on a ledger by its Checkout's id and kept until
/// spent.
pub const PACK_LOOKUP_KEY: &str = "fragment_credit_pack";
pub const PACK_CENTS: i64 = 2_500;
pub const PACK_MICROS: i64 = PACK_CENTS * CENT_MICROS;

/// What is sold, as a page shows it (`GET /api/seat`'s `offer`): the one
/// seat, its price and the credit it includes each month (the ledger's,
/// `ledger::included`), and a pack's price. Pages read these, never their
/// own copies.
pub fn offer() -> Offer {
    Offer { kind: SeatKind::SOLD, seat_micros: SEAT_MICROS, included_micros: crate::ledger::included(SeatKind::SOLD.plan(), SeatState::Active), pack_micros: PACK_MICROS }
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

/// A subscription as the registry keeps it: fragment's has one item, on
/// the seat's price, its quantity the org's paid seats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub customer: String,
    pub status: Status,
    /// Its one item's id, and the seats it bills.
    pub item: String,
    pub seats: u64,
    /// When the paid period (or the trial) ends, in seconds.
    pub period_end: Option<i64>,
    pub trial_end: Option<i64>,
    pub cancel_at_period_end: bool,
}

impl Subscription {
    /// The subscription as the registry keeps it. A status Stripe does not
    /// name, or anything but one item on the seat's price (`SEAT_LOOKUP_KEY`),
    /// is refused: not a subscription fragment made.
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let status = Status::parse(&self.status).ok_or_else(|| format!("{} has a status fragment does not know: {:?}", self.id, self.status))?;
        let [it] = self.items.data.as_slice() else {
            return Err(format!("{} has {} items: fragment's has one, the seat's", self.id, self.items.data.len()));
        };
        if self.items.has_more {
            return Err(format!("{} has more items than a page", self.id));
        }
        let key = it.price.lookup_key.as_deref().unwrap_or("");
        if key != SEAT_LOOKUP_KEY {
            return Err(format!("{}'s item {} is on a price that is no seat's ({key:?})", self.id, it.id));
        }
        let period_end = match status {
            Status::Trialing => self.trial_end,
            _ => it.current_period_end,
        };
        Ok(Snapshot {
            id: self.id.clone(),
            customer: self.customer.clone(),
            status,
            item: it.id.clone(),
            seats: it.quantity.unwrap_or(0),
            period_end,
            trial_end: self.trial_end,
            cancel_at_period_end: self.cancel_at_period_end,
        })
    }
}

/// The form for `POST /v1/subscriptions/<id>` that brings its seat item to
/// `want` seats, prorated onto the next invoice (decision 52); none when it
/// bills them already. No paid seat left is a cancel (`push`), never a
/// quantity of zero, so `want` is one at least.
pub fn quantity_form(snapshot: &Snapshot, want: u64) -> Option<Form> {
    assert!(want >= 1, "a subscription keeps a seat: with none left it is canceled");
    (snapshot.seats != want).then(|| {
        Form::new().push("items[0][id]", snapshot.item.clone()).push("items[0][quantity]", want.to_string()).push("proration_behavior", "create_prorations")
    })
}

/// What an org's paid seats ask of its subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Push {
    /// It bills them already, or it bills nothing and need not.
    Nothing,
    /// Its item's quantity, to the seats' count.
    Quantity(Form),
    /// No paid seat is left and it may still bill: it is canceled at once
    /// (`DELETE /v1/subscriptions/<id>`, no proration: what is owed back is
    /// a refund by hand in Stripe's dashboard). Its customer is kept.
    Cancel,
}

/// The push that brings `snapshot` to `want` paid seats: a quantity while
/// it is good, a cancel when none is left and it has not ended (an
/// `unpaid` one too: it would charge again were its invoice paid).
pub fn push(snapshot: &Snapshot, want: u64) -> Push {
    if want == 0 {
        return if snapshot.status.is_ended() { Push::Nothing } else { Push::Cancel };
    }
    if !snapshot.status.is_good() {
        return Push::Nothing;
    }
    quantity_form(snapshot, want).map_or(Push::Nothing, Push::Quantity)
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
        let s = subscription("active", json!([item("si_a", SEAT_LOOKUP_KEY, 3, 20)]));
        let snap = s.snapshot().unwrap();
        assert_eq!(snap.status, Status::Active);
        assert_eq!((snap.item.as_str(), snap.seats), ("si_a", 3));
        assert_eq!(snap.period_end, Some(20), "its item's period end");
        assert!(ours(&s.metadata, "https://p.example"));
        assert!(!ours(&s.metadata, "https://q.example"), "another deployment's");
        assert!(!ours(&BTreeMap::new(), "https://p.example"), "finite-mono's: no deployment of ours");
        let trialing = subscription("trialing", json!([item("si_a", SEAT_LOOKUP_KEY, 1, 10)])).snapshot().unwrap();
        assert_eq!(trialing.period_end, Some(1_700_000_000), "a trial ends at its trial's end");
    }

    #[test]
    fn a_subscription_fragment_did_not_make_is_refused() {
        assert!(subscription("active", json!([item("si_a", "finite_standard", 1, 10)])).snapshot().unwrap_err().contains("no seat's"));
        // the $100 seat's price, no longer sold (Paul, 2026-10-15)
        assert!(subscription("active", json!([item("si_a", "fragment_seat_month", 1, 10)])).snapshot().unwrap_err().contains("no seat's"));
        let two = subscription("active", json!([item("si_a", SEAT_LOOKUP_KEY, 1, 10), item("si_b", SEAT_LOOKUP_KEY, 1, 10)]));
        assert!(two.snapshot().unwrap_err().contains("2 items"));
        assert!(subscription("active", json!([])).snapshot().unwrap_err().contains("0 items"));
        assert!(subscription("bogus", json!([])).snapshot().unwrap_err().contains("status"));
    }

    #[test]
    fn a_quantity_form_brings_the_item_to_the_seats() {
        let snap = subscription("active", json!([item("si_a", SEAT_LOOKUP_KEY, 2, 10)])).snapshot().unwrap();
        assert_eq!(quantity_form(&snap, 2), None, "matching: nothing to change");
        let f = quantity_form(&snap, 3).unwrap();
        assert_eq!(f.pairs().iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(), ["items[0][id]=si_a", "items[0][quantity]=3", "proration_behavior=create_prorations"]);
    }

    #[test]
    #[should_panic(expected = "keeps a seat")]
    fn the_last_seat_never_goes_through_a_quantity_form() {
        let snap = subscription("active", json!([item("si_a", SEAT_LOOKUP_KEY, 1, 10)])).snapshot().unwrap();
        quantity_form(&snap, 0);
    }

    /// Goal: an org's seats bring its subscription to their count while it
    /// is good, cancel it once none is left (whatever it is, unless it has
    /// ended: an unpaid one could charge again), and touch nothing else.
    /// Method: each status at zero, one and the count it has.
    #[test]
    fn no_paid_seat_left_cancels_and_a_count_is_pushed_while_good() {
        let at = |status: &str| subscription(status, json!([item("si_a", SEAT_LOOKUP_KEY, 2, 10)])).snapshot().unwrap();
        for s in ["active", "trialing", "past_due", "unpaid", "paused", "incomplete"] {
            assert_eq!(push(&at(s), 0), Push::Cancel, "{s}, no seat left");
        }
        for s in ["canceled", "incomplete_expired"] {
            assert_eq!(push(&at(s), 0), Push::Nothing, "{s}: it bills nothing already");
            assert_eq!(push(&at(s), 3), Push::Nothing, "{s}: it buys again, through Checkout");
        }
        assert_eq!(push(&at("active"), 2), Push::Nothing, "it bills them already");
        assert!(matches!(push(&at("past_due"), 3), Push::Quantity(_)), "past due is good");
        assert_eq!(push(&at("unpaid"), 3), Push::Nothing, "lapsed: no count is pushed to it");
        assert_eq!(push(&at("active"), 1), Push::Quantity(quantity_form(&at("active"), 1).unwrap()));
    }

    #[test]
    fn only_the_always_on_seat_is_for_sale_and_events_are_four() {
        assert_eq!(for_sale(SeatKind::SeatAlwaysOn), Ok(SeatKind::SeatAlwaysOn));
        assert_eq!(for_sale(SeatKind::Seat), Err(NotSold(SeatKind::Seat)));
        assert!(NotSold(SeatKind::Seat).message().contains("$200"));
        assert_eq!((SEAT_LOOKUP_KEY, SEAT_CENTS), ("fragment_seat_always_on_month", 20_000));
        // what a page shows is these, the always-on seat's included credit the ledger's
        let o = offer();
        assert_eq!((o.kind, o.seat_micros, o.included_micros, o.pack_micros), (SeatKind::SeatAlwaysOn, 200_000_000, crate::ledger::SEAT_ALWAYS_ON_INCLUDED, 25_000_000));
        assert_eq!(EVENTS.len(), 4);
    }
}
