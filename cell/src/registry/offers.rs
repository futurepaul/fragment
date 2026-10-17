//! Offers (docs/billing.md, "The model"; the security audit's H1): nobody
//! joins an org, or takes an admin's place in one, without their own yes.
//! An org's admin offers a seat, an admin's place, or both, to an email
//! (seats.rs), one of its own members' too; the row waits on it with no
//! person (`org_members.person` NULL), and a paid seat offered bills from
//! the offer (decision 52). Whoever signs in with that email, verified,
//! sees it (`GET /api/seat`'s `offered`, with its org's name and who
//! offered it), and accepts it while its org pays (an operator's comp
//! stands alone), which makes them its person, or merges it into their own
//! row in that org, or declines it, which removes it. Signing in attaches
//! nothing.
//!
//! One function writes a row's person, `enroll`, and each caller names the
//! yes it acts on (`Yes`): the person's accept; their own Checkout, paid,
//! whose org it makes (billing.rs); or an operator's comp of a person
//! (orgs.rs; operators are trusted). An org's members given a seat need no
//! offer: they are in it by their yes, and leave it by their own (`leave`,
//! or stepping down as its admin).
//!
//! Accepting while in another org is a move (`fragment_core::org::moving`):
//! alone there (offers aside), that org ends with them, its offers
//! withdrawn; else they go, unless they are its last admin while other
//! people are in it. Their old seat goes with their old row, and a card's
//! owner who goes stops paying: its subscription is canceled at once.

use fragment_core::org::{self, Going, Place, OFFERS_PER_EMAIL_MAX, OFFER_MAILS_PER_ORG_DAILY_MAX};
use fragment_proto::org::{MySeat, OfferedSeat, SeatKind};
use serde::Serialize;

use super::calls::{By, Call};
use super::orgs::{fresh_id, not_found, MemberRow, MEMBER_COLUMNS};
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS offer_mails (
  org TEXT NOT NULL, day INTEGER NOT NULL, mailed INTEGER NOT NULL, PRIMARY KEY (org, day));
