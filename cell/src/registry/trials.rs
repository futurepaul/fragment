//! Trials (docs/billing.md, "Trials"; decision 56): our codes on Stripe's
//! trial. An operator makes a code that names a seat's kind, its days (1
//! to 30), its places (1 to 10,000) and when it stops; a person who never
//! paid redeems it through a Checkout that takes a card first, with
//! Stripe's `trial_period_days`. One trial per person, ever.
//!
//! A place is held by a Checkout still open (its use's `expires_at`, the
//! Checkout's own) or by a subscription the use bought; a Checkout never
//! completed frees its place when it expires, so nothing waits on
//! `checkout.session.expired`. A trial's end is Stripe's: it charges the
//! card, and a decline lapses the seat as any seat's (decision 53).

use fragment_core::org::{self, TRIAL_CAPACITY_MAX, TRIAL_CODES_MAX, TRIAL_DAYS_MAX, TRIAL_NAME_MAX_BYTES};
use fragment_proto::org::{NewTrialCode, SeatKind, TrialCode, TrialCodeChange, TrialUse};
use serde::Serialize;

use super::calls::Call;
use super::orgs::not_found;
use super::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS trial_codes (
  id TEXT PRIMARY KEY, code TEXT NOT NULL UNIQUE, name TEXT NOT NULL, kind TEXT NOT NULL, days INTEGER NOT NULL,
  capacity INTEGER NOT NULL, expires_at INTEGER, active INTEGER NOT NULL, revision INTEGER NOT NULL,
  created_at INTEGER NOT NULL, created_by TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS trial_uses (
  person TEXT PRIMARY KEY, code TEXT NOT NULL, at INTEGER NOT NULL, expires_at INTEGER NOT NULL, subscription TEXT);
CREATE INDEX IF NOT EXISTS trial_uses_code ON trial_uses (code, expires_at);
";

/// `POST /trials/new`: an operator's new code (`by`, for the record).
#[derive(Serialize, Deserialize)]
pub(crate) struct TrialNew {
    pub by: String,
    pub code: NewTrialCode,
}

impl Call for TrialNew {
    const PATH: &'static str = "/trials/new";
    type Answer = TrialCode;
}

/// `POST /trials/list`: every code, newest first (their uses counted).
#[derive(Serialize, Deserialize)]
pub(crate) struct TrialList {}

#[derive(Serialize, Deserialize)]
pub(crate) struct TrialCodes {
    pub codes: Vec<TrialCode>,
}

impl Call for TrialList {
    const PATH: &'static str = "/trials/list";
    type Answer = TrialCodes;
}

/// `POST /trials/get`: one code, with its uses.
#[derive(Serialize, Deserialize)]
pub(crate) struct TrialGet {
    pub id: String,
}

impl Call for TrialGet {
    const PATH: &'static str = "/trials/get";
    type Answer = TrialCode;
}

/// `POST /trials/change`: a code changed, compare-and-set on its revision.
#[derive(Serialize, Deserialize)]
pub(crate) struct TrialChange {
    pub by: String,
    pub id: String,
    pub change: TrialCodeChange,
}

impl Call for TrialChange {
    const PATH: &'static str = "/trials/change";
    type Answer = TrialCode;
}

/// A code's offer to a person: the seat it gives, and for how long.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TrialOffer {
    pub id: String,
    pub kind: SeatKind,
    pub days: u32,
}

#[derive(Deserialize)]
struct CodeRow {
    id: String,
    code: String,
    name: String,
    kind: String,
    days: i64,
    capacity: i64,
    expires_at: Option<i64>,
    active: i64,
    revision: i64,
    created_at: i64,
    created_by: String,
}

const CODE_COLUMNS: &str = "id, code, name, kind, days, capacity, expires_at, active, revision, created_at, created_by";

impl CodeRow {
    fn kind(&self) -> CellResult<SeatKind> {
        SeatKind::parse(&self.kind).ok_or_else(|| CellError::host(format!("trial_codes.kind of {} is {:?}", self.id, self.kind)))
    }
}

