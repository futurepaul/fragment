//! A wipe's registry side (docs/api.md, Operators; the router's
//! cell/src/wipe.rs runs the wipe, `fragment_core::wipe` holds its rules).
//! The Registry already serializes every change to an identity, so it is
//! where a wipe is recorded, locked and finished:
//!
//! - **The record** (`wipes`): one row per person a wipe began for, with how
//!   many of its steps are done (`fragment_core::wipe::Progress`), and their
//!   agents as they were when it began (`wiped_agents`), so a wipe run again
//!   after the registry forgot them still finds their lists and ledgers. A
//!   finished wipe's row keeps the identity and its times, nothing of the
//!   person.
//! - **The lock**, from `begin` until the last step: their and their
//!   agents' sessions and keys end at once (a signed-in browser is out, a
//!   CLI's key is no one's), and nothing acts as them or for them again
//!   (`wiping`: `by`, a sign-in's `person_for`, `lookup`, so no one adds
//!   them to a fragment); their sign-ins and email stay theirs, so no one
//!   signs in as them meanwhile.
//! - **Their org** (`Step::Billing`, before the end; docs/billing.md): in
//!   one turn their place goes, and with it every row of an org it ends
//!   (no one else in it, or they its last admin: `fragment_core::org::
//!   wiped`); what each org then asks of Stripe is queued (its
//!   subscription's cancel, at once, when it ends or their card pays for
//!   it; else its seats' count) and recorded (`wiped_orgs`), so a call cut
//!   anywhere goes on with it. The step is done once Stripe has it all.
//! - **The end** (`Step::Registry`, the last): every row naming them or
//!   their agents goes in one turn: identities, keys (their own sealed
//!   key with the rest), sign-in subjects and emails, picture, agent
//!   fragments, sessions, consents. Their next sign-in finds no subject,
//!   and is a new person, named by a new key.

use fragment_core::org;
use fragment_core::wipe::{self, Completed, Named, Progress, Refusal, Step};
use fragment_proto::wipe::OrgFound;

use super::calls::{WipeBegin, WipeBilled, WipeBilling, WipeDone, WipeFacts, WipeLook, WipePicture, WipeStep};
use super::*;

/// The wipes' record, beside the Registry's own tables. `wiped_orgs`
/// holds the orgs a wipe's `billing` step settled (ids and counts, nothing
/// of anyone), until its last step.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS wipes (
  identity TEXT PRIMARY KEY, done INTEGER NOT NULL, started_at INTEGER NOT NULL, by TEXT NOT NULL, finished_at INTEGER);
CREATE TABLE IF NOT EXISTS wiped_agents (
  identity TEXT NOT NULL, agent TEXT NOT NULL, PRIMARY KEY (identity, agent));
CREATE TABLE IF NOT EXISTS wiped_orgs (
  identity TEXT NOT NULL, org TEXT NOT NULL, ended INTEGER NOT NULL, canceled INTEGER NOT NULL, lapsed INTEGER NOT NULL,
  PRIMARY KEY (identity, org));
";

/// The names a `billing` step's note lists of those whose seats end.
const LAPSES_NAMED_MAX: usize = 10;
/// The orgs a wiped person may pay for at once (their own, and those whose
/// cancel has not landed yet).
const PAID_ORGS_MAX: usize = 50;

/// Whose rows a wipe's statements take: the person, every agent they own,
/// and every agent a wipe of theirs recorded (each bound to the person:
/// `theirs`), so no statement binds a list (a Durable Object's SQL takes at
/// most 100 bindings).
const THEIRS: &str =
    "(identity = ? OR identity IN (SELECT id FROM identities WHERE owner = ?) OR identity IN (SELECT agent FROM wiped_agents WHERE identity = ?))";

/// `THEIRS`' bindings for the person `identity`.
fn theirs(identity: &str) -> Vec<SqlStorageValue> {
    vec![identity.into(), identity.into(), identity.into()]
}

/// A refusal of the operator's, as the router answers it.
pub(crate) fn refused(r: Refusal) -> CellError {
    let code = match &r {
        Refusal::Mismatch { .. } => ErrorCode::AlreadyExists,
        Refusal::Malformed(_) | Refusal::NotAPerson | Refusal::Unconfirmed | Refusal::Themselves => ErrorCode::InvalidRequest,
    };
    CellError::new(code, r.message())
}

#[derive(Deserialize)]
struct DoneRow {
    done: i64,
    finished_at: Option<i64>,
}

