//! Sign-in (phase 4 slice B): people come from WorkOS, keyed by their
//! verified `(issuer, subject)`, never by email; browsers hold sessions.
//! A platform session lives on the platform origin; each fragment origin
//! gets its own site session through a single-use redemption the platform
//! mints (finite-sites ADR 0025), so one fragment's cookie means nothing on
//! another. Every session is looked up live on each request, and logging
//! out ends the platform session and every site session made from it.
//! A CLI key joins a person when the person approves it in a signed-in
//! browser and the key then claims the approval (its signature is the
//! proof of possession).
//!
//! Tokens and states are 32 random bytes; the cell keeps their SHA-256.
//!
//!   POST /login/begin   {returnTo, linkTo?}                 → {state}
//!   POST /login/finish  {state, issuer, subject, email, workosSid?} → {token, id, created, linked, returnTo}
//!   POST /session       {token, fragment?}                  → {id, kind, owner} (401 when not live)
//!   POST /logout        {token}                             → {workosSid}
//!   POST /redeem/mint   {token, fragment, returnTo}         → {redeem}
//!   POST /redeem        {redeem, fragment}                  → {token, returnTo}
//!   POST /cli/approve   {token, key}                        → {id, key}
//!   POST /cli/claim     {key}                               → {id, claimed} (404 until approved)

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::*;

const LOGIN_TTL_MS: i64 = 10 * 60 * 1000;
pub const SESSION_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
const REDEEM_TTL_MS: i64 = 60 * 1000;
const APPROVAL_TTL_MS: i64 = 10 * 60 * 1000;
/// Subjects one person may link, and an email's length.
const SUBJECTS_MAX: u64 = 8;
const EMAIL_MAX: usize = 320;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS logins (
  state TEXT PRIMARY KEY, return_to TEXT NOT NULL, link_to TEXT, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
  hash TEXT PRIMARY KEY, identity TEXT NOT NULL, fragment TEXT, parent TEXT, workos_sid TEXT,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, revoked_at INTEGER);
