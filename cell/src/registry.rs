//! The `Registry` cell: fragment's stand-in for finite.computer's BANKS
//! (FIN-11; docs/finite-integration.md). One cell for the fleet holds every
//! identity (a person or an agent), the public keys each has held, and the
//! sign-in subjects that name a person (phase 4 slice B). It never holds a
//! grant or a private key: fragments keep their members, and keys stay with
//! whoever signs.
//!
//! The router asks it about every signed request (`/resolve`), live, and
//! refuses the request when it cannot answer (rule 7). One cell keeps each
//! key change in one transaction (an agent is never left keyless, a revoked
//! key never comes back) and costs one hop per request.
//!
//! Inner routes (only the router and fragments reach them):
//!
//!   POST /resolve {key}                   → {id, kind, owner} (401 unknown or revoked)
//!   POST /lookup  {who}                   → {id, kind, owner}: an `id:` or an active key
//!   POST /agents  {owner, key}            register an agent its owner vouches for
//!   POST /keys    {identity, key, by}     add a key (its proof was checked by the router)
//!   POST /revoke  {identity, key, by}     revoke one
//!   POST /view    {identity, by}          the identity, as it or its owner sees it
//!   POST /check   {identity, key, by}     → {active}: is `key` one of `identity`'s?
//!   POST /username/claim  {identity, username}    a person's username, chosen once
//!   POST /username/lookup {username}              → the facts of whoever holds it
//!   POST /picture/set     {identity, sha, mime}   a person's picture (its bytes are in BLOBS)
//!   POST /test    {down} | {signins}      dev fleets: answer 503 to everything else, or count,
//!                                         expire, or sweep sign-in's rows
//!
//! Each route's body and answer are types in `calls.rs`, shared with the
//! askers. An `Identity` (`/resolve`, `/lookup`, a session) carries the
//! identity's username, and an agent's carries its owner's (the namespace
//! it makes fragments in). Rows are read into structs: a NOT NULL column
//! is never defaulted, and a row naming an identity that is not there is a
//! host fault, not a 404.
//!
//! People come from sign-in, and browsers hold sessions: `signin.rs`.

use std::collections::BTreeMap;

use fragment_core::{npub, registry};
use fragment_proto::{limits, ErrorCode, Identity, IdentityKind, IdentityView, KeyView};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use worker::*;

use crate::error::{CellError, CellResult};
use crate::js;

/// The identities one `/profiles` answers.
const PROFILES_MAX: usize = 64;

pub(crate) mod calls;
mod signin;
use calls::{
    Active, AddKey, Call, CheckKey, ClaimUsername, Claimed, FindUsername, Holder, Lookup, Picture, Profile, Profiles, ProfilesAnswer, RegisterAgent,
    Released, ReleaseUsername, Resolve, RevokeKey, SetPicture, TestAnswer, TestHook, View,
};
pub use signin::SESSION_TTL_MS;

/// The fleet's one registry cell.
pub const NAME: &str = "registry";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, owner TEXT, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS identities_owner ON identities (owner) WHERE owner IS NOT NULL;
CREATE TABLE IF NOT EXISTS keys (
  key TEXT PRIMARY KEY, identity TEXT NOT NULL, added_at INTEGER NOT NULL, added_by TEXT NOT NULL, revoked_at INTEGER);
CREATE INDEX IF NOT EXISTS keys_identity ON keys (identity);
CREATE TABLE IF NOT EXISTS subjects (
  issuer TEXT NOT NULL, subject TEXT NOT NULL, identity TEXT NOT NULL, linked_at INTEGER NOT NULL, email TEXT,
  PRIMARY KEY (issuer, subject));
