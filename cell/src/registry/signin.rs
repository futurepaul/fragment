//! Sign-in (phase 4 slice B): people come from WorkOS, keyed by their
//! verified `(issuer, subject)`, never by email; browsers hold sessions.
//! A platform session lives on the platform origin; each fragment origin
//! gets its own site session through a single-use redemption the platform
//! mints (finite-sites ADR 0025), so one fragment's cookie means nothing on
//! another. Every session is looked up live on each request, and logging
//! out ends the platform session and every site session made from it.
//! A CLI key joins a person when the person approves it in a signed-in
//! browser: the approval link carries a proof by the key itself (checked
//! by the router), so approving adds the key at once, and the CLI only
//! waits until its key works.
//!
//! Tokens and states are 32 random bytes; the cell keeps their SHA-256.
//!
//! Every sign-in table is bounded: a pending sign-in is kept through the
//! fleet's next `FRAGMENT_SIGNINS_PENDING_MAX` starts (default
//! `limits::SIGNINS_PENDING_MAX_DEFAULT`), and a platform session keeps its
//! newest `REDEMPTIONS_PER_SESSION_MAX` unspent redemptions and its newest
//! `SITE_SESSIONS_PER_FRAGMENT_MAX` sessions on each fragment.
//! Expired rows go in batches on the Registry's alarm, never on a request:
//! an anonymous `/auth/login` must not scan the tables every signed
//! request waits on.
//!
//!   POST /login/begin   {returnTo, linkTo?}                 → {state}
//!   POST /login/exchange {state, code, clientId, issuer}    → {token, id, created, linked, returnTo}:
//!                                                            WorkOS's code exchanged through KEYS
//!                                                            (the API key is the node's), then the
//!                                                            sign-in finished
//!   POST /session       {token, fragment?}                  → {id, kind, owner} (401 when not live)
//!   POST /session/end   {token, fragment}                   → {ended}: a fragment's `__signout`
//!   POST /logout        {token}                             → {workosSid}
//!   POST /redeem/mint   {token, fragment, returnTo}         → {redeem}
//!   POST /redeem        {redeem, fragment}                  → {token, returnTo}
//!   POST /cli/add       {token, key}                        → {id, key, added}: the key joins the
//!                                                            session's person (its proof was checked)

use fragment_proto::Subject;
use serde::de::IgnoredAny;
use sha2::{Digest, Sha256};

use super::calls::{
    ApproveKey, Approved, Began, Begin, EndSession, Ended, Exchange, Exchanged, LoggedOut, Logout, Mint, Minted, Redeem, Redeemed, Session,
    SigninCounts, SigninsHook,
};
use super::*;

const LOGIN_TTL_MS: i64 = 10 * 60 * 1000;
pub const SESSION_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
const REDEEM_TTL_MS: i64 = 60 * 1000;
/// Subjects one person may link, and an email's length.
const SUBJECTS_MAX: u64 = 8;
const EMAIL_MAX: usize = 320;
/// The sweep of expired rows: at most this many from each table a run, a
/// run this long after the earliest row expires (so one run takes many),
/// and the next run this soon when a table held more than a batch.
const SWEEP_BATCH: u64 = 256;
const SWEEP_SLACK_MS: i64 = 60 * 1000;
const SWEEP_AGAIN_MS: i64 = 1000;

const _: () = assert!(limits::SIGNINS_PENDING_MAX_DEFAULT >= 1 && limits::SITE_SESSIONS_PER_FRAGMENT_MAX >= 1 && limits::REDEMPTIONS_PER_SESSION_MAX >= 1);

