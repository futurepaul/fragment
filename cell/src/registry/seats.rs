//! An org's admins and their paid seats (docs/billing.md; decisions 51 and
//! 52): while the org pays, an admin offers a seat or an admin's place by
//! email (offers.rs: no one's until they accept it), removes one, and
//! removes admins. One of the org's own members is given a seat or an
//! admin's place at once: they are in it by their yes. A paid seat is the
//! one sold, `seat_always_on` (Paul, 2026-10-15), and bills from its offer:
//! the org's subscription counts every paid seat, held or offered.
//!
//! Who pays (billing.rs `org_payers`: the person whose Checkout made its
//! subscription) is no other admin's to remove, and steps down only once
//! their card bills on no more (the subscription canceled, or set to end):
//! no admin can leave a card paying for an org its owner no longer manages.
//!
//! The seats are the registry's; Stripe's item quantity is a copy. A
//! change queues the org in `quantity_syncs`, in the change's own turn, and
//! the registry's alarm pushes what the count read fresh asks
//! (`fragment_core::stripe::push`): its quantity, prorated onto the next
//! invoice, or, no paid seat left, a cancel (`stripe_cancels`, below). Two
//! changes at once never leave Stripe a seat short: the push after both
//! reads both. A push that fails is tried again; a seat change is never
//! undone for it. An org no one is in or offered goes once nothing of it
//! bills (`drop_if_empty`); its Stripe customer is kept.
//!
//! An admin's removal of the org's last paid seat is refused: its
//! subscription is canceled in Stripe's portal, at the period's end. A
//! member's going (leaving, moving, wiped) that leaves none cancels it at
//! once.

use fragment_core::org::{Status, ADMINS_PER_ORG_MAX, SYNC_BATCH, SYNC_RETRY_MS};
use fragment_core::stripe::{self as core_stripe, Push, Snapshot, Subscription};
use fragment_proto::org::{OrgMember, SeatKind};
use serde::Serialize;

