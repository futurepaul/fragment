//! Paid seats (docs/billing.md, "Stripe"; decisions 51 to 53): what the
//! registry keeps of each org's Stripe customer and subscription, and the
//! seat a Checkout buys its buyer. Stripe holds the payment state; the
//! registry keeps a copy (`orgs`' columns), only ever written from a
//! subscription fetched from Stripe (by the Checkout's return, a webhook,
//! or the daily reconcile), never from an event's own copy of it. An org's
//! customer is written only so, from its subscription: never before a
//! payment.
//!
//! An org is made only once paid (the audit's H1): a person's first
//! Checkout writes nothing but a trial's place, and names in its session
//! the org it will make (`checkout_begin`); its completion makes it, its
//! buyer its admin and its paid seat's holder (`apply_checkout`). Its
//! customer is the one Stripe's Checkout made. Only an admin of their org
//! that pays for nothing (lapsed, or comped and ended) buys again in it, on
//! its own customer. A Checkout paid whose buyer can no longer take its
//! seat (they joined another org meanwhile, left this one, or are being
//! wiped) is canceled at once (seats.rs `queue_cancel`): it seats no one.
//!
//! An org has one subscription. A newer one replaces it only once it has
//! ended (a lapsed org that buys again); an event older than the last one
//! applied changes nothing. A change to whether the org pays queues a plan
//! push for each of its paid seats' holders (orgs.rs). Who pays for it is
//! the buyer whose Checkout made the subscription it has (`org_payers`).

use fragment_core::org::{self, Status};
use fragment_core::stripe::Snapshot;
use fragment_proto::org::SeatKind;
use serde::Serialize;

use super::calls::{By, Call};
use super::offers::Yes;
use super::orgs::{fresh_id, not_found, MemberRow, MEMBER_COLUMNS};
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
    /// Its seat item and quantity, as `orgs.items` has kept them since
    /// seats were sold of two kinds: `[(kind, item, quantity)]`.
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
            items: vec![(SeatKind::SOLD, s.item, s.seats)],
            period_end: s.period_end,
            trial_end: s.trial_end,
            cancel_at_end: s.cancel_at_period_end,
        }
    }
}

/// `POST /billing/checkout`: a person is to buy a seat. The org it buys in
/// (their own that buys again, or the id of the one its payment will
/// make: nothing is made before), and what Stripe needs to sell it.
#[derive(Serialize, Deserialize)]
pub(crate) struct CheckoutBegin {
    pub by: By,
    /// The seat's kind, as asked: only the one sold is.
    pub kind: SeatKind,
    /// A trial code, as typed.
    pub trial: Option<String>,
    /// Until when a trial's place is held: the Checkout's own expiry (ms).
    pub until_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckoutPlan {
    pub person: String,
    pub email: String,
    pub org: String,
    pub org_name: String,
    /// The org's own customer, when it buys again; `None`: Checkout makes
    /// one for the buyer's email (a new org's), never another's.
    pub customer: Option<String>,
    pub kind: SeatKind,
    /// The seats it bills from the start: a new org's one, or an org that
    /// buys again's paid seats, the buyer's own included (Stripe's page
    /// shows the whole).
    pub quantity: u64,
    pub trial: Option<super::trials::TrialOffer>,
}

impl Call for CheckoutBegin {
    const PATH: &'static str = "/billing/checkout";
    type Answer = CheckoutPlan;
}

/// `POST /billing/pack`: a credit pack is to be bought by `by`, for
/// themselves (a seat's holder) or for a member of the org they admin who
/// holds a seat. Whose ledger it grants, and what Stripe needs.
#[derive(Serialize, Deserialize)]
pub(crate) struct PackBegin {
    pub by: By,
    pub member: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PackPlan {
    pub buyer: String,
    pub person: String,
    pub email: String,
    pub org: String,
    pub org_name: String,
    pub customer: Option<String>,
}

impl Call for PackBegin {
    const PATH: &'static str = "/billing/pack";
    type Answer = PackPlan;
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Customer {
    pub customer: String,
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
    /// The trial code it redeemed.
    #[serde(default)]
    pub trial: Option<String>,
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
    /// A subscription naming an org the registry has no row of (its
    /// Checkout never applied: the router looks it up, else cancels it).
    #[serde(default)]
    pub orphan: bool,
}

impl Call for ApplySubscription {
    const PATH: &'static str = "/billing/subscription";
    type Answer = Applied;
}

/// `POST /billing/orphan`: a live subscription of this deployment's names
/// an org the registry has no row of, and no Checkout of its applies (its
/// completion never reached the platform, and Stripe has none to look
/// up): it is canceled after a grace (`ORPHAN_GRACE_MS`), unless an org
/// takes it meanwhile, so no card pays for an org that is not there.
#[derive(Serialize, Deserialize)]
pub(crate) struct CancelOrphan {
    pub subscription: String,
    pub org: String,
}

impl Call for CancelOrphan {
    const PATH: &'static str = "/billing/orphan";
    type Answer = Applied;
}

/// How long an orphan subscription's cancel waits: its Checkout's
/// completion, or the return, may still make its org.
pub(super) const ORPHAN_GRACE_MS: i64 = 3600 * 1000;

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
CREATE TABLE IF NOT EXISTS org_payers (
  org TEXT PRIMARY KEY, person TEXT NOT NULL, subscription TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS org_payers_person ON org_payers (person);
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
    /// An org's billing columns; `None`: no such org (one that ended).
    fn billing_row(&self, org: &str) -> CellResult<Option<BillingRow>> {
        self.row::<BillingRow>("SELECT customer, subscription, status, event_at FROM orgs WHERE id = ?", vec![org.into()])
    }

    /// Who pays for `org`: the buyer whose Checkout made the subscription
    /// it has now, if one did (`None`: never paid, or comped only).
    pub(super) fn payer_of(&self, org: &str) -> CellResult<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            person: String,
        }
        let q = "SELECT p.person FROM org_payers p JOIN orgs o ON o.id = p.org AND o.subscription = p.subscription WHERE p.org = ?";
        Ok(self.row::<Row>(q, vec![org.into()])?.map(|r| r.person))
    }

