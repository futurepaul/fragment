//! The operators' admin (docs/billing.md, "The operator's admin";
//! decision 59), as far as the registry answers it: people and orgs, a
//! page at a time; billing's health (its queues, the copies of Stripe's
//! subscriptions); and the log of what operators did, written in the same
//! turn as each act (`log_admin`). The router checked who asks.

use fragment_core::org::Status;
use fragment_proto::org::{AdminHealth, AdminLog, AdminLogEntry, AdminOrg, AdminOrgs, AdminPeople, AdminPerson, AdminSeat, Failing, OrgRef, SeatKind};
use serde::Serialize;

use super::calls::Call;
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS admin_log (
  n INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, operator TEXT NOT NULL, action TEXT NOT NULL,
  target TEXT NOT NULL, detail TEXT NOT NULL);
";

/// People and orgs a page lists; the log's entries a page shows.
pub(crate) const ADMIN_PAGE: u32 = 100;
/// The log's entries kept (the oldest go).
const ADMIN_LOG_MAX: i64 = 10_000;
/// A log entry's detail, at most.
const DETAIL_MAX_BYTES: usize = 1_000;

/// `POST /admin/people`: people by identity after `after`, their latest
/// email starting with `q` when it is named.
#[derive(Serialize, Deserialize)]
pub(crate) struct People {
    pub q: Option<String>,
    pub after: Option<String>,
}

impl Call for People {
    const PATH: &'static str = "/admin/people";
    type Answer = AdminPeople;
}

/// `POST /admin/person`: one person, as the list shows them.
#[derive(Serialize, Deserialize)]
pub(crate) struct Person {
    pub person: String,
}

impl Call for Person {
    const PATH: &'static str = "/admin/person";
    type Answer = AdminPerson;
}

/// `POST /admin/orgs`: orgs by id after `after`.
#[derive(Serialize, Deserialize)]
pub(crate) struct Orgs {
    pub after: Option<String>,
}

impl Call for Orgs {
    const PATH: &'static str = "/admin/orgs";
    type Answer = AdminOrgs;
}

/// `POST /admin/health`.
#[derive(Serialize, Deserialize)]
pub(crate) struct Health {}

impl Call for Health {
    const PATH: &'static str = "/admin/health";
    type Answer = AdminHealth;
}

/// `POST /admin/log`: the entries before `before`, newest first.
#[derive(Serialize, Deserialize)]
pub(crate) struct Log {
    pub before: Option<u64>,
}

impl Call for Log {
    const PATH: &'static str = "/admin/log";
    type Answer = AdminLog;
}

/// `POST /admin/note`: an act of an operator's done elsewhere (a grant on
/// a ledger, a trial mailed), for the log.
#[derive(Serialize, Deserialize)]
pub(crate) struct Note {
    pub by: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

impl Call for Note {
    const PATH: &'static str = "/admin/note";
    type Answer = ();
}

#[derive(Deserialize)]
struct PersonRow {
    id: String,
    created_at: i64,
    email: Option<String>,
    last_sign_in: Option<i64>,
    member: Option<String>,
    org: Option<String>,
    org_name: Option<String>,
    status: Option<String>,
    seat: Option<String>,
    comped: Option<i64>,
    admin: Option<i64>,
}

const PERSON_SELECT: &str = "SELECT i.id, i.created_at,
  (SELECT email FROM subjects WHERE identity = i.id ORDER BY signed_in_at DESC LIMIT 1) AS email,
  (SELECT MAX(signed_in_at) FROM subjects WHERE identity = i.id) AS last_sign_in,
  m.id AS member, m.org, o.name AS org_name, o.status, m.seat, m.comped, m.admin
  FROM identities i LEFT JOIN org_members m ON m.person = i.id LEFT JOIN orgs o ON o.id = m.org";

impl PersonRow {
    fn wire(self) -> CellResult<AdminPerson> {
        let comped = self.comped.unwrap_or(0) != 0;
        let status = self.status.as_deref().and_then(Status::parse);
        let seat = match (&self.member, self.seat.as_deref()) {
            (Some(id), Some(s)) => {
                let kind = SeatKind::parse(s).ok_or_else(|| CellError::host(format!("org_members.seat of {id} is {s:?}")))?;
                Some(AdminSeat { id: id.clone(), kind, comped, good: comped || status.is_some_and(Status::is_good) })
            }
            _ => None,
        };
        let org = self.org.zip(self.org_name).map(|(id, name)| OrgRef { id, name });
        Ok(AdminPerson { npub: self.id, email: self.email, joined_at: self.created_at, last_sign_in_at: self.last_sign_in, org, admin: self.admin.unwrap_or(0) != 0, seat })
    }
}

impl RegistryCell {
    /// A line in the operators' log, in the act's own turn; the oldest
    /// past `ADMIN_LOG_MAX` go.
    pub(super) fn log_admin(&self, operator: &str, action: &str, target: &str, detail: &str) -> CellResult<()> {
        let detail: String = detail.chars().take(DETAIL_MAX_BYTES).collect();
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let n = self
            .row::<N>(
                "INSERT INTO admin_log (at, operator, action, target, detail) VALUES (?, ?, ?, ?, ?) RETURNING n",
                vec![SqlStorageValue::Integer(js::now_ms()), operator.into(), action.into(), target.into(), detail.into()],
            )?
            .ok_or_else(|| CellError::host("an admin log's insert answered no row"))?
            .n;
        self.exec("DELETE FROM admin_log WHERE n <= ?", vec![SqlStorageValue::Integer(n - ADMIN_LOG_MAX)])
    }

