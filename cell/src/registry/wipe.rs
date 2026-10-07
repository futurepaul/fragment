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
//!   them to a fragment); their username stays theirs, so no one takes it
//!   meanwhile and no fragment is made under it.
//! - **The end** (`Step::Registry`, the last): every row naming them or
//!   their agents goes in one turn: identities, keys, sign-in subjects,
//!   username, picture, agent fragments, sessions, consents. Their next
//!   sign-in finds no subject, and is a new person.

use fragment_core::wipe::{self, Completed, Named, Progress, Refusal, Step};

use super::calls::{WipeBegin, WipeDone, WipeFacts, WipeLook, WipePicture, WipeStep};
use super::*;

/// The wipes' record, beside the Registry's own tables.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS wipes (
  identity TEXT PRIMARY KEY, done INTEGER NOT NULL, started_at INTEGER NOT NULL, by TEXT NOT NULL, finished_at INTEGER);
CREATE TABLE IF NOT EXISTS wiped_agents (
  identity TEXT NOT NULL, agent TEXT NOT NULL, PRIMARY KEY (identity, agent));
";

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
        match self.row::<DoneRow>("SELECT done FROM wipes WHERE identity = ?", vec![identity.into()])? {
            None => Ok(None),
            Some(r) => Progress::stored(r.done).map(Some).map_err(|c| CellError::host(format!("wipes.done of {identity} is {}: no wipe counts so", c.0))),
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
            Named::Username(u) => {
                let holder = self.row::<HolderRow>("SELECT identity FROM usernames WHERE username = ?", vec![u.as_str().into()])?;
                holder.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no one is {u}")))?.identity
            }
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
                    username: None,
                    agents: recorded()?,
                    agent_fragments: vec![],
                    sign_ins: 0,
                    keys: 0,
                    sessions: 0,
                    pictures: vec![],
                    done: Some(p.done()),
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
            username: self.username_of(identity)?,
            agents,
            agent_fragments,
            sign_ins: self.count("SELECT COUNT(*) AS n FROM subjects WHERE identity = ?", vec![identity.into()])?,
            keys: held("keys")?,
            sessions: held("sessions")?,
            pictures,
            done: progress.map(Progress::done),
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
    /// platform's), the redemptions of those, their keys, their connected
    /// clients and the codes of those, and the sign-ins pending to link to them: from now nothing signed or signed in is
    /// them. Keys are deleted, not revoked: a wiped identity holds nothing,
    /// and the key's holder may bring it to the new person they become.
    fn end_access(&self, identity: &str) -> CellResult<()> {
        self.exec(&format!("DELETE FROM redemptions WHERE session IN (SELECT hash FROM sessions WHERE {THEIRS})"), theirs(identity))?;
        self.exec(&format!("DELETE FROM sessions WHERE {THEIRS}"), theirs(identity))?;
        self.exec(&format!("DELETE FROM keys WHERE {THEIRS}"), theirs(identity))?;
        self.exec(&format!("DELETE FROM oauth_codes WHERE {THEIRS}"), theirs(identity))?;
        self.exec(&format!("DELETE FROM connections WHERE {THEIRS}"), theirs(identity))?;
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
        self.exec("DELETE FROM usernames WHERE identity = ?", vec![identity.into()])?;
        self.exec("DELETE FROM pictures WHERE identity = ?", vec![identity.into()])?;
        self.exec("DELETE FROM identities WHERE id IN (SELECT agent FROM wiped_agents WHERE identity = ?) OR id = ?", twice())?;
        let left = self.count(
            "SELECT (SELECT COUNT(*) FROM identities WHERE id = ? OR owner = ?) + (SELECT COUNT(*) FROM subjects WHERE identity = ?)
               + (SELECT COUNT(*) FROM usernames WHERE identity = ?) AS n",
            vec![identity.into(), identity.into(), identity.into(), identity.into()],
        )?;
        assert_eq!(left, 0, "no row names a wiped person");
        Ok(())
    }
}