CREATE TABLE IF NOT EXISTS offer_answers (
  offer TEXT PRIMARY KEY, person TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS offer_answers_person ON offer_answers (person);
";

const DAY_MS: i64 = 24 * 3600 * 1000;
/// How long a merged offer's answer is kept for a replay of its accept.
const ANSWERS_KEPT_MS: i64 = 7 * DAY_MS;

/// `POST /orgs/offers/accept`: the asker accepts an offer to an email of
/// theirs. Answers their seat as it is then; accepted already, the same.
#[derive(Serialize, Deserialize)]
pub(crate) struct AcceptOffer {
    pub by: By,
    pub offer: String,
}

impl Call for AcceptOffer {
    const PATH: &'static str = "/orgs/offers/accept";
    type Answer = MySeat;
}

/// `POST /orgs/offers/decline`: the asker declines an offer to an email of
/// theirs: it goes. One that is none (declined already) is 404.
#[derive(Serialize, Deserialize)]
pub(crate) struct DeclineOffer {
    pub by: By,
    pub offer: String,
}

impl Call for DeclineOffer {
    const PATH: &'static str = "/orgs/offers/decline";
    type Answer = MySeat;
}

/// `POST /orgs/leave`: a seat's holder who is no admin leaves their org.
#[derive(Serialize, Deserialize)]
pub(crate) struct LeaveOrg {
    pub by: By,
}

impl Call for LeaveOrg {
    const PATH: &'static str = "/orgs/leave";
    type Answer = MySeat;
}

/// The yes a person's place in an org stands on: the only reasons
/// `enroll` writes a row's person.
pub(super) enum Yes<'a> {
    /// They accepted its offer (`accept`).
    Accepted(&'a str),
    /// Their own Checkout, paid, made its org (billing.rs `apply_checkout`).
    Bought(&'a str),
    /// An operator comped them its seat (orgs.rs `comp_seat`).
    Comped(&'a str),
}

impl Yes<'_> {
    fn person(&self) -> &str {
        match self {
            Yes::Accepted(p) | Yes::Bought(p) | Yes::Comped(p) => p,
        }
    }

    fn why(&self) -> &'static str {
        match self {
            Yes::Accepted(_) => "accepted",
            Yes::Bought(_) => "bought",
            Yes::Comped(_) => "comped",
        }
    }
}

#[derive(Deserialize)]
struct EmailRow {
    email: String,
}

impl RegistryCell {
    /// An offer of `org`'s to `email`: a seat (`seat`), an admin's place,
    /// or both; no one's until its yes (`enroll`).
    pub(super) fn insert_offer(&self, org: &str, email: &str, admin: bool, seat: Option<SeatKind>, comped: bool, by: &str) -> CellResult<MemberRow> {
        assert!(admin || seat.is_some(), "an offer is of a seat, an admin's place, or both");
        assert!(!comped || seat.is_some(), "only a seat is comped");
        let row = MemberRow {
            id: fresh_id("mem-"),
            org: org.to_string(),
            person: None,
            email: email.to_string(),
            admin: i64::from(admin),
            seat: seat.map(|k| k.as_str().to_string()),
            comped: i64::from(comped),
            sleeps: 0,
            added_at: js::now_ms(),
        };
        self.exec(
            "INSERT INTO org_members (id, org, person, email, admin, seat, comped, sleeps, added_at, added_by) VALUES (?, ?, NULL, ?, ?, ?, ?, 0, ?, ?)",
            vec![
                row.id.as_str().into(),
                org.into(),
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

    /// The one writer of a row's person: `offer`'s becomes the person `yes`
    /// names, in the caller's turn. They are in no org (the caller moved
    /// them first: `person` is UNIQUE besides), and the row waits on no one.
    pub(super) fn enroll(&self, offer: &MemberRow, yes: Yes) -> CellResult<MemberRow> {
        let person = yes.person();
        assert!(offer.person.is_none(), "an offer has no person until its yes");
        if self.member_of(person)?.is_some() {
            return Err(CellError::host(format!("{person} is enrolled in {} while in another org", offer.org)));
        }
        self.exec("UPDATE org_members SET person = ? WHERE id = ? AND person IS NULL", vec![person.into(), offer.id.as_str().into()])?;
        let m = self.member(&offer.id)?;
        assert_eq!(m.person.as_deref(), Some(person), "an enrolled row is its yes's person's");
        if m.seat.is_some() {
            self.queue_sync(person)?;
        }
        console_log!("{}", json!({ "enrolled": m.id, "org": m.org, "why": yes.why() }));
        Ok(m)
    }

    /// The emails `person` signs in with (at most `SUBJECTS_MAX`).
    fn emails_of(&self, person: &str) -> CellResult<Vec<String>> {
        Ok(self.rows::<EmailRow>("SELECT DISTINCT email FROM subjects WHERE identity = ?", vec![person.into()])?.into_iter().map(|r| r.email).collect())
    }

    /// The offers to the emails `person` signs in with, oldest first, each
    /// with its org and who made it. Bounded: at most `SUBJECTS_MAX` emails,
    /// each offered by at most `OFFERS_PER_EMAIL_MAX` orgs.
    pub(super) fn offers_to(&self, person: &str) -> CellResult<Vec<OfferedSeat>> {
        #[derive(Deserialize)]
        struct Row {
            #[serde(flatten)]
            m: MemberRow,
            from: Option<String>,
        }
        let rows = self.rows::<Row>(
            &format!(
                "SELECT {MEMBER_COLUMNS}, (SELECT s.email FROM subjects s WHERE s.identity = m.added_by ORDER BY s.signed_in_at DESC LIMIT 1) AS \"from\"
                 FROM org_members m WHERE m.person IS NULL AND m.email IN (SELECT email FROM subjects WHERE identity = ?) ORDER BY m.added_at, m.id"
            ),
            vec![person.into()],
        )?;
        let mut offered = Vec::with_capacity(rows.len());
        for r in rows {
            let kind = r.m.seat()?;
            if kind.is_none() && r.m.admin == 0 {
                return Err(CellError::host(format!("the offer {} is of nothing", r.m.id)));
            }
            offered.push(OfferedSeat { id: r.m.id.clone(), org: self.stored_org(&r.m.org)?.reference(), kind, admin: r.m.admin != 0, comped: r.m.comped != 0, from: r.from, email: r.m.email });
        }
        Ok(offered)
    }

    /// The offer of `org`'s waiting on `email`, if any (an org offers an
    /// email once: `org_members_org_email`).
    pub(super) fn offer_in(&self, org: &str, email: &str) -> CellResult<Option<MemberRow>> {
        self.row(&format!("SELECT {MEMBER_COLUMNS} FROM org_members m WHERE m.org = ? AND m.email = ? AND m.person IS NULL"), vec![org.into(), email.into()])
    }

    /// Refuses a new offer to `email` when `OFFERS_PER_EMAIL_MAX` orgs
    /// offer it something already.
    pub(super) fn check_offer_room(&self, email: &str) -> CellResult<()> {
        if self.count("SELECT COUNT(*) AS n FROM org_members WHERE person IS NULL AND email = ?", vec![email.into()])? >= OFFERS_PER_EMAIL_MAX {
            return Err(CellError::invalid(format!("{OFFERS_PER_EMAIL_MAX} orgs offer {email} something already: it accepts or declines one first")));
        }
        Ok(())
    }

    /// Refuses an offer's mail past its org's day (UTC) of them (429).
    /// Read only: the caller counts it (`count_offer_mail`) once its offer
    /// is written, so a refusal anywhere between counts nothing.
    pub(super) fn offer_mail_room(&self, org: &str) -> CellResult<()> {
        let day = js::now_ms() / DAY_MS;
        let mailed = self.count("SELECT COALESCE(SUM(mailed), 0) AS n FROM offer_mails WHERE org = ? AND day = ?", vec![org.into(), SqlStorageValue::Integer(day)])?;
        if mailed >= OFFER_MAILS_PER_ORG_DAILY_MAX {
            return Err(CellError::new(ErrorCode::RateLimited, format!("your org has mailed {OFFER_MAILS_PER_ORG_DAILY_MAX} offers today, the most a day: offer more tomorrow")));
        }
        Ok(())
    }

    /// Counts an offer's mail in its org's day, after `offer_mail_room` and
    /// the offer's write. Days before yesterday are forgotten.
    pub(super) fn count_offer_mail(&self, org: &str) -> CellResult<()> {
        let day = js::now_ms() / DAY_MS;
        self.exec(
            "INSERT INTO offer_mails (org, day, mailed) VALUES (?, ?, 1) ON CONFLICT (org, day) DO UPDATE SET mailed = mailed + 1",
            vec![org.into(), SqlStorageValue::Integer(day)],
        )?;
        self.exec("DELETE FROM offer_mails WHERE day < ?", vec![SqlStorageValue::Integer(day - 1)])
    }

    /// A member's place in their org, as they go from it: its people (not
    /// its offers, no one's yet), and whether their card pays for it.
    pub(super) fn place_of(&self, m: &MemberRow) -> CellResult<Place> {
        let person = m.person.as_deref().ok_or_else(|| CellError::host(format!("{} is an offer, not a member's place", m.id)))?;
        let admins = self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND admin = 1 AND person IS NOT NULL", vec![m.org.as_str().into()])?;
        let others = self.count("SELECT COUNT(*) AS n FROM org_members WHERE org = ? AND id != ? AND person IS NOT NULL", vec![m.org.as_str().into(), m.id.as_str().into()])?;
        let place = Place { admin: m.admin != 0, admins, others, pays: self.pays_for(&m.org, person)? };
        assert!(!place.admin || place.admins >= 1, "an admin who said yes is one of its admins");
        Ok(place)
    }

    /// `person` goes from their org (`m`, their row) for another's offer
    /// (`fragment_core::org::moving`), unless that leaves its people with no
    /// admin: their row goes, or, alone there, every row of it (its offers
    /// withdrawn); their card stops paying for it at once; and its count is
    /// pushed (an org left with no one is dropped once nothing of it bills).
    fn go_from(&self, m: &MemberRow) -> CellResult<()> {
        let from = self.stored_org(&m.org)?;
        let place = self.place_of(m)?;
        let gone = org::moving(place).map_err(|stuck| conflict(stuck.message(&from.name)))?;
        if place.pays {
            self.cancel_for_payer(&m.org)?;
        }
        match gone.going {
            Going::Ends => self.exec("DELETE FROM org_members WHERE org = ?", vec![m.org.as_str().into()])?,
            Going::Leaves => self.exec("DELETE FROM org_members WHERE id = ?", vec![m.id.as_str().into()])?,
        }
        if let Some(p) = m.person.as_deref().filter(|_| m.seat.is_some()) {
            self.queue_sync(p)?;
        }
        self.queue_quantities(&m.org)
    }

    /// An offer of `who`'s own org's (an admin's place for them, or one to
    /// another email of theirs): what it offers joins their row, and it
    /// goes, its answer kept so the same accept again answers the same.
    fn merge(&self, mine: &MemberRow, offer: &MemberRow) -> CellResult<()> {
        assert_eq!(mine.org, offer.org, "a merge is within one org");
        let person = mine.person.as_deref().ok_or_else(|| CellError::host(format!("{} merges into an offer", offer.id)))?;
        let (seat, comped) = match (mine.seat()?, offer.seat()?) {
            (Some(_), Some(_)) => return Err(conflict("you hold a seat in this org already: decline this offer")),
            (_, Some(k)) => (Some(k), offer.comped),
            (held, None) => (held, mine.comped),
        };
        self.exec("DELETE FROM org_members WHERE id = ?", vec![offer.id.as_str().into()])?;
        self.exec(
            "UPDATE org_members SET admin = MAX(admin, ?), seat = ?, comped = ? WHERE id = ?",
            vec![SqlStorageValue::Integer(offer.admin), seat.map_or(SqlStorageValue::Null, |k| k.as_str().into()), SqlStorageValue::Integer(comped), mine.id.as_str().into()],
        )?;
        let now = js::now_ms();
        self.exec(
            "INSERT INTO offer_answers (offer, person, at) VALUES (?, ?, ?) ON CONFLICT (offer) DO NOTHING",
            vec![offer.id.as_str().into(), person.into(), SqlStorageValue::Integer(now)],
        )?;
        self.exec("DELETE FROM offer_answers WHERE at < ?", vec![SqlStorageValue::Integer(now - ANSWERS_KEPT_MS)])?;
        self.queue_sync(person)?;
        self.queue_quantities(&mine.org)
    }

    /// An offer `who` may answer: one waiting on an email of theirs. Any
    /// other id is none of theirs (404, the same as one that is not).
    fn offer_for(&self, who: &str, id: &str) -> CellResult<MemberRow> {
        let row = self.member(id)?;
        if row.person.is_none() && self.emails_of(who)?.contains(&row.email) {
            return Ok(row);
        }
        Err(not_found(format!("no offer {id}")))
    }

    /// Whether `who` accepted the offer `id` already: its row is theirs, or
    /// it merged into theirs (`offer_answers`).
    fn accepted(&self, who: &str, id: &str) -> CellResult<bool> {
        if !org::valid_member_id(id) {
            return Ok(false);
        }
        let held = self.count("SELECT COUNT(*) AS n FROM org_members WHERE id = ? AND person = ?", vec![id.into(), who.into()])? > 0;
        Ok(held || self.count("SELECT COUNT(*) AS n FROM offer_answers WHERE offer = ? AND person = ?", vec![id.into(), who.into()])? > 0)
    }

    fn accept(&self, b: AcceptOffer) -> CellResult<MySeat> {
        let who = self.person_by(&b.by)?;
        // accepted already (a replay): their seat as it is
        if self.accepted(&who.id, &b.offer)? {
            return self.my_seat(&who.id);
        }
        let offer = self.offer_for(&who.id, &b.offer)?;
        // an admin's offer stands while its org pays (an operator's comp
        // stands alone): one that lapsed buys again before anyone joins
        if offer.comped == 0 && !self.org_pays(&offer.org)? {
            let o = self.stored_org(&offer.org)?;
            return Err(CellError::invalid(format!("{} pays for nothing now: its admins buy again before anyone takes its offers", o.name)));
        }
        match self.member_of(&who.id)? {
            Some(mine) if mine.org == offer.org => self.merge(&mine, &offer)?,
            Some(mine) => {
                self.go_from(&mine)?;
                self.enroll(&offer, Yes::Accepted(&who.id))?;
            }
            None => {
                self.enroll(&offer, Yes::Accepted(&who.id))?;
            }
        }
        let mine = self.member_of(&who.id)?.ok_or_else(|| CellError::host("an accepted offer leaves its person in no org"))?;
        assert_eq!(mine.org, offer.org, "an accepted offer's org is its person's");
        self.my_seat(&who.id)
    }

    fn decline(&self, b: DeclineOffer) -> CellResult<MySeat> {
        let who = self.person_by(&b.by)?;
        let offer = self.offer_for(&who.id, &b.offer)?;
        self.exec("DELETE FROM org_members WHERE id = ? AND person IS NULL", vec![offer.id.as_str().into()])?;
        // a paid seat declined bills no more; an org left with no one ends
        self.queue_quantities(&offer.org)?;
        self.my_seat(&who.id)
    }

    fn leave(&self, b: LeaveOrg) -> CellResult<MySeat> {
        let who = self.person_by(&b.by)?;
        let m = self.member_of(&who.id)?.ok_or_else(|| CellError::invalid("you are in no org"))?;
        if m.admin != 0 {
            let o = self.stored_org(&m.org)?;
            return Err(CellError::invalid(format!("you admin {}: step down as its admin first, then leave", o.name)));
        }
        // who pays (stepped down, their card still on it) stops paying as they go
        if self.pays_for(&m.org, &who.id)? {
            self.cancel_for_payer(&m.org)?;
        }
        self.exec("DELETE FROM org_members WHERE id = ?", vec![m.id.as_str().into()])?;
        if m.seat.is_some() {
            self.queue_sync(&who.id)?;
        }
        self.queue_quantities(&m.org)?;
        self.my_seat(&who.id)
    }

    /// The registry's routes for a person's own answer to offers, or `None`.
    pub(super) async fn offers_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            AcceptOffer::PATH => reply::<AcceptOffer>(self.armed_seats(|| self.accept(body(bytes)?)).await),
            DeclineOffer::PATH => reply::<DeclineOffer>(self.armed_seats(|| self.decline(body(bytes)?)).await),
            LeaveOrg::PATH => reply::<LeaveOrg>(self.armed_seats(|| self.leave(body(bytes)?)).await),
            _ => return None,
        })
    }
}