#[derive(Deserialize)]
struct WipedOrgRow {
    org: String,
    ended: i64,
    canceled: i64,
    lapsed: i64,
}

#[derive(Deserialize)]
struct PersonRow {
    person: String,
}

#[derive(Deserialize)]
struct OrgIdRow {
    org: String,
}

#[derive(Deserialize)]
struct AgentRow {
    agent: String,
}

#[derive(Deserialize)]
struct FragmentRow {
    fragment: String,
}

#[derive(Deserialize)]
struct PictureRow {
    sha: String,
    shared: i64,
}

impl RegistryCell {
    /// How far a wipe of `identity` has got, or `None` when none began. A
    /// count no wipe makes is a host fault.
    fn wipe_progress(&self, identity: &str) -> CellResult<Option<Progress>> {
        match self.row::<DoneRow>("SELECT done, finished_at FROM wipes WHERE identity = ?", vec![identity.into()])? {
            None => Ok(None),
            Some(r) => Progress::recorded(r.done, r.finished_at.is_some())
                .map(Some)
                .map_err(|c| CellError::host(format!("wipes.done of {identity} is {}: no wipe counts so", c.0))),
        }
    }

    /// Whether a wipe of `identity` runs: begun and not finished. While it
    /// does, nothing acts as them or for them.
    pub(super) fn wiping(&self, identity: &str) -> CellResult<bool> {
        Ok(self.count("SELECT COUNT(*) AS n FROM wipes WHERE identity = ? AND finished_at IS NULL", vec![identity.into()])? > 0)
    }

    /// Refuses `who` (and an agent's owner) while a wipe of them runs.
    pub(super) fn not_wiping(&self, who: &Identity) -> CellResult<()> {
        let person = who.owner.as_deref().unwrap_or(&who.id);
        if self.wiping(person)? {
            return Err(CellError::new(ErrorCode::Forbidden, format!("{person} is being wiped: nothing acts as them, or for them")));
        }
        Ok(())
    }

    pub(super) fn wipe_look(&self, b: WipeLook) -> CellResult<WipeFacts> {
        let identity = match wipe::named(&b.person).map_err(refused)? {
            Named::Identity(id) => id,
            Named::Email(email) => self.person_by_email(&email)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no one signs in as {email}")))?,
        };
        self.wipe_facts(&identity)
    }

    /// What the registry holds of the person `identity`, and their wipe's
    /// progress. A wiped person holds nothing but the agents their wipe
    /// recorded; one no wipe ever named, and whom no row names, is 404.
    fn wipe_facts(&self, identity: &str) -> CellResult<WipeFacts> {
        let progress = self.wipe_progress(identity)?;
        let recorded = || -> CellResult<Vec<String>> {
            let rows = self.rows::<AgentRow>("SELECT agent FROM wiped_agents WHERE identity = ? ORDER BY agent", vec![identity.into()])?;
            Ok(rows.into_iter().map(|r| r.agent).collect())
        };
        let Some(who) = self.identity(identity)? else {
            return match progress {
                Some(p) if p.finished() => Ok(WipeFacts {
                    identity: identity.to_string(),
                    email: None,
                    agents: recorded()?,
                    agent_fragments: vec![],
                    sign_ins: 0,
                    keys: 0,
                    sessions: 0,
                    pictures: vec![],
                    done: Some(p.done()),
                    orgs: vec![],
                }),
                Some(p) => Err(CellError::host(format!("a wipe of {identity} has done {} steps, and its identity is gone before the last", p.done()))),
                None => Err(CellError::new(ErrorCode::NotFound, format!("no identity {identity}"))),
            };
        };
        if who.kind != IdentityKind::Person {
            return Err(refused(Refusal::NotAPerson));
        }
        // an unfinished wipe's agents are those it recorded and any made
        // before it began (none after: `register_agent` refuses)
        let mut agents = recorded()?;
        let owned = self.rows::<IdRow>("SELECT id FROM identities WHERE owner = ? AND kind = 'agent' ORDER BY id", vec![identity.into()])?;
        for a in owned {
            if !agents.contains(&a.id) {
                agents.push(a.id);
            }
        }
        agents.sort();
        assert!(agents.len() as u64 <= limits::AGENTS_PER_OWNER_MAX * 2, "a person's agents are bounded");
        let agent_fragments = self
            .rows::<FragmentRow>(
                "SELECT fragment FROM agent_fragments WHERE identity IN (SELECT id FROM identities WHERE owner = ?) ORDER BY fragment",
                vec![identity.into()],
            )?
            .into_iter()
            .map(|r| r.fragment)
            .collect();
        let held = |table: &str| self.count(&format!("SELECT COUNT(*) AS n FROM {table} WHERE {THEIRS}"), theirs(identity));
        let pictures = self
            .rows::<PictureRow>(
                "SELECT sha, EXISTS (SELECT 1 FROM pictures q WHERE q.sha = p.sha AND q.identity != p.identity) AS shared FROM pictures p WHERE identity = ?",
                vec![identity.into()],
            )?
            .into_iter()
            .map(|r| WipePicture { sha: r.sha, shared: r.shared != 0 })
            .collect::<Vec<_>>();
        if pictures.iter().any(|p| !blob::valid_sha(&p.sha)) {
            return Err(CellError::host(format!("a picture of {identity} is not named by a SHA-256")));
        }
        Ok(WipeFacts {
            identity: identity.to_string(),
            email: self.email_of(identity)?,
            agents,
            agent_fragments,
            sign_ins: self.count("SELECT COUNT(*) AS n FROM subjects WHERE identity = ?", vec![identity.into()])?,
            keys: held("keys")?,
            sessions: held("sessions")?,
            pictures,
            done: progress.map(Progress::done),
            orgs: self.orgs_found(identity)?,
        })
    }

