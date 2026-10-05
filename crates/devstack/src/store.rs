//! Cloudflare Secrets Store, through wrangler (`wrangler secrets-store`, on
//! the pinned Node like every wrangler call): the deployment's own secrets
//! live there (docs/secrets.md). `cargo xtask secret` sets and lists them,
//! `cargo xtask deploy` checks that every one its config names is there and
//! binds them by name, and dev and the e2e seed wrangler's local store with
//! values of their own (`seed_local`).
//!
//! A value reaches wrangler on its standard input, or through wrangler's
//! own hidden prompt in a terminal, never on a command line (`--value` is
//! never passed: `args_hold_no_value` checks every command built here), and
//! nothing here prints one. wrangler's output never holds one either (it
//! says `Value: REDACTED`), and its debug log leaves request bodies out
//! (`WRANGLER_LOG_SANITIZE`, set on every command).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use fragment_core::secrets_store as bindings;

use crate::Tools;

/// A store secret's name, at most (our limit; wrangler takes
/// `[A-Za-z0-9_-]+`).
pub const NAME_MAX_BYTES: usize = 64;
/// A value, at most: the store's own limit (wrangler refuses more too).
pub const VALUE_MAX_BYTES: usize = 64 * 1024;
/// The most secrets an account's store holds (Cloudflare's limit), so one
/// page of a listing holds them all.
pub const SECRETS_MAX: usize = 100;
/// The store `cargo xtask secret set` makes when the account has none
/// (an account has one store at most).
pub const STORE_NAME: &str = "fragment";
/// The store dev and the e2e bind, in wrangler's local store: any name
/// works there, and it names the directory miniflare keeps it in.
pub const LOCAL_STORE_ID: &str = "fragment-local";
/// What `secret list` and `store list` say, on their error stream, when
/// there is nothing to list (wrangler 4.145.0, src/secrets-store/commands.ts).
const NO_SECRETS: &str = "List request returned no secrets.";
const NO_STORES: &str = "List request returned no stores.";
/// Where `seed_local` notes what it seeded, in the local state directory.
const SEEDED_STAMP: &str = "fragment-secrets-store.sha256";

/// Whether `name` is a store secret's name as this repo writes them.
pub fn valid_name(name: &str) -> bool {
    (1..=NAME_MAX_BYTES).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Where a command acts.
#[derive(Debug, Clone, Copy)]
pub enum Store<'a> {
    /// The account's own store (`--remote`).
    Account { account_id: &'a str, store_id: &'a str },
    /// wrangler's local store, persisted under a node's state directory
    /// (`wrangler dev --persist-to`).
    Local { persist: &'a Path },
}

impl Store<'_> {
    fn id(&self) -> &str {
        match self {
            Store::Account { store_id, .. } => store_id,
            Store::Local { .. } => LOCAL_STORE_ID,
        }
    }

    fn target(&self) -> Vec<OsString> {
        match self {
            Store::Account { .. } => vec!["--remote".into()],
            Store::Local { persist } => vec!["--persist-to".into(), persist.as_os_str().to_owned()],
        }
    }

    fn account(&self) -> Option<&str> {
        match self {
            Store::Account { account_id, .. } => Some(account_id),
            Store::Local { .. } => None,
        }
    }
}

/// An account's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStore {
    pub name: String,
    pub id: String,
}

/// A secret in a store: its name and id, and when it was made and last
/// changed (as wrangler prints them). Never its value: the store gives
/// none back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub id: String,
    pub created: String,
    pub modified: String,
}

/// A value to put in the store.
pub enum Value {
    /// wrangler's own hidden prompt, in this terminal.
    Prompt,
    /// Bytes read here (a file, a pipe) or made here, checked, and written
    /// to wrangler's standard input.
    Bytes(Vec<u8>),
}