    /// Whether `person` pays for `org`: their Checkout made its
    /// subscription, and that has not ended, so their card may still be
    /// charged (one set to cancel at its period's end may be renewed in the
    /// portal: it counts).
    pub(super) fn pays_for(&self, org: &str, person: &str) -> CellResult<bool> {
        if self.payer_of(org)?.as_deref() != Some(person) {
            return Ok(false);
        }
        let sub = self.org_sub(org)?.ok_or_else(|| CellError::host(format!("{person} pays for an org that is gone: {org}")))?;
        let status = sub.status.as_deref().and_then(Status::parse).ok_or_else(|| CellError::host(format!("orgs.status of {org} is {:?}", sub.status)))?;
        Ok(!status.is_ended())
    }

    /// The person whose Checkout bought `org`'s last subscription, whether
    /// or not it still has one (`None`: never, or forgotten once its payer
    /// went: seats.rs `forget_payer`). Only they buy again on its customer.
    fn last_payer(&self, org: &str) -> CellResult<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            person: String,
        }
        Ok(self.row::<Row>("SELECT person FROM org_payers WHERE org = ?", vec![org.into()])?.map(|r| r.person))
    }

    /// `org`'s payer goes (steps down, leaves, moves, or is wiped): their
    /// card stops at once. Its subscription's cancel is queued, and once it
    /// lands the org forgets their customer (seats.rs `run_cancels`).
    pub(super) fn cancel_for_payer(&self, org: &str) -> CellResult<()> {
        let sub = self.org_sub(org)?.and_then(|s| s.subscription).ok_or_else(|| CellError::host(format!("{org}'s payer goes, and it has no subscription")))?;
        self.queue_cancel(&sub, org, super::seats::WHY_PAYER)
    }

    /// `person`'s Checkout made `org`'s subscription: they pay for it.
    fn set_payer(&self, org: &str, person: &str, subscription: &str) -> CellResult<()> {
        self.exec(
            "INSERT INTO org_payers (org, person, subscription) VALUES (?, ?, ?)
             ON CONFLICT (org) DO UPDATE SET person = excluded.person, subscription = excluded.subscription",
            vec![org.into(), person.into(), subscription.into()],
        )?;
        assert_eq!(self.payer_of(org)?.as_deref(), Some(person), "who pays is the org's subscription's buyer");
        Ok(())
    }

    /// The email a person signed in with last.
    fn latest_email(&self, person: &str) -> CellResult<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            email: String,
        }
        Ok(self.row::<Row>("SELECT email FROM subjects WHERE identity = ? ORDER BY signed_in_at DESC LIMIT 1", vec![person.into()])?.map(|r| r.email))
    }

    /// A person's Checkout, before it is paid: everything checked, nothing
    /// written but a trial's place (no org, no customer: the audit's H1).
    /// Someone in no org buys a new org of one, named in the session and
    /// made once it is paid; an admin of their org that pays for nothing
    /// buys again in it, on its own customer, for its paid seats.
    fn checkout_begin(&self, b: CheckoutBegin) -> CellResult<CheckoutPlan> {
        let kind = fragment_core::stripe::for_sale(b.kind)?;
        let who = self.person_by(&b.by)?;
        let email = self.latest_email(&who.id)?.ok_or_else(|| CellError::invalid("a seat is bought by someone who signs in, with their email"))?;
        let again = match self.member_of(&who.id)? {
            Some(m) if m.comped != 0 => return Err(conflict("you hold a seat already")),
            Some(m) => {
                let o = self.stored_org(&m.org)?;
                let paying = good(self.billing_row(&o.id)?.and_then(|r| r.status).as_deref());
                match (paying, m.admin != 0) {
                    (true, _) if m.seat.is_some() => return Err(conflict("you hold a seat already")),
                    (true, true) => return Err(conflict(format!("{} pays for its seats already: add yours on its page", o.name))),
                    (true, false) | (false, false) => return Err(conflict(format!("you are in {}: its admins give you a seat", o.name))),
                    // an admin of their org that pays for nothing (lapsed, or
                    // comped and ended) buys again: their seat, if they had one
                    (false, true) => Some((o, m)),
                }
            }
            None => None,
        };
        let trial = b.trial.as_deref().map(|raw| self.trial_offer(&who.id, raw, again.as_ref().map(|(o, _)| o.id.as_str()))).transpose()?;
        let plan = match again {
            None => CheckoutPlan { person: who.id.clone(), email: email.clone(), org: fresh_id("org-"), org_name: email, customer: None, kind, quantity: 1, trial },
            Some((o, mine)) => {
                // their own lapsed paid seat is counted; a seatless admin buys one
                let quantity = self.paid_seats(&o.id)? + u64::from(mine.seat.is_none());
                if trial.is_some() && quantity != 1 {
                    return Err(conflict("a trial is for a seat of one"));
                }
                // the org's customer is its last payer's: anyone else buys on
                // one of their own, which the org then keeps
                let customer = match self.last_payer(&o.id)?.as_deref() == Some(who.id.as_str()) {
                    true => self.billing_row(&o.id)?.and_then(|r| r.customer),
                    false => None,
                };
                CheckoutPlan { person: who.id.clone(), email, org: o.id, org_name: o.name, customer, kind, quantity, trial }
            }
        };
        assert!(plan.quantity >= 1 && plan.quantity <= fragment_core::org::SEATS_PER_ORG_MAX, "a Checkout bills one seat at least, an org's at most");
        if let Some(t) = &plan.trial {
            self.hold_trial_place(&t.id, &who.id, b.until_ms)?;
        }
        Ok(plan)
    }

    fn pack_begin(&self, b: PackBegin) -> CellResult<PackPlan> {
        let who = self.person_by(&b.by)?;
        let email = self.latest_email(&who.id)?.ok_or_else(|| CellError::invalid("a pack is bought by someone who signs in, with their email"))?;
        let mine = self.member_of(&who.id)?;
        let (org, person) = match (&b.member, mine) {
            (None, Some(m)) if m.seat.is_some() => (m.org, who.id.clone()),
            (None, _) => return Err(CellError::invalid("credit packs are for a seat's holder: you hold none")),
            (Some(id), Some(m)) if m.admin != 0 => {
                let member = self.member(id)?;
                match (member.org == m.org, member.person, member.seat) {
                    (true, Some(p), Some(_)) => (m.org, p),
                    (true, None, Some(_)) => return Err(CellError::invalid("that seat waits on its email: credit is bought once someone holds it")),
                    _ => return Err(not_found(format!("no seat {id} in your org"))),
                }
            }
            (Some(_), _) => return Err(CellError::new(ErrorCode::Forbidden, "only an org's admins buy credit for its members")),
        };
        let o = self.stored_org(&org)?;
        let customer = self.billing_row(&org)?.and_then(|r| r.customer);
        Ok(PackPlan { buyer: who.id, person, email, org, org_name: o.name, customer })
    }

    /// The customer whose portal the asker opens: only who pays for their
    /// org opens its billing (Paul's to reverse: the review of #367), since
    /// the portal renews a subscription and changes its card and email.
    /// Its other admins manage its seats in the shell, never its billing.
    fn customer_of(&self, b: CustomerOf) -> CellResult<Customer> {
        let who = self.person_by(&b.by)?;
        let m = self.member_of(&who.id)?.ok_or_else(|| CellError::new(ErrorCode::Forbidden, "only who pays for an org opens its billing"))?;
        if self.payer_of(&m.org)?.as_deref() != Some(who.id.as_str()) {
            return Err(CellError::new(ErrorCode::Forbidden, "only who pays for your org opens its billing: its admins manage its seats here"));
        }
        let customer = self.billing_row(&m.org)?.and_then(|r| r.customer).ok_or_else(|| CellError::host(format!("{} pays for {} and it has no customer", who.id, m.org)))?;
        Ok(Customer { customer })
    }

    /// Writes `s` as `org`'s subscription, when it may (module docs). A
    /// change in whether the org pays queues a push for each paid seat's
    /// holder. Answers whether it was written: an org that is gone (it
    /// ended, its subscription canceled) takes nothing.
    pub(super) fn apply_subscription_copy(&self, org: &str, s: &SubscriptionCopy, at: Option<i64>) -> CellResult<bool> {
        let Some(row) = self.billing_row(org)? else { return Ok(false) };
        let status = Status::parse(&s.status).ok_or_else(|| CellError::invalid(format!("a subscription's status is Stripe's, not {:?}", s.status)))?;
        match row.subscription.as_deref() {
            // an org with none takes only one that pays: a late event of one
            // canceled and forgotten (its payer went) never comes back
            None if !status.is_good() => return Ok(false),
            None => {}
            // a newer subscription replaces one that ended, never one that
            // pays; its customer (its buyer's) becomes the org's
            Some(current) if current != s.id => {
                if good(row.status.as_deref()) || !status.is_good() {
                    return Ok(false);
                }
            }
            Some(_) if at.zip(row.event_at).is_some_and(|(at, last)| at < last) => return Ok(false),
            // its own subscription: of the customer it has, or a contradiction
            Some(_) if row.customer.as_deref().is_some_and(|c| c != s.customer) => {
                return Err(CellError::host(format!("{org}'s subscription {} is another customer's ({})", s.id, s.customer)));
            }
            Some(_) => {}
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

    /// An orphan subscription's cancel, after its grace (`CancelOrphan`).
    fn cancel_orphan(&self, b: CancelOrphan) -> CellResult<Applied> {
        if !org::valid_org_id(&b.org) || !b.subscription.starts_with("sub_") {
            return Err(CellError::invalid("an orphan is a subscription naming an org"));
        }
        // an org took it meanwhile: it is that org's
        if self.billing_row(&b.org)?.and_then(|r| r.subscription).as_deref() == Some(b.subscription.as_str()) {
            return Ok(Applied { applied: false, orphan: false });
        }
        self.queue_cancel_after(&b.subscription, &b.org, super::seats::WHY_ORPHAN, ORPHAN_GRACE_MS)?;
        Ok(Applied { applied: true, orphan: true })
    }

    fn apply_subscription(&self, b: ApplySubscription) -> CellResult<Applied> {
        let orphan = org::valid_org_id(&b.org) && self.org_row(&b.org)?.is_none();
        Ok(Applied { applied: self.apply_subscription_copy(&b.org, &b.subscription, b.at)?, orphan })
    }

    /// The seat a completed Checkout bought its buyer, once by its session:
    /// a new org it names made, its buyer its admin and its paid seat's
    /// holder (`Yes::Bought`), or their org that bought again paying, their
    /// seat the kind bought. A Checkout whose buyer can no longer take it
    /// (in another org now, no longer their org's admin, being wiped, or
    /// another subscription pays already) seats no one, and its
    /// subscription is canceled at once.
    fn apply_checkout(&self, b: ApplyCheckout) -> CellResult<Applied> {
        if self.count("SELECT COUNT(*) AS n FROM checkouts WHERE session = ?", vec![b.session.as_str().into()])? > 0 {
            return Ok(Applied { applied: false, orphan: false });
        }
        if !org::valid_org_id(&b.org) {
            return Err(not_found(format!("no org {}", b.org)));
        }
        let applied = match self.seat_bought(&b)? {
            true => {
                if let Some(code) = &b.trial {
                    self.trial_bought(code, &b.person, &b.subscription.id)?;
                }
                true
            }
            false => {
                console_log!("{}", json!({ "checkout-stray": b.session, "org": b.org, "subscription": b.subscription.id }));
                self.queue_cancel(&b.subscription.id, &b.org, super::seats::WHY_STRAY)?;
                false
            }
        };
        self.exec(
            "INSERT INTO checkouts (session, org, person, kind, at) VALUES (?, ?, ?, ?, ?)",
            vec![b.session.as_str().into(), b.org.as_str().into(), b.person.as_str().into(), b.kind.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(Applied { applied, orphan: false })
    }

    /// `b`'s seat, its buyer's, when they can take it: answers whether they did.
    fn seat_bought(&self, b: &ApplyCheckout) -> CellResult<bool> {
        let buyer = self.identity(&b.person)?;
        if !buyer.as_ref().is_some_and(|i| i.kind == IdentityKind::Person) || self.wiping(&b.person)? {
            return Ok(false);
        }
        let mine = self.member_of(&b.person)?;
        match self.org_row(&b.org)? {
            // a new org: its buyer's alone, once paid
            None => {
                if mine.is_some() {
                    return Ok(false);
                }
                let email = self.latest_email(&b.person)?.ok_or_else(|| CellError::host(format!("{} bought a seat and signs in with no email", b.person)))?;
                self.new_org(&b.org, &email, &b.person)?;
                let offer = self.insert_offer(&b.org, &email, true, Some(b.kind), false, &b.person)?;
                self.enroll(&offer, Yes::Bought(&b.person))?;
                if !self.apply_subscription_copy(&b.org, &b.subscription, None)? {
                    return Err(CellError::host(format!("a new org {} refused its first subscription {}", b.org, b.subscription.id)));
                }
            }
            // their org buying again: its admin still, and nothing else pays
            Some(_) => {
                let Some(m) = mine.filter(|m| m.org == b.org && m.admin != 0) else { return Ok(false) };
                self.apply_subscription_copy(&b.org, &b.subscription, None)?;
                if self.billing_row(&b.org)?.and_then(|r| r.subscription).as_deref() != Some(b.subscription.id.as_str()) {
                    return Ok(false);
                }
                // their seat is the kind bought (a lapsed paid one of theirs,
                // or none), unless they came to hold a comped one meanwhile
                if m.comped == 0 {
                    self.exec("UPDATE org_members SET seat = ?, comped = 0 WHERE id = ?", vec![b.kind.as_str().into(), m.id.as_str().into()])?;
                    self.queue_sync(&b.person)?;
                    self.queue_quantities(&b.org)?;
                }
            }
        }
        self.set_payer(&b.org, &b.person, &b.subscription.id)?;
        assert!(self.member_of(&b.person)?.is_some_and(|m| m.org == b.org), "a buyer seated is in the org they bought in");
        Ok(true)
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
            PackBegin::PATH => reply::<PackBegin>(body(bytes).and_then(|b| self.pack_begin(b))),
            CustomerOf::PATH => reply::<CustomerOf>(body(bytes).and_then(|b| self.customer_of(b))),
            ApplyCheckout::PATH => reply::<ApplyCheckout>(self.armed_billing(|| self.apply_checkout(body(bytes)?)).await),
            ApplySubscription::PATH => reply::<ApplySubscription>(self.armed_billing(|| self.apply_subscription(body(bytes)?)).await),
            CancelOrphan::PATH => reply::<CancelOrphan>(self.armed_billing(|| self.cancel_orphan(body(bytes)?)).await),
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

