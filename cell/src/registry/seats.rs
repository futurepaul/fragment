//! An org's admins and their paid seats (docs/billing.md; decisions 51 and
//! 52): an admin adds a seat by email (held at once by its person, or
//! waiting on the email), changes a seat's kind, removes one, and adds or
//! removes admins. A paid seat bills from its invite: the org's
//! subscription counts every paid seat, held or pending.
//!
//! The seats are the registry's; Stripe's item quantities are a copy. A
//! change queues the org in `quantity_syncs`, in the change's own turn, and
//! the registry's alarm pushes the counts read fresh (`fragment_core::
//! stripe::item_changes`), prorated onto the next invoice, then writes the
//! subscription Stripe answers. Two changes at once never leave Stripe a
//! seat short: the push after both reads both. A push that fails is tried
//! again; a seat change is never undone for it.
//!
//! An org's last paid seat is not removed here: its subscription is
//! canceled in Stripe's portal (at the period's end).

use fragment_core::org::{Status, ADMINS_PER_ORG_MAX, SYNC_BATCH, SYNC_RETRY_MS};
use fragment_core::stripe::{self as core_stripe, Counts, List, Price, Snapshot};
use fragment_proto::org::{OrgMember, SeatKind};
use serde::Serialize;

use super::billing::SubscriptionCopy;
use super::calls::{By, Call};
use super::orgs::{not_found, MemberRow, MEMBER_COLUMNS};
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS quantity_syncs (
  org TEXT PRIMARY KEY, since INTEGER NOT NULL, due INTEGER NOT NULL, tries INTEGER NOT NULL);
";

/// `POST /orgs/seats/add`: an admin adds a paid seat for `email`.
#[derive(Serialize, Deserialize)]
pub(crate) struct AddSeat {
    pub by: By,
    pub email: String,
    pub kind: SeatKind,
}

/// A seat or admin added: the row, and whether this call made it.
#[derive(Serialize, Deserialize)]
pub(crate) struct Added {
    pub member: OrgMember,
    pub org_name: String,
    pub created: bool,
}

impl Call for AddSeat {
    const PATH: &'static str = "/orgs/seats/add";
    type Answer = Added;
}

/// `POST /orgs/seats/kind`: an admin changes a paid seat's kind.
#[derive(Serialize, Deserialize)]
pub(crate) struct SeatKindChange {
    pub by: By,
    pub seat: String,
    pub kind: SeatKind,
}

impl Call for SeatKindChange {
    const PATH: &'static str = "/orgs/seats/kind";
    type Answer = OrgMember;
}

/// `POST /orgs/seats/remove`: an admin removes a paid seat. An admin's
/// own row stays, seatless. Answers the row as it was.
#[derive(Serialize, Deserialize)]
pub(crate) struct RemoveSeat {
    pub by: By,
    pub seat: String,
}

impl Call for RemoveSeat {
    const PATH: &'static str = "/orgs/seats/remove";
    type Answer = OrgMember;
}

/// `POST /orgs/admins/add`: an admin makes someone (by email) an admin too.
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
/// keeps it; one without goes). The org keeps one admin at least.
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
struct OrgSub {
    subscription: Option<String>,
    status: Option<String>,
    items: Option<String>,
    customer: Option<String>,
    period_end: Option<i64>,
    trial_end: Option<i64>,
    cancel_at_end: i64,
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

    fn org_sub(&self, org: &str) -> CellResult<OrgSub> {
        self.row::<OrgSub>("SELECT subscription, status, items, customer, period_end, trial_end, cancel_at_end FROM orgs WHERE id = ?", vec![org.into()])?
            .ok_or_else(|| not_found(format!("no org {org}")))
    }

    /// An org that pays: its seats may change. One that never paid, or
    /// lapsed, buys again first.
    fn paying(&self, org: &str) -> CellResult<()> {
        let sub = self.org_sub(org)?;
        if sub.subscription.is_none() || !sub.status.as_deref().and_then(Status::parse).is_some_and(Status::is_good) {
            return Err(CellError::invalid("your org pays for no seats now: buy one first (Checkout), then add the rest"));
        }
        Ok(())
    }