impl Value {
    /// `bytes` as a value, checked as the store checks one: text, not
    /// empty, within `VALUE_MAX_BYTES` once its trailing whitespace goes
    /// (wrangler trims it).
    pub fn checked(bytes: Vec<u8>, from: &str) -> Result<Value, StoreError> {
        let text = std::str::from_utf8(&bytes).map_err(|_| StoreError::InvalidValue(format!("{from} is not text")))?;
        let kept = text.trim_end().len();
        if kept == 0 {
            return Err(StoreError::InvalidValue(format!("{from} is empty")));
        }
        if kept > VALUE_MAX_BYTES {
            return Err(StoreError::InvalidValue(format!("{from} holds {kept} bytes; a store secret is at most {VALUE_MAX_BYTES}")));
        }
        Ok(Value::Bytes(bytes))
    }

    /// A file's contents.
    pub fn from_file(path: &Path) -> Result<Value, StoreError> {
        let size = std::fs::metadata(path).map_err(|source| StoreError::Io { what: format!("read {}", path.display()), source })?.len();
        // whitespace past the limit is still trimmed: a little slack, then refused unread
        if size > (VALUE_MAX_BYTES + 4096) as u64 {
            return Err(StoreError::InvalidValue(format!("{} holds {size} bytes; a store secret is at most {VALUE_MAX_BYTES}", path.display())));
        }
        let bytes = std::fs::read(path).map_err(|source| StoreError::Io { what: format!("read {}", path.display()), source })?;
        Value::checked(bytes, &path.display().to_string())
    }

    /// This process's standard input, which is not a terminal: read to its
    /// end, bounded.
    pub fn from_stdin() -> Result<Value, StoreError> {
        let mut bytes = Vec::new();
        let limit = (VALUE_MAX_BYTES + 4096) as u64;
        io::stdin().lock().take(limit + 1).read_to_end(&mut bytes).map_err(|source| StoreError::Io { what: "read standard input".into(), source })?;
        if bytes.len() as u64 > limit {
            return Err(StoreError::InvalidValue(format!("standard input holds more than {VALUE_MAX_BYTES} bytes; a store secret is at most that")));
        }
        Value::checked(bytes, "standard input")
    }
}

/// What a put did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    Created,
    Updated,
}

/// Why a store command did not do what was asked.
#[derive(Debug)]
pub enum StoreError {
    InvalidName(String),
    /// A value refused before wrangler runs.
    InvalidValue(String),
    /// wrangler failed: what was asked, and what it said (never a value).
    Wrangler { what: String, said: String },
    /// wrangler answered in a shape this does not read (another version?).
    Unread { what: String, said: String },
    /// The account has more than the one store Cloudflare allows.
    Stores(Vec<AccountStore>),
    Io { what: String, source: io::Error },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::InvalidName(name) => write!(f, "{name:?} is not a store secret's name: 1-{NAME_MAX_BYTES} of A-Z, a-z, 0-9, _ and -"),
            StoreError::InvalidValue(why) => write!(f, "{why}"),
            StoreError::Wrangler { what, said } => write!(f, "wrangler could not {what}:\n{said}"),
            StoreError::Unread { what, said } => write!(f, "wrangler's answer to {what} is not one this reads (wrangler {}?):\n{said}", crate::WRANGLER_VERSION),
            StoreError::Stores(stores) => {
                let listed: Vec<String> = stores.iter().map(|s| format!("{} ({})", s.name, s.id)).collect();
                write!(f, "the account has {} Secrets Stores ({}); this uses an account's one", stores.len(), listed.join(", "))
            }
            StoreError::Io { what, source } => write!(f, "{what}: {source}"),
        }
    }
}

/// Each message carries its cause's text, so it names no `source`.
impl std::error::Error for StoreError {}

fn checked_name(name: &str) -> Result<(), StoreError> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(StoreError::InvalidName(name.to_string()))
    }
}

/// `secrets-store store list`: the account's stores (one page holds them).
pub fn stores_args() -> Vec<OsString> {
    ["secrets-store", "store", "list", "--remote", "--per-page", "10"].map(OsString::from).to_vec()
}

