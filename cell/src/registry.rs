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
//! Facts (`/resolve`, `/lookup`, a session) carry the identity's username,
//! and an agent's carry its owner's (the namespace it makes fragments in).
//!
//! People come from sign-in, and browsers hold sessions: `signin.rs`.

use fragment_core::{npub, registry};
use fragment_proto::{limits, ErrorCode, IdentityKind, IdentityView, KeyView};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::js;

/// The identities one `/profiles` answers.
const PROFILES_MAX: usize = 64;

mod signin;
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
}

impl DurableObject for RegistryCell {
    fn new(state: State, env: Env) -> Self {
        state.storage().sql().exec(SCHEMA, None).expect("the Registry schema applies");
        state.storage().sql().exec(signin::SCHEMA, None).expect("the sign-in schema applies");
        RegistryCell { state, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(v) => Response::from_json(&v),
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

/// An identity's facts, as a request resolves to them.
struct Facts {
    id: String,
    kind: IdentityKind,
    owner: Option<String>,
    /// A person's username; an agent's owner's (where it makes fragments).
    username: Option<String>,
}

impl Facts {
    fn json(&self) -> Value {
        json!({ "id": self.id, "kind": self.kind, "owner": self.owner, "username": self.username })
    }
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

#[derive(Deserialize)]
struct WhoBody {
    who: String,
}

#[derive(Deserialize)]
struct AgentBody {
    owner: String,
    key: String,
}

#[derive(Deserialize)]
struct ChangeBody {
    identity: String,
    key: String,
    by: String,
}

#[derive(Deserialize)]
struct ViewBody {
    identity: String,
    by: String,
}

#[derive(Deserialize)]
struct ClaimBody {
    identity: String,
    username: String,
}

#[derive(Deserialize)]
struct UsernameBody {
    username: String,
}

#[derive(Deserialize)]
struct PictureBody {
    identity: String,
    sha: String,
    mime: String,
}

/// Dev fleets' controls (`FRAGMENT_TEST_HOOKS=allow`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum TestHook {
    /// Answer 503 to everything else (`true`), or answer again.
    Down(bool),
    Signins(signin::SigninsHook),
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

impl RegistryCell {
    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.state.storage().sql().exec(q, binds)?.to_array::<Value>()?)
    }

    fn exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.state.storage().sql().exec(q, binds)?;
        Ok(())
    }

    fn count(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<u64> {
        Ok(self.rows(q, binds)?.first().and_then(|r| r["n"].as_u64()).unwrap_or(0))
    }

    fn facts(&self, id: &str) -> CellResult<Option<Facts>> {
        let rows = self.rows("SELECT id, kind, owner FROM identities WHERE id = ?", vec![id.into()])?;
        let Some(r) = rows.first() else { return Ok(None) };
        let kind = r["kind"].as_str().and_then(IdentityKind::parse).ok_or_else(|| CellError::host("identities.kind"))?;
        let owner = r["owner"].as_str().map(str::to_string);
        let username = self.username_of(owner.as_deref().unwrap_or(id))?;
        Ok(Some(Facts { id: id.to_string(), kind, owner, username }))
    }

    fn username_of(&self, id: &str) -> CellResult<Option<String>> {
        Ok(self.rows("SELECT username FROM usernames WHERE identity = ?", vec![id.into()])?.first().and_then(|r| r["username"].as_str()).map(str::to_string))
    }

    /// A person's username, chosen once: taken names and reserved words are refused.
    fn claim_username(&self, b: ClaimBody) -> CellResult<Value> {
        let facts = self.must_facts(&b.identity)?;
        if facts.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "only a person chooses a username (an agent makes fragments under its owner's)"));
        }
        if !fragment_proto::valid_username(&b.username) {
            return Err(CellError::invalid(format!(
                "a username is {}-{} lowercase letters, digits, and single dashes, not starting or ending with one, and not a reserved word",
                limits::USERNAME_MIN_BYTES,
                limits::USERNAME_MAX_BYTES
            )));
        }
        match facts.username {
            Some(u) if u == b.username => return Ok(json!({ "username": u, "claimed": false })),
            Some(u) => return Err(conflict(format!("you are {u}: a username is chosen once"))),
            None => {}
        }
        if !self.rows("SELECT identity FROM usernames WHERE username = ?", vec![b.username.as_str().into()])?.is_empty() {
            return Err(conflict(format!("{} is taken", b.username)));
        }
        self.exec(
            "INSERT INTO usernames (username, identity, claimed_at) VALUES (?, ?, ?)",
            vec![b.username.as_str().into(), facts.id.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(json!({ "username": b.username, "claimed": true }))
    }

    /// Whoever holds a username, and their picture (sha, mime).
    fn username_lookup(&self, username: &str) -> CellResult<Value> {
        let rows = self.rows("SELECT identity FROM usernames WHERE username = ?", vec![username.into()])?;
        let id = rows.first().and_then(|r| r["identity"].as_str()).ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no one is {username}")))?.to_string();
        let mut v = self.must_facts(&id)?.json();
        let pic = self.rows("SELECT sha, mime FROM pictures WHERE identity = ?", vec![id.as_str().into()])?;
        v["picture"] = pic.first().map_or(Value::Null, |r| json!({ "sha": r["sha"], "mime": r["mime"] }));
        Ok(v)
    }

    fn set_picture(&self, b: PictureBody) -> CellResult<Value> {
        let facts = self.must_facts(&b.identity)?;
        if facts.kind != IdentityKind::Person || facts.username.is_none() {
            return Err(CellError::invalid("choose a username before a picture"));
        }
        if b.sha.len() != 64 || !b.sha.bytes().all(|c| c.is_ascii_hexdigit()) || !matches!(b.mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
            return Err(CellError::invalid("a picture is a PNG, JPEG, WebP, or GIF, named by its SHA-256"));
        }
        self.exec(
            "INSERT INTO pictures (identity, sha, mime, set_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (identity) DO UPDATE SET sha = excluded.sha, mime = excluded.mime, set_at = excluded.set_at",
            vec![facts.id.as_str().into(), b.sha.as_str().into(), b.mime.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(json!({ "sha": b.sha, "mime": b.mime }))
    }

    fn must_facts(&self, id: &str) -> CellResult<Facts> {
        self.facts(id)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no identity {id}")))
    }

    /// The key's row: the identity holding it and whether it was revoked.
    fn key_row(&self, key: &str) -> CellResult<Option<(String, bool)>> {
        let rows = self.rows("SELECT identity, revoked_at FROM keys WHERE key = ?", vec![key.into()])?;
        Ok(rows.first().map(|r| (r["identity"].as_str().unwrap_or("").to_string(), !r["revoked_at"].is_null())))
    }

    fn resolve(&self, key: &str) -> CellResult<Facts> {
        check_key(key)?;
        match self.key_row(key)? {
            None => Err(unauthenticated(format!(
                "the key {} belongs to no one on this fleet (add it to you: `fragment login`)",
                npub::encode(key)
            ))),
            Some((_, true)) => Err(unauthenticated(format!("the key {} was revoked", npub::encode(key)))),
            Some((id, false)) => self.facts(&id)?.ok_or_else(|| CellError::host(format!("key {key} names a missing identity"))),
        }
    }

    /// What anyone may know of an identity, as a page shows a name: a
    /// person's username and picture, or that it is someone's agent. An id
    /// the registry does not hold (an anonymous visitor's) is left out.
    fn profiles(&self, ids: &[String]) -> CellResult<serde_json::Map<String, Value>> {
        let mut out = serde_json::Map::new();
        for id in ids {
            let Some(facts) = self.facts(id)? else { continue };
            let picture = match (&facts.kind, &facts.username) {
                (IdentityKind::Person, Some(u)) => self
                    .rows("SELECT sha FROM pictures WHERE identity = ?", vec![id.as_str().into()])?
                    .first()
                    .and_then(|r| r["sha"].as_str())
                    .map(|sha| format!("/api/users/{u}/picture?v={}", &sha[..12])),
                _ => None,
            };
            // an agent's facts carry its owner's username
            out.insert(id.clone(), json!({ "kind": facts.kind, "username": facts.username, "picture": picture }));
        }
        Ok(out)
    }

    fn view(&self, facts: &Facts, created: Option<bool>) -> CellResult<IdentityView> {
        let keys = self
            .rows("SELECT key, added_at, added_by, revoked_at FROM keys WHERE identity = ? ORDER BY added_at, key", vec![facts.id.as_str().into()])?
            .iter()
            .map(|r| KeyView {
                npub: npub::encode(r["key"].as_str().unwrap_or("")),
                added_at: r["added_at"].as_i64().unwrap_or(0),
                added_by: r["added_by"].as_str().unwrap_or("").to_string(),
                revoked_at: r["revoked_at"].as_i64(),
            })
            .collect();
        let agents = self
            .rows("SELECT id FROM identities WHERE owner = ? ORDER BY created_at, id", vec![facts.id.as_str().into()])?
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        let created_at = self.rows("SELECT created_at FROM identities WHERE id = ?", vec![facts.id.as_str().into()])?;
        // a person's own username (an agent's facts carry its owner's)
        let username = if facts.kind == IdentityKind::Person { facts.username.clone() } else { None };
        let picture = match &username {
            Some(u) => self
                .rows("SELECT sha FROM pictures WHERE identity = ?", vec![facts.id.as_str().into()])?
                .first()
                .and_then(|r| r["sha"].as_str())
                .map(|sha| format!("/api/users/{u}/picture?v={}", &sha[..12])),
            None => None,
        };
        Ok(IdentityView {
            id: facts.id.clone(),
            kind: facts.kind,
            owner: facts.owner.clone(),
            username,
            picture,
            created_at: created_at.first().and_then(|r| r["created_at"].as_i64()).unwrap_or(0),
            keys,
            agents,
            subjects: self.subjects_of(&facts.id)?,
            created,
        })
    }

    /// A new identity holding `key`, in one transaction (a DO turn without
    /// an await between its writes).
    fn make(&self, kind: IdentityKind, owner: Option<&str>, key: &str, by: &str) -> CellResult<Facts> {
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
        Ok(Facts { id, kind, owner: owner.map(str::to_string), username })
    }

    fn register_agent(&self, body: AgentBody) -> CellResult<IdentityView> {
        check_key(&body.key)?;
        let owner = self.must_facts(&body.owner)?;
        if owner.kind != IdentityKind::Person {
            return Err(CellError::new(ErrorCode::Forbidden, "an agent's owner is a person"));
        }
        match self.key_row(&body.key)? {
            Some((_, true)) => Err(conflict("this key was revoked")),
            Some((id, false)) => {
                let facts = self.must_facts(&id)?;
                // a replay of the same registration answers the same agent
                if facts.kind == IdentityKind::Agent && facts.owner.as_deref() == Some(owner.id.as_str()) {
                    return self.view(&facts, Some(false));
                }
                Err(conflict("this key already belongs to someone"))
            }
            None => {
                let n = self.count("SELECT COUNT(*) AS n FROM identities WHERE owner = ?", vec![owner.id.as_str().into()])?;
                if n >= limits::AGENTS_PER_OWNER_MAX {
                    return Err(CellError::invalid(format!("a person owns at most {} agents", limits::AGENTS_PER_OWNER_MAX)));
                }
                let facts = self.make(IdentityKind::Agent, Some(&owner.id), &body.key, &owner.id)?;
                self.view(&facts, Some(true))
            }
        }
    }

    fn managed(&self, identity: &str, by: &str) -> CellResult<Facts> {
        let facts = self.must_facts(identity)?;
        if !registry::may_manage_keys(by, &facts.id, facts.kind, facts.owner.as_deref()) {
            return Err(CellError::new(
                ErrorCode::Forbidden,
                match facts.kind {
                    IdentityKind::Person => "only this person manages their keys",
                    IdentityKind::Agent => "only the agent's owner manages its keys",
                },
            ));
        }
        Ok(facts)
    }

    fn add_key(&self, body: ChangeBody) -> CellResult<IdentityView> {
        check_key(&body.key)?;
        let facts = self.managed(&body.identity, &body.by)?;
        match self.key_row(&body.key)? {
            Some((id, false)) if id == facts.id => return self.view(&facts, Some(false)),
            Some((id, true)) if id == facts.id => return Err(conflict("a revoked key stays revoked; make a new one")),
            Some(_) => return Err(conflict("this key already belongs to someone")),
            None => {}
        }
        let n = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ?", vec![facts.id.as_str().into()])?;
        if n >= limits::KEYS_PER_IDENTITY_MAX {
            return Err(CellError::invalid(format!("an identity holds at most {} keys, revoked ones included", limits::KEYS_PER_IDENTITY_MAX)));
        }
        self.exec(
            "INSERT INTO keys (key, identity, added_at, added_by) VALUES (?, ?, ?, ?)",
            vec![body.key.as_str().into(), facts.id.as_str().into(), SqlStorageValue::Integer(js::now_ms()), body.by.as_str().into()],
        )?;
        self.view(&facts, Some(true))
    }

    fn revoke(&self, body: ChangeBody) -> CellResult<IdentityView> {
        check_key(&body.key)?;
        let facts = self.managed(&body.identity, &body.by)?;
        match self.key_row(&body.key)? {
            Some((id, _)) if id != facts.id => return Err(CellError::new(ErrorCode::NotFound, "not one of this identity's keys")),
            None => return Err(CellError::new(ErrorCode::NotFound, "not one of this identity's keys")),
            Some((_, true)) => return self.view(&facts, Some(false)),
            Some((_, false)) => {}
        }
        // an agent always keeps a key; a person who signs in may hold none
        let active = self.count("SELECT COUNT(*) AS n FROM keys WHERE identity = ? AND revoked_at IS NULL", vec![facts.id.as_str().into()])?;
        let signs_in = facts.kind == IdentityKind::Person && self.signs_in(&facts.id)?;
        if active <= 1 && !signs_in {
            return Err(CellError::invalid("an agent keeps at least one key: add the new key before revoking the last"));
        }
        self.exec("UPDATE keys SET revoked_at = ? WHERE key = ?", vec![SqlStorageValue::Integer(js::now_ms()), body.key.as_str().into()])?;
        self.view(&facts, Some(true))
    }

    fn lookup(&self, who: &str) -> CellResult<Facts> {
        let missing = || CellError::new(ErrorCode::NotFound, format!("{who} names no one on this fleet (they register with `fragment login`)"));
        match npub::parse_named(who) {
            Some(npub::Named::Identity(id)) => self.facts(&id)?.ok_or_else(missing),
            Some(npub::Named::Key(key)) => match self.key_row(&key)? {
                Some((id, false)) => self.must_facts(&id),
                Some((_, true)) => Err(CellError::new(ErrorCode::NotFound, format!("the key {who} was revoked"))),
                None => Err(missing()),
            },
            None => Err(CellError::invalid(format!("{who:?} is not an identity (id:…), an npub, or a 64-hex key"))),
        }
    }

    fn down(&self) -> CellResult<bool> {
        Ok(self.rows("SELECT value FROM meta WHERE key = 'down'", vec![])?.first().is_some_and(|r| r["value"] == "1"))
    }

    async fn route(&self, mut req: Request) -> CellResult<Value> {
        let path = req.path();
        let body: Value = {
            let bytes = req.bytes().await?;
            if bytes.is_empty() {
                json!({})
            } else {
                serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))?
            }
        };
        if path == "/test" {
            if self.env.var("FRAGMENT_TEST_HOOKS").map(|v| v.to_string()).ok().as_deref() != Some("allow") {
                return Err(CellError::new(ErrorCode::NotFound, "no route /test"));
            }
            return match from::<TestHook>(body)? {
                TestHook::Down(down) => {
                    self.exec(
                        "INSERT INTO meta (key, value) VALUES ('down', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                        vec![if down { "1" } else { "0" }.into()],
                    )?;
                    Ok(json!({ "down": down }))
                }
                TestHook::Signins(hook) => self.signins_hook(hook).await,
            };
        }
        if self.down()? {
            return Err(CellError::new(ErrorCode::RegistryUnavailable, "the registry is down (a test hook)"));
        }
        if let Some(v) = self.route_signin(&path, body.clone()).await? {
            return Ok(v);
        }
        match path.as_str() {
            "/username/claim" => self.claim_username(from(body)?),
            "/username/lookup" => {
                let b: UsernameBody = from(body)?;
                self.username_lookup(&b.username)
            }
            "/picture/set" => self.set_picture(from(body)?),
            "/resolve" => {
                let b: KeyBody = from(body)?;
                Ok(self.resolve(&b.key)?.json())
            }
            "/lookup" => {
                let b: WhoBody = from(body)?;
                Ok(self.lookup(&b.who)?.json())
            }
            "/agents" => to(self.register_agent(from(body)?)?),
            "/profiles" => {
                let ids: Vec<String> = from(body["ids"].clone())?;
                if ids.len() > PROFILES_MAX {
                    return Err(CellError::invalid(format!("at most {PROFILES_MAX} identities at once")));
                }
                Ok(json!({ "profiles": self.profiles(&ids)? }))
            }
            "/keys" => to(self.add_key(from(body)?)?),
            "/revoke" => to(self.revoke(from(body)?)?),
            "/view" => {
                let b: ViewBody = from(body)?;
                let facts = self.must_facts(&b.identity)?;
                if !registry::may_view(&b.by, &facts.id, facts.owner.as_deref()) {
                    return Err(CellError::new(ErrorCode::NotFound, format!("no identity {}", b.identity)));
                }
                to(self.view(&facts, None)?)
            }
            "/check" => {
                let b: ChangeBody = from(body)?;
                check_key(&b.key)?;
                let by = self.must_facts(&b.by)?;
                if !registry::may_check_key(&by.id, by.owner.as_deref(), &b.identity) {
                    return Err(CellError::new(ErrorCode::Forbidden, "only an identity and its agents ask about its keys"));
                }
                let active = matches!(self.key_row(&b.key)?, Some((id, false)) if id == b.identity);
                Ok(json!({ "active": active }))
            }
            p => Err(CellError::new(ErrorCode::NotFound, format!("no route {p}"))),
        }
    }
}

fn from<T: serde::de::DeserializeOwned>(v: Value) -> CellResult<T> {
    serde_json::from_value(v).map_err(|e| CellError::invalid(format!("body: {e}")))
}

fn to<T: serde::Serialize>(v: T) -> CellResult<Value> {
    serde_json::to_value(v).map_err(|e| CellError::host(e.to_string()))
}