    /// A wipe of the person `identity` begins (or, begun, goes on): in one
    /// turn, the record, the agents it covers, and the end of every
    /// session and key of theirs and their agents'. A finished wipe's
    /// person is answered as they are, nothing begun again.
    pub(super) fn wipe_begin(&self, b: WipeBegin) -> CellResult<WipeFacts> {
        if !npub::is_identity(&b.identity) || b.by.is_empty() {
            return Err(CellError::invalid("a wipe begins for an identity, by an operator"));
        }
        let facts = self.wipe_facts(&b.identity)?;
        if facts.done.is_some_and(|d| d as usize >= wipe::STEPS.len()) {
            return Ok(facts);
        }
        let now = SqlStorageValue::Integer(js::now_ms());
        let id = b.identity.as_str();
        self.exec(
            "INSERT INTO wipes (identity, done, started_at, by) VALUES (?, 0, ?, ?) ON CONFLICT (identity) DO NOTHING",
            vec![id.into(), now, b.by.as_str().into()],
        )?;
        self.exec(
            "INSERT INTO wiped_agents (identity, agent) SELECT ?, id FROM identities WHERE owner = ? AND kind = 'agent' ON CONFLICT (identity, agent) DO NOTHING",
            vec![id.into(), id.into()],
        )?;
        self.end_access(id)?;
        assert!(self.wiping(id)?, "a wipe begun runs until its last step");
        let facts = self.wipe_facts(id)?;
        assert_eq!((facts.keys, facts.sessions), (0, 0), "a wipe begun leaves no key or session of theirs");
        Ok(facts)
    }

    /// Their and their agents' sessions (a site's and a frame's with the
    /// platform's), the redemptions of those, their keys, and the sign-ins
    /// pending to link to them: from now nothing signed or signed in is
    /// them. Keys are deleted, not revoked: a wiped identity holds nothing,
    /// and the key's holder may bring it to the new person they become.
    fn end_access(&self, identity: &str) -> CellResult<()> {
        self.exec(&format!("DELETE FROM redemptions WHERE session IN (SELECT hash FROM sessions WHERE {THEIRS})"), theirs(identity))?;
        self.exec(&format!("DELETE FROM sessions WHERE {THEIRS}"), theirs(identity))?;
        self.exec(&format!("DELETE FROM keys WHERE {THEIRS}"), theirs(identity))?;
        self.exec("DELETE FROM person_keys WHERE identity = ?", vec![identity.into()])?;
        self.exec("DELETE FROM logins WHERE link_to = ?", vec![identity.into()])?;
        Ok(())
    }

