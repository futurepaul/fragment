//! Orgs and seats (docs/billing.md; docs/cloudflare-v1.md, decisions 51 to
//! 53), in the registry beside the people they name. An org is who pays;
//! its members are rows of `org_members`: an admin, a seat's holder, or
//! both, held by a person or pending on an email until someone signs in
//! with it verified (`claim_seats`, from sign-in's turn). A person is in at
//! most one org (`person` is UNIQUE) and holds at most one seat.
//!
//! The registry is the one writer of a seat holder's plan. A change to a
//! seat queues its holder in `plan_syncs`, in the change's own turn; the
//! registry's alarm pushes what `fragment_core::org::desired` says to their
//! ledger (`SetPlan`, `SetSeat` in its own order, `plan_seq`) and their
//! computer (always-on), each push read fresh, so a late one never undoes a
//! newer change. A push that fails is tried again on the alarm; one queued
//! again meanwhile (`since`) stays queued.
//!
//! Comped seats are an operator's (the router checked who asks). Paid
//! seats, and an org's own admins changing them, come with Stripe.

use fragment_core::org::{self, Held, Status, OFFERS_PER_EMAIL_MAX, SEATS_PER_ORG_MAX, SYNC_BATCH, SYNC_RETRY_MS};
use fragment_proto::ledger::{SetPlan, SetSeat};
use fragment_proto::org::{CompSeat, Comped, MySeat, OfferedSeat, OrgMember, OrgRef, OrgView, SeatKind, SeatView};
use serde::Serialize;

use super::calls::{Call, By};
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS orgs (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL, created_by TEXT NOT NULL, status TEXT,
  customer TEXT UNIQUE, subscription TEXT UNIQUE, items TEXT, period_end INTEGER, trial_end INTEGER,
  cancel_at_end INTEGER NOT NULL DEFAULT 0, event_at INTEGER, synced_at INTEGER);