/// `secrets-store store create <name>`: a store on the account.
pub fn create_store_args(name: &str) -> Vec<OsString> {
    ["secrets-store", "store", "create", name, "--remote"].map(OsString::from).to_vec()
}

/// `secrets-store secret list`: every secret in `store` (one page holds them).
pub fn list_args(store: &Store) -> Vec<OsString> {
    let per_page = SECRETS_MAX.to_string();
    let mut args: Vec<OsString> = ["secrets-store", "secret", "list", store.id(), "--per-page", &per_page].map(OsString::from).to_vec();
    args.extend(store.target());
    args
}

/// `secrets-store secret create`: `name` in `store`, for Workers, its value
/// on standard input (or wrangler's prompt).
pub fn create_args(store: &Store, name: &str) -> Vec<OsString> {
    assert!(valid_name(name), "a name is checked before a command is built");
    let mut args: Vec<OsString> = ["secrets-store", "secret", "create", store.id(), "--name", name, "--scopes", "workers"].map(OsString::from).to_vec();
    args.extend(store.target());
    args
}

/// `secrets-store secret update`: the secret `secret_id` in `store`, its
/// new value on standard input (or wrangler's prompt, after it asks
/// whether to change the value).
pub fn update_args(store: &Store, secret_id: &str) -> Vec<OsString> {
    assert!(!secret_id.is_empty() && secret_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "a secret's id is the store's");
    let mut args: Vec<OsString> = ["secrets-store", "secret", "update", store.id(), "--secret-id", secret_id].map(OsString::from).to_vec();
    args.extend(store.target());
    args
}

/// Whether a command's arguments hold no value: no `--value`, which
/// would put one on a command line (and in shell history).
pub fn args_hold_no_value(args: &[OsString]) -> bool {
    !args.iter().any(|a| a.to_string_lossy().starts_with("--value"))
}

/// wrangler, for `account` (remote) or the local store, with no colours
/// and its debug log sanitized.
fn wrangler(tools: &Tools, account: Option<&str>, args: Vec<OsString>) -> Result<Command, StoreError> {
    assert!(args_hold_no_value(&args), "no value on a command line");
    let mut cmd = tools.wrangler().map_err(|e| StoreError::Io { what: "locate wrangler".into(), source: io::Error::other(e.to_string()) })?;
    cmd.args(args).env("FORCE_COLOR", "0").env("WRANGLER_LOG_SANITIZE", "true").current_dir(crate::repo_root());
    match account {
        Some(a) => cmd.env("CLOUDFLARE_ACCOUNT_ID", a),
        None => cmd.env_remove("CLOUDFLARE_ACCOUNT_ID"),
    };
    Ok(cmd)
}

/// wrangler's output, both streams, as text.
fn said(out: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string()
}

/// Runs `cmd` with nothing on its standard input: (whether it succeeded, what it said).
fn captured(mut cmd: Command, what: &str) -> Result<(bool, String), StoreError> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|source| StoreError::Io { what: format!("run wrangler to {what}"), source })?;
    Ok((out.status.success(), said(&out)))
}

/// The rows of the first table in wrangler's output (cli-table3's box
/// drawing), each a map from its column's heading to its cell.
pub fn parse_table(text: &str) -> Vec<BTreeMap<String, String>> {
    let lines: Vec<Vec<String>> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('│'))
        .map(|l| l.trim_matches('│').split('│').map(|c| c.trim().to_string()).collect())
        .collect();
    let Some((head, rows)) = lines.split_first() else { return vec![] };
    rows.iter().filter(|r| r.len() == head.len()).map(|r| head.iter().cloned().zip(r.iter().cloned()).collect()).collect()
}

