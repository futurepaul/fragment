//! The `Registry` cell: fragment's stand-in for finite.computer's BANKS
//! (FIN-11; docs/finite-integration.md). One cell for the fleet holds every
//! identity (a person or an agent), the public keys each has held, and the
//! sign-in subjects that name a person (phase 4 slice B). It never holds a
//! grant or a private key: fragments keep their members, and keys stay with
//! whoever signs.
//!
//! It is asked live about every signed request whose answer depends on who
//! is asking (`/resolve`, `/session`: the router on the control API, a
//! fragment on its site), and a request it cannot answer for is refused
//! (rule 7). One cell keeps each key change in one transaction (an agent is
//! never left keyless, a revoked key never comes back) and costs one hop
//! per request that asks it (two to set a picture, whose bytes are stored
//! between).
//!
//! Inner routes (only the router and fragments reach them):
//!
//!   POST /resolve {key}                   → {id, kind, owner} (401 unknown or revoked)
//!   POST /lookup  {who}                   → {id, kind, owner}: an `id:` or an active key
//!   POST /agents  {owner, key}            register an agent its owner vouches for
//!   POST /keys    {identity?, key, by}    add a key (its proof was checked by the router)
//!   POST /revoke  {identity?, key, by}    revoke one
//!   POST /view    {identity?, by}         the identity, as it or its owner sees it
//!   POST /check   {identity?, key, by}    → {active}: is `key` one of `identity`'s?
//!   POST /username/claim  {by, username}          the asker's username, chosen once
//!   POST /username/lookup {username}              → the facts of whoever holds it
//!   POST /picture/set     {by, sha, mime}         the asker's picture (its bytes are in BLOBS)
//!   POST /test    {down} | {calls} | {hold} | {signins}   dev fleets: answer 503 to
//!                                         everything else, count the calls since the cell
//!                                         started, hold the next call's answer, or count,
//!                                         expire, or sweep sign-in's rows, or expire one session
//!
//! Each route's body and answer are types in `calls.rs`, shared with the
//! askers. A call that acts names who asks (`by`, an `owner`: `calls::By`,
//! a key, a platform session, or a resolved identity), and the Registry
//! resolves them in the same turn as the act: one round trip, and a key
//! revoked a moment before cannot act. `identity` left out is the asker's
//! own (a path's `me`). An `Identity` (`/resolve`, `/lookup`, a session) carries the
//! identity's username, and an agent's carries its owner's (the namespace
//! it makes fragments in). Rows are read into structs: a NOT NULL column
//! is never defaulted, and a row naming an identity that is not there is a
//! host fault, not a 404. A lookup (`/resolve`, `/session`) is one
//! statement: the key's or the session's row joined with its identity and
//! the username it makes fragments under (`username_join!`).
//!
//! People come from sign-in, and browsers hold sessions: `signin.rs`.

use std::cell::Cell;
use std::collections::BTreeMap;

use fragment_core::{npub, registry};
use fragment_proto::{limits, ErrorCode, Identity, IdentityKind, IdentityView, KeyView};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;

/// The identities one `/profiles` answers.
const PROFILES_MAX: usize = 64;

/// Joins an identity's row (`i`) to the username it makes fragments under
/// (`u`): its own, or an agent's owner's. `usernames.identity` is UNIQUE,
/// so the join adds no row. A macro, so each statement that reads an
/// identity with its username stays one literal that spells it the same.
macro_rules! username_join {
    () => {
        "LEFT JOIN usernames u ON u.identity = COALESCE(i.owner, i.id)"
    };
}