CREATE INDEX IF NOT EXISTS subjects_identity ON subjects (identity);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS usernames (
  username TEXT PRIMARY KEY, identity TEXT NOT NULL UNIQUE, claimed_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pictures (
  identity TEXT PRIMARY KEY, sha TEXT NOT NULL, mime TEXT NOT NULL, set_at INTEGER NOT NULL);
";

#[durable_object]
pub struct RegistryCell {
    state: State,
    env: Env,
    /// `FRAGMENT_SIGNINS_PENDING_MAX`, read once when the cell starts (a
    /// fleet's variables change only with a deploy, which starts it again).
    signins_pending_max: u64,
}

impl DurableObject for RegistryCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Registry schema applies");
        state.storage().sql().exec(signin::SCHEMA, None).expect("the sign-in schema applies");
        let signins_pending_max = crate::config::Config::from_env(&env).signins_pending_max;
        assert!(signins_pending_max >= 1, "a fresh sign-in always fits under the cap");
        RegistryCell { state, env, signins_pending_max }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(resp) => Ok(resp),
            Err(e) => e.response(),
        }
    }

    /// Sign-in's sweep of expired rows (signin.rs), off every request. A
    /// failed sweep is logged and arms the next itself; failing that too,
    /// the alarm fails and the node retries it.
    async fn alarm(&self) -> Result<Response> {
        if let Err(e) = self.sweep_alarm().await {
            console_error!("the registry's sweep failed ({:?}): {}", e.code, e.message);
            self.sweep_later().await.map_err(|e| Error::RustError(e.message))?;
        }
        Response::ok("")
    }
}

/// `identities` (the row for an id).
#[derive(Deserialize)]
struct IdentityRow {
    kind: IdentityKind,
    owner: Option<String>,
    created_at: i64,
}

/// `keys` (the row for a key): who holds it, and whether it was revoked.
#[derive(Deserialize)]
struct KeyRow {
    identity: String,
    revoked_at: Option<i64>,
}

impl KeyRow {
    fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// `keys`, as an identity's view lists them.
#[derive(Deserialize)]
struct KeyViewRow {
    key: String,
    added_at: i64,
    added_by: String,
    revoked_at: Option<i64>,
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Deserialize)]
struct HolderRow {
    identity: String,
}

#[derive(Deserialize)]
struct UsernameRow {
    username: String,
}

#[derive(Deserialize)]
struct CountRow {
    n: u64,
}

#[derive(Deserialize)]
struct MetaRow {
    value: String,
}

fn unauthenticated(m: impl Into<String>) -> CellError {
    CellError::new(ErrorCode::Unauthenticated, m)
}

fn conflict(m: impl Into<String>) -> CellError {
    CellError::new(ErrorCode::AlreadyExists, m)
}

fn check_key(key: &str) -> CellResult<()> {
    if npub::is_hex_key(key) {
        Ok(())
    } else {
        Err(CellError::invalid("a key is 64 lowercase hex characters"))
    }
}

/// A call's body, decoded once.
fn body<C: Call>(bytes: &[u8]) -> CellResult<C> {
    serde_json::from_slice(bytes).map_err(|e| CellError::invalid(format!("body: {e}")))
}

/// A call's answer: the type its `Call` names, so a route cannot answer
/// what its askers do not decode.
fn reply<C: Call>(answer: CellResult<C::Answer>) -> CellResult<Response> {
    Ok(Response::from_json(&answer?)?)
}

