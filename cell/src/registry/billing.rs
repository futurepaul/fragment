//! Paid seats (docs/billing.md, "Stripe"; decisions 51 to 53): what the
//! registry keeps of each org's Stripe customer and subscription, and the
//! seat a Checkout buys its buyer. Stripe holds the payment state; the
//! registry keeps a copy (`orgs`' columns), only ever written from a
//! subscription fetched from Stripe (by the Checkout's return, a webhook,
//! or the daily reconcile), never from an event's own copy of it.
//!
//! An org has one subscription. A newer one replaces it only once it has
//! ended (a lapsed org that buys again); an event older than the last one
//! applied changes nothing. A change to whether the org pays queues a plan
//! push for each of its paid seats' holders (orgs.rs).

use fragment_core::org::{self, Status};
use fragment_core::stripe::Snapshot;
use fragment_proto::org::SeatKind;
use serde::Serialize;

use super::calls::{By, Call};
use super::orgs::{not_found, MemberRow, MEMBER_COLUMNS};
use super::*;

/// Orgs one run of the alarm reconciles.
const RECONCILE_BATCH: u32 = 20;
/// How old an org's copy of its subscription is let get.
const RECONCILE_EVERY_MS: i64 = 24 * 3600 * 1000;

/// The subscription a call carries, as fetched from Stripe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscriptionCopy {
    pub id: String,
    pub customer: String,
    pub status: String,
    /// Each kind's item and quantity.
    pub items: Vec<(SeatKind, String, u64)>,
    pub period_end: Option<i64>,
    pub trial_end: Option<i64>,
    pub cancel_at_end: bool,
}

impl From<Snapshot> for SubscriptionCopy {
    fn from(s: Snapshot) -> SubscriptionCopy {
        SubscriptionCopy {
            id: s.id,
            customer: s.customer,
            status: s.status.as_str().to_string(),
            items: s.items,
            period_end: s.period_end,
            trial_end: s.trial_end,
            cancel_at_end: s.cancel_at_period_end,
        }
    }
}

/// `POST /billing/checkout`: a person is to buy a seat. Their org (made,
/// of one, if they are in none), and what Stripe needs to sell it.
#[derive(Serialize, Deserialize)]
pub(crate) struct CheckoutBegin {
    pub by: By,
    pub kind: SeatKind,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckoutPlan {
    pub person: String,
    pub email: String,
    pub org: String,
    pub org_name: String,
    pub customer: Option<String>,
}

impl Call for CheckoutBegin {
    const PATH: &'static str = "/billing/checkout";
    type Answer = CheckoutPlan;
}

/// `POST /billing/customer`: an org's Stripe customer, once: the first
/// named is kept, and answered.
#[derive(Serialize, Deserialize)]
pub(crate) struct SetCustomer {
    pub org: String,
    pub customer: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Customer {
    pub customer: String,
}

impl Call for SetCustomer {
    const PATH: &'static str = "/billing/customer";
    type Answer = Customer;
}

/// `POST /billing/checked-out`: a Checkout that completed, and the
/// subscription it made: its buyer's seat, once by its session.
#[derive(Serialize, Deserialize)]
pub(crate) struct ApplyCheckout {
    pub session: String,
    pub org: String,
    pub person: String,
    pub kind: SeatKind,
    pub subscription: SubscriptionCopy,
}

impl Call for ApplyCheckout {
    const PATH: &'static str = "/billing/checked-out";
    type Answer = Applied;
}

/// `POST /billing/subscription`: an org's subscription, as fetched; `at`
/// is the event's time (none: a fetch of the platform's own).
#[derive(Serialize, Deserialize)]
pub(crate) struct ApplySubscription {
    pub org: String,
    pub subscription: SubscriptionCopy,
    pub at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Applied {
    pub applied: bool,
}

impl Call for ApplySubscription {
    const PATH: &'static str = "/billing/subscription";
    type Answer = Applied;
}

/// `POST /billing/customer-of`: the customer of the org a person admins
/// (the portal's).
#[derive(Serialize, Deserialize)]
pub(crate) struct CustomerOf {
    pub by: By,
}

impl Call for CustomerOf {
    const PATH: &'static str = "/billing/customer-of";
    type Answer = Customer;
}

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS checkouts (
  session TEXT PRIMARY KEY, org TEXT NOT NULL, person TEXT NOT NULL, kind TEXT NOT NULL, at INTEGER NOT NULL);
";

/// An org's billing columns.
#[derive(Deserialize)]
struct BillingRow {
    customer: Option<String>,
    subscription: Option<String>,
    status: Option<String>,
    event_at: Option<i64>,
}

fn good(status: Option<&str>) -> bool {
    status.and_then(Status::parse).is_some_and(Status::is_good)
}

impl RegistryCell {
    fn billing_row(&self, org: &str) -> CellResult<BillingRow> {
        self.row::<BillingRow>("SELECT customer, subscription, status, event_at FROM orgs WHERE id = ?", vec![org.into()])?.ok_or_else(|| not_found(format!("no org {org}")))
    }