    fn people(&self, b: People) -> CellResult<AdminPeople> {
        let after = b.after.unwrap_or_default();
        let limit = SqlStorageValue::Integer(ADMIN_PAGE.into());
        let rows = match b.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => self.rows::<PersonRow>(
                &format!("{PERSON_SELECT} WHERE i.kind = 'person' AND i.id > ? AND i.id IN (SELECT identity FROM subjects WHERE email LIKE ? ESCAPE '\\') ORDER BY i.id LIMIT ?"),
                vec![after.as_str().into(), fragment_core::org::like_prefix(q).into(), limit],
            )?,
            None => self.rows::<PersonRow>(&format!("{PERSON_SELECT} WHERE i.kind = 'person' AND i.id > ? ORDER BY i.id LIMIT ?"), vec![after.as_str().into(), limit])?,
        };
        assert!(rows.len() <= ADMIN_PAGE as usize, "a page is bounded");
        let next = (rows.len() == ADMIN_PAGE as usize).then(|| rows.last().map(|r| r.id.clone())).flatten();
        Ok(AdminPeople { people: rows.into_iter().map(PersonRow::wire).collect::<CellResult<_>>()?, next })
    }

    fn person(&self, b: Person) -> CellResult<AdminPerson> {
        self.row::<PersonRow>(&format!("{PERSON_SELECT} WHERE i.kind = 'person' AND i.id = ?"), vec![b.person.as_str().into()])?
            .ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no person {}", b.person)))?
            .wire()
    }

    fn orgs_page(&self, b: Orgs) -> CellResult<AdminOrgs> {
        #[derive(Deserialize)]
        struct Row {
            id: String,
            name: String,
            created_at: i64,
            status: Option<String>,
            customer: Option<String>,
            subscription: Option<String>,
            period_end: Option<i64>,
            cancel_at_end: i64,
            admins: u64,
            paid_seats: u64,
            comped_seats: u64,
            pending: u64,
        }
        let rows = self.rows::<Row>(
            "SELECT o.id, o.name, o.created_at, o.status, o.customer, o.subscription, o.period_end, o.cancel_at_end,
               (SELECT COUNT(*) FROM org_members WHERE org = o.id AND admin = 1) AS admins,
               (SELECT COUNT(*) FROM org_members WHERE org = o.id AND seat IS NOT NULL AND comped = 0) AS paid_seats,
               (SELECT COUNT(*) FROM org_members WHERE org = o.id AND seat IS NOT NULL AND comped = 1) AS comped_seats,
               (SELECT COUNT(*) FROM org_members WHERE org = o.id AND person IS NULL) AS pending
             FROM orgs o WHERE o.id > ? ORDER BY o.id LIMIT ?",
            vec![b.after.unwrap_or_default().into(), SqlStorageValue::Integer(ADMIN_PAGE.into())],
        )?;
        let next = (rows.len() == ADMIN_PAGE as usize).then(|| rows.last().map(|r| r.id.clone())).flatten();
        let orgs = rows
            .into_iter()
            .map(|r| AdminOrg {
                id: r.id,
                name: r.name,
                created_at: r.created_at,
                status: r.status,
                customer: r.customer,
                subscription: r.subscription,
                period_end: r.period_end,
                cancel_at_end: r.cancel_at_end != 0,
                admins: r.admins,
                paid_seats: r.paid_seats,
                comped_seats: r.comped_seats,
                pending: r.pending,
            })
            .collect();
        Ok(AdminOrgs { orgs, next })
    }