/// The expiry columns are indexed for the sweep, and a site session's
/// `(parent, fragment, created_at)` for its bound (that index replaced the
/// parent-only one the fleet made first).
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS logins (
  state TEXT PRIMARY KEY, return_to TEXT NOT NULL, link_to TEXT, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS logins_created ON logins (created_at);
CREATE TABLE IF NOT EXISTS sessions (
  hash TEXT PRIMARY KEY, identity TEXT NOT NULL, fragment TEXT, parent TEXT, workos_sid TEXT,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, revoked_at INTEGER);
DROP INDEX IF EXISTS sessions_parent;
CREATE INDEX IF NOT EXISTS sessions_parent_fragment ON sessions (parent, fragment, created_at) WHERE parent IS NOT NULL;
CREATE INDEX IF NOT EXISTS sessions_expires ON sessions (expires_at);
CREATE TABLE IF NOT EXISTS redemptions (
  hash TEXT PRIMARY KEY, session TEXT NOT NULL, fragment TEXT NOT NULL, return_to TEXT NOT NULL,
  expires_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS redemptions_session ON redemptions (session, expires_at);
CREATE INDEX IF NOT EXISTS redemptions_expires ON redemptions (expires_at);
";

fn sha(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn fresh_token() -> String {
    hex::encode(js::random_bytes::<32>())
}

fn well_formed(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn not_signed_in() -> CellError {
    CellError::new(ErrorCode::Unauthenticated, "not signed in (the session ended or never began)")
}

/// What the sign-in reads of WorkOS's answer to a code.
#[derive(Deserialize)]
struct Authenticated {
    user: WorkOsUser,
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct WorkOsUser {
    id: String,
    email: Option<String>,
}

/// WorkOS's refusal, as far as it says why.
#[derive(Deserialize)]
struct Refusal {
    error_description: Option<String>,
    message: Option<String>,
}

/// The claim of WorkOS's access token the sign-out needs.
#[derive(Deserialize)]
struct Claims {
    sid: Option<String>,
}

/// The `sid` claim of an access token WorkOS answered the exchange with
/// (read, not trusted from a browser: it came from WorkOS over TLS).
fn sid_of(access_token: &str) -> Option<String> {
    use base64::Engine;
    let payload = access_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Claims>(&bytes).ok()?.sid
}

pub(super) struct Finish {
    state: String,
    issuer: String,
    subject: String,
    email: String,
    workos_sid: Option<String>,
}

/// A live session's row, with whether its parent is live and its
/// identity with its username, all in one statement (`live_session`).
#[derive(Deserialize)]
struct SessionRow {
    identity: String,
    fragment: Option<String>,
    parent: Option<String>,
    /// The parent's hash when the parent is live too.
    parent_live: Option<String>,
    kind: Option<IdentityKind>,
    owner: Option<String>,
    username: Option<String>,
}

/// `logins` (a pending sign-in, spent).
#[derive(Deserialize)]
struct LoginRow {
    return_to: String,
    link_to: Option<String>,
}

/// `redemptions` (one spent).
#[derive(Deserialize)]
struct RedemptionRow {
    session: String,
    return_to: String,
}

/// A live platform session, as a redemption spends it.
#[derive(Deserialize)]
struct ParentRow {
    identity: String,
    expires_at: i64,
}

#[derive(Deserialize)]
struct SidRow {
    workos_sid: Option<String>,
}

#[derive(Deserialize)]
struct RowidRow {
    n: i64,
}

/// The earliest expiry in each sign-in table (`None`: it is empty).
#[derive(Deserialize)]
struct EarliestRow {
    login: Option<i64>,
    redeem: Option<i64>,
    session: Option<i64>,
}

/// An alarm at `at_ms`, a time since the epoch.
fn alarm_at(at_ms: i64) -> ScheduledTime {
    ScheduledTime::new(js_sys::Date::new(&worker::wasm_bindgen::JsValue::from_f64(at_ms as f64)))
}

impl RegistryCell {
    /// Deletes a batch of expired rows from each sign-in table (by their
    /// indexed expiry); answers whether any table held more.
    fn sweep_signin(&self, now: i64) -> CellResult<bool> {
        let batch = SqlStorageValue::Integer(SWEEP_BATCH as i64);
        // only how many went matters: each deleted row is read as nothing
        let swept = [
            self.rows::<IgnoredAny>(
                "DELETE FROM logins WHERE state IN (SELECT state FROM logins WHERE created_at <= ? LIMIT ?) RETURNING state",
                vec![SqlStorageValue::Integer(now - LOGIN_TTL_MS), batch.clone()],
            )?
            .len(),
            self.rows::<IgnoredAny>(
                "DELETE FROM redemptions WHERE hash IN (SELECT hash FROM redemptions WHERE expires_at <= ? LIMIT ?) RETURNING hash",
                vec![SqlStorageValue::Integer(now), batch.clone()],
            )?
            .len(),
            self.rows::<IgnoredAny>(
                "DELETE FROM sessions WHERE hash IN (SELECT hash FROM sessions WHERE expires_at <= ? LIMIT ?) RETURNING hash",
                vec![SqlStorageValue::Integer(now), batch],
            )?
            .len(),
        ];
        assert!(swept.iter().all(|n| *n as u64 <= SWEEP_BATCH), "a sweep deletes at most a batch from a table");
        Ok(swept.iter().any(|n| *n as u64 == SWEEP_BATCH))
    }

    /// When the next sweep is due: a slack after the earliest row in any
    /// sign-in table expires (`None`: they are empty). Each is an indexed MIN.
    fn sweep_due(&self) -> CellResult<Option<i64>> {
        let r = self
            .row::<EarliestRow>(
                "SELECT (SELECT MIN(created_at) FROM logins) AS login, (SELECT MIN(expires_at) FROM redemptions) AS redeem,
                        (SELECT MIN(expires_at) FROM sessions) AS session",
                vec![],
            )?
            .ok_or_else(|| CellError::host("the sweep's MIN answered no row"))?;
        let earliest = [r.login.map(|c| c + LOGIN_TTL_MS), r.redeem, r.session].into_iter().flatten().min();
        Ok(earliest.map(|at| at + SWEEP_SLACK_MS))
    }

    /// Arms the sweep to run by the time a new row expires (and its slack),
    /// unless it is armed sooner already: one read of the alarm, and most
    /// calls change nothing. Called before a row's writes, so a failure
    /// leaves no row unswept.
    async fn sweep_by(&self, expires_at: i64) -> CellResult<()> {
        let due = expires_at + SWEEP_SLACK_MS;
        let storage = self.state.storage();
        match storage.get_alarm().await? {
            Some(armed) if armed <= due => Ok(()),
            _ => Ok(storage.set_alarm(alarm_at(due)).await?),
        }
    }

    /// The Registry's alarm: a sweep, then the next one armed (soon when a
    /// table held more than a batch, else by the earliest expiry).
    pub(super) async fn sweep_alarm(&self) -> CellResult<()> {
        let now = js::now_ms();
        let more = self.sweep_signin(now)?;
        let next = if more { Some(now + SWEEP_AGAIN_MS) } else { self.sweep_due()? };
        if let Some(due) = next {
            self.state.storage().set_alarm(alarm_at(due.max(now + SWEEP_AGAIN_MS))).await?;
        }
        Ok(())
    }

    /// After a failed sweep: the next one, a slack from now, so a failure
    /// never ends the sweeping.
    pub(super) async fn sweep_later(&self) -> CellResult<()> {
        Ok(self.state.storage().set_alarm(alarm_at(js::now_ms() + SWEEP_SLACK_MS)).await?)
    }

    fn signin_counts(&self) -> CellResult<SigninCounts> {
        self.row::<SigninCounts>(
            "SELECT (SELECT COUNT(*) FROM logins) AS logins, (SELECT COUNT(*) FROM redemptions) AS redemptions,
                    (SELECT COUNT(*) FROM sessions) AS sessions",
            vec![],
        )?
        .ok_or_else(|| CellError::host("COUNT answered no row"))
    }

    pub(super) async fn signins_hook(&self, hook: SigninsHook) -> CellResult<SigninCounts> {
        match hook {
            SigninsHook::Count => {}
            SigninsHook::Expire => {
                let now = js::now_ms();
                self.exec("UPDATE logins SET created_at = ?", vec![SqlStorageValue::Integer(now - LOGIN_TTL_MS)])?;
                self.exec("UPDATE redemptions SET expires_at = ?", vec![SqlStorageValue::Integer(now)])?;
            }
            SigninsHook::Sweep => self.state.storage().set_alarm(alarm_at(js::now_ms())).await?,
        }
        self.signin_counts()
    }

    /// The live session a token names: not revoked, not expired, for this
    /// fragment (`None`: a platform session), its parent live too. Answers
    /// the session's hash and its identity.
    fn live_session(&self, token: &str, fragment: Option<&str>) -> CellResult<(String, Identity)> {
        // one statement: the session (its hash is the key), its parent (by
        // its hash), its identity, and that identity's username
        const Q: &str = concat!(
            "SELECT s.identity, s.fragment, s.parent, p.hash AS parent_live, i.kind, i.owner, u.username FROM sessions s ",
            "LEFT JOIN sessions p ON p.hash = s.parent AND p.revoked_at IS NULL AND p.expires_at > ? ",
            "LEFT JOIN identities i ON i.id = s.identity ",
            username_join!(),
            " WHERE s.hash = ? AND s.revoked_at IS NULL AND s.expires_at > ?"
        );
        if !well_formed(token) {
            return Err(not_signed_in());
        }
        let hash = sha(token);
        let now = SqlStorageValue::Integer(js::now_ms());
        let row = self.row::<SessionRow>(Q, vec![now.clone(), hash.as_str().into(), now])?.ok_or_else(not_signed_in)?;
        if row.fragment.as_deref() != fragment {
            return Err(not_signed_in());
        }
        // a site session lives only while the platform session it came from does
        if row.parent.is_some() && row.parent_live.is_none() {
            return Err(not_signed_in());
        }
        let who = joined_identity(row.identity, row.kind, row.owner, row.username, "a session")?;
        Ok((hash, who))
    }

    fn new_session(&self, identity: &str, fragment: Option<&str>, parent: Option<&str>, sid: Option<&str>, expires_at: i64) -> CellResult<String> {
        let token = fresh_token();
        let opt = |v: Option<&str>| v.map_or(SqlStorageValue::Null, |s| s.into());
        self.exec(
            "INSERT INTO sessions (hash, identity, fragment, parent, workos_sid, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            vec![
                sha(&token).into(),
                identity.into(),
                opt(fragment),
                opt(parent),
                opt(sid),
                SqlStorageValue::Integer(js::now_ms()),
                SqlStorageValue::Integer(expires_at),
            ],
        )?;
        Ok(token)
    }

    pub(super) async fn begin(&self, b: Begin) -> CellResult<Began> {
        let link_to = match &b.link_to {
            Some(token) => Some(self.live_session(token, None)?.1),
            None => None,
        };
        if let Some(p) = &link_to {
            if p.kind != IdentityKind::Person {
                return Err(CellError::new(ErrorCode::Forbidden, "only a person links a sign-in"));
            }
        }
        let now = js::now_ms();
        self.sweep_by(now + LOGIN_TTL_MS).await?;
        let state = fresh_token();
        let n = self
            .row::<RowidRow>(
                "INSERT INTO logins (state, return_to, link_to, created_at) VALUES (?, ?, ?, ?) RETURNING rowid AS n",
                vec![
                    sha(&state).into(),
                    b.return_to.as_str().into(),
                    link_to.map_or(SqlStorageValue::Null, |p| p.id.into()),
                    SqlStorageValue::Integer(now),
                ],
            )?
            .ok_or_else(|| CellError::host("a sign-in's insert answered no rowid"))?
            .n;
        // Bounded: a new row's rowid is one past the largest left, so the
        // rows within the cap's rowids of it are the newest starts, and the
        // older go (this one never: the cap is at least 1). A range of the
        // rowid, one row a start while a flood lasts; ranking the pending
        // rows instead walked all of them on every start once a flood had
        // filled the table.
        let cap = i64::try_from(self.cfg.signins_pending_max).expect("the cap fits a rowid");
        self.exec("DELETE FROM logins WHERE rowid <= ?", vec![SqlStorageValue::Integer(n.saturating_sub(cap))])?;
        Ok(Began { state })
    }

    /// WorkOS's code, exchanged by `KEYS` for the Registry, then the sign-in
    /// finished. A code is single-use at WorkOS; the state here.
    pub(super) async fn exchange(&self, b: Exchange) -> CellResult<Exchanged> {
        if b.state.is_empty() || b.code.is_empty() || b.client_id.is_empty() {
            return Err(CellError::invalid("an exchange names its state, code, and client"));
        }
        let (status, answer) = crate::keys::workos_authenticate(&self.env, &b.client_id, &b.code).await?;
        if status != 200 {
            let refusal = serde_json::from_value::<Refusal>(answer).ok();
            let why = refusal.and_then(|r| r.error_description.or(r.message)).unwrap_or_else(|| "no reason given".into());
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS refused the sign-in ({status}): {why}")));
        }
        let signed_in = serde_json::from_value::<Authenticated>(answer)
            .ok()
            .filter(|a| !a.user.id.is_empty())
            .ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, "WorkOS answered no user id"))?;
        self.sweep_by(js::now_ms() + SESSION_TTL_MS).await?;
        self.finish(Finish {
            state: b.state,
            issuer: b.issuer,
            subject: signed_in.user.id,
            email: signed_in.user.email.unwrap_or_default(),
            workos_sid: signed_in.access_token.as_deref().and_then(sid_of),
        })
    }

    fn finish(&self, b: Finish) -> CellResult<Exchanged> {
        if b.issuer.is_empty() || b.subject.is_empty() || b.email.len() > EMAIL_MAX {
            return Err(CellError::invalid("a sign-in names its issuer and subject"));
        }
        let login = self
            .row::<LoginRow>(
                "DELETE FROM logins WHERE state = ? AND created_at > ? RETURNING return_to, link_to",
                vec![sha(&b.state).into(), SqlStorageValue::Integer(js::now_ms() - LOGIN_TTL_MS)],
            )?
            .ok_or_else(|| CellError::invalid("this sign-in expired or was used; start again"))?;
        let now = SqlStorageValue::Integer(js::now_ms());
        let known = self
            .row::<HolderRow>("SELECT identity FROM subjects WHERE issuer = ? AND subject = ?", vec![b.issuer.as_str().into(), b.subject.as_str().into()])?
            .map(|r| r.identity);
        let (id, created, linked) = match (login.link_to.as_deref(), known) {
            // linking: explicit, from a signed-in session, never by email
            (Some(to), Some(owner)) if owner == to => (owner, false, false),
            (Some(_), Some(_)) => return Err(conflict("that sign-in already belongs to someone else")),
            (Some(to), None) => {
                if self.count("SELECT COUNT(*) AS n FROM subjects WHERE identity = ?", vec![to.into()])? >= SUBJECTS_MAX {
                    return Err(CellError::invalid(format!("a person links at most {SUBJECTS_MAX} sign-ins")));
                }
                (to.to_string(), false, true)
            }
            (None, Some(owner)) => (owner, false, false),
            (None, None) => {
                let id = npub::identity(js::random_bytes::<16>());
                self.exec(
                    "INSERT INTO identities (id, kind, owner, created_at) VALUES (?, 'person', NULL, ?)",
                    vec![id.as_str().into(), now.clone()],
                )?;
                (id, true, false)
            }
        };
        // the email is an attribute, refreshed at each sign-in and never matched
        self.exec(
            "INSERT INTO subjects (issuer, subject, identity, linked_at, email) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (issuer, subject) DO UPDATE SET email = excluded.email",
            vec![b.issuer.as_str().into(), b.subject.as_str().into(), id.as_str().into(), now, b.email.as_str().into()],
        )?;
        let token = self.new_session(&id, None, None, b.workos_sid.as_deref(), js::now_ms() + SESSION_TTL_MS)?;
        Ok(Exchanged { token, id, created, linked, return_to: login.return_to })
    }

    pub(super) fn session(&self, b: Session) -> CellResult<Identity> {
        Ok(self.live_session(&b.token, b.fragment.as_deref())?.1)
    }

    /// A fragment's `__signout`: the site session its cookie carries ends
    /// (its row goes; the platform session and its other sessions stay).
    /// Answers whether there was one, so a second sign-out is no error.
    pub(super) fn end_site_session(&self, b: EndSession) -> CellResult<Ended> {
        if !well_formed(&b.token) {
            return Ok(Ended { ended: false });
        }
        let ended = self.row::<IgnoredAny>("DELETE FROM sessions WHERE hash = ? AND fragment = ? RETURNING hash", vec![sha(&b.token).into(), b.fragment.as_str().into()])?;
        Ok(Ended { ended: ended.is_some() })
    }

    pub(super) fn logout(&self, b: Logout) -> CellResult<LoggedOut> {
        let (hash, _) = self.live_session(&b.token, None)?;
        let now = SqlStorageValue::Integer(js::now_ms());
        let sid = self
            .row::<SidRow>("SELECT workos_sid FROM sessions WHERE hash = ?", vec![hash.as_str().into()])?
            .ok_or_else(|| CellError::host("a live session went missing during its logout"))?;
        self.exec("UPDATE sessions SET revoked_at = ? WHERE (hash = ? OR parent = ?) AND revoked_at IS NULL", vec![now, hash.as_str().into(), hash.as_str().into()])?;
        Ok(LoggedOut { workos_sid: sid.workos_sid })
    }

    pub(super) async fn mint(&self, b: Mint) -> CellResult<Minted> {
        let (hash, _) = self.live_session(&b.token, None)?;
        let expires_at = js::now_ms() + REDEEM_TTL_MS;
        self.sweep_by(expires_at).await?;
        let redeem = fresh_token();
        let redeem_hash = sha(&redeem);
        self.exec(
            "INSERT INTO redemptions (hash, session, fragment, return_to, expires_at) VALUES (?, ?, ?, ?, ?)",
            vec![
                redeem_hash.as_str().into(),
                hash.as_str().into(),
                b.fragment.as_str().into(),
                b.return_to.as_str().into(),
                SqlStorageValue::Integer(expires_at),
            ],
        )?;
        // bounded: the session keeps this redemption and its newest others (a
        // browser opening several fragments at once needs a few, never more)
        self.exec(
            "DELETE FROM redemptions WHERE hash IN (SELECT hash FROM redemptions WHERE session = ? AND hash != ? ORDER BY expires_at DESC LIMIT -1 OFFSET ?)",
            vec![hash.as_str().into(), redeem_hash.as_str().into(), SqlStorageValue::Integer(limits::REDEMPTIONS_PER_SESSION_MAX as i64 - 1)],
        )?;
        Ok(Minted { redeem })
    }

    /// A redemption spent on its fragment: a site session, expiring with
    /// its parent (whose row already armed the sweep for that time).
    pub(super) fn redeem(&self, b: Redeem) -> CellResult<Redeemed> {
        let refused = || CellError::new(ErrorCode::Unauthenticated, "this sign-in link expired, was used, or is for another fragment; sign in again");
        if !well_formed(&b.redeem) {
            return Err(refused());
        }
        // shown to another fragment, it is refused and stays unspent
        let row = self
            .row::<RedemptionRow>(
                "DELETE FROM redemptions WHERE hash = ? AND fragment = ? AND expires_at > ? RETURNING session, return_to",
                vec![sha(&b.redeem).into(), b.fragment.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
            )?
            .ok_or_else(refused)?;
        let parent = row.session.as_str();
        let p = self
            .row::<ParentRow>(
                "SELECT identity, expires_at FROM sessions WHERE hash = ? AND fragment IS NULL AND revoked_at IS NULL AND expires_at > ?",
                vec![parent.into(), SqlStorageValue::Integer(js::now_ms())],
            )?
            .ok_or_else(refused)?;
        let token = self.new_session(&p.identity, Some(&b.fragment), Some(parent), None, p.expires_at)?;
        // bounded: this session and the newest others on this fragment are
        // kept, the oldest end (a browser holds one cookie an origin)
        self.exec(
            "DELETE FROM sessions WHERE hash IN (SELECT hash FROM sessions WHERE parent = ? AND fragment = ? AND hash != ? ORDER BY created_at DESC LIMIT -1 OFFSET ?)",
            vec![
                parent.into(),
                b.fragment.as_str().into(),
                sha(&token).into(),
                SqlStorageValue::Integer(limits::SITE_SESSIONS_PER_FRAGMENT_MAX as i64 - 1),
            ],
        )?;
        Ok(Redeemed { token, return_to: row.return_to })
    }

    /// A key the signed-in person approved joins them (the router checked
    /// the key's own proof in the approval link).
    pub(super) fn add_by_session(&self, b: ApproveKey) -> CellResult<Approved> {
        check_key(&b.key)?;
        let (_, person) = self.live_session(&b.token, None)?;
        match self.key_row(&b.key)? {
            Some(row) if row.identity == person.id && row.active() => return Ok(Approved { id: person.id, key: npub::encode(&b.key), added: false }),
            Some(_) => return Err(conflict("this key already belongs to someone (or was revoked)")),
            None => {}
        }
        let n = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ?", vec![person.id.as_str().into()])?;
        if n >= limits::KEYS_PER_IDENTITY_MAX {
            return Err(CellError::invalid(format!("an identity holds at most {} keys, revoked ones included", limits::KEYS_PER_IDENTITY_MAX)));
        }
        self.exec(
            "INSERT INTO keys (key, identity, added_at, added_by) VALUES (?, ?, ?, ?)",
            vec![b.key.as_str().into(), person.id.as_str().into(), SqlStorageValue::Integer(js::now_ms()), person.id.as_str().into()],
        )?;
        Ok(Approved { id: person.id, key: npub::encode(&b.key), added: true })
    }

    /// Whether a person signs in (and so may hold no key).
    pub(super) fn signs_in(&self, id: &str) -> CellResult<bool> {
        Ok(self.count("SELECT COUNT(*) AS n FROM subjects WHERE identity = ?", vec![id.into()])? > 0)
    }

    /// A person's sign-ins (at most `SUBJECTS_MAX`), read straight into
    /// the wire type.
    pub(super) fn subjects_of(&self, id: &str) -> CellResult<Vec<Subject>> {
        self.rows::<Subject>("SELECT issuer, email, linked_at AS linkedAt FROM subjects WHERE identity = ? ORDER BY linked_at", vec![id.into()])
    }

    pub(super) async fn route_signin(&self, path: &str, bytes: &[u8]) -> CellResult<Option<Response>> {
        Ok(Some(match path {
            Begin::PATH => reply::<Begin>(self.begin(body(bytes)?).await)?,
            Exchange::PATH => reply::<Exchange>(self.exchange(body(bytes)?).await)?,
            Session::PATH => reply::<Session>(self.session(body(bytes)?))?,
            EndSession::PATH => reply::<EndSession>(self.end_site_session(body(bytes)?))?,
            Logout::PATH => reply::<Logout>(self.logout(body(bytes)?))?,
            Mint::PATH => reply::<Mint>(self.mint(body(bytes)?).await)?,
            Redeem::PATH => reply::<Redeem>(self.redeem(body(bytes)?))?,
            ApproveKey::PATH => reply::<ApproveKey>(self.add_by_session(body(bytes)?))?,
            _ => return Ok(None),
        }))
    }
}