    /// The email a person signed in with last.
    fn latest_email(&self, person: &str) -> CellResult<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            email: String,
        }
        Ok(self.row::<Row>("SELECT email FROM subjects WHERE identity = ? ORDER BY signed_in_at DESC LIMIT 1", vec![person.into()])?.map(|r| r.email))
    }

    fn checkout_begin(&self, b: CheckoutBegin) -> CellResult<CheckoutPlan> {
        let who = self.person_by(&b.by)?;
        let email = self.latest_email(&who.id)?.ok_or_else(|| CellError::invalid("a seat is bought by someone who signs in, with their email"))?;
        let org = match self.member_of(&who.id)? {
            Some(m) if m.comped != 0 => return Err(conflict("you hold a seat already")),
            Some(m) => {
                let o = self.stored_org(&m.org)?;
                let paying = good(self.billing_row(&o.id)?.status.as_deref());
                match (paying, m.admin != 0) {
                    (true, _) if m.seat.is_some() => return Err(conflict("you hold a seat already")),
                    (true, true) => return Err(conflict(format!("{} pays for its seats already: add yours on its page", o.name))),
                    (true, false) | (false, false) => return Err(conflict(format!("you are in {}: its admins give you a seat", o.name))),
                    // an admin of an org that pays for nothing (new, or
                    // lapsed) buys again: their lapsed seat, if they had one
                    (false, true) => o,
                }
            }
            None => {
                let o = self.new_org(&email, &who.id)?;
                self.insert_member(&o.id, Some(&who.id), &email, true, None, false, &who.id)?;
                o
            }
        };
        let customer = self.billing_row(&org.id)?.customer;
        Ok(CheckoutPlan { person: who.id, email, org: org.id, org_name: org.name, customer })
    }

    fn set_customer(&self, b: SetCustomer) -> CellResult<Customer> {
        let row = self.billing_row(&b.org)?;
        if let Some(c) = row.customer {
            return Ok(Customer { customer: c });
        }
        self.exec("UPDATE orgs SET customer = ? WHERE id = ?", vec![b.customer.as_str().into(), b.org.as_str().into()])?;
        Ok(Customer { customer: b.customer })
    }

    fn customer_of(&self, b: CustomerOf) -> CellResult<Customer> {
        let who = self.person_by(&b.by)?;
        let m = self.member_of(&who.id)?.filter(|m| m.admin != 0).ok_or_else(|| CellError::new(ErrorCode::Forbidden, "only an org's admins manage its payment"))?;
        let customer = self.billing_row(&m.org)?.customer.ok_or_else(|| CellError::invalid("your org has never paid: buy a seat first"))?;
        Ok(Customer { customer })
    }

    /// Writes `s` as `org`'s subscription, when it may (module docs). A
    /// change in whether the org pays queues a push for each paid seat's
    /// holder. Answers whether it was written.
    pub(super) fn apply_subscription_copy(&self, org: &str, s: &SubscriptionCopy, at: Option<i64>) -> CellResult<bool> {
        let row = self.billing_row(org)?;
        let status = Status::parse(&s.status).ok_or_else(|| CellError::invalid(format!("a subscription's status is Stripe's, not {:?}", s.status)))?;
        match row.subscription.as_deref() {
            Some(current) if current != s.id => {
                // a newer subscription replaces one that ended, never one that pays
                if good(row.status.as_deref()) || !status.is_good() {
                    return Ok(false);
                }
            }
            Some(_) if at.zip(row.event_at).is_some_and(|(at, last)| at < last) => return Ok(false),
            _ => {}
        }
        if row.customer.as_deref().is_some_and(|c| c != s.customer) {
            return Err(CellError::host(format!("{org}'s subscription {} is another customer's ({})", s.id, s.customer)));
        }
        let was = good(row.status.as_deref());
        let items = serde_json::to_string(&s.items).map_err(|e| CellError::host(e.to_string()))?;
        let opt = |v: Option<i64>| v.map_or(SqlStorageValue::Null, SqlStorageValue::Integer);
        let event_at = match (at, row.event_at) {
            (Some(at), Some(last)) => Some(at.max(last)),
            (at, last) => at.or(last),
        };
        self.exec(
            "UPDATE orgs SET customer = ?, subscription = ?, status = ?, items = ?, period_end = ?, trial_end = ?, cancel_at_end = ?, event_at = ?, synced_at = ? WHERE id = ?",
            vec![
                s.customer.as_str().into(),
                s.id.as_str().into(),
                status.as_str().into(),
                items.into(),
                opt(s.period_end),
                opt(s.trial_end),
                SqlStorageValue::Integer(i64::from(s.cancel_at_end)),
                opt(event_at),
                SqlStorageValue::Integer(js::now_ms()),
                org.into(),
            ],
        )?;
        if was != status.is_good() {
            // bounded: an org holds at most SEATS_PER_ORG_MAX seats
            let paid = self.rows::<MemberRow>(
                &format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.org = ? AND m.seat IS NOT NULL AND m.comped = 0 AND m.person IS NOT NULL"),
                vec![org.into()],
            )?;
            for m in paid {
                self.queue_sync(m.person.as_deref().expect("selected for its person"))?;
            }
        }
        // the seats are the truth: counts Stripe holds otherwise are pushed
        self.queue_quantities_if_drifted(org, s)?;
        Ok(true)
    }