impl RegistryCell {
    /// A statement's rows, each read into `T` (a column that does not fit
    /// its field is a host fault).
    fn rows<T: DeserializeOwned>(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<T>> {
        Ok(self.state.storage().sql().exec(q, binds)?.to_array::<T>()?)
    }

    /// The row a key (a primary key, or a unique one) names, if any.
    fn row<T: DeserializeOwned>(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Option<T>> {
        let mut rows = self.rows::<T>(q, binds)?;
        assert!(rows.len() <= 1, "a key names one row: {q}");
        Ok(rows.pop())
    }

    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.state.storage().sql().exec(q, binds)?;
        Ok(())
    }

    /// A `SELECT COUNT(*) AS n`.
    fn count(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<u64> {
        self.row::<CountRow>(q, binds)?.map(|r| r.n).ok_or_else(|| CellError::host(format!("COUNT answered no row: {q}")))
    }

    fn identity(&self, id: &str) -> CellResult<Option<Identity>> {
        let Some(row) = self.row::<IdentityRow>("SELECT kind, owner, created_at FROM identities WHERE id = ?", vec![id.into()])? else {
            return Ok(None);
        };
        let username = self.username_of(row.owner.as_deref().unwrap_or(id))?;
        Ok(Some(Identity { id: id.to_string(), kind: row.kind, owner: row.owner, username }))
    }

    /// An identity a request names: missing, it is 404.
    fn named_identity(&self, id: &str) -> CellResult<Identity> {
        self.identity(id)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no identity {id}")))
    }

    /// An identity a row names (a key's holder, a session's, a username's):
    /// missing, the rows contradict each other, a host fault.
    fn stored_identity(&self, id: &str, named_by: &str) -> CellResult<Identity> {
        self.identity(id)?.ok_or_else(|| CellError::host(format!("{named_by} names a missing identity {id}")))
    }

    fn username_of(&self, id: &str) -> CellResult<Option<String>> {
        Ok(self.row::<UsernameRow>("SELECT username FROM usernames WHERE identity = ?", vec![id.into()])?.map(|r| r.username))
    }

    /// A person's username, chosen once: taken names and reserved words are refused.
    fn claim_username(&self, b: ClaimUsername) -> CellResult<Claimed> {
        let who = self.named_identity(&b.identity)?;
        if who.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "only a person chooses a username (an agent makes fragments under its owner's)"));
        }
        if !fragment_proto::valid_username(&b.username) {
            return Err(CellError::invalid(format!(
                "a username is {}-{} lowercase letters, digits, and single dashes, not starting or ending with one, and not a reserved word",
                limits::USERNAME_MIN_BYTES,
                limits::USERNAME_MAX_BYTES
            )));
        }
        match who.username {
            Some(u) if u == b.username => return Ok(Claimed { username: u, claimed: false }),
            Some(u) => return Err(conflict(format!("you are {u}: a username is chosen once"))),
            None => {}
        }
        if self.row::<HolderRow>("SELECT identity FROM usernames WHERE username = ?", vec![b.username.as_str().into()])?.is_some() {
            return Err(conflict(format!("{} is taken", b.username)));
        }
        self.exec(
            "INSERT INTO usernames (username, identity, claimed_at) VALUES (?, ?, ?)",
            vec![b.username.as_str().into(), who.id.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(Claimed { username: b.username, claimed: true })
    }

    /// An operator's undo of a username taken by mistake: its person may
    /// choose again (the router has checked they own nothing under it).
    fn release_username(&self, b: ReleaseUsername) -> CellResult<Released> {
        let Some(holder) = self.row::<HolderRow>("SELECT identity FROM usernames WHERE username = ?", vec![b.username.as_str().into()])? else {
            return Err(CellError::new(ErrorCode::NotFound, format!("no one is {}", b.username)));
        };
        self.exec("DELETE FROM usernames WHERE username = ?", vec![b.username.as_str().into()])?;
        Ok(Released { username: b.username, identity: holder.identity, released: true })
    }

    /// Whoever holds a username, and their picture.
    fn find_username(&self, b: FindUsername) -> CellResult<Holder> {
        let Some(holder) = self.row::<HolderRow>("SELECT identity FROM usernames WHERE username = ?", vec![b.username.as_str().into()])? else {
            return Err(CellError::new(ErrorCode::NotFound, format!("no one is {}", b.username)));
        };
        let identity = self.stored_identity(&holder.identity, &format!("the username {}", b.username))?;
        let picture = self.picture_of(&holder.identity)?;
        Ok(Holder { identity, picture })
    }

    /// A person's picture, its SHA-256 checked as it was when it was set
    /// (its URLs are built from it).
    fn picture_of(&self, id: &str) -> CellResult<Option<Picture>> {
        let picture = self.row::<Picture>("SELECT sha, mime FROM pictures WHERE identity = ?", vec![id.into()])?;
        if picture.as_ref().is_some_and(|p| p.sha.len() != 64 || !p.sha.bytes().all(|c| c.is_ascii_hexdigit())) {
            return Err(CellError::host(format!("pictures.sha of {id} is not a SHA-256")));
        }
        Ok(picture)
    }

    fn set_picture(&self, b: SetPicture) -> CellResult<Picture> {
        let who = self.named_identity(&b.identity)?;
        if who.kind != IdentityKind::Person || who.username.is_none() {
            return Err(CellError::invalid("choose a username before a picture"));
        }
        if b.sha.len() != 64 || !b.sha.bytes().all(|c| c.is_ascii_hexdigit()) || !matches!(b.mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
            return Err(CellError::invalid("a picture is a PNG, JPEG, WebP, or GIF, named by its SHA-256"));
        }
        self.exec(
            "INSERT INTO pictures (identity, sha, mime, set_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (identity) DO UPDATE SET sha = excluded.sha, mime = excluded.mime, set_at = excluded.set_at",
            vec![who.id.as_str().into(), b.sha.as_str().into(), b.mime.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(Picture { sha: b.sha, mime: b.mime })
    }

    /// The key's row: the identity holding it and whether it was revoked.
    fn key_row(&self, key: &str) -> CellResult<Option<KeyRow>> {
        self.row::<KeyRow>("SELECT identity, revoked_at FROM keys WHERE key = ?", vec![key.into()])
    }

    fn resolve(&self, b: Resolve) -> CellResult<Identity> {
        check_key(&b.key)?;
        match self.key_row(&b.key)? {
            None => Err(unauthenticated(format!(
                "the key {} belongs to no one on this fleet (add it to you: `fragment login`)",
                npub::encode(&b.key)
            ))),
            Some(row) if !row.active() => Err(unauthenticated(format!("the key {} was revoked", npub::encode(&b.key)))),
            Some(row) => self.stored_identity(&row.identity, &format!("the key {}", b.key)),
        }
    }

    /// What anyone may know of identities, as a page shows a name: a
    /// person's username and picture, or that it is someone's agent. An id
    /// the registry does not hold (an anonymous visitor's) is left out.
    fn profiles(&self, b: Profiles) -> CellResult<ProfilesAnswer> {
        if b.ids.len() > PROFILES_MAX {
            return Err(CellError::invalid(format!("at most {PROFILES_MAX} identities at once")));
        }
        let mut profiles = BTreeMap::new();
        // bounded: at most PROFILES_MAX ids, just checked
        for id in b.ids {
            let Some(who) = self.identity(&id)? else { continue };
            let picture = match (who.kind, &who.username) {
                (IdentityKind::Person, Some(u)) => self.picture_of(&id)?.map(|p| format!("/api/users/{u}/picture?v={}", &p.sha[..12])),
                _ => None,
            };
            // an agent's identity carries its owner's username
            profiles.insert(id, Profile { kind: who.kind, username: who.username, picture });
        }
        Ok(ProfilesAnswer { profiles })
    }

    fn view(&self, who: &Identity, created: Option<bool>) -> CellResult<IdentityView> {
        let mut keys = vec![];
        let key_rows = self.rows::<KeyViewRow>(
            "SELECT key, added_at, added_by, revoked_at FROM keys WHERE identity = ? ORDER BY added_at, key",
            vec![who.id.as_str().into()],
        )?;
        // bounded: an identity holds at most KEYS_PER_IDENTITY_MAX keys
        for r in key_rows {
            if !npub::is_hex_key(&r.key) {
                return Err(CellError::host(format!("keys.key {:?} is not 64 hex", r.key)));
            }
            keys.push(KeyView { npub: npub::encode(&r.key), added_at: r.added_at, added_by: r.added_by, revoked_at: r.revoked_at });
        }
        let agents = self
            .rows::<IdRow>("SELECT id FROM identities WHERE owner = ? ORDER BY created_at, id", vec![who.id.as_str().into()])?
            .into_iter()
            .map(|r| r.id)
            .collect();
        let row = self
            .row::<IdentityRow>("SELECT kind, owner, created_at FROM identities WHERE id = ?", vec![who.id.as_str().into()])?
            .ok_or_else(|| CellError::host(format!("the identity {} went missing during its view", who.id)))?;
        // a person's own username (an agent's identity carries its owner's)
        let username = if who.kind == IdentityKind::Person { who.username.clone() } else { None };
        let picture = match &username {
            Some(u) => self.picture_of(&who.id)?.map(|p| format!("/api/users/{u}/picture?v={}", &p.sha[..12])),
            None => None,
        };
        Ok(IdentityView {
            id: who.id.clone(),
            kind: who.kind,
            owner: who.owner.clone(),
            username,
            picture,
            created_at: row.created_at,
            keys,
            agents,
            subjects: self.subjects_of(&who.id)?,
            created,
        })
    }

    /// A new identity holding `key`, in one transaction (a DO turn without
    /// an await between its writes).
    fn make(&self, kind: IdentityKind, owner: Option<&str>, key: &str, by: &str) -> CellResult<Identity> {
        let id = npub::identity(js::random_bytes::<16>());
        let now = js::now_ms();
        let owner_v = owner.map_or(SqlStorageValue::Null, |o| o.into());
        self.exec(
            "INSERT INTO identities (id, kind, owner, created_at) VALUES (?, ?, ?, ?)",
            vec![id.as_str().into(), kind.as_str().into(), owner_v, SqlStorageValue::Integer(now)],
        )?;
        // an identity added its own first key; an agent's owner added its
        let added_by = if by.is_empty() { id.as_str() } else { by };
        self.exec(
            "INSERT INTO keys (key, identity, added_at, added_by) VALUES (?, ?, ?, ?)",
            vec![key.into(), id.as_str().into(), SqlStorageValue::Integer(now), added_by.into()],
        )?;
        let username = owner.map(|o| self.username_of(o)).transpose()?.flatten();
        Ok(Identity { id, kind, owner: owner.map(str::to_string), username })
    }

    fn register_agent(&self, b: RegisterAgent) -> CellResult<IdentityView> {
        check_key(&b.key)?;
        let owner = self.named_identity(&b.owner)?;
        if owner.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "an agent's owner is a person"));
        }
        match self.key_row(&b.key)? {
            Some(row) if !row.active() => Err(conflict("this key was revoked")),
            Some(row) => {
                let holder = self.stored_identity(&row.identity, &format!("the key {}", b.key))?;
                // a replay of the same registration answers the same agent
                if holder.kind == IdentityKind::Agent && holder.owner.as_deref() == Some(owner.id.as_str()) {
                    return self.view(&holder, Some(false));
                }
                Err(conflict("this key already belongs to someone"))
            }
            None => {
                let n = self.count("SELECT COUNT(*) AS n FROM identities WHERE owner = ?", vec![owner.id.as_str().into()])?;
                if n >= limits::AGENTS_PER_OWNER_MAX {
                    return Err(CellError::invalid(format!("a person owns at most {} agents", limits::AGENTS_PER_OWNER_MAX)));
                }
                let agent = self.make(IdentityKind::Agent, Some(&owner.id), &b.key, &owner.id)?;
                self.view(&agent, Some(true))
            }
        }
    }