use super::billing::SubscriptionCopy;
use super::calls::{By, Call};
use super::orgs::{not_found, MemberRow};
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS quantity_syncs (
  org TEXT PRIMARY KEY, since INTEGER NOT NULL, due INTEGER NOT NULL, tries INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS stripe_cancels (
  subscription TEXT PRIMARY KEY, org TEXT NOT NULL, why TEXT NOT NULL, since INTEGER NOT NULL, due INTEGER NOT NULL, tries INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS stripe_cancels_org ON stripe_cancels (org);
";

/// `POST /orgs/seats/add`: an admin offers a paid seat to `email`, of the
/// kind sold (any other is refused); one of the org's members takes it at once.
#[derive(Serialize, Deserialize)]
pub(crate) struct AddSeat {
    pub by: By,
    pub email: String,
    pub kind: SeatKind,
}

/// A seat or admin offered or given: the row, its org's name, whether this
/// call made it, and who made it (their email, for the offer's mail).
#[derive(Serialize, Deserialize)]
pub(crate) struct Added {
    pub member: OrgMember,
    pub org_name: String,
    pub created: bool,
    pub from: Option<String>,
}

impl Call for AddSeat {
    const PATH: &'static str = "/orgs/seats/add";
    type Answer = Added;
}

/// `POST /orgs/seats/remove`: an admin removes a paid seat, held or
/// offered. An admin's own row stays, seatless. Answers the row as it was.
#[derive(Serialize, Deserialize)]
pub(crate) struct RemoveSeat {
    pub by: By,
    pub seat: String,
}

impl Call for RemoveSeat {
    const PATH: &'static str = "/orgs/seats/remove";
    type Answer = OrgMember;
}

/// `POST /orgs/admins/add`: an admin offers someone (by email) an admin's
/// place too; one of the org's members takes it at once.
#[derive(Serialize, Deserialize)]
pub(crate) struct AddAdmin {
    pub by: By,
    pub email: String,
}

impl Call for AddAdmin {
    const PATH: &'static str = "/orgs/admins/add";
    type Answer = Added;
}

/// `POST /orgs/admins/remove`: an admin stops being one (a row with a seat
/// keeps it; one without goes), or an admin's place offered is withdrawn.
/// The org keeps one admin at least, and who pays steps down only themselves.
#[derive(Serialize, Deserialize)]
pub(crate) struct RemoveAdmin {
    pub by: By,
    pub member: String,
}

impl Call for RemoveAdmin {
    const PATH: &'static str = "/orgs/admins/remove";
    type Answer = OrgMember;
}

#[derive(Deserialize)]
struct QuantityRow {
    org: String,
    since: i64,
    tries: i64,
}

#[derive(Deserialize)]
struct CancelRow {
    subscription: String,
    org: String,
    why: String,
    since: i64,
    tries: i64,
}

#[derive(Deserialize)]
pub(super) struct OrgSub {
    pub(super) subscription: Option<String>,
    pub(super) status: Option<String>,
    items: Option<String>,
    customer: Option<String>,
    period_end: Option<i64>,
    trial_end: Option<i64>,
    pub(super) cancel_at_end: i64,
}

impl RegistryCell {
    /// The org `by` admins, and their row.
    fn admin_of(&self, by: &By) -> CellResult<(Identity, MemberRow)> {
        let who = self.person_by(by)?;
        let m = self.member_of(&who.id)?.filter(|m| m.admin != 0).ok_or_else(|| CellError::new(ErrorCode::Forbidden, "only an org's admins change its seats and admins"))?;
        Ok((who, m))
    }

    /// One of `org`'s rows, by its id: another org's is none.
    fn member_in(&self, org: &str, id: &str) -> CellResult<MemberRow> {
        let m = self.member(id)?;
        if m.org != org {
            return Err(not_found(format!("no seat {id}")));
        }
        Ok(m)
    }

    /// An org's billing columns; `None`: no such org (one that ended).
    pub(super) fn org_sub(&self, org: &str) -> CellResult<Option<OrgSub>> {
        self.row::<OrgSub>("SELECT subscription, status, items, customer, period_end, trial_end, cancel_at_end FROM orgs WHERE id = ?", vec![org.into()])
    }

    /// An org that pays: its seats and admins may change, and it offers
    /// them. One that never paid, or lapsed, buys again first; a comped
    /// org's seats are its operators'.
    fn paying(&self, org: &str) -> CellResult<()> {
        let sub = self.org_sub(org)?.ok_or_else(|| not_found(format!("no org {org}")))?;
        if sub.subscription.is_none() || !sub.status.as_deref().and_then(Status::parse).is_some_and(Status::is_good) {
            return Err(CellError::invalid("your org pays for no seats now: buy one first (Checkout), then offer the rest"));
        }
        Ok(())
    }

    /// The org's paid seats, held and offered.
    pub(super) fn paid_seats(&self, org: &str) -> CellResult<u64> {
        self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND seat IS NOT NULL AND comped = 0", vec![org.into()])
    }

    /// Queues `org`'s counts to be pushed to Stripe, in the caller's turn:
    /// after every change to its rows, so an org left with no paid seat has
    /// its subscription canceled, and one left with no one is dropped.
    pub(super) fn queue_quantities(&self, org: &str) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO quantity_syncs (org, since, due, tries) VALUES (?, ?, ?, 0)
             ON CONFLICT (org) DO UPDATE SET since = excluded.since, due = excluded.due, tries = 0",
            vec![org.into(), now.clone(), now],
        )
    }

    fn email_arg(raw: &str) -> CellResult<String> {
        super::signin::email_of(raw).ok_or_else(|| CellError::invalid(format!("{raw:?} is not an email")))
    }

    /// The email's person, refused while a wipe of them runs.
    fn person_for_offer(&self, email: &str) -> CellResult<Option<String>> {
        let person = self.person_by_email(email)?;
        if let Some(p) = &person {
            self.not_wiping_id(p)?;
        }
        Ok(person)
    }

    /// `org`'s member the email's person is, if they are one.
    fn member_by_email(&self, org: &str, person: Option<&str>) -> CellResult<Option<MemberRow>> {
        match person {
            Some(p) => Ok(self.member_of(p)?.filter(|m| m.org == org)),
            None => Ok(None),
        }
    }

    fn admins_room(&self, org: &str) -> CellResult<()> {
        // offers count: an admin's place offered is one of the ten
        if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND admin = 1", vec![org.into()])? >= ADMINS_PER_ORG_MAX {
            return Err(CellError::invalid(format!("an org has at most {ADMINS_PER_ORG_MAX} admins, offered ones included")));
        }
        Ok(())
    }

    fn add_seat(&self, b: AddSeat) -> CellResult<Added> {
        let (who, admin) = self.admin_of(&b.by)?;
        core_stripe::for_sale(b.kind)?;
        let org_row = self.stored_org(&admin.org)?;
        self.paying(&org_row.id)?;
        let email = Self::email_arg(&b.email)?;
        let person = self.person_for_offer(&email)?;
        let from = self.email_of(&who.id)?;
        let added = |member: &MemberRow, created: bool| -> CellResult<Added> { Ok(Added { member: member.wire(None)?, org_name: org_row.name.clone(), created, from: from.clone() }) };
        // one of its members: the seat is theirs at once, in their own row
        if let Some(m) = self.member_by_email(&org_row.id, person.as_deref())? {
            return match m.seat()? {
                Some(k) if k == b.kind && m.comped == 0 => added(&m, false),
                Some(_) => Err(conflict(format!("{email} holds a seat here already"))),
                None => {
                    self.check_seats_room(&org_row.id)?;
                    self.exec("UPDATE org_members SET seat = ?, comped = 0 WHERE id = ?", vec![b.kind.as_str().into(), m.id.as_str().into()])?;
                    if let Some(p) = &m.person {
                        self.queue_sync(p)?;
                    }
                    self.queue_quantities(&org_row.id)?;
                    added(&self.member(&m.id)?, true)
                }
            };
        }
        // anyone else is offered it: it bills from now (decision 52), and is
        // theirs once they accept it
        let offer = match self.offer_in(&org_row.id, &email)? {
            Some(o) if o.seat.is_some() => {
                // the same offer again is answered again
                if o.seat()? == Some(b.kind) && o.comped == 0 {
                    return added(&o, false);
                }
                return Err(conflict(format!("{email} is offered a seat here already")));
            }
            Some(o) => {
                // an admin's place offered: the seat joins it
                self.check_seats_room(&org_row.id)?;
                self.count_offer_mail(&org_row.id)?;
                self.exec("UPDATE org_members SET seat = ?, comped = 0 WHERE id = ?", vec![b.kind.as_str().into(), o.id.as_str().into()])?;
                self.member(&o.id)?
            }
            None => {
                self.check_offer_room(&email)?;
                self.check_seats_room(&org_row.id)?;
                self.count_offer_mail(&org_row.id)?;
                self.insert_offer(&org_row.id, &email, false, Some(b.kind), false, &who.id)?
            }
        };
        assert!(offer.person.is_none(), "an offer is no one's until they accept it");
        self.queue_quantities(&org_row.id)?;
        added(&offer, true)
    }

    /// A paid seat of the org `by` admins, held or offered.
    fn paid_seat(&self, by: &By, seat: &str) -> CellResult<MemberRow> {
        let (_, admin) = self.admin_of(by)?;
        let m = self.member_in(&admin.org, seat)?;
        match m.seat()? {
            Some(_) if m.comped == 0 => Ok(m),
            Some(_) => Err(CellError::invalid(format!("{seat} is comped: an operator changes it"))),
            None => Err(not_found(format!("no seat {seat}"))),
        }
    }

    fn remove_seat(&self, b: RemoveSeat) -> CellResult<OrgMember> {
        let m = self.paid_seat(&b.by, &b.seat)?;
        if self.paid_seats(&m.org)? <= 1 {
            return Err(CellError::invalid("this is the org's last paid seat: cancel its subscription in Payment and invoices (it ends with the period)"));
        }
        if m.admin != 0 {
            // an admin (or an admin's place offered) keeps their place
            self.exec("UPDATE org_members SET seat = NULL, sleeps = 0 WHERE id = ?", vec![m.id.as_str().into()])?;
        } else {
            self.exec("DELETE FROM org_members WHERE id = ?", vec![m.id.as_str().into()])?;
        }
        if let Some(p) = &m.person {
            self.queue_sync(p)?;
        }
        self.queue_quantities(&m.org)?;
        m.wire(None)
    }

    fn add_admin(&self, b: AddAdmin) -> CellResult<Added> {
        let (who, admin) = self.admin_of(&b.by)?;
        let org_row = self.stored_org(&admin.org)?;
        self.paying(&org_row.id)?;
        let email = Self::email_arg(&b.email)?;
        let person = self.person_for_offer(&email)?;
        let from = self.email_of(&who.id)?;
        let added = |member: &MemberRow, created: bool| -> CellResult<Added> { Ok(Added { member: member.wire(None)?, org_name: org_row.name.clone(), created, from: from.clone() }) };
        // one of its members: an admin at once, in their own row
        if let Some(m) = self.member_by_email(&org_row.id, person.as_deref())? {
            if m.admin != 0 {
                return added(&m, false);
            }
            self.admins_room(&org_row.id)?;
            self.exec("UPDATE org_members SET admin = 1 WHERE id = ?", vec![m.id.as_str().into()])?;
            return added(&self.member(&m.id)?, true);
        }
        // anyone else is offered it: an admin once they accept it
        let offer = match self.offer_in(&org_row.id, &email)? {
            Some(o) if o.admin != 0 => return added(&o, false),
            Some(o) => {
                self.admins_room(&org_row.id)?;
                self.count_offer_mail(&org_row.id)?;
                self.exec("UPDATE org_members SET admin = 1 WHERE id = ?", vec![o.id.as_str().into()])?;
                self.member(&o.id)?
            }
            None => {
                self.check_offer_room(&email)?;
                self.admins_room(&org_row.id)?;
                self.count_offer_mail(&org_row.id)?;
                self.insert_offer(&org_row.id, &email, true, None, false, &who.id)?
            }
        };
        assert!(offer.person.is_none(), "an offer is no one's until they accept it");
        added(&offer, true)
    }

    fn remove_admin(&self, b: RemoveAdmin) -> CellResult<OrgMember> {
        let (who, admin) = self.admin_of(&b.by)?;
        let m = self.member_in(&admin.org, &b.member)?;
        if m.admin == 0 {
            return Err(not_found(format!("{} is no admin", b.member)));
        }
        // an admin's place offered is withdrawn; a member's is guarded
        if let Some(person) = &m.person {
            if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND admin = 1 AND person IS NOT NULL", vec![m.org.as_str().into()])? <= 1 {
                return Err(CellError::invalid("an org keeps one admin at least: add another first"));
            }
            if self.payer_of(&m.org)?.as_deref() == Some(person.as_str()) {
                if *person != who.id {
                    return Err(CellError::new(ErrorCode::Forbidden, format!("{} pays for this org: only they step down as its admin", m.email)));
                }
                if self.bills(&m.org, person)? {
                    return Err(CellError::invalid("you pay for this org: cancel its subscription in Payment and invoices first (it ends with the period)"));
                }
            }
        }
        if m.seat.is_some() {
            self.exec("UPDATE org_members SET admin = 0 WHERE id = ?", vec![m.id.as_str().into()])?;
        } else {
            self.exec("DELETE FROM org_members WHERE id = ?", vec![m.id.as_str().into()])?;
            self.queue_quantities(&m.org)?;
        }
        m.wire(None)
    }

    /// The org's subscription as the registry keeps it. A copy with no
    /// item of the seat sold (one kept from a subscription of the $100
    /// seat, no longer sold) is refused, as Stripe's own is.
    fn snapshot_of(&self, sub: &OrgSub) -> CellResult<Option<Snapshot>> {
        let (Some(id), Some(status), Some(items), Some(customer)) = (&sub.subscription, &sub.status, &sub.items, &sub.customer) else {
            return Ok(None);
        };
        let items: Vec<(SeatKind, String, u64)> = serde_json::from_str(items).map_err(|e| CellError::host(format!("orgs.items: {e}")))?;
        let Some((_, item, seats)) = items.into_iter().find(|(k, _, _)| *k == SeatKind::SOLD) else {
            return Err(CellError::host(format!("{id} has no item of the seat sold")));
        };
        let status = Status::parse(status).ok_or_else(|| CellError::host(format!("orgs.status {status:?}")))?;
        Ok(Some(Snapshot {
            id: id.clone(),
            customer: customer.clone(),
            status,
            item,
            seats,
            period_end: sub.period_end,
            trial_end: sub.trial_end,
            cancel_at_period_end: sub.cancel_at_end != 0,
        }))
    }

    /// Brings `org`'s subscription to what its paid seats ask
    /// (`fragment_core::stripe::push`): its quantity, writing what Stripe
    /// answers, or, none left, its cancel queued. Then an org no one is in
    /// goes once nothing of it bills. An org already gone has nothing to push.
    async fn push_quantities(&self, org: &str, key: &str) -> CellResult<()> {
        let Some(sub) = self.org_sub(org)? else { return Ok(()) };
        if let Some(snapshot) = self.snapshot_of(&sub)? {
            match core_stripe::push(&snapshot, self.paid_seats(org)?) {
                Push::Nothing => {}
                Push::Quantity(form) => {
                    let stripe = crate::stripe::client(&self.env, self.cfg).await?;
                    let answered: Subscription = stripe.post(&format!("/v1/subscriptions/{}", snapshot.id), &form, key).await?;
                    let copy: SubscriptionCopy = answered.snapshot().map_err(CellError::host)?.into();
                    self.apply_subscription_copy(org, &copy, None)?;
                }
                Push::Cancel => self.queue_cancel(&snapshot.id, org, "no paid seat is left")?,
            }
        }
        self.drop_if_empty(org)
    }

    /// An org no one is in or offered goes once nothing of it bills (no
    /// subscription, or one ended, its cancel landed): its row, who paid,
    /// its queued push. Its Stripe customer is kept (invoices, tax,
    /// refunds); the registry's link to it goes with the row.
    pub(super) fn drop_if_empty(&self, org: &str) -> CellResult<()> {
        if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ?", vec![org.into()])? > 0 {
            return Ok(());
        }
        let Some(sub) = self.org_sub(org)? else { return Ok(()) };
        if let Some(id) = &sub.subscription {
            let status = sub.status.as_deref().and_then(Status::parse).ok_or_else(|| CellError::host(format!("orgs.status of {org} is {:?}", sub.status)))?;
            let canceling = self.count("SELECT COUNT(*) AS n FROM stripe_cancels WHERE subscription = ?", vec![id.as_str().into()])? > 0;
            if !status.is_ended() || canceling {
                // its cancel is queued (`push_quantities`): it goes once that lands
                return Ok(());
            }
        }
        self.exec("DELETE FROM orgs WHERE id = ?", vec![org.into()])?;
        self.exec("DELETE FROM org_payers WHERE org = ?", vec![org.into()])?;
        self.exec("DELETE FROM quantity_syncs WHERE org = ?", vec![org.into()])?;
        console_log!("{}", json!({ "org-ended": org }));
        assert!(self.org_sub(org)?.is_none(), "an org dropped is gone");
        Ok(())
    }

    /// One queued push of `r.org`'s counts; the queue's row goes once it
    /// lands (unless queued again meanwhile), else it is tried again later.
    pub(super) async fn push_queued(&self, org: &str) -> CellResult<bool> {
        let Some(r) = self.row::<QuantityRow>("SELECT org, since, tries FROM quantity_syncs WHERE org = ?", vec![org.into()])? else { return Ok(true) };
        self.push_row(r).await
    }

    async fn push_row(&self, r: QuantityRow) -> CellResult<bool> {
        let since = SqlStorageValue::Integer(r.since);
        let key = format!("fragment-qty-{}-{}-{}", r.org, r.since, r.tries);
        match self.push_quantities(&r.org, &key).await {
            Ok(()) => {
                self.exec("DELETE FROM quantity_syncs WHERE org = ? AND since = ?", vec![r.org.as_str().into(), since])?;
                Ok(true)
            }
            Err(e) => {
                console_error!("{}", json!({ "quantity-sync": r.org, "tries": r.tries + 1, "failed": e.message }));
                let wait = SYNC_RETRY_MS.saturating_mul(1 << r.tries.clamp(0, 6));
                self.exec(
                    "UPDATE quantity_syncs SET tries = tries + 1, due = ? WHERE org = ? AND since = ?",
                    vec![SqlStorageValue::Integer(js::now_ms() + wait), r.org.as_str().into(), since],
                )?;
                Ok(false)
            }
        }
    }

    /// The alarm's quantity pushes: a batch of the queued ones that are
    /// due. A pushed one leaves the queue unless queued again meanwhile.
    pub(super) async fn quantity_alarm(&self) -> CellResult<()> {
        let now = js::now_ms();
        let due = self.rows::<QuantityRow>(
            "SELECT org, since, tries FROM quantity_syncs WHERE due <= ? ORDER BY due LIMIT ?",
            vec![SqlStorageValue::Integer(now), SqlStorageValue::Integer(SYNC_BATCH.into())],
        )?;
        // bounded: at most SYNC_BATCH pushes a run
        for r in due {
            self.push_row(r).await?;
        }
        self.arm_quantities().await
    }

    pub(super) async fn arm_quantities(&self) -> CellResult<()> {
        #[derive(Deserialize)]
        struct Due {
            due: Option<i64>,
        }
        let due = self.row::<Due>("SELECT MIN(due) AS due FROM (SELECT due FROM quantity_syncs UNION ALL SELECT due FROM stripe_cancels)", vec![])?.and_then(|r| r.due);
        match due {
            Some(due) => self.arm_at(due).await,
            None => Ok(()),
        }
    }

    /// A reconcile, or a webhook, that finds Stripe's count other than the
    /// seats' queues a push (the seats are the truth), as does a
    /// subscription that may still bill an org with no paid seat left.
    pub(super) fn queue_quantities_if_drifted(&self, org: &str, copy: &SubscriptionCopy) -> CellResult<()> {
        let have: u64 = copy.items.iter().filter(|(k, _, _)| *k == SeatKind::SOLD).map(|(_, _, q)| q).sum();
        let want = self.paid_seats(org)?;
        let due = match Status::parse(&copy.status) {
            Some(s) if want == 0 => !s.is_ended(),
            Some(s) => s.is_good() && have != want,
            None => false,
        };
        if due {
            self.queue_quantities(org)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------- cancels

    /// Queues `subscription`'s cancel, at once, in the caller's turn: no
    /// paid seat is left of its org, its payer was wiped, or a paid
    /// Checkout seated no one (`why`, for the log). Queued twice, the first
    /// stays.
    pub(super) fn queue_cancel(&self, subscription: &str, org: &str, why: &str) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO stripe_cancels (subscription, org, why, since, due, tries) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT (subscription) DO NOTHING",
            vec![subscription.into(), org.into(), why.into(), now.clone(), now],
        )
    }

    /// Cancels `subscription` in Stripe, at once (no proration: anything
    /// owed back is a refund by hand in Stripe's dashboard), its customer
    /// kept: fetched first, never one not this deployment's (`None`), and
    /// one ended already is answered as it is. A cancel that raced another
    /// is fetched again.
    async fn cancel_one(&self, stripe: &crate::stripe::Stripe, subscription: &str) -> CellResult<Option<SubscriptionCopy>> {
        let path = format!("/v1/subscriptions/{subscription}");
        let sub: Subscription = stripe.get(&path).await?;
        if !core_stripe::ours(&sub.metadata, self.cfg.stripe_deployment()) {
            return Ok(None);
        }
        let ended = |s: &Subscription| Status::parse(&s.status).is_some_and(Status::is_ended);
        let canceled = match ended(&sub) {
            true => sub,
            false => match stripe.delete::<Subscription>(&path).await {
                Ok(s) => s,
                Err(e) if matches!(e.code, ErrorCode::InvalidRequest | ErrorCode::NotFound) => {
                    let again: Subscription = stripe.get(&path).await?;
                    if !ended(&again) {
                        return Err(e);
                    }
                    again
                }
                Err(e) => return Err(e),
            },
        };
        if !ended(&canceled) {
            return Err(CellError::host(format!("Stripe answered the cancel of {subscription} with {}", canceled.status)));
        }
        Ok(Some(canceled.snapshot().map_err(CellError::host)?.into()))
    }

    /// The queued cancels that are due (only `org`'s, when named), each
    /// tried once: one that lands leaves the queue, its org told (when the
    /// subscription is its own: its paid seats lapse) and dropped if no one
    /// is in it; one that fails is tried again later. Answers whether none
    /// of them is left.
    pub(super) async fn run_cancels(&self, org: Option<&str>) -> CellResult<bool> {
        let now = SqlStorageValue::Integer(js::now_ms());
        let batch = SqlStorageValue::Integer(SYNC_BATCH.into());
        let due = match org {
            Some(o) => self.rows::<CancelRow>("SELECT subscription, org, why, since, tries FROM stripe_cancels WHERE org = ? ORDER BY due LIMIT ?", vec![o.into(), batch])?,
            None => self.rows::<CancelRow>("SELECT subscription, org, why, since, tries FROM stripe_cancels WHERE due <= ? ORDER BY due LIMIT ?", vec![now, batch])?,
        };
        if due.is_empty() {
            return Ok(true);
        }
        let stripe = crate::stripe::client(&self.env, self.cfg).await?;
        let mut left = false;
        // bounded: at most SYNC_BATCH cancels a run
        for r in due {
            match self.cancel_one(&stripe, &r.subscription).await {
                Ok(copy) => {
                    self.exec("DELETE FROM stripe_cancels WHERE subscription = ?", vec![r.subscription.as_str().into()])?;
                    console_log!("{}", json!({ "stripe-cancel": r.subscription, "org": r.org, "why": r.why, "ours": copy.is_some() }));
                    let current = self.org_sub(&r.org)?.and_then(|s| s.subscription);
                    if let Some(c) = copy.filter(|c| current.as_deref() == Some(c.id.as_str())) {
                        self.apply_subscription_copy(&r.org, &c, None)?;
                    }
                    self.drop_if_empty(&r.org)?;
                }
                Err(e) => {
                    left = true;
                    console_error!("{}", json!({ "stripe-cancel": r.subscription, "org": r.org, "tries": r.tries + 1, "failed": e.message }));
                    let wait = SYNC_RETRY_MS.saturating_mul(1 << r.tries.clamp(0, 6));
                    self.exec(
                        "UPDATE stripe_cancels SET tries = tries + 1, due = ? WHERE subscription = ? AND since = ?",
                        vec![SqlStorageValue::Integer(js::now_ms() + wait), r.subscription.as_str().into(), SqlStorageValue::Integer(r.since)],
                    )?;
                }
            }
        }
        Ok(!left)
    }

    /// The alarm's cancels, after its quantity pushes (one that queued a
    /// cancel has it run in the same alarm).
    pub(super) async fn cancel_alarm(&self) -> CellResult<()> {
        self.run_cancels(None).await?;
        self.arm_quantities().await
    }

    /// The registry's routes for an org's admins, or `None`.
    pub(super) async fn seats_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            AddSeat::PATH => reply::<AddSeat>(self.armed_seats(|| self.add_seat(body(bytes)?)).await),
            RemoveSeat::PATH => reply::<RemoveSeat>(self.armed_seats(|| self.remove_seat(body(bytes)?)).await),
            AddAdmin::PATH => reply::<AddAdmin>(self.armed_seats(|| self.add_admin(body(bytes)?)).await),
            RemoveAdmin::PATH => reply::<RemoveAdmin>(self.armed_seats(|| self.remove_admin(body(bytes)?)).await),
            _ => return None,
        })
    }

    pub(super) async fn armed_seats<T>(&self, change: impl FnOnce() -> CellResult<T>) -> CellResult<T> {
        let answer = change()?;
        self.arm_syncs().await?;
        self.arm_quantities().await?;
        Ok(answer)
    }
}