    fn health(&self) -> CellResult<AdminHealth> {
        #[derive(Deserialize)]
        struct Row {
            paying: u64,
            lapsed: u64,
            seats_paid: u64,
            seats_comped: u64,
            plans: u64,
            quantities: u64,
            last_event: Option<i64>,
            oldest: Option<i64>,
        }
        let r = self
            .row::<Row>(
                "SELECT (SELECT COUNT(*) FROM orgs WHERE status IN ('trialing', 'active', 'past_due')) AS paying,
                        (SELECT COUNT(*) FROM orgs WHERE subscription IS NOT NULL AND status NOT IN ('trialing', 'active', 'past_due')) AS lapsed,
                        (SELECT COUNT(*) FROM org_members WHERE seat IS NOT NULL AND comped = 0) AS seats_paid,
                        (SELECT COUNT(*) FROM org_members WHERE seat IS NOT NULL AND comped = 1) AS seats_comped,
                        (SELECT COUNT(*) FROM plan_syncs) AS plans,
                        (SELECT COUNT(*) FROM quantity_syncs) AS quantities,
                        (SELECT MAX(event_at) FROM orgs) AS last_event,
                        (SELECT MIN(synced_at) FROM orgs WHERE subscription IS NOT NULL) AS oldest",
                vec![],
            )?
            .ok_or_else(|| CellError::host("COUNT answered no row"))?;
        #[derive(Deserialize)]
        struct F {
            queue: String,
            target: String,
            tries: u64,
            due: i64,
        }
        let failing = self
            .rows::<F>(
                "SELECT 'plan' AS queue, person AS target, tries, due FROM plan_syncs WHERE tries > 0
                 UNION ALL SELECT 'quantity' AS queue, org AS target, tries, due FROM quantity_syncs WHERE tries > 0
                 ORDER BY tries DESC LIMIT 50",
                vec![],
            )?
            .into_iter()
            .map(|f| Failing { queue: f.queue, target: f.target, tries: f.tries, due: f.due })
            .collect();
        Ok(AdminHealth {
            orgs_paying: r.paying,
            orgs_lapsed: r.lapsed,
            seats_paid: r.seats_paid,
            seats_comped: r.seats_comped,
            plan_pushes_queued: r.plans,
            quantity_pushes_queued: r.quantities,
            failing,
            last_event_at: r.last_event,
            oldest_copy_at: r.oldest,
        })
    }

    fn log_page(&self, b: Log) -> CellResult<AdminLog> {
        #[derive(Deserialize)]
        struct Row {
            n: u64,
            at: i64,
            operator: String,
            action: String,
            target: String,
            detail: String,
        }
        let before = b.before.map_or(i64::MAX, |n| n as i64);
        let rows = self.rows::<Row>(
            "SELECT n, at, operator, action, target, detail FROM admin_log WHERE n < ? ORDER BY n DESC LIMIT ?",
            vec![SqlStorageValue::Integer(before), SqlStorageValue::Integer(ADMIN_PAGE.into())],
        )?;
        let next = (rows.len() == ADMIN_PAGE as usize).then(|| rows.last().map(|r| r.n)).flatten();
        let entries = rows.into_iter().map(|r| AdminLogEntry { n: r.n, at: r.at, operator: r.operator, action: r.action, target: r.target, detail: r.detail }).collect();
        Ok(AdminLog { entries, next })
    }

    pub(super) fn admin_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            People::PATH => reply::<People>(body(bytes).and_then(|b| self.people(b))),
            Person::PATH => reply::<Person>(body(bytes).and_then(|b| self.person(b))),
            Orgs::PATH => reply::<Orgs>(body(bytes).and_then(|b| self.orgs_page(b))),
            Health::PATH => reply::<Health>(self.health()),
            Log::PATH => reply::<Log>(body(bytes).and_then(|b| self.log_page(b))),
            Note::PATH => reply::<Note>(body::<Note>(bytes).and_then(|b| self.log_admin(&b.by, &b.action, &b.target, &b.detail))),
            _ => return None,
        })
    }
}