    fn managed(&self, identity: &str, by: &str) -> CellResult<Identity> {
        let who = self.named_identity(identity)?;
        if !registry::may_manage_keys(by, &who.id, who.kind, who.owner.as_deref()) {
            return Err(CellError::new(
                ErrorCode::Forbidden,
                match who.kind {
                    IdentityKind::Person => "only this person manages their keys",
                    IdentityKind::Agent => "only the agent's owner manages its keys",
                },
            ));
        }
        Ok(who)
    }

    fn add_key(&self, AddKey(b): AddKey) -> CellResult<IdentityView> {
        check_key(&b.key)?;
        let who = self.managed(&b.identity, &b.by)?;
        match self.key_row(&b.key)? {
            Some(row) if row.identity == who.id && row.active() => return self.view(&who, Some(false)),
            Some(row) if row.identity == who.id => return Err(conflict("a revoked key stays revoked; make a new one")),
            Some(_) => return Err(conflict("this key already belongs to someone")),
            None => {}
        }
        let n = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ?", vec![who.id.as_str().into()])?;
        if n >= limits::KEYS_PER_IDENTITY_MAX {
            return Err(CellError::invalid(format!("an identity holds at most {} keys, revoked ones included", limits::KEYS_PER_IDENTITY_MAX)));
        }
        self.exec(
            "INSERT INTO keys (key, identity, added_at, added_by) VALUES (?, ?, ?, ?)",
            vec![b.key.as_str().into(), who.id.as_str().into(), SqlStorageValue::Integer(js::now_ms()), b.by.as_str().into()],
        )?;
        self.view(&who, Some(true))
    }