#[derive(Deserialize)]
struct UseRow {
    subscription: Option<String>,
}

fn not_a_code() -> CellError {
    CellError::new(ErrorCode::NotFound, "no such trial code")
}

impl RegistryCell {
    fn code_row(&self, id: &str) -> CellResult<CodeRow> {
        self.row::<CodeRow>(&format!("SELECT {CODE_COLUMNS} FROM trial_codes WHERE id = ?"), vec![id.into()])?.ok_or_else(|| not_found(format!("no trial code {id}")))
    }

    /// How many places a code's subscriptions and open Checkouts hold (a
    /// person's own open Checkout aside: they may open another).
    fn places_taken(&self, code: &str, besides: Option<&str>) -> CellResult<(u64, u64)> {
        #[derive(Deserialize)]
        struct Row {
            subscribed: u64,
            open: u64,
        }
        let r = self
            .row::<Row>(
                "SELECT (SELECT COUNT(*) FROM trial_uses WHERE code = ?1 AND subscription IS NOT NULL) AS subscribed,
                        (SELECT COUNT(*) FROM trial_uses WHERE code = ?1 AND subscription IS NULL AND expires_at > ?2 AND person != ?3) AS open",
                vec![code.into(), SqlStorageValue::Integer(js::now_ms()), besides.unwrap_or("").into()],
            )?
            .ok_or_else(|| CellError::host("COUNT answered no row"))?;
        Ok((r.subscribed, r.open))
    }