pub(crate) mod calls;
mod signin;
use calls::{
    Active, AddKey, By, Call, CheckKey, ClaimUsername, Claimed, FindUsername, Holder, Lookup, Picture, Profile, Profiles, ProfilesAnswer,
    RegisterAgent, Released, ReleaseUsername, Resolve, RevokeKey, SetPicture, TestAnswer, TestHook, View, TEST_HOLD_MAX_MS,
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
DROP TABLE IF EXISTS meta;
CREATE TABLE IF NOT EXISTS usernames (
  username TEXT PRIMARY KEY, identity TEXT NOT NULL UNIQUE, claimed_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pictures (
  identity TEXT PRIMARY KEY, sha TEXT NOT NULL, mime TEXT NOT NULL, set_at INTEGER NOT NULL);
";

#[durable_object]
pub struct RegistryCell {
    state: State,
    env: Env,
    /// The isolate's settings (config.rs: built once per isolate).
    cfg: &'static Config,
    /// Dev fleets' test hook: every call but the hooks' is 503. It lasts
    /// for this cell's life (a restart answers again), and only the hook,
    /// which answers only with `FRAGMENT_TEST_HOOKS=allow`, sets it.
    down: Cell<bool>,
    /// The calls answered (or refused) since this cell started, the hooks'
    /// aside: what a test counts a request's Registry round trips by.
    calls: Cell<u64>,
    /// Dev fleets' test hook: how long the next call waits before it is
    /// answered (0: it does not), set only by the hook.
    hold_ms: Cell<u32>,
}

impl DurableObject for RegistryCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Registry schema applies");
        state.storage().sql().exec(signin::SCHEMA, None).expect("the sign-in schema applies");
        let cfg = Config::from_env(&env);
        assert!(cfg.signins_pending_max >= 1, "a fresh sign-in always fits under the cap");
        RegistryCell { state, env, cfg, down: Cell::new(false), calls: Cell::new(0), hold_ms: Cell::new(0) }
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

/// `identities` (the row for an id), with its username (`username_join!`).
#[derive(Deserialize)]
struct IdentityRow {
    kind: IdentityKind,
    owner: Option<String>,
    username: Option<String>,
}

/// When an identity was made.
#[derive(Deserialize)]
struct CreatedRow {
    created_at: i64,
}

/// A key's row, with its holder's identity and username in the same
/// statement (`/resolve`). The holder's columns are `None` when the key
/// names an identity that is not there.
#[derive(Deserialize)]
struct KeyHolderRow {
    identity: String,
    revoked_at: Option<i64>,
    kind: Option<IdentityKind>,
    owner: Option<String>,
    username: Option<String>,
}

/// The identity a joined row names (a key's holder, a session's): its
/// `identities` columns missing, the rows contradict each other, a host
/// fault.
fn joined_identity(id: String, kind: Option<IdentityKind>, owner: Option<String>, username: Option<String>, named_by: &str) -> CellResult<Identity> {
    match kind {
        Some(kind) => Ok(Identity { id, kind, owner, username }),
        None => Err(CellError::host(format!("{named_by} names a missing identity {id}"))),
    }
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

    /// An identity with its username, in one statement.
    fn identity(&self, id: &str) -> CellResult<Option<Identity>> {
        const Q: &str = concat!("SELECT i.kind, i.owner, u.username FROM identities i ", username_join!(), " WHERE i.id = ?");
        let row = self.row::<IdentityRow>(Q, vec![id.into()])?;
        Ok(row.map(|r| Identity { id: id.to_string(), kind: r.kind, owner: r.owner, username: r.username }))
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

    /// Whoever asks, resolved in this turn (`calls::By`).
    fn by(&self, by: &By) -> CellResult<Identity> {
        match by {
            By::Key(key) => self.key_holder(key),
            By::Session(token) => Ok(self.live_session(token, None)?.1.identity),
            By::Identity(id) => self.named_identity(id),
        }
    }

    /// A person's username, chosen once: taken names and reserved words are refused.
    fn claim_username(&self, b: ClaimUsername) -> CellResult<Claimed> {
        let who = self.by(&b.by)?;
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
        let who = self.by(&b.by)?;
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

    /// The identity holding a key, in one statement: the key's row joined
    /// with its holder and the holder's username.
    fn key_holder(&self, key: &str) -> CellResult<Identity> {
        const Q: &str = concat!(
            "SELECT k.identity, k.revoked_at, i.kind, i.owner, u.username FROM keys k LEFT JOIN identities i ON i.id = k.identity ",
            username_join!(),
            " WHERE k.key = ?"
        );
        check_key(key)?;
        match self.row::<KeyHolderRow>(Q, vec![key.into()])? {
            None => Err(unauthenticated(format!(
                "the key {} belongs to no one on this fleet (add it to you: `fragment login`)",
                npub::encode(key)
            ))),
            Some(row) if row.revoked_at.is_some() => Err(unauthenticated(format!("the key {} was revoked", npub::encode(key)))),
            Some(row) => joined_identity(row.identity, row.kind, row.owner, row.username, &format!("the key {key}")),
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
            .row::<CreatedRow>("SELECT created_at FROM identities WHERE id = ?", vec![who.id.as_str().into()])?
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
        let owner = self.by(&b.owner)?;
        check_key(&b.key)?;
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

    /// The identity whose keys change (`None`: the asker's own), if `by` manages them.
    fn managed(&self, identity: Option<&str>, by: &Identity) -> CellResult<Identity> {
        let who = match identity {
            Some(id) => self.named_identity(id)?,
            None => by.clone(),
        };
        if !registry::may_manage_keys(&by.id, &who.id, who.kind, who.owner.as_deref()) {
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
        let by = self.by(&b.by)?;
        check_key(&b.key)?;
        let who = self.managed(b.identity.as_deref(), &by)?;
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
            vec![b.key.as_str().into(), who.id.as_str().into(), SqlStorageValue::Integer(js::now_ms()), by.id.as_str().into()],
        )?;
        self.view(&who, Some(true))
    }

    fn revoke(&self, RevokeKey(b): RevokeKey) -> CellResult<IdentityView> {
        let by = self.by(&b.by)?;
        check_key(&b.key)?;
        let who = self.managed(b.identity.as_deref(), &by)?;
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
        let by = self.by(&b.by)?;
        let who = match &b.identity {
            Some(id) => self.named_identity(id)?,
            None => by.clone(),
        };
        if !registry::may_view(&by.id, &who.id, who.owner.as_deref()) {
            return Err(CellError::new(ErrorCode::NotFound, format!("no identity {}", who.id)));
        }
        self.view(&who, None)
    }

    fn check(&self, CheckKey(b): CheckKey) -> CellResult<Active> {
        let by = self.by(&b.by)?;
        check_key(&b.key)?;
        let identity = b.identity.unwrap_or_else(|| by.id.clone());
        if !registry::may_check_key(&by.id, by.owner.as_deref(), &identity) {
            return Err(CellError::new(ErrorCode::Forbidden, "only an identity and its agents ask about its keys"));
        }
        let active = self.key_row(&b.key)?.is_some_and(|row| row.identity == identity && row.active());
        Ok(Active { active })
    }

    async fn test_hook(&self, hook: TestHook) -> CellResult<TestAnswer> {
        assert!(self.cfg.test_hooks, "the hooks answer only on fleets with test hooks");
        match hook {
            TestHook::Down(down) => {
                self.down.set(down);
                Ok(TestAnswer::Down { down })
            }
            TestHook::Calls => Ok(TestAnswer::Calls { calls: self.calls.get() }),
            TestHook::Hold(ms) => {
                if ms > TEST_HOLD_MAX_MS {
                    return Err(CellError::invalid(format!("hold at most {TEST_HOLD_MAX_MS} ms")));
                }
                self.hold_ms.set(ms);
                Ok(TestAnswer::Hold { hold: ms })
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
            if !self.cfg.test_hooks {
                return Err(CellError::new(ErrorCode::NotFound, "no route /test"));
            }
            return reply::<TestHook>(self.test_hook(body(&bytes)?).await);
        }
        self.calls.set(self.calls.get() + 1);
        let hold = self.hold_ms.replace(0);
        if hold > 0 {
            // other calls are answered meanwhile: the Registry's turn is open
            Delay::from(std::time::Duration::from_millis(hold.into())).await;
        }
        if self.down.get() {
            return Err(CellError::new(ErrorCode::RegistryUnavailable, "the registry is down (a test hook)"));
        }
        if let Some(resp) = self.route_signin(&path, &bytes).await? {
            return Ok(resp);
        }
        match path.as_str() {
            Resolve::PATH => reply::<Resolve>(self.key_holder(&body::<Resolve>(&bytes)?.key)),
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