    fn revoke(&self, RevokeKey(b): RevokeKey) -> CellResult<IdentityView> {
        check_key(&b.key)?;
        let who = self.managed(&b.identity, &b.by)?;
        match self.key_row(&b.key)? {
            Some(row) if row.identity != who.id => return Err(CellError::new(ErrorCode::NotFound, "not one of this identity's keys")),
            None => return Err(CellError::new(ErrorCode::NotFound, "not one of this identity's keys")),
            Some(row) if !row.active() => return self.view(&who, Some(false)),
            Some(_) => {}
        }
        // an agent always keeps a key; a person who signs in may hold none
        let active = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ? AND revoked_at IS NULL", vec![who.id.as_str().into()])?;
        let signs_in = who.kind == IdentityKind::Person && self.signs_in(&who.id)?;
        if active <= 1 && !signs_in {
            return Err(CellError::invalid("an agent keeps at least one key: add the new key before revoking the last"));
        }
        self.exec("UPDATE keys SET revoked_at = ? WHERE key = ?", vec![SqlStorageValue::Integer(js::now_ms()), b.key.as_str().into()])?;
        self.view(&who, Some(true))
    }

    fn lookup(&self, b: Lookup) -> CellResult<Identity> {
        let who = b.who;
        let missing = || CellError::new(ErrorCode::NotFound, format!("{who} names no one on this fleet (they register with `fragment login`)"));
        match npub::parse_named(&who) {
            Some(npub::Named::Identity(id)) => self.identity(&id)?.ok_or_else(missing),
            Some(npub::Named::Key(key)) => match self.key_row(&key)? {
                Some(row) if row.active() => self.stored_identity(&row.identity, &format!("the key {key}")),
                Some(_) => Err(CellError::new(ErrorCode::NotFound, format!("the key {who} was revoked"))),
                None => Err(missing()),
            },
            None => Err(CellError::invalid(format!("{who:?} is not an identity (id:…), an npub, or a 64-hex key"))),
        }
    }