    /// A step of the wipe of `identity` is done. The last (`registry`)
    /// deletes, in the same turn, every row that names them or their agents;
    /// reported again, it changes nothing.
    pub(super) fn wipe_step(&self, b: WipeStep) -> CellResult<WipeDone> {
        let id = b.identity.as_str();
        let mut progress = self.wipe_progress(id)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no wipe of {id} began")))?;
        let before = progress.done();
        match progress.complete(b.step) {
            Ok(Completed::Replayed) => return Ok(WipeDone { done: before }),
            Ok(Completed::Advanced) => {}
            Err(e) => {
                let next = e.next.map_or("none", Step::name);
                return Err(CellError::host(format!("a wipe of {id} reported {} done before {next}", e.step.name())));
            }
        }
        if b.step == Step::Registry {
            self.wipe_rows(id)?;
        }
        let finished = match progress.finished() {
            true => SqlStorageValue::Integer(js::now_ms()),
            false => SqlStorageValue::Null,
        };
        self.exec(
            "UPDATE wipes SET done = ?, finished_at = COALESCE(finished_at, ?) WHERE identity = ?",
            vec![SqlStorageValue::Integer(i64::from(progress.done())), finished, id.into()],
        )?;
        let stored = self.wipe_progress(id)?.ok_or_else(|| CellError::host("a wipe's row went missing"))?;
        assert_eq!(stored, progress, "a wipe's progress reads back as it was written");
        assert_eq!(self.wiping(id)?, !progress.finished(), "a wipe is locked exactly until its last step");
        Ok(WipeDone { done: progress.done() })
    }

    /// Every row that names the person `identity` or an agent of theirs, in
    /// one turn (no await): what is left of them here is their wipe's row.
    fn wipe_rows(&self, identity: &str) -> CellResult<()> {
        let twice = || vec![SqlStorageValue::from(identity), SqlStorageValue::from(identity)];
        // an agent made between the wipe's begin and now (none: `by` refuses)
        self.exec(
            "INSERT INTO wiped_agents (identity, agent) SELECT ?, id FROM identities WHERE owner = ? AND kind = 'agent' ON CONFLICT (identity, agent) DO NOTHING",
            twice(),
        )?;
        self.end_access(identity)?;
        self.exec(&format!("DELETE FROM consents WHERE {THEIRS}"), theirs(identity))?;
        self.exec(&format!("DELETE FROM agent_fragments WHERE {THEIRS}"), theirs(identity))?;
        self.exec("DELETE FROM subjects WHERE identity = ?", vec![identity.into()])?;
        self.exec("DELETE FROM pictures WHERE identity = ?", vec![identity.into()])?;
        self.exec("DELETE FROM invite_mails WHERE sharer = ?", vec![identity.into()])?;
        self.wipe_orgs(identity)?;
        self.exec("DELETE FROM wiped_orgs WHERE identity = ?", vec![identity.into()])?;
        self.exec("DELETE FROM offer_answers WHERE person = ?", vec![identity.into()])?;
        self.exec("DELETE FROM identities WHERE id IN (SELECT agent FROM wiped_agents WHERE identity = ?) OR id = ?", twice())?;
        let left = self.count(
            "SELECT (SELECT COUNT(*) FROM identities WHERE id = ? OR owner = ?) + (SELECT COUNT(*) FROM subjects WHERE identity = ?) AS n",
            vec![identity.into(), identity.into(), identity.into()],
        )?;
        assert_eq!(left, 0, "no row names a wiped person");
        Ok(())
    }

    /// The orgs a wipe of `person` settles, and what it does to each
    /// (`fragment_core::org::wiped`): theirs, and any other whose
    /// subscription their card pays and that has not ended (one they left
    /// whose cancel has not landed yet). The dry run shows these; the
    /// `billing` step does them.
    fn orgs_found(&self, person: &str) -> CellResult<Vec<OrgFound>> {
        let mut found = vec![];
        let mine = self.member_of(person)?;
        if let Some(m) = &mine {
            let o = self.stored_org(&m.org)?;
            let w = org::wiped(self.place_of(m)?);
            let cancels = w.cancels && self.lives(&o.id)?;
            let lapses = match (w.ends, cancels) {
                (true, _) => self.seats_besides(&o.id, &m.id, false)?,
                (false, true) => self.seats_besides(&o.id, &m.id, true)?,
                (false, false) => vec![],
            };
            found.push(OrgFound { org: o.id, name: o.name, ends: w.ends, cancels, lapses });
        }
        // bounded: a payer who goes stops paying at once, so a person pays
        // for their own org and those whose cancel is still on its way; past
        // PAID_ORGS_MAX the wipe fails, saying so, rather than skip one
        let paid = self.rows::<OrgIdRow>(
            "SELECT p.org FROM org_payers p JOIN orgs o ON o.id = p.org AND o.subscription = p.subscription WHERE p.person = ? ORDER BY p.org LIMIT ?",
            vec![person.into(), SqlStorageValue::Integer(PAID_ORGS_MAX as i64 + 1)],
        )?;
        if paid.len() > PAID_ORGS_MAX {
            return Err(CellError::host(format!("{person} pays for more than {PAID_ORGS_MAX} orgs: settle them by hand")));
        }
        for p in paid {
            if mine.as_ref().is_some_and(|m| m.org == p.org) || !self.pays_for(&p.org, person)? {
                continue;
            }
            let o = self.stored_org(&p.org)?;
            let lapses = self.seats_besides(&o.id, "", true)?;
            found.push(OrgFound { org: o.id, name: o.name, ends: false, cancels: true, lapses });
        }
        Ok(found)
    }