CREATE INDEX IF NOT EXISTS orgs_synced ON orgs (synced_at) WHERE subscription IS NOT NULL;
CREATE TABLE IF NOT EXISTS org_members (
  id TEXT PRIMARY KEY, org TEXT NOT NULL, person TEXT UNIQUE, email TEXT NOT NULL, admin INTEGER NOT NULL,
  seat TEXT, comped INTEGER NOT NULL, sleeps INTEGER NOT NULL, added_at INTEGER NOT NULL, added_by TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS org_members_org ON org_members (org);
CREATE UNIQUE INDEX IF NOT EXISTS org_members_org_email ON org_members (org, email);
CREATE INDEX IF NOT EXISTS org_members_offers ON org_members (email, added_at) WHERE person IS NULL;
CREATE TABLE IF NOT EXISTS plan_syncs (
  person TEXT PRIMARY KEY, since INTEGER NOT NULL, due INTEGER NOT NULL, tries INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS plan_syncs_due ON plan_syncs (due);
CREATE TABLE IF NOT EXISTS plan_seq (
  one INTEGER PRIMARY KEY CHECK (one = 1), seq INTEGER NOT NULL);
";

// ------------------------------------------------------------- calls

/// `POST /orgs/comp`: an operator (`by`, for the record; the router checked
/// them) comps a seat.
#[derive(Serialize, Deserialize)]
pub(crate) struct CompSeatCall {
    pub by: String,
    pub comp: CompSeat,
}

impl Call for CompSeatCall {
    const PATH: &'static str = "/orgs/comp";
    type Answer = Comped;
}

/// `POST /orgs/comp/kind`: an operator changes a comped seat's kind.
#[derive(Serialize, Deserialize)]
pub(crate) struct CompKind {
    pub seat: String,
    pub kind: SeatKind,
}

impl Call for CompKind {
    const PATH: &'static str = "/orgs/comp/kind";
    type Answer = OrgMember;
}

/// `POST /orgs/comp/end`: an operator ends a comp. An admin keeps their
/// place, seatless; anyone else's row goes. Answers the row as it was.
#[derive(Serialize, Deserialize)]
pub(crate) struct EndComp {
    pub seat: String,
}

impl Call for EndComp {
    const PATH: &'static str = "/orgs/comp/end";
    type Answer = OrgMember;
}

/// `POST /orgs/mine`: the asker's seat, org, and the seats offered them.
#[derive(Serialize, Deserialize)]
pub(crate) struct Mine {
    pub by: By,
}

impl Call for Mine {
    const PATH: &'static str = "/orgs/mine";
    type Answer = MySeat;
}

/// `POST /orgs/sleeps`: a seat's holder lets their always-on computer sleep, or not.
#[derive(Serialize, Deserialize)]
pub(crate) struct Sleeps {
    pub by: By,
    pub sleeps: bool,
}

impl Call for Sleeps {
    const PATH: &'static str = "/orgs/sleeps";
    type Answer = MySeat;
}

/// `POST /orgs/view`: an org, to one of its admins (`org` none: theirs), or
/// to an operator (`by` none: the router checked them; `org` named).
#[derive(Serialize, Deserialize)]
pub(crate) struct OrgOf {
    pub by: Option<By>,
    pub org: Option<String>,
}

impl Call for OrgOf {
    const PATH: &'static str = "/orgs/view";
    type Answer = OrgView;
}

/// `POST /orgs/sync`: a person's plan is pushed again, if they hold a seat
/// (their computer was just made, and learns whether it stays awake).
#[derive(Serialize, Deserialize)]
pub(crate) struct SyncSeat {
    pub person: String,
}

impl Call for SyncSeat {
    const PATH: &'static str = "/orgs/sync";
    type Answer = ();
}

// ------------------------------------------------------------- rows

/// `org_members`, read whole.
#[derive(Deserialize)]
pub(super) struct MemberRow {
    pub(super) id: String,
    pub(super) org: String,
    pub(super) person: Option<String>,
    pub(super) email: String,
    pub(super) admin: i64,
    pub(super) seat: Option<String>,
    pub(super) comped: i64,
    pub(super) sleeps: i64,
    pub(super) added_at: i64,
}

pub(super) const MEMBER_COLUMNS: &str = "m.id, m.org, m.person, m.email, m.admin, m.seat, m.comped, m.sleeps, m.added_at";

impl MemberRow {
    pub(super) fn seat(&self) -> CellResult<Option<SeatKind>> {
        self.seat.as_deref().map(|s| SeatKind::parse(s).ok_or_else(|| CellError::host(format!("org_members.seat of {} is {s:?}", self.id)))).transpose()
    }

    pub(super) fn wire(&self, email: Option<String>) -> CellResult<OrgMember> {
        Ok(OrgMember {
            id: self.id.clone(),
            person: self.person.clone(),
            email: email.unwrap_or_else(|| self.email.clone()),
            admin: self.admin != 0,
            seat: self.seat()?,
            comped: self.comped != 0,
            added_at: self.added_at,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct OrgRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) created_at: i64,
    pub(super) status: Option<String>,
}

impl OrgRow {
    pub(super) fn status(&self) -> CellResult<Option<Status>> {
        self.status.as_deref().map(|s| Status::parse(s).ok_or_else(|| CellError::host(format!("orgs.status of {} is {s:?}", self.id)))).transpose()
    }

    pub(super) fn reference(&self) -> OrgRef {
        OrgRef { id: self.id.clone(), name: self.name.clone() }
    }
}

#[derive(Deserialize)]
struct SyncRow {
    person: String,
    since: i64,
    tries: i64,
}

#[derive(Deserialize)]
struct DueRow {
    due: Option<i64>,
}

#[derive(Deserialize)]
struct SeqRow {
    seq: i64,
}

pub(super) fn fresh_id(prefix: &str) -> String {
    format!("{prefix}{}", hex::encode(js::random_bytes::<8>()))
}

pub(super) fn not_found(m: impl Into<String>) -> CellError {
    CellError::new(ErrorCode::NotFound, m)
}

impl RegistryCell {
    pub(super) fn member_of(&self, person: &str) -> CellResult<Option<MemberRow>> {
        self.row(&format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.person = ?"), vec![person.into()])
    }

    pub(super) fn member(&self, id: &str) -> CellResult<MemberRow> {
        if !org::valid_member_id(id) {
            return Err(not_found(format!("no seat {id}")));
        }
        self.row(&format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.id = ?"), vec![id.into()])?.ok_or_else(|| not_found(format!("no seat {id}")))
    }

    /// When an org's trial ends (seconds), as its subscription says.
    fn trial_end_of(&self, org: &str) -> CellResult<Option<i64>> {
        #[derive(Deserialize)]
        struct Row {
            trial_end: Option<i64>,
        }
        Ok(self.row::<Row>("SELECT trial_end FROM orgs WHERE id = ?", vec![org.into()])?.and_then(|r| r.trial_end))
    }

    pub(super) fn org_row(&self, id: &str) -> CellResult<Option<OrgRow>> {
        self.row::<OrgRow>("SELECT id, name, created_at, status FROM orgs WHERE id = ?", vec![id.into()])
    }

    /// The org a row names: missing, the rows contradict each other.
    pub(super) fn stored_org(&self, id: &str) -> CellResult<OrgRow> {
        self.org_row(id)?.ok_or_else(|| CellError::host(format!("org_members names a missing org {id}")))
    }

    /// The person a verified email names, if one signs in with it.
    pub(super) fn person_by_email(&self, email: &str) -> CellResult<Option<String>> {
        Ok(self.row::<HolderRow>("SELECT identity FROM subjects WHERE email = ? LIMIT 1", vec![email.into()])?.map(|r| r.identity))
    }

    /// Queues `person`'s plan to be pushed (again), in the caller's turn.
    pub(super) fn queue_sync(&self, person: &str) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO plan_syncs (person, since, due, tries) VALUES (?, ?, ?, 0)
             ON CONFLICT (person) DO UPDATE SET since = excluded.since, due = excluded.due, tries = 0",
            vec![person.into(), now.clone(), now],
        )
    }

    /// Arms the alarm for the earliest queued push, unless it is armed
    /// sooner already. After the turn that queued it: a failure here
    /// leaves the push queued for the alarm's next run.
    pub(super) async fn arm_syncs(&self) -> CellResult<()> {
        let due = self.row::<DueRow>("SELECT MIN(due) AS due FROM plan_syncs", vec![])?.and_then(|r| r.due);
        match due {
            Some(due) => self.arm_at(due).await,
            None => Ok(()),
        }
    }

    /// Arms the alarm for `due`, unless it is armed sooner already.
    pub(super) async fn arm_at(&self, due: i64) -> CellResult<()> {
        let storage = self.state.storage();
        match storage.get_alarm().await? {
            Some(armed) if armed <= due => Ok(()),
            _ => Ok(storage.set_alarm(super::signin::alarm_at(due)).await?),
        }
    }

    pub(super) fn check_seats_room(&self, org: &str) -> CellResult<()> {
        if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND seat IS NOT NULL", vec![org.into()])? >= SEATS_PER_ORG_MAX {
            return Err(CellError::invalid(format!("an org holds at most {SEATS_PER_ORG_MAX} seats")));
        }
        Ok(())
    }

    pub(super) fn new_org(&self, name: &str, by: &str) -> CellResult<OrgRow> {
        let name = org::org_name(name).ok_or_else(|| CellError::invalid("an org's name is 1-320 bytes, no control characters"))?;
        let row = OrgRow { id: fresh_id("org-"), name, created_at: js::now_ms(), status: None };
        self.exec(
            "INSERT INTO orgs (id, name, created_at, created_by, status) VALUES (?, ?, ?, ?, NULL)",
            vec![row.id.as_str().into(), row.name.as_str().into(), SqlStorageValue::Integer(row.created_at), by.into()],
        )?;
        Ok(row)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn insert_member(&self, org: &str, person: Option<&str>, email: &str, admin: bool, seat: Option<SeatKind>, comped: bool, by: &str) -> CellResult<MemberRow> {
        let row = MemberRow {
            id: fresh_id("mem-"),
            org: org.to_string(),
            person: person.map(str::to_string),
            email: email.to_string(),
            admin: i64::from(admin),
            seat: seat.map(|k| k.as_str().to_string()),
            comped: i64::from(comped),
            sleeps: 0,
            added_at: js::now_ms(),
        };
        self.exec(
            "INSERT INTO org_members (id, org, person, email, admin, seat, comped, sleeps, added_at, added_by) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
            vec![
                row.id.as_str().into(),
                org.into(),
                person.map_or(SqlStorageValue::Null, |p| p.into()),
                email.into(),
                SqlStorageValue::Integer(row.admin),
                seat.map_or(SqlStorageValue::Null, |k| k.as_str().into()),
                SqlStorageValue::Integer(row.comped),
                SqlStorageValue::Integer(row.added_at),
                by.into(),
            ],
        )?;
        Ok(row)
    }

    /// An operator's comp: in `org` when named, else in the org the email's
    /// person is in, else in a new org of one, named by the email, whose
    /// seat's holder is its admin. A person who holds a seat already is
    /// refused, unless it is this same comp (a replay answers it again).
    fn comp_seat(&self, b: CompSeatCall) -> CellResult<Comped> {
        let email = super::signin::email_of(&b.comp.email).ok_or_else(|| CellError::invalid(format!("{:?} is not an email", b.comp.email)))?;
        let kind = b.comp.kind;
        let named = match b.comp.org.as_deref() {
            Some(id) if org::valid_org_id(id) => Some(self.org_row(id)?.ok_or_else(|| not_found(format!("no org {id}")))?),
            Some(id) => return Err(not_found(format!("no org {id}"))),
            None => None,
        };
        let person = self.person_by_email(&email)?;
        if let Some(p) = &person {
            self.not_wiping_id(p)?;
        }
        // whatever row already names them: theirs, or one pending on the email
        let existing = match &person {
            Some(p) => self.member_of(p)?,
            None => match &named {
                Some(o) => self.row(&format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.org = ? AND m.email = ?"), vec![o.id.as_str().into(), email.as_str().into()])?,
                None => self.row(
                    &format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.person IS NULL AND m.email = ? AND m.seat IS NOT NULL AND m.comped = 1 ORDER BY m.added_at LIMIT 1"),
                    vec![email.as_str().into()],
                )?,
            },
        };
        if let Some(m) = existing {
            if named.as_ref().is_some_and(|o| o.id != m.org) {
                return Err(conflict(format!("{email} is in another org already")));
            }
            let org_row = self.stored_org(&m.org)?;
            match m.seat()? {
                Some(k) if m.comped != 0 && k == kind => return Ok(Comped { seat: m.wire(None)?, org: org_row.reference(), created: false, mailed: false }),
                Some(_) => return Err(conflict(format!("{email} holds a seat already: change its kind, or end it first"))),
                None => {
                    // an admin without a seat takes the comped one
                    self.check_seats_room(&m.org)?;
                    self.exec("UPDATE org_members SET seat = ?, comped = 1 WHERE id = ?", vec![kind.as_str().into(), m.id.as_str().into()])?;
                    if let Some(p) = &m.person {
                        self.queue_sync(p)?;
                    }
                    let m = self.member(&m.id)?;
                    return Ok(Comped { seat: m.wire(None)?, org: org_row.reference(), created: true, mailed: false });
                }
            }
        }
        if person.is_none() && self.count("SELECT COUNT(*) AS n FROM org_members WHERE person IS NULL AND email = ?", vec![email.as_str().into()])? >= OFFERS_PER_EMAIL_MAX {
            return Err(CellError::invalid(format!("{OFFERS_PER_EMAIL_MAX} orgs have offered {email} a seat already")));
        }
        let (org_row, admin) = match named {
            Some(o) => (o, false),
            None => (self.new_org(&email, &b.by)?, true),
        };
        self.check_seats_room(&org_row.id)?;
        let m = self.insert_member(&org_row.id, person.as_deref(), &email, admin, Some(kind), true, &b.by)?;
        if let Some(p) = &person {
            self.queue_sync(p)?;
        }
        Ok(Comped { seat: m.wire(None)?, org: org_row.reference(), created: true, mailed: false })
    }

    /// A comped seat, by its row's id: refused for a paid one (Stripe's).
    fn comped(&self, id: &str) -> CellResult<(MemberRow, SeatKind)> {
        let m = self.member(id)?;
        match m.seat()? {
            Some(k) if m.comped != 0 => Ok((m, k)),
            Some(_) => Err(CellError::invalid(format!("{id} is a paid seat: its org's admins change it"))),
            None => Err(not_found(format!("no seat {id}"))),
        }
    }

    fn comp_kind(&self, b: CompKind) -> CellResult<OrgMember> {
        let (m, kind) = self.comped(&b.seat)?;
        if kind != b.kind {
            self.exec("UPDATE org_members SET seat = ? WHERE id = ?", vec![b.kind.as_str().into(), m.id.as_str().into()])?;
            if let Some(p) = &m.person {
                self.queue_sync(p)?;
            }
        }
        self.member(&m.id)?.wire(None)
    }

    fn end_comp(&self, b: EndComp) -> CellResult<OrgMember> {
        let (m, _) = self.comped(&b.seat)?;
        if m.admin != 0 {
            self.exec("UPDATE org_members SET seat = NULL, comped = 0, sleeps = 0 WHERE id = ?", vec![m.id.as_str().into()])?;
        } else {
            self.exec("DELETE FROM org_members WHERE id = ?", vec![m.id.as_str().into()])?;
        }
        if let Some(p) = &m.person {
            self.queue_sync(p)?;
        }
        m.wire(None)
    }

    /// Sign-in's turn: a person not yet in an org takes the oldest seat (or
    /// admin's place) offered to the email they signed in with. Answers
    /// whether a push was queued.
    pub(super) fn claim_seats(&self, person: &str, email: &str) -> CellResult<bool> {
        if self.member_of(person)?.is_some() {
            return Ok(false);
        }
        let offered = self.row::<MemberRow>(
            &format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.person IS NULL AND m.email = ? ORDER BY m.added_at, m.id LIMIT 1"),
            vec![email.into()],
        )?;
        let Some(m) = offered else { return Ok(false) };
        self.exec("UPDATE org_members SET person = ? WHERE id = ?", vec![person.into(), m.id.as_str().into()])?;
        if m.seat.is_some() {
            self.queue_sync(person)?;
        }
        Ok(m.seat.is_some())
    }

    /// The person a call names, a person (an agent holds no seat).
    pub(super) fn person_by(&self, by: &By) -> CellResult<Identity> {
        let who = self.by(by)?;
        if who.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "seats are people's: an agent's owner holds one"));
        }
        Ok(who)
    }

    fn mine(&self, b: Mine) -> CellResult<MySeat> {
        let who = self.person_by(&b.by)?;
        self.my_seat(&who.id)
    }

    pub(super) fn my_seat(&self, person: &str) -> CellResult<MySeat> {
        let m = self.member_of(person)?;
        let (seat, org_ref, admin) = match &m {
            None => (None, None, false),
            Some(m) => {
                let o = self.stored_org(&m.org)?;
                let seat = match m.seat()? {
                    Some(kind) => {
                        let held = Held { kind, comped: m.comped != 0, status: o.status()?, sleeps: m.sleeps != 0 };
                        let trial_ends = match (held.comped, held.status) {
                            (false, Some(Status::Trialing)) => self.trial_end_of(&o.id)?,
                            _ => None,
                        };
                        Some(SeatView { id: m.id.clone(), org: o.reference(), kind, comped: held.comped, good: held.good(), sleeps: held.sleeps, admin: m.admin != 0, trial_ends })
                    }
                    None => None,
                };
                (seat, Some(o.reference()), m.admin != 0)
            }
        };
        // bounded: a person signs in with at most SUBJECTS_MAX emails, each offered by at most OFFERS_PER_EMAIL_MAX orgs
        let rows = self.rows::<MemberRow>(
            &format!(
                "SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.person IS NULL AND m.seat IS NOT NULL
                 AND m.email IN (SELECT email FROM subjects WHERE identity = ?) ORDER BY m.added_at, m.id"
            ),
            vec![person.into()],
        )?;
        let mut offered = vec![];
        for r in rows {
            let kind = r.seat()?.ok_or_else(|| CellError::host("an offer selected for its seat has none"))?;
            offered.push(OfferedSeat { id: r.id.clone(), org: self.stored_org(&r.org)?.reference(), kind });
        }
        Ok(MySeat { seat, org: org_ref, admin, offered })
    }

    fn sleeps(&self, b: Sleeps) -> CellResult<MySeat> {
        let who = self.person_by(&b.by)?;
        let m = self.member_of(&who.id)?.filter(|m| m.seat.is_some()).ok_or_else(|| CellError::invalid("you hold no seat"))?;
        if (m.sleeps != 0) != b.sleeps {
            self.exec("UPDATE org_members SET sleeps = ? WHERE id = ?", vec![SqlStorageValue::Integer(i64::from(b.sleeps)), m.id.as_str().into()])?;
            self.queue_sync(&who.id)?;
        }
        self.my_seat(&who.id)
    }

    fn org_of(&self, b: OrgOf) -> CellResult<OrgView> {
        let org_row = match (&b.by, b.org.as_deref()) {
            // an operator's (the router checked them)
            (None, Some(id)) if org::valid_org_id(id) => self.org_row(id)?.ok_or_else(|| not_found(format!("no org {id}")))?,
            (None, Some(id)) => return Err(not_found(format!("no org {id}"))),
            (None, None) => return Err(CellError::invalid("an operator names the org")),
            (Some(by), named) => {
                let who = self.person_by(by)?;
                let m = self.member_of(&who.id)?.filter(|m| m.admin != 0).ok_or_else(|| CellError::new(ErrorCode::Forbidden, "only an org's admins see it"))?;
                if named.is_some_and(|id| id != m.org) {
                    return Err(CellError::new(ErrorCode::Forbidden, "only an org's admins see it"));
                }
                self.stored_org(&m.org)?
            }
        };
        #[derive(Deserialize)]
        struct Row {
            #[serde(flatten)]
            m: MemberRow,
            latest: Option<String>,
        }
        // a held seat shows its holder's latest sign-in's email; bounded: an
        // org holds at most SEATS_PER_ORG_MAX seats and ADMINS_PER_ORG_MAX admins
        let rows = self.rows::<Row>(
            &format!(
                "SELECT {MEMBER_COLUMNS}, (SELECT email FROM subjects WHERE identity = m.person ORDER BY signed_in_at DESC LIMIT 1) AS latest
                 FROM org_members m WHERE m.org = ? ORDER BY m.added_at, m.id"
            ),
            vec![org_row.id.as_str().into()],
        )?;
        let members = rows.into_iter().map(|r| r.m.wire(r.latest)).collect::<CellResult<Vec<_>>>()?;
        Ok(OrgView { id: org_row.id, name: org_row.name, created_at: org_row.created_at, members })
    }

    fn sync_seat(&self, b: SyncSeat) -> CellResult<()> {
        if self.member_of(&b.person)?.is_some_and(|m| m.seat.is_some()) {
            self.queue_sync(&b.person)?;
        }
        Ok(())
    }

    /// What `person`'s ledger and computer are told now.
    fn desired_of(&self, person: &str) -> CellResult<org::Desired> {
        let held = match self.member_of(person)? {
            Some(m) => match m.seat()? {
                Some(kind) => Some(Held { kind, comped: m.comped != 0, status: self.stored_org(&m.org)?.status()?, sleeps: m.sleeps != 0 }),
                None => None,
            },
            None => None,
        };
        Ok(org::desired(held))
    }

    /// The next place in the ledgers' seat order (`fragment_core::org::next_seq`).
    fn take_seq(&self) -> CellResult<u64> {
        let last = self.row::<SeqRow>("SELECT seq FROM plan_seq WHERE one = 1", vec![])?.map_or(0, |r| r.seq.max(0) as u64);
        let seq = org::next_seq(js::now_ms(), last);
        let stored = i64::try_from(seq).map_err(|_| CellError::host("the plans' seq outgrew an i64"))?;
        self.exec("INSERT INTO plan_seq (one, seq) VALUES (1, ?) ON CONFLICT (one) DO UPDATE SET seq = excluded.seq", vec![SqlStorageValue::Integer(stored)])?;
        Ok(seq)
    }

    /// Tells `person`'s ledger and computer what `d` says. Each command's id
    /// is its push's own, so a ledger keeps each once.
    async fn push(&self, person: &str, d: org::Desired, seq: u64) -> CellResult<()> {
        let refused = |e: crate::ledger::LedgerError| CellError::new(e.code, format!("{person}'s ledger: {}", e.message));
        if let Some(plan) = d.plan {
            crate::ledger::ask(&self.env, person, &SetPlan { id: format!("org:{seq}:plan"), plan }).await.map_err(refused)?;
        }
        crate::ledger::ask(&self.env, person, &SetSeat { id: format!("org:{seq}:seat"), seat: d.seat, seq }).await.map_err(refused)?;
        let computer = fragment_core::computer::default_computer_of(person);
        match crate::computer::ask(&self.env, &computer, "computer/always-on", &json!({ "on": d.always_on })).await {
            // no computer yet: it asks for a push when it is made
            Err(e) if e.code == ErrorCode::NotFound => Ok(()),
            answered => answered.map(|_| ()),
        }
    }

    /// The alarm's pushes: a batch of the queued ones that are due, each
    /// read fresh. A pushed one leaves the queue unless it was queued again
    /// meanwhile; a failed one is tried again later. Then the alarm is
    /// armed for the next due one.
    pub(super) async fn sync_alarm(&self) -> CellResult<()> {
        let now = js::now_ms();
        let due = self.rows::<SyncRow>(
            "SELECT person, since, tries FROM plan_syncs WHERE due <= ? ORDER BY due LIMIT ?",
            vec![SqlStorageValue::Integer(now), SqlStorageValue::Integer(SYNC_BATCH.into())],
        )?;
        // bounded: at most SYNC_BATCH pushes a run
        for r in due {
            let d = self.desired_of(&r.person)?;
            let seq = self.take_seq()?;
            let since = SqlStorageValue::Integer(r.since);
            match self.push(&r.person, d, seq).await {
                Ok(()) => self.exec("DELETE FROM plan_syncs WHERE person = ? AND since = ?", vec![r.person.as_str().into(), since])?,
                Err(e) => {
                    console_error!("{}", json!({ "plan-sync": r.person, "tries": r.tries + 1, "failed": e.message }));
                    let wait = SYNC_RETRY_MS.saturating_mul(1 << r.tries.clamp(0, 6));
                    self.exec(
                        "UPDATE plan_syncs SET tries = tries + 1, due = ? WHERE person = ? AND since = ?",
                        vec![SqlStorageValue::Integer(js::now_ms() + wait), r.person.as_str().into(), since],
                    )?;
                }
            }
        }
        self.arm_syncs().await
    }

    /// After a failed run: the next one, a retry's wait from now.
    pub(super) async fn sync_later(&self) -> CellResult<()> {
        Ok(self.state.storage().set_alarm(super::signin::alarm_at(js::now_ms() + SYNC_RETRY_MS)).await?)
    }

    /// The wipe's registry step: the person's rows here go, and an org left
    /// with no one in it goes with them.
    pub(super) fn wipe_orgs(&self, person: &str) -> CellResult<()> {
        let org_id = self.member_of(person)?.map(|m| m.org);
        self.exec("DELETE FROM org_members WHERE person = ?", vec![person.into()])?;
        self.exec("DELETE FROM plan_syncs WHERE person = ?", vec![person.into()])?;
        if let Some(o) = org_id {
            if self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ?", vec![o.as_str().into()])? == 0 {
                self.exec("DELETE FROM orgs WHERE id = ?", vec![o.as_str().into()])?;
            }
        }
        Ok(())
    }

    /// The registry's org routes, or `None` for a path that is not one.
    pub(super) async fn org_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            CompSeatCall::PATH => reply::<CompSeatCall>(self.armed(|| self.comp_seat(body(bytes)?)).await),
            CompKind::PATH => reply::<CompKind>(self.armed(|| self.comp_kind(body(bytes)?)).await),
            EndComp::PATH => reply::<EndComp>(self.armed(|| self.end_comp(body(bytes)?)).await),
            Mine::PATH => reply::<Mine>(body(bytes).and_then(|b| self.mine(b))),
            Sleeps::PATH => reply::<Sleeps>(self.armed(|| self.sleeps(body(bytes)?)).await),
            OrgOf::PATH => reply::<OrgOf>(body(bytes).and_then(|b| self.org_of(b))),
            SyncSeat::PATH => reply::<SyncSeat>(self.armed(|| self.sync_seat(body(bytes)?)).await),
            _ => return None,
        })
    }

    /// A change in one turn, then the alarm armed for what it queued.
    async fn armed<T>(&self, change: impl FnOnce() -> CellResult<T>) -> CellResult<T> {
        let answer = change()?;
        self.arm_syncs().await?;
        Ok(answer)
    }

    /// A person a wipe of whom runs is no one to comp.
    pub(super) fn not_wiping_id(&self, person: &str) -> CellResult<()> {
        if self.wiping(person)? {
            return Err(not_found(format!("{person} is being wiped")));
        }
        Ok(())
    }
}