    /// The org's paid seats by kind, held and pending.
    fn paid_counts(&self, org: &str) -> CellResult<Counts> {
        #[derive(Deserialize)]
        struct Row {
            seat: String,
            n: u64,
        }
        let rows = self.rows::<Row>("SELECT seat, COUNT(*) AS n FROM org_members WHERE org = ? AND seat IS NOT NULL AND comped = 0 GROUP BY seat", vec![org.into()])?;
        let mut c = Counts::default();
        for r in rows {
            match SeatKind::parse(&r.seat) {
                Some(SeatKind::Seat) => c.seat = r.n,
                Some(SeatKind::SeatAlwaysOn) => c.seat_always_on = r.n,
                None => return Err(CellError::host(format!("org_members.seat {:?} in {org}", r.seat))),
            }
        }
        Ok(c)
    }

    /// Queues `org`'s counts to be pushed to Stripe, in the caller's turn.
    fn queue_quantities(&self, org: &str) -> CellResult<()> {
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

    fn add_seat(&self, b: AddSeat) -> CellResult<Added> {
        let (who, admin) = self.admin_of(&b.by)?;
        let org_row = self.stored_org(&admin.org)?;
        self.paying(&org_row.id)?;
        let email = Self::email_arg(&b.email)?;
        let person = self.person_by_email(&email)?;
        if let Some(p) = &person {
            self.not_wiping_id(p)?;
        }
        let existing = match &person {
            Some(p) => self.member_of(p)?,
            None => self.row(&format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.org = ? AND m.email = ? AND m.person IS NULL"), vec![org_row.id.as_str().into(), email.as_str().into()])?,
        };
        let member = match existing {
            Some(m) if m.org != org_row.id => return Err(conflict(format!("{email} is in another org"))),
            Some(m) if m.seat.is_some() => {
                // the same seat asked for again is answered again
                if m.seat()? == Some(b.kind) && m.comped == 0 {
                    return Ok(Added { member: m.wire(None)?, org_name: org_row.name, created: false });
                }
                return Err(conflict(format!("{email} holds a seat here already: change its kind")));
            }
            Some(m) => {
                self.check_seats_room(&org_row.id)?;
                self.exec("UPDATE org_members SET seat = ?, comped = 0 WHERE id = ?", vec![b.kind.as_str().into(), m.id.as_str().into()])?;
                self.member(&m.id)?
            }
            None => {
                self.check_seats_room(&org_row.id)?;
                self.insert_member(&org_row.id, person.as_deref(), &email, false, Some(b.kind), false, &who.id)?
            }
        };
        if let Some(p) = &member.person {
            self.queue_sync(p)?;
        }
        self.queue_quantities(&org_row.id)?;
        Ok(Added { member: member.wire(None)?, org_name: org_row.name, created: true })
    }

    /// A paid seat of the org `by` admins.
    fn paid_seat(&self, by: &By, seat: &str) -> CellResult<(MemberRow, SeatKind)> {
        let (_, admin) = self.admin_of(by)?;
        let m = self.member_in(&admin.org, seat)?;
        match m.seat()? {
            Some(k) if m.comped == 0 => Ok((m, k)),
            Some(_) => Err(CellError::invalid(format!("{seat} is comped: an operator changes it"))),
            None => Err(not_found(format!("no seat {seat}"))),
        }
    }

    fn seat_kind(&self, b: SeatKindChange) -> CellResult<OrgMember> {
        let (m, kind) = self.paid_seat(&b.by, &b.seat)?;
        self.paying(&m.org)?;
        if kind != b.kind {
            self.exec("UPDATE org_members SET seat = ? WHERE id = ?", vec![b.kind.as_str().into(), m.id.as_str().into()])?;
            if let Some(p) = &m.person {
                self.queue_sync(p)?;
            }
            self.queue_quantities(&m.org)?;
        }
        self.member(&m.id)?.wire(None)
    }

    fn remove_seat(&self, b: RemoveSeat) -> CellResult<OrgMember> {
        let (m, _) = self.paid_seat(&b.by, &b.seat)?;
        if self.paid_counts(&m.org)?.total() <= 1 {
            return Err(CellError::invalid("this is the org's last paid seat: cancel its subscription in Payment and invoices (it ends with the period)"));
        }
        if m.admin != 0 {
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
        let email = Self::email_arg(&b.email)?;
        let person = self.person_by_email(&email)?;
        let existing = match &person {
            Some(p) => self.member_of(p)?,
            None => self.row(&format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.org = ? AND m.email = ? AND m.person IS NULL"), vec![org_row.id.as_str().into(), email.as_str().into()])?,
        };
        if existing.as_ref().is_some_and(|m| m.org != org_row.id) {
            return Err(conflict(format!("{email} is in another org")));
        }
        if let Some(m) = existing.as_ref().filter(|m| m.admin != 0) {
            return Ok(Added { member: m.wire(None)?, org_name: org_row.name, created: false });
        }
        if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND admin = 1", vec![org_row.id.as_str().into()])? >= ADMINS_PER_ORG_MAX {
            return Err(CellError::invalid(format!("an org has at most {ADMINS_PER_ORG_MAX} admins")));
        }
        let member = match existing {
            Some(m) => {
                self.exec("UPDATE org_members SET admin = 1 WHERE id = ?", vec![m.id.as_str().into()])?;
                self.member(&m.id)?
            }
            None => self.insert_member(&org_row.id, person.as_deref(), &email, true, None, false, &who.id)?,
        };
        Ok(Added { member: member.wire(None)?, org_name: org_row.name, created: true })
    }

    fn remove_admin(&self, b: RemoveAdmin) -> CellResult<OrgMember> {
        let (_, admin) = self.admin_of(&b.by)?;
        let m = self.member_in(&admin.org, &b.member)?;
        if m.admin == 0 {
            return Err(not_found(format!("{} is no admin", b.member)));
        }
        if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND admin = 1", vec![m.org.as_str().into()])? <= 1 {
            return Err(CellError::invalid("an org keeps one admin at least: add another first"));
        }
        if m.seat.is_some() {
            self.exec("UPDATE org_members SET admin = 0 WHERE id = ?", vec![m.id.as_str().into()])?;
        } else {
            self.exec("DELETE FROM org_members WHERE id = ?", vec![m.id.as_str().into()])?;
        }
        m.wire(None)
    }

    /// The org's subscription as the registry keeps it.
    fn snapshot_of(&self, sub: &OrgSub) -> CellResult<Option<Snapshot>> {
        let (Some(id), Some(status), Some(items), Some(customer)) = (&sub.subscription, &sub.status, &sub.items, &sub.customer) else {
            return Ok(None);
        };
        let items: Vec<(SeatKind, String, u64)> = serde_json::from_str(items).map_err(|e| CellError::host(format!("orgs.items: {e}")))?;
        let status = Status::parse(status).ok_or_else(|| CellError::host(format!("orgs.status {status:?}")))?;
        Ok(Some(Snapshot {
            id: id.clone(),
            customer: customer.clone(),
            status,
            items,
            period_end: sub.period_end,
            trial_end: sub.trial_end,
            cancel_at_period_end: sub.cancel_at_end != 0,
        }))
    }

    /// Pushes `org`'s paid seats' counts to its subscription, if they
    /// differ from what Stripe has, and writes what Stripe answers. An
    /// org that pays for nothing now has nothing to push: it buys again.
    async fn push_quantities(&self, org: &str, key: &str) -> CellResult<()> {
        let sub = self.org_sub(org)?;
        let Some(snapshot) = self.snapshot_of(&sub)? else { return Ok(()) };
        if !snapshot.status.is_good() {
            return Ok(());
        }
        let want = self.paid_counts(org)?;
        if want.total() == 0 {
            return Ok(());
        }
        let changes = core_stripe::item_changes(&snapshot, want);
        if changes.is_empty() {
            return Ok(());
        }
        let stripe = crate::stripe::client(&self.env, self.cfg).await?;
        let mut prices = vec![];
        for kind in changes.iter().filter_map(|c| match c {
            core_stripe::ItemChange::Add { kind, .. } => Some(*kind),
            _ => None,
        }) {
            let lookup = core_stripe::lookup_key(kind);
            let found: List<Price> = stripe.get(&format!("/v1/prices?lookup_keys[]={lookup}&active=true")).await?;
            let p = found.data.into_iter().next().ok_or_else(|| CellError::host(format!("Stripe has no active price {lookup}")))?;
            prices.push((kind, p.id));
        }
        let form = core_stripe::items_form(&changes, |k| prices.iter().find(|(kind, _)| *kind == k).map(|(_, id)| id.clone()).unwrap_or_default());
        let answered: core_stripe::Subscription = stripe.post(&format!("/v1/subscriptions/{}", snapshot.id), &form, key).await?;
        let copy: SubscriptionCopy = answered.snapshot().map_err(CellError::host)?.into();
        self.apply_subscription_copy(org, &copy, None)?;
        Ok(())
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
            let since = SqlStorageValue::Integer(r.since);
            let key = format!("fragment-qty-{}-{}-{}", r.org, r.since, r.tries);
            match self.push_quantities(&r.org, &key).await {
                Ok(()) => self.exec("DELETE FROM quantity_syncs WHERE org = ? AND since = ?", vec![r.org.as_str().into(), since])?,
                Err(e) => {
                    console_error!("{}", json!({ "quantity-sync": r.org, "tries": r.tries + 1, "failed": e.message }));
                    let wait = SYNC_RETRY_MS.saturating_mul(1 << r.tries.clamp(0, 6));
                    self.exec(
                        "UPDATE quantity_syncs SET tries = tries + 1, due = ? WHERE org = ? AND since = ?",
                        vec![SqlStorageValue::Integer(js::now_ms() + wait), r.org.as_str().into(), since],
                    )?;
                }
            }
        }
        self.arm_quantities().await
    }

    pub(super) async fn arm_quantities(&self) -> CellResult<()> {
        #[derive(Deserialize)]
        struct Due {
            due: Option<i64>,
        }
        match self.row::<Due>("SELECT MIN(due) AS due FROM quantity_syncs", vec![])?.and_then(|r| r.due) {
            Some(due) => self.arm_at(due).await,
            None => Ok(()),
        }
    }

    /// A reconcile, or a webhook, that finds Stripe's counts other than the
    /// seats queues a push (the seats are the truth).
    pub(super) fn queue_quantities_if_drifted(&self, org: &str, copy: &SubscriptionCopy) -> CellResult<()> {
        let status = Status::parse(&copy.status).is_some_and(Status::is_good);
        let mut have = Counts::default();
        for (kind, _, q) in &copy.items {
            match kind {
                SeatKind::Seat => have.seat += q,
                SeatKind::SeatAlwaysOn => have.seat_always_on += q,
            }
        }
        let want = self.paid_counts(org)?;
        if status && want.total() > 0 && have != want {
            self.queue_quantities(org)?;
        }
        Ok(())
    }

    /// The registry's routes for an org's admins, or `None`.
    pub(super) async fn seats_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            AddSeat::PATH => reply::<AddSeat>(self.armed_seats(|| self.add_seat(body(bytes)?)).await),
            SeatKindChange::PATH => reply::<SeatKindChange>(self.armed_seats(|| self.seat_kind(body(bytes)?)).await),
            RemoveSeat::PATH => reply::<RemoveSeat>(self.armed_seats(|| self.remove_seat(body(bytes)?)).await),
            AddAdmin::PATH => reply::<AddAdmin>(body(bytes).and_then(|b| self.add_admin(b))),
            RemoveAdmin::PATH => reply::<RemoveAdmin>(body(bytes).and_then(|b| self.remove_admin(b))),
            _ => return None,
        })
    }

    async fn armed_seats<T>(&self, change: impl FnOnce() -> CellResult<T>) -> CellResult<T> {
        let answer = change()?;
        self.arm_syncs().await?;
        self.arm_quantities().await?;
        Ok(answer)
    }
}