    fn view_for(&self, b: View) -> CellResult<IdentityView> {
        let who = self.named_identity(&b.identity)?;
        if !registry::may_view(&b.by, &who.id, who.owner.as_deref()) {
            return Err(CellError::new(ErrorCode::NotFound, format!("no identity {}", b.identity)));
        }
        self.view(&who, None)
    }

    fn check(&self, CheckKey(b): CheckKey) -> CellResult<Active> {
        check_key(&b.key)?;
        let by = self.named_identity(&b.by)?;
        if !registry::may_check_key(&by.id, by.owner.as_deref(), &b.identity) {
            return Err(CellError::new(ErrorCode::Forbidden, "only an identity and its agents ask about its keys"));
        }
        let active = self.key_row(&b.key)?.is_some_and(|row| row.identity == b.identity && row.active());
        Ok(Active { active })
    }

    fn down(&self) -> CellResult<bool> {
        Ok(self.row::<MetaRow>("SELECT value FROM meta WHERE key = 'down'", vec![])?.is_some_and(|r| r.value == "1"))
    }

    async fn test_hook(&self, hook: TestHook) -> CellResult<TestAnswer> {
        match hook {
            TestHook::Down(down) => {
                self.exec(
                    "INSERT INTO meta (key, value) VALUES ('down', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    vec![if down { "1" } else { "0" }.into()],
                )?;
                Ok(TestAnswer::Down { down })
            }
            TestHook::Signins(hook) => Ok(TestAnswer::Signins(self.signins_hook(hook).await?)),
        }
    }

    /// Each route decodes its call's body and answers its call's answer
    /// (`calls.rs`), so a route and its askers cannot disagree.
    async fn route(&self, mut req: Request) -> CellResult<Response> {
        let path = req.path();
        let bytes = req.bytes().await?;
        if path == TestHook::PATH {
            if self.env.var("FRAGMENT_TEST_HOOKS").map(|v| v.to_string()).ok().as_deref() != Some("allow") {
                return Err(CellError::new(ErrorCode::NotFound, "no route /test"));
            }
            return reply::<TestHook>(self.test_hook(body(&bytes)?).await);
        }
        if self.down()? {
            return Err(CellError::new(ErrorCode::RegistryUnavailable, "the registry is down (a test hook)"));
        }
        if let Some(resp) = self.route_signin(&path, &bytes).await? {
            return Ok(resp);
        }
        match path.as_str() {
            Resolve::PATH => reply::<Resolve>(self.resolve(body(&bytes)?)),
            Lookup::PATH => reply::<Lookup>(self.lookup(body(&bytes)?)),
            RegisterAgent::PATH => reply::<RegisterAgent>(self.register_agent(body(&bytes)?)),
            Profiles::PATH => reply::<Profiles>(self.profiles(body(&bytes)?)),
            AddKey::PATH => reply::<AddKey>(self.add_key(body(&bytes)?)),
            RevokeKey::PATH => reply::<RevokeKey>(self.revoke(body(&bytes)?)),
            View::PATH => reply::<View>(self.view_for(body(&bytes)?)),
            CheckKey::PATH => reply::<CheckKey>(self.check(body(&bytes)?)),
            ClaimUsername::PATH => reply::<ClaimUsername>(self.claim_username(body(&bytes)?)),
            FindUsername::PATH => reply::<FindUsername>(self.find_username(body(&bytes)?)),
            ReleaseUsername::PATH => reply::<ReleaseUsername>(self.release_username(body(&bytes)?)),
            SetPicture::PATH => reply::<SetPicture>(self.set_picture(body(&bytes)?)),
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}"))),
        }
    }
}