/// `secret list`'s answer, as secrets.
pub fn parse_listed(ok: bool, text: &str) -> Result<Vec<Listed>, StoreError> {
    let what = "list a store's secrets";
    if !ok {
        return match text.contains(NO_SECRETS) {
            true => Ok(vec![]),
            false => Err(StoreError::Wrangler { what: what.into(), said: text.into() }),
        };
    }
    let rows = parse_table(text);
    let mut listed = vec![];
    for row in &rows {
        let cell = |k: &str| row.get(k).cloned().ok_or_else(|| StoreError::Unread { what: what.into(), said: text.into() });
        listed.push(Listed { name: cell("Name")?, id: cell("ID")?, created: cell("Created")?, modified: cell("Modified")? });
    }
    if listed.is_empty() {
        return Err(StoreError::Unread { what: what.into(), said: text.into() });
    }
    assert!(listed.len() <= SECRETS_MAX, "a store holds at most {SECRETS_MAX} secrets");
    Ok(listed)
}

/// `store list`'s answer, as stores.
pub fn parse_stores(ok: bool, text: &str) -> Result<Vec<AccountStore>, StoreError> {
    let what = "list the account's Secrets Stores";
    if !ok {
        return match text.contains(NO_STORES) {
            true => Ok(vec![]),
            false => Err(StoreError::Wrangler { what: what.into(), said: text.into() }),
        };
    }
    let rows = parse_table(text);
    let stores: Vec<AccountStore> = rows.iter().filter_map(|r| Some(AccountStore { name: r.get("Name")?.clone(), id: r.get("ID")?.clone() })).collect();
    if stores.is_empty() || stores.len() != rows.len() {
        return Err(StoreError::Unread { what: what.into(), said: text.into() });
    }
    Ok(stores)
}

/// `store create`'s answer: the new store's id (`Created store! (Name: …, ID: <id>)`).
pub fn parse_created_store(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains("Created store!"))?;
    let id = line.split("ID: ").nth(1)?.trim_end().trim_end_matches(')').trim();
    let ok = !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    ok.then(|| id.to_string())
}

/// The account's store: `None` when it has none yet, an error when it has
/// more than one. Read-only.
pub fn account_store(tools: &Tools, account_id: &str) -> Result<Option<AccountStore>, StoreError> {
    let (ok, text) = captured(wrangler(tools, Some(account_id), stores_args())?, "list the account's Secrets Stores")?;
    let mut stores = parse_stores(ok, &text)?;
    match stores.len() {
        0 => Ok(None),
        1 => Ok(stores.pop()),
        _ => Err(StoreError::Stores(stores)),
    }
}

/// Makes the account's store (`STORE_NAME`): the account has none.
pub fn create_store(tools: &Tools, account_id: &str) -> Result<AccountStore, StoreError> {
    let what = "make the account's Secrets Store";
    let (ok, text) = captured(wrangler(tools, Some(account_id), create_store_args(STORE_NAME))?, what)?;
    if !ok {
        return Err(StoreError::Wrangler { what: what.into(), said: text });
    }
    let id = parse_created_store(&text).ok_or(StoreError::Unread { what: what.into(), said: text })?;
    Ok(AccountStore { name: STORE_NAME.into(), id })
}

/// Every secret in `store`: names, ids and times, never a value. Read-only.
pub fn list(tools: &Tools, store: &Store) -> Result<Vec<Listed>, StoreError> {
    let (ok, text) = captured(wrangler(tools, store.account(), list_args(store))?, "list a store's secrets")?;
    parse_listed(ok, &text)
}

