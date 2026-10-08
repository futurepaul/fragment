//! The `Registry` cell: fragment's BANKS (docs/cloudflare-v1.md, decisions
//! 45 to 50). One cell for the fleet holds every identity (a person or an
//! agent, named by the npub of the key it was made with), the public keys
//! each has held, and the sign-in subjects that name a person, with their
//! verified emails. It never holds a grant: fragments keep their members.
//! The only private key it holds is each person's own, which it makes at
//! their first sign-in and keeps sealed for them (`person_keys`); every
//! other key stays with whoever signs.
//!
//! It is asked live about every signed request whose answer depends on who
//! is asking (`/resolve`, `/session`: the router on the control API, a
//! fragment on its site), and a request it cannot answer for is refused
//! (rule 7). One cell keeps each key change in one transaction (an agent is
//! never left keyless, a revoked key never comes back) and costs one hop
//! per request that asks it (two to set a picture, whose bytes are stored
//! between).
//!
//! Its inner routes (only the router and fragments reach them) are the
//! types in `calls.rs`: each one's path, body, and answer, and dev fleets'
//! `/test` hooks (`calls::TestHook`). `identity` left out of a call is the
//! asker's own (a path's `me`). Rows are read into structs: a NOT NULL
//! column is never defaulted, and a row naming an identity that is not
//! there is a host fault, not a 404.
//!
//! People come from sign-in, and browsers hold sessions: `signin.rs`.
//! An operator's wipe of a person is recorded here, locks them while it
//! runs, and ends with their rows: `wipe.rs`.

use std::cell::Cell;
use std::collections::BTreeMap;

use fragment_core::{blob, npub, registry};
use fragment_proto::{limits, ErrorCode, Identity, IdentityKind, IdentityView, KeyView, Role};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;

/// The identities one `/profiles` answers.
pub(crate) const PROFILES_MAX: usize = 64;

pub(crate) mod calls;
mod signin;
pub(crate) mod wipe;
use calls::{
    Active, AddKey, ApproveKey, Begin, By, Call, CheckKey, EndSession, Exchange, Hold, Logout, Lookup, Mint, Picture, PictureOf, Profile, Profiles,
    ProfilesAnswer, Redeem, RegisterAgent, Resolve, RevokeKey, Session, SetPicture, SubjectOf, TestHook, View, WipeBegin, WipeLook, WipeStep,
    TEST_HOLD_MAX_MS,
};
pub use signin::SESSION_TTL_MS;

/// The fleet's one registry cell.
pub const NAME: &str = "registry";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, owner TEXT, created_at INTEGER NOT NULL, held TEXT);
CREATE INDEX IF NOT EXISTS identities_owner ON identities (owner) WHERE owner IS NOT NULL;
CREATE TABLE IF NOT EXISTS keys (
  key TEXT PRIMARY KEY, identity TEXT NOT NULL, added_at INTEGER NOT NULL, added_by TEXT NOT NULL, revoked_at INTEGER);
CREATE INDEX IF NOT EXISTS keys_identity ON keys (identity);
CREATE TABLE IF NOT EXISTS subjects (
  issuer TEXT NOT NULL, subject TEXT NOT NULL, identity TEXT NOT NULL, linked_at INTEGER NOT NULL, email TEXT NOT NULL,
  signed_in_at INTEGER NOT NULL, PRIMARY KEY (issuer, subject));