    /// Whether `org`'s subscription may still bill (none ended).
    fn lives(&self, org: &str) -> CellResult<bool> {
        let Some(sub) = self.org_sub(org)? else { return Ok(false) };
        Ok(sub.subscription.is_some() && !sub.status.as_deref().and_then(org::Status::parse).is_some_and(org::Status::is_ended))
    }

    /// The emails (each holder's latest) of `org`'s members holding a seat
    /// (`paid`: a paid one), but the row `besides`. Bounded: an org holds
    /// at most `SEATS_PER_ORG_MAX` seats.
    fn seats_besides(&self, org: &str, besides: &str, paid: bool) -> CellResult<Vec<String>> {
        #[derive(Deserialize)]
        struct Row {
            email: String,
        }
        let paid = if paid { " AND m.comped = 0" } else { "" };
        let rows = self.rows::<Row>(
            &format!(
                "SELECT COALESCE((SELECT s.email FROM subjects s WHERE s.identity = m.person ORDER BY s.signed_in_at DESC LIMIT 1), m.email) AS email
                 FROM org_members m WHERE m.org = ? AND m.id != ? AND m.person IS NOT NULL AND m.seat IS NOT NULL{paid} ORDER BY m.added_at, m.id"
            ),
            vec![org.into(), besides.into()],
        )?;
        Ok(rows.into_iter().map(|r| r.email).collect())
    }

    /// The `billing` step's one turn: `person`'s place in their org goes
    /// (every row of an org it ends: the others' seats end, their plans
    /// pushed; offers withdrawn), each org's cancel (when it ends, or their
    /// card pays for it) or seats' count is queued, and each is recorded.
    fn settle_orgs(&self, person: &str) -> CellResult<Vec<OrgFound>> {
        let found = self.orgs_found(person)?;
        let mine = self.member_of(person)?;
        for f in &found {
            let theirs = mine.as_ref().is_some_and(|m| m.org == f.org);
            assert!(theirs || (!f.ends && f.cancels), "an org not theirs is settled only to cancel what they pay");
            if f.ends {
                let held = self.rows::<PersonRow>(
                    "SELECT person FROM org_members WHERE org = ? AND person IS NOT NULL AND person != ? AND seat IS NOT NULL",
                    vec![f.org.as_str().into(), person.into()],
                )?;
                self.exec("DELETE FROM org_members WHERE org = ?", vec![f.org.as_str().into()])?;
                // bounded: an org holds at most SEATS_PER_ORG_MAX seats
                for h in held {
                    self.queue_sync(&h.person)?;
                }
            } else if theirs {
                self.exec("DELETE FROM org_members WHERE person = ?", vec![person.into()])?;
            }
            if f.cancels {
                let sub = self.org_sub(&f.org)?.and_then(|s| s.subscription).ok_or_else(|| CellError::host(format!("{} cancels a subscription it lacks", f.org)))?;
                self.queue_cancel(&sub, &f.org, if f.ends { super::seats::WHY_ENDED } else { super::seats::WHY_PAYER })?;
            }
            // an org ended is dropped once its cancel lands; one they left
            // has its count pushed; one canceled for them is left as it is
            if f.ends || !f.cancels {
                self.queue_quantities(&f.org)?;
            }
            let lapsed = i64::try_from(f.lapses.len()).expect("an org's seats fit an i64");
            self.exec(
                "INSERT INTO wiped_orgs (identity, org, ended, canceled, lapsed) VALUES (?, ?, ?, ?, ?) ON CONFLICT (identity, org) DO NOTHING",
                vec![person.into(), f.org.as_str().into(), SqlStorageValue::Integer(i64::from(f.ends)), SqlStorageValue::Integer(i64::from(f.cancels)), SqlStorageValue::Integer(lapsed)],
            )?;
        }
        assert!(self.member_of(person)?.is_none(), "a wipe's billing step takes their place");
        Ok(found)
    }