CREATE INDEX IF NOT EXISTS sessions_parent ON sessions (parent) WHERE parent IS NOT NULL;
CREATE TABLE IF NOT EXISTS redemptions (
  hash TEXT PRIMARY KEY, session TEXT NOT NULL, fragment TEXT NOT NULL, return_to TEXT NOT NULL,
  expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS approvals (
  key TEXT PRIMARY KEY, identity TEXT NOT NULL, session TEXT NOT NULL, expires_at INTEGER NOT NULL);
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Begin {
    return_to: String,
    link_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Finish {
    state: String,
    issuer: String,
    subject: String,
    email: String,
    workos_sid: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SessionBody {
    token: String,
    fragment: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Mint {
    token: String,
    fragment: String,
    return_to: String,
}

#[derive(Deserialize)]
pub(super) struct Redeem {
    redeem: String,
    fragment: String,
}

#[derive(Deserialize)]
pub(super) struct Approve {
    token: String,
    key: String,
}

impl RegistryCell {
    fn sweep_signin(&self) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec("DELETE FROM logins WHERE created_at < ?", vec![SqlStorageValue::Integer(js::now_ms() - LOGIN_TTL_MS)])?;
        self.exec("DELETE FROM redemptions WHERE expires_at < ?", vec![now.clone()])?;
        self.exec("DELETE FROM approvals WHERE expires_at < ?", vec![now.clone()])?;
        self.exec("DELETE FROM sessions WHERE expires_at < ?", vec![now])
    }

    /// The live session a token names: not revoked, not expired, for this
    /// fragment (`None`: a platform session), its parent live too. Answers
    /// the session's hash and its identity.
    fn live_session(&self, token: &str, fragment: Option<&str>) -> CellResult<(String, Facts)> {
        if !well_formed(token) {
            return Err(not_signed_in());
        }
        let hash = sha(token);
        let now = js::now_ms();
        let rows = self.rows(
            "SELECT identity, fragment, parent FROM sessions WHERE hash = ? AND revoked_at IS NULL AND expires_at > ?",
            vec![hash.as_str().into(), SqlStorageValue::Integer(now)],
        )?;
        let row = rows.first().ok_or_else(not_signed_in)?;
        if row["fragment"].as_str() != fragment {
            return Err(not_signed_in());
        }
        if let Some(parent) = row["parent"].as_str() {
            let live = self.rows(
                "SELECT hash FROM sessions WHERE hash = ? AND revoked_at IS NULL AND expires_at > ?",
                vec![parent.into(), SqlStorageValue::Integer(now)],
            )?;
            if live.is_empty() {
                return Err(not_signed_in());
            }
        }
        let facts = self.must_facts(row["identity"].as_str().unwrap_or(""))?;
        Ok((hash, facts))
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

    pub(super) fn begin(&self, b: Begin) -> CellResult<Value> {
        self.sweep_signin()?;
        let link_to = match &b.link_to {
            Some(token) => Some(self.live_session(token, None)?.1),
            None => None,
        };
        if let Some(p) = &link_to {
            if p.kind != IdentityKind::Person {
                return Err(CellError::new(ErrorCode::Forbidden, "only a person links a sign-in"));
            }
        }
        let state = fresh_token();
        self.exec(
            "INSERT INTO logins (state, return_to, link_to, created_at) VALUES (?, ?, ?, ?)",
            vec![
                sha(&state).into(),
                b.return_to.as_str().into(),
                link_to.map_or(SqlStorageValue::Null, |p| p.id.into()),
                SqlStorageValue::Integer(js::now_ms()),
            ],
        )?;
        Ok(json!({ "state": state }))
    }

    pub(super) fn finish(&self, b: Finish) -> CellResult<Value> {
        if b.issuer.is_empty() || b.subject.is_empty() || b.email.len() > EMAIL_MAX {
            return Err(CellError::invalid("a sign-in names its issuer and subject"));
        }
        let rows = self.rows(
            "DELETE FROM logins WHERE state = ? AND created_at > ? RETURNING return_to, link_to",
            vec![sha(&b.state).into(), SqlStorageValue::Integer(js::now_ms() - LOGIN_TTL_MS)],
        )?;
        let login = rows.first().ok_or_else(|| CellError::invalid("this sign-in expired or was used; start again"))?;
        let return_to = login["return_to"].as_str().unwrap_or("/").to_string();
        let now = SqlStorageValue::Integer(js::now_ms());
        let known = self.rows("SELECT identity FROM subjects WHERE issuer = ? AND subject = ?", vec![b.issuer.as_str().into(), b.subject.as_str().into()])?;
        let known = known.first().and_then(|r| r["identity"].as_str()).map(str::to_string);
        let (id, created, linked) = match (login["link_to"].as_str(), known) {
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
        Ok(json!({ "token": token, "id": id, "created": created, "linked": linked, "returnTo": return_to }))
    }

    pub(super) fn session(&self, b: SessionBody) -> CellResult<Value> {
        Ok(self.live_session(&b.token, b.fragment.as_deref())?.1.json())
    }

    pub(super) fn logout(&self, b: SessionBody) -> CellResult<Value> {
        let (hash, _) = self.live_session(&b.token, None)?;
        let now = SqlStorageValue::Integer(js::now_ms());
        let sid = self.rows("SELECT workos_sid FROM sessions WHERE hash = ?", vec![hash.as_str().into()])?;
        self.exec("UPDATE sessions SET revoked_at = ? WHERE (hash = ? OR parent = ?) AND revoked_at IS NULL", vec![now, hash.as_str().into(), hash.as_str().into()])?;
        Ok(json!({ "workosSid": sid.first().map(|r| r["workos_sid"].clone()).unwrap_or(Value::Null) }))
    }

    pub(super) fn mint(&self, b: Mint) -> CellResult<Value> {
        self.sweep_signin()?;
        let (hash, _) = self.live_session(&b.token, None)?;
        let redeem = fresh_token();
        self.exec(
            "INSERT INTO redemptions (hash, session, fragment, return_to, expires_at) VALUES (?, ?, ?, ?, ?)",
            vec![
                sha(&redeem).into(),
                hash.into(),
                b.fragment.as_str().into(),
                b.return_to.as_str().into(),
                SqlStorageValue::Integer(js::now_ms() + REDEEM_TTL_MS),
            ],
        )?;
        Ok(json!({ "redeem": redeem }))
    }

    pub(super) fn redeem(&self, b: Redeem) -> CellResult<Value> {
        let refused = || CellError::new(ErrorCode::Unauthenticated, "this sign-in link expired, was used, or is for another fragment; sign in again");
        if !well_formed(&b.redeem) {
            return Err(refused());
        }
        // shown to another fragment, it is refused and stays unspent
        let rows = self.rows(
            "DELETE FROM redemptions WHERE hash = ? AND fragment = ? AND expires_at > ? RETURNING session, return_to",
            vec![sha(&b.redeem).into(), b.fragment.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        let row = rows.first().ok_or_else(refused)?;
        let parent = row["session"].as_str().unwrap_or("");
        let live = self.rows(
            "SELECT identity, expires_at FROM sessions WHERE hash = ? AND fragment IS NULL AND revoked_at IS NULL AND expires_at > ?",
            vec![parent.into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        let p = live.first().ok_or_else(refused)?;
        let token = self.new_session(p["identity"].as_str().unwrap_or(""), Some(&b.fragment), Some(parent), None, p["expires_at"].as_i64().unwrap_or(0))?;
        Ok(json!({ "token": token, "returnTo": row["return_to"] }))
    }

    pub(super) fn approve(&self, b: Approve) -> CellResult<Value> {
        self.sweep_signin()?;
        check_key(&b.key)?;
        let (hash, person) = self.live_session(&b.token, None)?;
        match self.key_row(&b.key)? {
            Some((id, false)) if id == person.id => return Ok(json!({ "id": person.id, "key": npub::encode(&b.key), "already": true })),
            Some(_) => return Err(conflict("this key already belongs to someone (or was revoked)")),
            None => {}
        }
        self.exec(
            "INSERT INTO approvals (key, identity, session, expires_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (key) DO UPDATE SET identity = excluded.identity, session = excluded.session, expires_at = excluded.expires_at",
            vec![b.key.as_str().into(), person.id.as_str().into(), hash.into(), SqlStorageValue::Integer(js::now_ms() + APPROVAL_TTL_MS)],
        )?;
        Ok(json!({ "id": person.id, "key": npub::encode(&b.key), "already": false }))
    }

    pub(super) fn claim(&self, key: &str) -> CellResult<Value> {
        check_key(key)?;
        match self.key_row(key)? {
            Some((_, true)) => return Err(unauthenticated(format!("the key {} was revoked", npub::encode(key)))),
            Some((id, false)) => return Ok(json!({ "id": id, "claimed": false })),
            None => {}
        }
        let rows = self.rows(
            "SELECT identity, session FROM approvals WHERE key = ? AND expires_at > ?",
            vec![key.into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        let row = rows.first().ok_or_else(|| CellError::new(ErrorCode::NotFound, "not approved yet"))?;
        let (id, session) = (row["identity"].as_str().unwrap_or("").to_string(), row["session"].as_str().unwrap_or(""));
        let live = self.rows(
            "SELECT hash FROM sessions WHERE hash = ? AND revoked_at IS NULL AND expires_at > ?",
            vec![session.into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        if live.is_empty() {
            return Err(CellError::new(ErrorCode::NotFound, "the approval's session ended; approve again"));
        }
        let n = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ?", vec![id.as_str().into()])?;
        if n >= limits::KEYS_PER_IDENTITY_MAX {
            return Err(CellError::invalid(format!("an identity holds at most {} keys, revoked ones included", limits::KEYS_PER_IDENTITY_MAX)));
        }
        self.exec(
            "INSERT INTO keys (key, identity, added_at, added_by) VALUES (?, ?, ?, ?)",
            vec![key.into(), id.as_str().into(), SqlStorageValue::Integer(js::now_ms()), id.as_str().into()],
        )?;
        self.exec("DELETE FROM approvals WHERE key = ?", vec![key.into()])?;
        Ok(json!({ "id": id, "claimed": true }))
    }

    /// Whether a person signs in (and so may hold no key).
    pub(super) fn signs_in(&self, id: &str) -> CellResult<bool> {
        Ok(self.count("SELECT COUNT(*) AS n FROM subjects WHERE identity = ?", vec![id.into()])? > 0)
    }

    pub(super) fn subjects_of(&self, id: &str) -> CellResult<Vec<Value>> {
        Ok(self
            .rows("SELECT issuer, email, linked_at FROM subjects WHERE identity = ? ORDER BY linked_at", vec![id.into()])?
            .into_iter()
            .map(|r| json!({ "issuer": r["issuer"], "email": r["email"], "linkedAt": r["linked_at"] }))
            .collect())
    }

    pub(super) fn route_signin(&self, path: &str, body: Value) -> CellResult<Option<Value>> {
        Ok(Some(match path {
            "/login/begin" => self.begin(from(body)?)?,
            "/login/finish" => self.finish(from(body)?)?,
            "/session" => self.session(from(body)?)?,
            "/logout" => self.logout(from(body)?)?,
            "/redeem/mint" => self.mint(from(body)?)?,
            "/redeem" => self.redeem(from(body)?)?,
            "/cli/approve" => self.approve(from(body)?)?,
            "/cli/claim" => {
                let b: KeyBody = from(body)?;
                self.claim(&b.key)?
            }
            _ => return Ok(None),
        }))
    }
}