/// Puts `value` in `store` as `name`: an update of the secret `existing`
/// (its id, from `list`) when there is one, else a new secret.
pub fn put(tools: &Tools, store: &Store, name: &str, value: Value, existing: Option<&str>) -> Result<Put, StoreError> {
    checked_name(name)?;
    let (args, put, what) = match existing {
        Some(id) => (update_args(store, id), Put::Updated, format!("update {name}")),
        None => (create_args(store, name), Put::Created, format!("create {name}")),
    };
    let mut cmd = wrangler(tools, store.account(), args)?;
    match value {
        Value::Prompt => {
            // wrangler prompts only when both of its ends are a terminal
            let status = cmd.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status().map_err(|source| StoreError::Io { what: format!("run wrangler to {what}"), source })?;
            if !status.success() {
                return Err(StoreError::Wrangler { what, said: format!("it exited {status} (its own words are above)") });
            }
        }
        Value::Bytes(bytes) => {
            let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|source| StoreError::Io { what: format!("run wrangler to {what}"), source })?;
            let mut stdin = child.stdin.take().expect("its standard input is piped");
            // written beside the reads of its output, so neither pipe fills and stalls the other
            let out = std::thread::scope(|s| {
                let writer = s.spawn(move || stdin.write_all(&bytes));
                let out = child.wait_with_output();
                (writer.join().expect("the writer does not panic"), out)
            });
            let out = match out {
                (_, Err(source)) => return Err(StoreError::Io { what: format!("run wrangler to {what}"), source }),
                (Err(source), Ok(_)) => return Err(StoreError::Io { what: format!("hand wrangler the value to {what}"), source }),
                (Ok(()), Ok(out)) => out,
            };
            if !out.status.success() {
                return Err(StoreError::Wrangler { what, said: said(&out) });
            }
        }
    }
    Ok(put)
}

/// The store secrets a deployment's Workers are bound to, each by name;
/// the bindings' own names are `fragment_core::secrets_store`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub host_secret: String,
    /// Bound only while a rotation runs (docs/secrets.md, Rotating).
    pub host_secret_previous: Option<String>,
    pub codestorage_key: String,
    /// WorkOS's client id and API key: sign-in (a local fleet may run without).
    pub workos: Option<(String, String)>,
    /// Each operator key's provider and its secret's name.
    pub operator_keys: Vec<(String, String)>,
}

impl Bound {
    /// The names `deploy/example.jsonc` uses, which dev and the e2e bind in
    /// wrangler's local store: `fragment-` and what the secret's file was
    /// called before the store (docs/secrets.md, Migration).
    pub fn conventional(workos: bool, operator_keys: &[&str]) -> Bound {
        Bound {
            host_secret: "fragment-host-secret".into(),
            host_secret_previous: None,
            codestorage_key: "fragment-codestorage-private-key".into(),
            workos: workos.then(|| ("fragment-workos-client-id".into(), "fragment-workos-api-key".into())),
            operator_keys: operator_keys.iter().map(|p| (p.to_string(), format!("fragment-{p}-api-key"))).collect(),
        }
    }

    /// The agents' Worker's: the host secret alone (it seals agents' keys).
    pub fn agent(&self) -> Vec<(String, &str)> {
        let mut bound = vec![(bindings::HOST_SECRET.to_string(), self.host_secret.as_str())];
        if let Some(previous) = &self.host_secret_previous {
            bound.push((bindings::HOST_SECRET_PREVIOUS.to_string(), previous.as_str()));
        }
        bound
    }

    /// The platform Worker's: every one.
    pub fn cell(&self) -> Vec<(String, &str)> {
        let mut bound = self.agent();
        bound.push((bindings::CODESTORAGE_KEY.to_string(), self.codestorage_key.as_str()));
        if let Some((client, key)) = &self.workos {
            bound.push((bindings::WORKOS_CLIENT.to_string(), client.as_str()));
            bound.push((bindings::WORKOS_KEY.to_string(), key.as_str()));
        }
        for (provider, name) in &self.operator_keys {
            bound.push((bindings::operator_key(provider), name.as_str()));
        }
        assert!(bound.len() <= bindings::CACHE_ENTRIES_MAX, "the cell's cache holds every binding");
        bound
    }
}