CREATE INDEX IF NOT EXISTS subjects_identity ON subjects (identity);
CREATE INDEX IF NOT EXISTS subjects_email ON subjects (email);
CREATE TABLE IF NOT EXISTS person_keys (
  identity TEXT PRIMARY KEY, sealed TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pictures (
  identity TEXT PRIMARY KEY, sha TEXT NOT NULL, mime TEXT NOT NULL, set_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS agent_fragments (
  identity TEXT PRIMARY KEY, fragment TEXT NOT NULL);
";

#[durable_object]
pub struct RegistryCell {
    state: State,
    env: Env,
    /// The isolate's settings (config.rs: built once per isolate).
    cfg: &'static Config,
    /// Dev fleets' test hook: every call but the hooks' is 503. It lasts
    /// for this cell's life (a restart answers again), and only the hook,
    /// which answers only on a fleet with levers (`FRAGMENT_TEST_SECRET`), sets it.
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
        state.storage().sql().exec(wipe::SCHEMA, None).expect("the wipes' schema applies");
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

/// `identities` (the row for an id).
#[derive(Deserialize)]
struct IdentityRow {
    kind: IdentityKind,
    owner: Option<String>,
    held: Option<Role>,
}

#[derive(Deserialize)]
struct CreatedRow {
    created_at: i64,
}

/// A key's row, with its holder's identity in the same statement
/// (`/resolve`). The holder's columns are `None` when the key names an
/// identity that is not there.
#[derive(Deserialize)]
struct KeyHolderRow {
    identity: String,
    revoked_at: Option<i64>,
    kind: Option<IdentityKind>,
    owner: Option<String>,
    held: Option<Role>,
}

/// The identity a joined row names (a key's holder, a session's): its
/// `identities` columns missing, the rows contradict each other, a host
/// fault.
fn joined_identity(id: String, kind: Option<IdentityKind>, owner: Option<String>, held: Option<Role>, named_by: &str) -> CellResult<Identity> {
    match kind {
        Some(kind) => Ok(Identity { id, kind, owner, held }),
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

/// Where a person's picture is served, on the platform's origin: by their
/// identity, its digest's start the cache's key.
fn picture_path(identity: &str, p: &Picture) -> String {
    format!("/api/identities/{identity}/picture?v={}", &p.sha[..12])
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
        let row = self.row::<IdentityRow>("SELECT kind, owner, held FROM identities WHERE id = ?", vec![id.into()])?;
        Ok(row.map(|r| Identity { id: id.to_string(), kind: r.kind, owner: r.owner, held: r.held }))
    }

    /// An identity a request names: missing, it is 404.
    fn named_identity(&self, id: &str) -> CellResult<Identity> {
        self.identity(id)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no identity {id}")))
    }

    /// An identity a row names (a key's holder, a session's, an email's):
    /// missing, the rows contradict each other, a host fault.
    fn stored_identity(&self, id: &str, named_by: &str) -> CellResult<Identity> {
        self.identity(id)?.ok_or_else(|| CellError::host(format!("{named_by} names a missing identity {id}")))
    }

    /// A person's email: their latest sign-in's (signin.rs).
    fn email_of(&self, id: &str) -> CellResult<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            email: String,
        }
        let q = "SELECT email FROM subjects WHERE identity = ? ORDER BY signed_in_at DESC LIMIT 1";
        Ok(self.row::<Row>(q, vec![id.into()])?.map(|r| r.email))
    }

    /// Whoever asks, resolved in this turn (`calls::By`): never a person a
    /// wipe of whom runs, nor an agent of theirs (wipe.rs; their keys and
    /// sessions are gone already, so this refuses an identity the platform
    /// names).
    fn by(&self, by: &By) -> CellResult<Identity> {
        let who = match by {
            By::Key(key) => self.key_holder(key)?,
            By::Session(token) => self.live_session(token, None, false)?.session.identity,
            By::Identity(id) => self.named_identity(id)?,
        };
        self.not_wiping(&who)?;
        Ok(who)
    }

    /// A person's picture, its SHA-256 checked as it was when it was set
    /// (its URLs are built from it).
    fn picture_of(&self, id: &str) -> CellResult<Option<Picture>> {
        let picture = self.row::<Picture>("SELECT sha, mime FROM pictures WHERE identity = ?", vec![id.into()])?;
        if picture.as_ref().is_some_and(|p| !blob::valid_sha(&p.sha)) {
            return Err(CellError::host(format!("pictures.sha of {id} is not a SHA-256")));
        }
        Ok(picture)
    }

    fn set_picture(&self, b: SetPicture) -> CellResult<Picture> {
        let who = self.by(&b.by)?;
        if who.kind != IdentityKind::Person {
            return Err(CellError::invalid("a picture is a person's"));
        }
        if !blob::valid_sha(&b.sha) || !matches!(b.mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
            return Err(CellError::invalid("a picture is a PNG, JPEG, WebP, or GIF, named by its SHA-256"));
        }
        self.exec(
            "INSERT INTO pictures (identity, sha, mime, set_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (identity) DO UPDATE SET sha = excluded.sha, mime = excluded.mime, set_at = excluded.set_at",
            vec![who.id.as_str().into(), b.sha.as_str().into(), b.mime.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(Picture { sha: b.sha, mime: b.mime })
    }

    fn key_row(&self, key: &str) -> CellResult<Option<KeyRow>> {
        self.row::<KeyRow>("SELECT identity, revoked_at FROM keys WHERE key = ?", vec![key.into()])
    }

    /// The identity holding a key, in one statement: the key's row joined
    /// with its holder.
    fn key_holder(&self, key: &str) -> CellResult<Identity> {
        const Q: &str = "SELECT k.identity, k.revoked_at, i.kind, i.owner, i.held FROM keys k LEFT JOIN identities i ON i.id = k.identity WHERE k.key = ?";
        check_key(key)?;
        match self.row::<KeyHolderRow>(Q, vec![key.into()])? {
            None => Err(unauthenticated(format!(
                "the key {} belongs to no one on this fleet (add it to you: `fragment login`)",
                npub::encode(key)
            ))),
            Some(row) if row.revoked_at.is_some() => Err(unauthenticated(format!("the key {} was revoked", npub::encode(key)))),
            Some(row) => joined_identity(row.identity, row.kind, row.owner, row.held, &format!("the key {key}")),
        }
    }

    /// What a page shows of identities as names: a person's picture, and
    /// their email when the caller named them in `emails_of` (it may show
    /// it: calls.rs `Profiles`), or that it is someone's agent (and, made
    /// from an agent fragment, its name and fragment). An id the registry
    /// does not hold (an anonymous visitor's) is left out.
    fn profiles(&self, b: Profiles) -> CellResult<ProfilesAnswer> {
        if b.ids.len() > PROFILES_MAX || b.emails_of.len() > PROFILES_MAX {
            return Err(CellError::invalid(format!("at most {PROFILES_MAX} identities at once")));
        }
        let mut profiles = BTreeMap::new();
        // bounded: at most PROFILES_MAX ids, just checked
        for id in b.ids {
            let Some(who) = self.identity(&id)? else { continue };
            let (picture, email) = match who.kind {
                IdentityKind::Person => (
                    self.picture_of(&id)?.map(|p| picture_path(&id, &p)),
                    if b.emails_of.contains(&id) { self.email_of(&id)? } else { None },
                ),
                IdentityKind::Agent => (None, None),
            };
            let fragment = match who.kind {
                IdentityKind::Agent => self.agent_fragment(&id)?,
                _ => None,
            };
            // an agent's name is its fragment's label (docs/computers.md)
            let name = fragment.as_deref().and_then(fragment_proto::split_fragment_name).map(|(label, _)| label.to_string());
            profiles.insert(id, Profile { kind: who.kind, email, picture, name, fragment, title: None });
        }
        Ok(ProfilesAnswer { profiles })
    }

    /// The agent fragment an agent was made from, if it was.
    fn agent_fragment(&self, id: &str) -> CellResult<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            fragment: String,
        }
        let row = self.row::<Row>("SELECT fragment FROM agent_fragments WHERE identity = ?", vec![id.into()])?;
        if row.as_ref().is_some_and(|r| !fragment_proto::valid_fragment_name(&r.fragment)) {
            return Err(CellError::host(format!("agent_fragments.fragment of {id} is not a fragment's name")));
        }
        Ok(row.map(|r| r.fragment))
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
            .rows::<IdRow>("SELECT id FROM identities WHERE owner = ? AND kind = 'agent' ORDER BY created_at, id", vec![who.id.as_str().into()])?
            .into_iter()
            .map(|r| r.id)
            .collect();
        let row = self
            .row::<CreatedRow>("SELECT created_at FROM identities WHERE id = ?", vec![who.id.as_str().into()])?
            .ok_or_else(|| CellError::host(format!("the identity {} went missing during its view", who.id)))?;
        let (email, picture) = match who.kind {
            IdentityKind::Person => (self.email_of(&who.id)?, self.picture_of(&who.id)?.map(|p| picture_path(&who.id, &p))),
            IdentityKind::Agent => (None, None),
        };
        Ok(IdentityView {
            id: who.id.clone(),
            kind: who.kind,
            owner: who.owner.clone(),
            email,
            picture,
            created_at: row.created_at,
            keys,
            agents,
            subjects: self.subjects_of(&who.id)?,
            created,
        })
    }

    /// A new identity holding `key`, named by it, in one transaction (a DO
    /// turn without an await between its writes).
    fn make(&self, kind: IdentityKind, owner: Option<&str>, key: &str, by: &str) -> CellResult<Identity> {
        let id = npub::identity_of(key);
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
        Ok(Identity { id, kind, owner: owner.map(str::to_string), held: None })
    }

    fn register_agent(&self, b: RegisterAgent) -> CellResult<IdentityView> {
        let owner = self.by(&b.owner)?;
        check_key(&b.key)?;
        if owner.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "an agent's owner is a person"));
        }
        if b.fragment.as_deref().is_some_and(|f| !fragment_proto::valid_fragment_name(f)) {
            return Err(CellError::invalid("an agent's fragment is named <label>--<suffix>"));
        }
        let (agent, created) = match self.key_row(&b.key)? {
            Some(row) if !row.active() => return Err(conflict("this key was revoked")),
            Some(row) => {
                let holder = self.stored_identity(&row.identity, &format!("the key {}", b.key))?;
                // a replay of the same registration answers the same agent
                if holder.kind != IdentityKind::Agent || holder.owner.as_deref() != Some(owner.id.as_str()) {
                    return Err(conflict("this key already belongs to someone"));
                }
                (holder, false)
            }
            None => {
                let n = self.count("SELECT COUNT(*) AS n FROM identities WHERE owner = ? AND kind = 'agent'", vec![owner.id.as_str().into()])?;
                if n >= limits::AGENTS_PER_OWNER_MAX {
                    return Err(CellError::invalid(format!("a person owns at most {} agents", limits::AGENTS_PER_OWNER_MAX)));
                }
                (self.make(IdentityKind::Agent, Some(&owner.id), &b.key, &owner.id)?, true)
            }
        };
        // its name, for pages (`profiles`): the fragment its key is
        if let Some(fragment) = &b.fragment {
            self.exec(
                "INSERT INTO agent_fragments (identity, fragment) VALUES (?, ?) ON CONFLICT (identity) DO UPDATE SET fragment = excluded.fragment",
                vec![agent.id.as_str().into(), fragment.as_str().into()],
            )?;
        }
        self.view(&agent, Some(created))
    }

    /// Holds an agent below its owner (`held`: the most it acts with), or
    /// lets it go; only its owner does, and never above editor (an agent
    /// never acts as an owner).
    fn hold(&self, b: Hold) -> CellResult<IdentityView> {
        let by = self.by(&b.by)?;
        let agent = self.named_identity(&b.agent)?;
        if agent.kind != IdentityKind::Agent || agent.owner.as_deref() != Some(by.id.as_str()) {
            return Err(CellError::new(ErrorCode::Forbidden, "only an agent's owner holds it"));
        }
        if b.held.is_some_and(|r| r == Role::Public || r > fragment_core::access::AGENT_ROLE_MAX) {
            return Err(CellError::invalid("an agent is held at viewer or editor"));
        }
        let held = b.held.map_or(SqlStorageValue::Null, |r| r.as_str().into());
        self.exec("UPDATE identities SET held = ? WHERE id = ?", vec![held, agent.id.as_str().into()])?;
        self.view(&self.named_identity(&agent.id)?, Some(false))
    }

    /// The identity a call names (`None`: the asker's own).
    fn named_or(&self, identity: Option<&str>, by: &Identity) -> CellResult<Identity> {
        identity.map_or_else(|| Ok(by.clone()), |id| self.named_identity(id))
    }

    /// The identity whose keys change (`None`: the asker's own), if `by` manages them.
    fn managed(&self, identity: Option<&str>, by: &Identity) -> CellResult<Identity> {
        let who = self.named_or(identity, by)?;
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

    /// `key` joins `who`, added by `by`: false when it is theirs already.
    fn insert_key(&self, who: &str, key: &str, by: &str) -> CellResult<bool> {
        match self.key_row(key)? {
            Some(row) if row.identity == who && row.active() => return Ok(false),
            Some(row) if row.identity == who => return Err(conflict("a revoked key stays revoked; make a new one")),
            Some(_) => return Err(conflict("this key already belongs to someone")),
            None => {}
        }
        let n = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ?", vec![who.into()])?;
        if n >= limits::KEYS_PER_IDENTITY_MAX {
            return Err(CellError::invalid(format!("an identity holds at most {} keys, revoked ones included", limits::KEYS_PER_IDENTITY_MAX)));
        }
        self.exec(
            "INSERT INTO keys (key, identity, added_at, added_by) VALUES (?, ?, ?, ?)",
            vec![key.into(), who.into(), SqlStorageValue::Integer(js::now_ms()), by.into()],
        )?;
        Ok(true)
    }

    fn add_key(&self, AddKey(b): AddKey) -> CellResult<IdentityView> {
        let by = self.by(&b.by)?;
        check_key(&b.key)?;
        let who = self.managed(b.identity.as_deref(), &by)?;
        let added = self.insert_key(&who.id, &b.key, &by.id)?;
        self.view(&who, Some(added))
    }

    fn revoke(&self, RevokeKey(b): RevokeKey) -> CellResult<IdentityView> {
        let by = self.by(&b.by)?;
        check_key(&b.key)?;
        let who = self.managed(b.identity.as_deref(), &by)?;
        match self.key_row(&b.key)?.filter(|row| row.identity == who.id) {
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

    /// The person a verified email names (an email names at most one:
    /// signin.rs), if anyone.
    pub(crate) fn person_by_email(&self, email: &str) -> CellResult<Option<String>> {
        let rows = self.rows::<HolderRow>("SELECT identity FROM subjects WHERE email = ? LIMIT 1", vec![email.to_ascii_lowercase().into()])?;
        Ok(rows.into_iter().next().map(|r| r.identity))
    }

    /// Who an npub (or 64 hex) names: the identity it is, or else the one
    /// holding it as an active key; or an email: the person it is verified
    /// for.
    fn lookup(&self, b: Lookup) -> CellResult<Identity> {
        let who = b.who;
        let missing = || CellError::new(ErrorCode::NotFound, format!("{who} names no one on this fleet (they sign in, or register with `fragment login`)"));
        let found = if fragment_core::mail::valid_address(&who) {
            let id = self.person_by_email(&who)?.ok_or_else(missing)?;
            self.stored_identity(&id, &format!("the email {who}"))?
        } else {
            let Some(key) = npub::parse(&who) else {
                return Err(CellError::invalid(format!("{who:?} is not an npub, a 64-hex key, or an email")));
            };
            match self.identity(&npub::identity_of(&key))? {
                Some(identity) => identity,
                None => match self.key_row(&key)? {
                    Some(row) if row.active() => self.stored_identity(&row.identity, &format!("the key {key}"))?,
                    Some(_) => return Err(CellError::new(ErrorCode::NotFound, format!("the key {who} was revoked"))),
                    None => return Err(missing()),
                },
            }
        };
        // no one adds a person being wiped (or their agent) to a fragment
        if self.wiping(found.owner.as_deref().unwrap_or(&found.id))? {
            return Err(missing());
        }
        Ok(found)
    }

    fn view_for(&self, b: View) -> CellResult<IdentityView> {
        let by = self.by(&b.by)?;
        let who = self.named_or(b.identity.as_deref(), &by)?;
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

    async fn test_hook(&self, hook: TestHook) -> CellResult<Value> {
        match hook {
            TestHook::Down(down) => {
                self.down.set(down);
                Ok(json!({ "down": down }))
            }
            TestHook::Calls => Ok(json!({ "calls": self.calls.get() })),
            TestHook::Hold(ms) => {
                if ms > TEST_HOLD_MAX_MS {
                    return Err(CellError::invalid(format!("hold at most {TEST_HOLD_MAX_MS} ms")));
                }
                self.hold_ms.set(ms);
                Ok(json!({ "hold": ms }))
            }
            TestHook::Signins(hook) => Ok(json!(self.signins_hook(hook).await?)),
            TestHook::E2eSignIn(asked) => Ok(json!(self.e2e_sign_in(&asked.email, asked.paid_calls).await?)),
            TestHook::E2ePeople(page) => Ok(json!(self.e2e_people(page.after.as_deref())?)),
            TestHook::E2eIs(identity) => Ok(json!({ "e2e": self.is_e2e(&identity)? })),
        }
    }

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
        match path.as_str() {
            Resolve::PATH => reply::<Resolve>(self.key_holder(&body::<Resolve>(&bytes)?.key)),
            Lookup::PATH => reply::<Lookup>(self.lookup(body(&bytes)?)),
            RegisterAgent::PATH => reply::<RegisterAgent>(self.register_agent(body(&bytes)?)),
            Hold::PATH => reply::<Hold>(self.hold(body(&bytes)?)),
            SubjectOf::PATH => reply::<SubjectOf>(self.subject_of(body(&bytes)?)),
            Profiles::PATH => reply::<Profiles>(self.profiles(body(&bytes)?)),
            AddKey::PATH => reply::<AddKey>(self.add_key(body(&bytes)?)),
            RevokeKey::PATH => reply::<RevokeKey>(self.revoke(body(&bytes)?)),
            View::PATH => reply::<View>(self.view_for(body(&bytes)?)),
            CheckKey::PATH => reply::<CheckKey>(self.check(body(&bytes)?)),
            PictureOf::PATH => reply::<PictureOf>(self.picture_of(&body::<PictureOf>(&bytes)?.identity)),
            SetPicture::PATH => reply::<SetPicture>(self.set_picture(body(&bytes)?)),
            Begin::PATH => reply::<Begin>(self.begin(body(&bytes)?).await),
            Exchange::PATH => reply::<Exchange>(self.exchange(body(&bytes)?).await),
            Session::PATH => reply::<Session>(self.session(body(&bytes)?)),
            EndSession::PATH => reply::<EndSession>(self.end_site_session(body(&bytes)?)),
            Logout::PATH => reply::<Logout>(self.logout(body(&bytes)?)),
            Mint::PATH => reply::<Mint>(self.mint(body(&bytes)?).await),
            Redeem::PATH => reply::<Redeem>(self.redeem(body(&bytes)?)),
            ApproveKey::PATH => reply::<ApproveKey>(self.add_by_session(body(&bytes)?)),
            WipeLook::PATH => reply::<WipeLook>(self.wipe_look(body(&bytes)?)),
            WipeBegin::PATH => reply::<WipeBegin>(self.wipe_begin(body(&bytes)?)),
            WipeStep::PATH => reply::<WipeStep>(self.wipe_step(body(&bytes)?)),
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}"))),
        }
    }
}