    /// `POST /wipe/billing`: the wipe of `identity`'s `billing` step, as far
    /// as one call goes. Its first call takes their place in one turn
    /// (`settle_orgs`); each call then pushes what each org it recorded
    /// asks of Stripe (the alarm does too), and it is done once none is
    /// left. Called past the step, it is done.
    pub(super) async fn wipe_billing(&self, b: WipeBilling) -> CellResult<WipeBilled> {
        let id = b.identity.as_str();
        let progress = self.wipe_progress(id)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no wipe of {id} began")))?;
        if progress.has_done(Step::Billing) {
            return Ok(WipeBilled { done: true, orgs: 0, note: None });
        }
        if progress.next() != Some(Step::Billing) {
            return Err(CellError::host(format!("a wipe of {id} asked for its billing step before {}", progress.next().map_or("none", Step::name))));
        }
        let recorded = |me: &Self| me.rows::<WipedOrgRow>("SELECT org, ended, canceled, lapsed FROM wiped_orgs WHERE identity = ? ORDER BY org", vec![id.into()]);
        let now = match recorded(self)?.is_empty() {
            true => self.settle_orgs(id)?,
            false => vec![],
        };
        let orgs = recorded(self)?;
        let mut waiting = vec![];
        // bounded: a person settles their org and the few they pay for
        for r in &orgs {
            let pushed = self.push_queued(&r.org).await?;
            let canceled = self.run_cancels(Some(&r.org)).await?;
            let queued = self.count(
                "SELECT (SELECT COUNT(*) FROM quantity_syncs WHERE org = ?1) + (SELECT COUNT(*) FROM stripe_cancels WHERE org = ?1) AS n",
                vec![r.org.as_str().into()],
            )?;
            if !pushed || !canceled || queued > 0 {
                waiting.push(r.org.clone());
            }
        }
        self.arm_syncs().await?;
        self.arm_quantities().await?;
        let note = billing_note(&now, &orgs, &waiting);
        Ok(WipeBilled { done: waiting.is_empty(), orgs: orgs.len() as u64, note })
    }
}

/// What a `billing` step says: what it did to each org this call (named,
/// with whose seats end), or, going on, what it recorded; and what still
/// waits on Stripe.
fn billing_note(now: &[OrgFound], recorded: &[WipedOrgRow], waiting: &[String]) -> Option<String> {
    let names = |l: &[String]| {
        let shown: Vec<&str> = l.iter().take(LAPSES_NAMED_MAX).map(String::as_str).collect();
        let more = l.len().saturating_sub(LAPSES_NAMED_MAX);
        if more > 0 { format!("{} and {more} more", shown.join(", ")) } else { shown.join(", ") }
    };
    let mut said: Vec<String> = now
        .iter()
        .map(|f| match (f.ends, f.cancels) {
            (true, _) => {
                let canceled = if f.cancels { ": its subscription canceled at once, its Stripe customer kept" } else { "" };
                let lapses = if f.lapses.is_empty() { String::new() } else { format!("; the seats of {} end", names(&f.lapses)) };
                format!("{} ended with them{canceled}{lapses}", f.name)
            }
            (false, true) => format!(
                "{} goes on without them: its subscription, which their card paid, canceled at once{}",
                f.name,
                if f.lapses.is_empty() { String::new() } else { format!("; the paid seats of {} lapse until its admins buy again", names(&f.lapses)) }
            ),
            (false, false) => format!("they left {}: its seats' count pushed to Stripe", f.name),
        })
        .collect();
    if now.is_empty() && !recorded.is_empty() {
        let ended = recorded.iter().filter(|r| r.ended != 0).count();
        let canceled = recorded.iter().filter(|r| r.canceled != 0).count();
        let lapsed: i64 = recorded.iter().map(|r| r.lapsed).sum();
        said.push(format!("{} org(s) settled: {ended} ended, {canceled} subscription(s) canceled, {lapsed} other seat(s) ended or lapsed", recorded.len()));
    }
    if !waiting.is_empty() {
        said.push(format!("waiting on Stripe for {} (its push or cancel failed this time): the next call tries again", waiting.join(", ")));
    }
    (!said.is_empty()).then(|| said.join(". "))
}