    fn apply_subscription(&self, b: ApplySubscription) -> CellResult<Applied> {
        Ok(Applied { applied: self.apply_subscription_copy(&b.org, &b.subscription, b.at)? })
    }

    /// The seat a completed Checkout bought its buyer, once by its session.
    fn apply_checkout(&self, b: ApplyCheckout) -> CellResult<Applied> {
        if self.count("SELECT COUNT(*) AS n FROM checkouts WHERE session = ?", vec![b.session.as_str().into()])? > 0 {
            return Ok(Applied { applied: false });
        }
        if !org::valid_org_id(&b.org) {
            return Err(not_found(format!("no org {}", b.org)));
        }
        self.apply_subscription_copy(&b.org, &b.subscription, None)?;
        // the buyer takes the seat (a lapsed paid one of theirs becomes the
        // kind bought), unless they came to hold a comped one meanwhile
        if let Some(m) = self.member_of(&b.person)?.filter(|m| m.org == b.org && m.comped == 0) {
            self.exec("UPDATE org_members SET seat = ?, comped = 0 WHERE id = ?", vec![b.kind.as_str().into(), m.id.as_str().into()])?;
            self.queue_sync(&b.person)?;
        }
        self.exec(
            "INSERT INTO checkouts (session, org, person, kind, at) VALUES (?, ?, ?, ?, ?)",
            vec![b.session.as_str().into(), b.org.as_str().into(), b.person.as_str().into(), b.kind.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(Applied { applied: true })
    }

    /// The reconcile: orgs whose copy of their subscription is a day old
    /// or more, fetched from Stripe again and written (a webhook lost, or
    /// one never sent). Run by the alarm; a failure is logged and the org
    /// tried at the next run.
    pub(super) async fn reconcile_alarm(&self) -> CellResult<()> {
        if self.cfg.stripe().is_err() {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct Due {
            id: String,
            subscription: String,
        }
        let due = self.rows::<Due>(
            "SELECT id, subscription FROM orgs WHERE subscription IS NOT NULL AND (synced_at IS NULL OR synced_at <= ?) ORDER BY synced_at LIMIT ?",
            vec![SqlStorageValue::Integer(js::now_ms() - RECONCILE_EVERY_MS), SqlStorageValue::Integer(RECONCILE_BATCH.into())],
        )?;
        if due.is_empty() {
            return Ok(());
        }
        let stripe = crate::stripe::client(&self.env, self.cfg).await?;
        // bounded: at most RECONCILE_BATCH orgs a run
        for d in due {
            let fetched = stripe.get::<fragment_core::stripe::Subscription>(&format!("/v1/subscriptions/{}", d.subscription)).await;
            let copy = fetched.and_then(|s| s.snapshot().map_err(CellError::host)).map(SubscriptionCopy::from);
            match copy.and_then(|c| self.apply_subscription_copy(&d.id, &c, None)) {
                Ok(_) => {}
                Err(e) => {
                    console_error!("{}", json!({ "reconcile": d.id, "failed": e.message }));
                    // tried again at the next run, not at once
                    self.exec("UPDATE orgs SET synced_at = ? WHERE id = ?", vec![SqlStorageValue::Integer(js::now_ms() - RECONCILE_EVERY_MS + 3600 * 1000), d.id.as_str().into()])?;
                }
            }
        }
        Ok(())
    }

    /// The registry's billing routes, or `None` for a path that is not one.
    pub(super) async fn billing_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            CheckoutBegin::PATH => reply::<CheckoutBegin>(body(bytes).and_then(|b| self.checkout_begin(b))),
            SetCustomer::PATH => reply::<SetCustomer>(body(bytes).and_then(|b| self.set_customer(b))),
            CustomerOf::PATH => reply::<CustomerOf>(body(bytes).and_then(|b| self.customer_of(b))),
            ApplyCheckout::PATH => reply::<ApplyCheckout>(self.armed_billing(|| self.apply_checkout(body(bytes)?)).await),
            ApplySubscription::PATH => reply::<ApplySubscription>(self.armed_billing(|| self.apply_subscription(body(bytes)?)).await),
            _ => return None,
        })
    }

    async fn armed_billing<T>(&self, change: impl FnOnce() -> CellResult<T>) -> CellResult<T> {
        let answer = change()?;
        self.arm_syncs().await?;
        self.arm_quantities().await?;
        self.arm_reconcile().await?;
        Ok(answer)
    }

    /// Arms the alarm for the next org whose copy turns a day old.
    pub(super) async fn arm_reconcile(&self) -> CellResult<()> {
        #[derive(Deserialize)]
        struct Oldest {
            at: Option<i64>,
        }
        let oldest = self.row::<Oldest>("SELECT MIN(synced_at) AS at FROM orgs WHERE subscription IS NOT NULL", vec![])?.and_then(|r| r.at);
        match oldest {
            Some(at) => self.arm_at((at + RECONCILE_EVERY_MS).max(js::now_ms() + 1000)).await,
            None => Ok(()),
        }
    }
}