/// A Worker config's `secrets_store_secrets`: each binding to its secret
/// in the store `store_id`.
pub fn bindings_json(store_id: &str, bound: &[(String, &str)]) -> Json {
    assert!(!store_id.is_empty(), "a binding names its store");
    let mut seen = std::collections::BTreeSet::new();
    let rows: Vec<Json> = bound
        .iter()
        .map(|(binding, name)| {
            assert!(seen.insert(binding.as_str()), "{binding} is bound once");
            assert!(valid_name(name), "{binding}'s secret {name:?} is a store name (the config is checked first)");
            json!({ "binding": binding, "store_id": store_id, "secret_name": name })
        })
        .collect();
    Json::Array(rows)
}

/// The names `bound` names that `listed` lacks, each with its binding, in
/// binding order: what a deploy refuses to go without.
pub fn missing<'a>(bound: &[(String, &'a str)], listed: &[Listed]) -> Vec<(String, &'a str)> {
    bound.iter().filter(|(_, name)| !listed.iter().any(|l| l.name == *name)).cloned().collect()
}

/// Seeds wrangler's local store under `persist` (a node's state directory)
/// with `values` (each a name and its value), unless it holds exactly
/// these already: a stamp beside the state records what was seeded, and a
/// cleared state takes it along. Locally, a `create` of a name the store
/// holds replaces its value, so this never lists or updates. Answers
/// whether it seeded.
pub fn seed_local(tools: &Tools, persist: &Path, values: &[(&str, &str)]) -> Result<bool, StoreError> {
    assert!(persist.is_absolute(), "a state directory is named absolutely: wrangler runs from the repo root");
    assert!(values.len() <= bindings::CACHE_ENTRIES_MAX, "at most a Worker's bindings");
    let mut digest = Sha256::new();
    digest.update(LOCAL_STORE_ID.as_bytes());
    for (name, value) in values {
        checked_name(name)?;
        for part in [name.as_bytes(), value.as_bytes()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part);
        }
    }
    let wanted: String = digest.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let stamp: PathBuf = persist.join(SEEDED_STAMP);
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(wanted.as_str()) {
        return Ok(false);
    }
    std::fs::create_dir_all(persist).map_err(|source| StoreError::Io { what: format!("create {}", persist.display()), source })?;
    let store = Store::Local { persist };
    // bounded: at most a Worker's bindings, one wrangler run each
    for (name, value) in values {
        let value = Value::checked(value.as_bytes().to_vec(), name)?;
        put(tools, &store, name, value, None)?;
    }
    // written last: a seeding cut short leaves no stamp, and runs again
    std::fs::write(&stamp, &wanted).map_err(|source| StoreError::Io { what: format!("write {}", stamp.display()), source })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What wrangler 4.145.0 printed, piped, for a local `secret list`.
    const LISTED: &str = "\n ⛅️ wrangler 4.145.0\n───────────\n🔐 Listing secrets... (store-id: fragment-local, page: 1, per-page: 100)\n\
┌───────────┬──────────────────────────────────┬─────────┬────────┬─────────┬───────────────────────┬───────────────────────┐\n\
│ Name      │ ID                               │ Comment │ Scopes │ Status  │ Created               │ Modified              │\n\
├───────────┼──────────────────────────────────┼─────────┼────────┼─────────┼───────────────────────┼───────────────────────┤\n\
│ spike-one │ 17cfa8d60d6f4c4c882c49134504ca8b │         │        │ active  │ 10/5/2026, 8:04:03 AM │ 10/5/2026, 8:04:03 AM │\n\
│ spike-two │ 2e185a515b208be2c13f9029d3bf1d6a │         │ workers│ pending │ 10/5/2026, 8:05:00 AM │ 10/5/2026, 8:06:00 AM │\n\
└───────────┴──────────────────────────────────┴─────────┴────────┴─────────┴───────────────────────┴───────────────────────┘\n";

    /// A listing's table reads as its secrets; nothing to list is no
    /// secrets; any other failure, or an answer with no table, is an error.
    #[test]
    fn a_listing_reads_as_its_secrets() {
        let listed = parse_listed(true, LISTED).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0], Listed { name: "spike-one".into(), id: "17cfa8d60d6f4c4c882c49134504ca8b".into(), created: "10/5/2026, 8:04:03 AM".into(), modified: "10/5/2026, 8:04:03 AM".into() });
        assert_eq!(listed[1].modified, "10/5/2026, 8:06:00 AM");
        assert_eq!(parse_listed(false, "✘ [ERROR] List request returned no secrets.\n").unwrap(), vec![]);
        assert!(matches!(parse_listed(false, "✘ [ERROR] Authentication error [code: 10000]"), Err(StoreError::Wrangler { .. })));
        assert!(matches!(parse_listed(true, "🔐 Listing secrets..."), Err(StoreError::Unread { .. })));
    }

    /// The account's stores: none is said, one is read with its id, and a
    /// store made is read from what wrangler says it made.
    #[test]
    fn stores_read_from_wrangler() {
        assert_eq!(parse_stores(false, "🔐 Listing stores...\n✘ [ERROR] List request returned no stores.\n").unwrap(), vec![]);
        let one = "│ Name     │ ID                               │ AccountID │ Created │ Modified │\n├──┤\n│ fragment │ 0f0e0d0c0b0a09080706050403020100 │ 4833      │ x       │ y        │\n";
        assert_eq!(parse_stores(true, one).unwrap(), vec![AccountStore { name: "fragment".into(), id: "0f0e0d0c0b0a09080706050403020100".into() }]);
        assert!(matches!(parse_stores(false, "✘ [ERROR] Not logged in."), Err(StoreError::Wrangler { .. })));
        let made = "🔐 Creating store... (Name: fragment)\n✅ Created store! (Name: fragment, ID: 0f0e0d0c0b0a09080706050403020100)\n";
        assert_eq!(parse_created_store(made).as_deref(), Some("0f0e0d0c0b0a09080706050403020100"));
        assert_eq!(parse_created_store("✅ Created store! (Name: fragment, ID: )"), None);
        assert_eq!(parse_created_store("something else"), None);
    }

    /// Every command names the store and where it is, and none carries a
    /// value: a value goes on standard input or into wrangler's prompt.
    #[test]
    fn the_commands_carry_no_value() {
        let account = Store::Account { account_id: "acct", store_id: "0f0e" };
        let local = Store::Local { persist: Path::new("/r/target/e2e/run/cell/.wrangler/state") };
        let s = |args: Vec<OsString>| args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        assert_eq!(s(create_args(&account, "fragment-host-secret")), "secrets-store secret create 0f0e --name fragment-host-secret --scopes workers --remote");
        assert_eq!(
            s(create_args(&local, "fragment-host-secret")),
            "secrets-store secret create fragment-local --name fragment-host-secret --scopes workers --persist-to /r/target/e2e/run/cell/.wrangler/state"
        );
        assert_eq!(s(update_args(&account, "17cfa8d6")), "secrets-store secret update 0f0e --secret-id 17cfa8d6 --remote");
        assert_eq!(s(list_args(&account)), "secrets-store secret list 0f0e --per-page 100 --remote");
        assert_eq!(s(list_args(&local)), "secrets-store secret list fragment-local --per-page 100 --persist-to /r/target/e2e/run/cell/.wrangler/state");
        assert_eq!(s(stores_args()), "secrets-store store list --remote --per-page 10");
        assert_eq!(s(create_store_args(STORE_NAME)), "secrets-store store create fragment --remote");
        for args in [create_args(&account, "a"), update_args(&local, "b"), list_args(&account), stores_args(), create_store_args("fragment")] {
            assert!(args_hold_no_value(&args), "{}", s(args.clone()));
        }
        assert!(!args_hold_no_value(&["--value=x".into()]) && !args_hold_no_value(&["--value".into(), "x".into()]));
        assert!(std::panic::catch_unwind(|| create_args(&account, "has space")).is_err(), "a name is checked first");
        assert!(std::panic::catch_unwind(|| update_args(&account, "id; rm")).is_err());
    }

    /// Names: the store's characters, and a length this repo bounds.
    #[test]
    fn store_names() {
        for ok in ["fragment-host-secret", "FRAGMENT_KEY", "a", &"a".repeat(NAME_MAX_BYTES)] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "~/.config/fragment/secrets/host-secret", "has space", "a.b", "é", &"a".repeat(NAME_MAX_BYTES + 1)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    /// A value is text, not empty, and within the store's limit once its
    /// trailing whitespace goes; a PEM's lines are kept.
    #[test]
    fn values_are_checked_as_the_store_checks_them() {
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGH\nAgEA\n-----END PRIVATE KEY-----\n";
        assert!(matches!(Value::checked(pem.as_bytes().to_vec(), "pem"), Ok(Value::Bytes(b)) if b == pem.as_bytes()));
        assert!(matches!(Value::checked(b" \n\t".to_vec(), "blank"), Err(StoreError::InvalidValue(_))));
        assert!(matches!(Value::checked(vec![0xff, 0xfe], "binary"), Err(StoreError::InvalidValue(_))));
        let mut big = "a".repeat(VALUE_MAX_BYTES).into_bytes();
        big.extend(b"\n\n");
        assert!(Value::checked(big.clone(), "at the limit").is_ok());
        big.push(b'a');
        assert!(matches!(Value::checked(big, "over"), Err(StoreError::InvalidValue(why)) if !why.contains("aaaa")), "the refusal names no value");
    }

    /// The Workers' bindings: the agents' the host secret alone, the
    /// platform's every one, each once, each to its name in one store; and
    /// what a store lacks is named with its binding.
    #[test]
    fn bindings_name_each_secret_once() {
        let mut bound = Bound::conventional(true, &["perplexity", "google-places"]);
        bound.host_secret_previous = Some("fragment-host-secret-2025".into());
        let cell = bound.cell();
        let json = bindings_json("0f0e", &cell);
        assert_eq!(json[0], json!({ "binding": "HOST_SECRET", "store_id": "0f0e", "secret_name": "fragment-host-secret" }));
        assert_eq!(json[1], json!({ "binding": "HOST_SECRET_PREVIOUS", "store_id": "0f0e", "secret_name": "fragment-host-secret-2025" }));
        let names: Vec<(&str, &str)> = cell.iter().map(|(b, n)| (b.as_str(), *n)).collect();
        assert_eq!(
            names[2..],
            [
                ("CODESTORAGE_KEY", "fragment-codestorage-private-key"),
                ("WORKOS_CLIENT", "fragment-workos-client-id"),
                ("WORKOS_KEY", "fragment-workos-api-key"),
                ("OPERATOR_KEY_PERPLEXITY", "fragment-perplexity-api-key"),
                ("OPERATOR_KEY_GOOGLE_PLACES", "fragment-google-places-api-key"),
            ]
        );
        assert_eq!(bindings_json("0f0e", &bound.agent()).as_array().map(Vec::len), Some(2), "the agents' Worker holds the host secrets alone");
        let doubled = vec![("HOST_SECRET".to_string(), "a"), ("HOST_SECRET".to_string(), "b")];
        assert!(std::panic::catch_unwind(|| bindings_json("0f0e", &doubled)).is_err());

        let listed = |names: &[&str]| names.iter().map(|n| Listed { name: n.to_string(), id: "i".into(), created: String::new(), modified: String::new() }).collect::<Vec<_>>();
        assert_eq!(missing(&bound.agent(), &listed(&["fragment-host-secret", "fragment-host-secret-2025", "other"])), vec![]);
        let gone = missing(&cell, &listed(&["fragment-host-secret", "fragment-codestorage-private-key"]));
        let gone: Vec<&str> = gone.iter().map(|(_, n)| *n).collect();
        assert_eq!(gone, ["fragment-host-secret-2025", "fragment-workos-client-id", "fragment-workos-api-key", "fragment-perplexity-api-key", "fragment-google-places-api-key"]);
    }
}