    fn trial_view(&self, r: &CodeRow, with_uses: bool) -> CellResult<TrialCode> {
        let (subscribed, open) = self.places_taken(&r.id, None)?;
        let uses = if with_uses {
            #[derive(Deserialize)]
            struct Row {
                person: String,
                at: i64,
                status: Option<String>,
                email: Option<String>,
            }
            // bounded: a code has at most TRIAL_CAPACITY_MAX subscribed uses, and its open ones expire in 31 minutes
            self.rows::<Row>(
                "SELECT u.person, u.at, o.status, (SELECT email FROM subjects WHERE identity = u.person ORDER BY signed_in_at DESC LIMIT 1) AS email
                 FROM trial_uses u LEFT JOIN orgs o ON o.subscription = u.subscription
                 WHERE u.code = ? AND (u.subscription IS NOT NULL OR u.expires_at > ?) ORDER BY u.at",
                vec![r.id.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
            )?
            .into_iter()
            .map(|u| TrialUse { person: u.person, email: u.email, at: u.at, status: u.status })
            .collect()
        } else {
            vec![]
        };
        Ok(TrialCode {
            id: r.id.clone(),
            code: org::shown_trial_code(&r.code),
            name: r.name.clone(),
            kind: r.kind()?,
            days: u32::try_from(r.days).map_err(|_| CellError::host("trial_codes.days"))?,
            capacity: u64::try_from(r.capacity).map_err(|_| CellError::host("trial_codes.capacity"))?,
            expires_at: r.expires_at,
            active: r.active != 0,
            revision: u64::try_from(r.revision).map_err(|_| CellError::host("trial_codes.revision"))?,
            created_at: r.created_at,
            created_by: r.created_by.clone(),
            subscribed,
            open,
            uses,
        })
    }

    /// A code's typed form, free of any other code's.
    fn free_code(&self, raw: &str, besides: Option<&str>) -> CellResult<String> {
        let code = org::trial_code(raw).ok_or_else(|| CellError::invalid("a trial code is 8-64 of A-Z (no I or O) and 2-9, dashes and spaces aside"))?;
        let holder = self.row::<IdRow>("SELECT id FROM trial_codes WHERE code = ?", vec![code.as_str().into()])?;
        if holder.is_some_and(|h| Some(h.id.as_str()) != besides) {
            return Err(conflict("another trial code is that already"));
        }
        Ok(code)
    }

    fn trial_new(&self, b: TrialNew) -> CellResult<TrialCode> {
        let n = b.code;
        let name = n.name.trim();
        if name.is_empty() || name.len() > TRIAL_NAME_MAX_BYTES || name.chars().any(char::is_control) {
            return Err(CellError::invalid(format!("a trial code's name is 1-{TRIAL_NAME_MAX_BYTES} bytes on one line")));
        }
        if !(1..=TRIAL_DAYS_MAX).contains(&n.days) {
            return Err(CellError::invalid(format!("a trial is 1-{TRIAL_DAYS_MAX} days")));
        }
        if !(1..=TRIAL_CAPACITY_MAX).contains(&n.capacity) {
            return Err(CellError::invalid(format!("a code has 1-{TRIAL_CAPACITY_MAX} places")));
        }
        let now = js::now_ms();
        if n.expires_at.is_some_and(|at| at <= now) {
            return Err(CellError::invalid("a code's expiry is in the future"));
        }
        if self.count("SELECT COUNT(*) AS n FROM trial_codes", vec![])? >= TRIAL_CODES_MAX {
            return Err(CellError::invalid(format!("a fleet keeps at most {TRIAL_CODES_MAX} trial codes")));
        }
        let code = match &n.code {
            Some(raw) => self.free_code(raw, None)?,
            None => org::trial_code(&org::new_trial_code(js::random_bytes::<10>())).expect("a made code reads back"),
        };
        if self.count("SELECT COUNT(*) AS n FROM trial_codes WHERE code = ?", vec![code.as_str().into()])? > 0 {
            return Err(conflict("that code was just made: make another"));
        }
        let id = format!("trial-{}", hex::encode(js::random_bytes::<8>()));
        let opt = |v: Option<i64>| v.map_or(SqlStorageValue::Null, SqlStorageValue::Integer);
        self.exec(
            &format!("INSERT INTO trial_codes ({CODE_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, 1, 1, ?, ?)"),
            vec![
                id.as_str().into(),
                code.as_str().into(),
                name.into(),
                n.kind.as_str().into(),
                SqlStorageValue::Integer(i64::from(n.days)),
                SqlStorageValue::Integer(n.capacity as i64),
                opt(n.expires_at),
                SqlStorageValue::Integer(now),
                b.by.as_str().into(),
            ],
        )?;
        self.log_admin(&b.by, "trial-new", &id, &format!("{name}: {} days of a {} seat, {} places", n.days, n.kind.as_str(), n.capacity))?;
        self.trial_view(&self.code_row(&id)?, true)
    }

    fn trial_list(&self) -> CellResult<TrialCodes> {
        // bounded: a fleet keeps at most TRIAL_CODES_MAX codes
        let rows = self.rows::<CodeRow>(&format!("SELECT {CODE_COLUMNS} FROM trial_codes ORDER BY created_at DESC, id"), vec![])?;
        Ok(TrialCodes { codes: rows.iter().map(|r| self.trial_view(r, false)).collect::<CellResult<_>>()? })
    }

    fn trial_change(&self, b: TrialChange) -> CellResult<TrialCode> {
        let r = self.code_row(&b.id)?;
        let c = b.change;
        if c.revision != r.revision as u64 {
            return Err(conflict(format!("the code changed since revision {} (it is at {}): read it again", c.revision, r.revision)));
        }
        if let Some(cap) = c.capacity {
            if cap < r.capacity as u64 || cap > TRIAL_CAPACITY_MAX {
                return Err(CellError::invalid(format!("a code's places only grow, to {TRIAL_CAPACITY_MAX} at most")));
            }
        }
        if c.expires_at.is_some_and(|at| at <= js::now_ms()) {
            return Err(CellError::invalid("a code's expiry is in the future"));
        }
        let code = c.code.as_deref().map(|raw| self.free_code(raw, Some(&r.id))).transpose()?;
        let opt = |v: Option<i64>| v.map_or(SqlStorageValue::Null, SqlStorageValue::Integer);
        self.exec(
            "UPDATE trial_codes SET capacity = COALESCE(?, capacity), active = COALESCE(?, active), code = COALESCE(?, code),
             expires_at = COALESCE(?, expires_at), revision = revision + 1 WHERE id = ?",
            vec![
                opt(c.capacity.map(|n| n as i64)),
                opt(c.active.map(i64::from)),
                code.map_or(SqlStorageValue::Null, |c| c.into()),
                opt(c.expires_at),
                r.id.as_str().into(),
            ],
        )?;
        self.log_admin(&b.by, "trial-change", &r.id, &serde_json::to_string(&c).unwrap_or_default())?;
        self.trial_view(&self.code_row(&r.id)?, true)
    }

    /// What a code offers `person`, or why not: it is unknown, off, past
    /// its expiry or full; they trialed already, or their org has paid.
    /// Changes nothing (`hold_trial_place` does, once all is checked).
    pub(super) fn trial_offer(&self, person: &str, raw: &str, kind: Option<SeatKind>, org: Option<&str>) -> CellResult<TrialOffer> {
        let code = org::trial_code(raw).ok_or_else(not_a_code)?;
        let r = self.row::<CodeRow>(&format!("SELECT {CODE_COLUMNS} FROM trial_codes WHERE code = ?"), vec![code.into()])?.ok_or_else(not_a_code)?;
        if r.active == 0 || r.expires_at.is_some_and(|at| at <= js::now_ms()) {
            return Err(CellError::invalid("this trial code has ended"));
        }
        if kind.is_some_and(|k| k != r.kind().unwrap_or(k)) {
            return Err(CellError::invalid(format!("this trial is of a {} seat", r.kind)));
        }
        let mine = self.row::<UseRow>("SELECT subscription FROM trial_uses WHERE person = ?", vec![person.into()])?;
        if mine.as_ref().is_some_and(|u| u.subscription.is_some()) {
            return Err(conflict("a trial is once a person: you had yours"));
        }
        if let Some(o) = org {
            #[derive(Deserialize)]
            struct Paid {
                subscription: Option<String>,
            }
            if self.row::<Paid>("SELECT subscription FROM orgs WHERE id = ?", vec![o.into()])?.and_then(|p| p.subscription).is_some() {
                return Err(conflict("a trial is for an org that never paid"));
            }
        }
        let (subscribed, open) = self.places_taken(&r.id, Some(person))?;
        if !org::trial_has_place(r.capacity as u64, subscribed, open) {
            return Err(conflict("this trial code's places are taken"));
        }
        Ok(TrialOffer { id: r.id.clone(), kind: r.kind()?, days: u32::try_from(r.days).map_err(|_| CellError::host("trial_codes.days"))? })
    }

    /// Holds a code's place for `person`'s Checkout until it expires.
    pub(super) fn hold_trial_place(&self, code: &str, person: &str, until_ms: i64) -> CellResult<()> {
        self.exec(
            "INSERT INTO trial_uses (person, code, at, expires_at, subscription) VALUES (?, ?, ?, ?, NULL)
             ON CONFLICT (person) DO UPDATE SET code = excluded.code, at = excluded.at, expires_at = excluded.expires_at
             WHERE trial_uses.subscription IS NULL",
            vec![person.into(), code.into(), SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(until_ms)],
        )
    }

    /// A trial's Checkout completed: its use holds its place for good.
    pub(super) fn trial_bought(&self, code: &str, person: &str, subscription: &str) -> CellResult<()> {
        self.exec("UPDATE trial_uses SET subscription = ? WHERE person = ? AND code = ?", vec![subscription.into(), person.into(), code.into()])
    }

    pub(super) fn trials_route(&self, path: &str, bytes: &[u8]) -> Option<CellResult<Response>> {
        Some(match path {
            TrialNew::PATH => reply::<TrialNew>(body(bytes).and_then(|b| self.trial_new(b))),
            TrialList::PATH => reply::<TrialList>(self.trial_list()),
            TrialGet::PATH => reply::<TrialGet>(body::<TrialGet>(bytes).and_then(|b| self.trial_view(&self.code_row(&b.id)?, true))),
            TrialChange::PATH => reply::<TrialChange>(body(bytes).and_then(|b| self.trial_change(b))),
            _ => return None,
        })
    }
}
