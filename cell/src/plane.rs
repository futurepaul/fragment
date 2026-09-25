//! The file plane (docs/api.md): every file is in the fragment's
//! code.storage repo. The cell pins `main` (the working copy) and `live`
//! (what is served and whose `app.mjs` runs), keeps a tree index of each
//! pin (metadata only), and moves a pin when a signed push webhook, an
//! editor's `refresh`, or the poll backstop sees its branch move. Editors
//! write to git directly with a storage token; deploy, preview, and
//! rollback are ref moves the CLI makes.

use std::collections::BTreeMap;

use fragment_core::codestorage::TreeEntry;
use fragment_core::{manifest, npub, site, webhook};
use fragment_proto::{limits, valid_repo_path, ChannelDecl, ErrorCode, OpDecl, OpKind, Role, TriggerDecl, TriggerOn};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::cs::Cs;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, missing, Caller, Facts, FragmentCell, MetaKey};
use crate::js;

/// Platform code that runs in the facet around the author's App class.
pub const PLATFORM_JS: &str = include_str!("../platform.mjs");
/// Webhook deliveries remembered for deduplication.
const DELIVERIES_KEPT: i64 = 10_000;
const DELIVERY_TTL_MS: i64 = 7 * 24 * 3600 * 1000;
/// Tree rows per insert statement.
const TREE_BATCH: usize = 20;
pub const REFS: [&str; 2] = ["main", "live"];

pub struct PinMove {
    pub changed: bool,
    pub to: Option<String>,
    pub paths: Vec<String>,
}

/// A file in a pin's tree index (metadata only; the bytes stay in git).
#[derive(Deserialize)]
pub(crate) struct TreeRow {
    pub path: String,
    pub size: u64,
    pub mode: String,
    /// The last commit that changed the file: with its path, it names
    /// the file's bytes.
    pub last_commit: String,
}

/// What the installed code declares, as its tables hold it: written with
/// the code row in one step (no await between), and read by key.
struct Installed<'a> {
    operations: &'a BTreeMap<String, OpDecl>,
    channels: &'a BTreeMap<String, ChannelDecl>,
    triggers: &'a [TriggerDecl],
}

/// Replaces the code tables' rows. The caller writes the code row in the
/// same step.
fn store_installed(sql: &SqlStorage, code: &Installed<'_>) -> Result<()> {
    assert!(code.operations.len() <= limits::OPERATIONS_MAX, "a manifest's operations are bounded");
    assert!(code.channels.len() <= limits::CHANNELS_MAX, "a manifest's channels are bounded");
    assert!(code.triggers.len() <= limits::TRIGGERS_MAX, "a manifest's triggers are bounded");
    clear_installed(sql)?;
    for (op, d) in code.operations {
        let input = match &d.input {
            Some(schema) => SqlStorageValue::from(schema.to_string()),
            None => SqlStorageValue::Null,
        };
        sql.exec("INSERT INTO code_ops (op, kind, role, input) VALUES (?, ?, ?, ?)", vec![op.as_str().into(), d.kind.as_str().into(), d.role.as_str().into(), input])?;
    }
    for (channel, d) in code.channels {
        sql.exec("INSERT INTO code_channels (channel, read) VALUES (?, ?)", vec![channel.as_str().into(), d.read.as_str().into()])?;
    }
    for (i, t) in code.triggers.iter().enumerate() {
        let (kind, target) = t.on.parts();
        let idx = SqlStorageValue::Integer(i64::try_from(i).expect("triggers are few"));
        sql.exec("INSERT INTO code_triggers (idx, kind, target, run) VALUES (?, ?, ?, ?)", vec![idx, kind.into(), target.into(), t.run.as_str().into()])?;
    }
    let count = |table: &str| -> Result<usize> {
        let rows: Vec<Value> = sql.exec(&format!("SELECT COUNT(*) AS n FROM {table}"), None)?.to_array()?;
        Ok(rows.first().and_then(|r| r["n"].as_u64()).expect("COUNT answers a row") as usize)
    };
    assert_eq!(count("code_ops")?, code.operations.len(), "every operation is stored");
    assert_eq!(count("code_channels")?, code.channels.len(), "every channel is stored");
    assert_eq!(count("code_triggers")?, code.triggers.len(), "every trigger is stored");
    Ok(())
}

fn clear_installed(sql: &SqlStorage) -> Result<()> {
    for table in ["code_ops", "code_channels", "code_triggers"] {
        sql.exec(&format!("DELETE FROM {table}"), None)?;
    }
    Ok(())
}

/// The columns the code row held before its tables: JSON text each.
const CODE_JSON_COLUMNS: [&str; 3] = ["operations", "channels", "triggers"];

/// A fragment stored before the code tables kept its operations, channels,
/// and triggers as JSON columns of the code row (and one from before
/// applib and notifyUrls, phase 2 slices B and C, lacks those columns).
/// Activation moves them into their tables in place, in one step, and
/// drops the columns. Stored JSON that does not decode, or holds more than
/// a manifest may (the cell wrote it from a checked manifest, so it never
/// has), fails closed: the code goes, and the next refresh or poll installs
/// live again. Answers why, then. Stored data never reaches
/// `store_installed`'s assertions: this runs in the constructor, where one
/// that failed would fail every activation of the fragment.
pub(crate) fn migrate_code(sql: &SqlStorage) -> Option<String> {
    let cols: Vec<Value> = sql.exec("PRAGMA table_info(code)", None).and_then(|c| c.to_array()).expect("the code table's columns read");
    let has = |col: &str| cols.iter().any(|c| c["name"] == col);
    for (col, decl) in [("modules", "modules TEXT NOT NULL DEFAULT '{}'"), ("notify", "notify TEXT NOT NULL DEFAULT '[]'")] {
        if !has(col) {
            sql.exec(&format!("ALTER TABLE code ADD COLUMN {decl}"), None).expect("the code table migrates");
        }
    }
    let json_columns: Vec<&str> = CODE_JSON_COLUMNS.into_iter().filter(|c| has(c)).collect();
    if json_columns.is_empty() {
        return None;
    }
    let read: Vec<String> = CODE_JSON_COLUMNS.iter().map(|c| if has(c) { (*c).to_string() } else { format!("NULL AS {c}") }).collect();
    let rows: Vec<Value> = sql.exec(&format!("SELECT {} FROM code WHERE id = 1", read.join(", ")), None).and_then(|c| c.to_array()).expect("the code row reads");
    let outcome = match rows.first() {
        None => {
            clear_installed(sql).expect("the code tables clear");
            None
        }
        Some(row) => {
            let text = |col: &str, empty: &'static str| row[col].as_str().unwrap_or(empty).to_string();
            let bounded = |what: &str, n: usize, max: usize| -> std::result::Result<(), String> {
                if n <= max {
                    Ok(())
                } else {
                    Err(format!("{what}: {n}, past the limit of {max}"))
                }
            };
            let decoded = (|| -> std::result::Result<_, String> {
                let operations: BTreeMap<String, OpDecl> = serde_json::from_str(&text("operations", "{}")).map_err(|e| format!("operations: {e}"))?;
                let channels: BTreeMap<String, ChannelDecl> = serde_json::from_str(&text("channels", "{}")).map_err(|e| format!("channels: {e}"))?;
                let triggers: Vec<TriggerDecl> = serde_json::from_str(&text("triggers", "[]")).map_err(|e| format!("triggers: {e}"))?;
                bounded("operations", operations.len(), limits::OPERATIONS_MAX)?;
                bounded("channels", channels.len(), limits::CHANNELS_MAX)?;
                bounded("triggers", triggers.len(), limits::TRIGGERS_MAX)?;
                Ok((operations, channels, triggers))
            })();
            match decoded {
                Ok((operations, channels, triggers)) => {
                    store_installed(sql, &Installed { operations: &operations, channels: &channels, triggers: &triggers }).expect("the code tables fill");
                    None
                }
                Err(why) => {
                    sql.exec("DELETE FROM code", None).expect("the code row goes");
                    clear_installed(sql).expect("the code tables clear");
                    sql.exec("DELETE FROM meta WHERE key = ?", vec![MetaKey::LiveReadAt.key().into()]).expect("live is read again");
                    Some(why)
                }
            }
        }
    };
    for col in json_columns {
        sql.exec(&format!("ALTER TABLE code DROP COLUMN {col}"), None).expect("the code table drops its JSON column");
    }
    outcome
}

#[derive(Deserialize)]
struct OpRow {
    op: String,
    kind: String,
    role: String,
    input: Option<String>,
}

/// One way to fail for a stored row that does not decode: the cell wrote
/// it from a checked manifest, so it is corruption.
fn stored(what: &str, why: impl std::fmt::Display) -> CellError {
    CellError::host(format!("the installed code's {what} does not decode: {why}"))
}

fn op_decl(row: OpRow) -> CellResult<(String, OpDecl)> {
    let kind = OpKind::parse(&row.kind).ok_or_else(|| stored(&format!("operation {}", row.op), format!("kind {:?}", row.kind)))?;
    let role = Role::parse(&row.role).ok_or_else(|| stored(&format!("operation {}", row.op), format!("role {:?}", row.role)))?;
    let input = match row.input {
        Some(text) => Some(serde_json::from_str(&text).map_err(|e| stored(&format!("operation {}", row.op), e))?),
        None => None,
    };
    Ok((row.op, OpDecl { kind, role, input }))
}

#[derive(Deserialize)]
struct ChannelRow {
    channel: String,
    read: String,
}

fn channel_decl(row: ChannelRow) -> CellResult<(String, ChannelDecl)> {
    let read = Role::parse(&row.read).ok_or_else(|| stored(&format!("channel {}", row.channel), format!("read {:?}", row.read)))?;
    Ok((row.channel, ChannelDecl { read }))
}

#[derive(Deserialize)]
struct TriggerRow {
    idx: i64,
    kind: String,
    target: String,
    run: String,
}

impl FragmentCell {
    pub(crate) fn cs(&self) -> CellResult<Cs<'_>> {
        Ok(Cs::new(self.cfg.codestorage()?, &self.env))
    }

    pub(crate) fn pin(&self, which: &str) -> CellResult<Option<String>> {
        self.meta(MetaKey::pin(which))
    }

    pub(crate) fn tree_row(&self, which: &str, path: &str) -> CellResult<Option<TreeRow>> {
        Ok(self.typed("SELECT path, size, mode, last_commit FROM tree WHERE ref = ? AND path = ?", vec![which.into(), path.into()])?.into_iter().next())
    }

    pub(crate) fn tree_rows(&self, which: &str) -> CellResult<Vec<TreeRow>> {
        self.typed("SELECT path, size, mode, last_commit FROM tree WHERE ref = ? ORDER BY path", vec![which.into()])
    }

    /// Reads the branch head and, when it moved, the new tree; the manifest
    /// (main) or the code (live) follow. Callers hold `self.plane`.
    async fn refresh_pin(&self, which: &str) -> CellResult<PinMove> {
        let [repo, from] = self.metas([MetaKey::Repo, MetaKey::pin(which)])?;
        let repo = repo.ok_or_else(|| missing(MetaKey::Repo))?;
        let cs = self.cs()?;
        let Some(to) = cs.branch_head(&repo, which).await? else {
            if from.is_none() {
                return Ok(PinMove { changed: false, to: None, paths: vec![] });
            }
            self.exec("DELETE FROM tree WHERE ref = ?", vec![which.into()])?;
            self.exec("DELETE FROM pointers WHERE ref = ?", vec![which.into()])?;
            self.del_meta(MetaKey::pin(which))?;
            self.event("git.refresh", &format!("{which}: the branch is gone; pin cleared"), json!({ "ref": which, "from": from }));
            return Ok(PinMove { changed: true, to: None, paths: vec![] });
        };
        if from.as_deref() == Some(to.as_str()) {
            return Ok(PinMove { changed: false, to: Some(to), paths: vec![] });
        }
        let entries = cs.tree(&repo, &to).await?;
        let prior: BTreeMap<String, (u64, String)> = self.tree_rows(which)?.into_iter().map(|r| (r.path, (r.size, r.last_commit))).collect();
        let mut paths: Vec<String> = entries
            .iter()
            .filter(|e| prior.get(&e.path).is_none_or(|(size, last)| *size != e.size || *last != e.last_commit_sha))
            .map(|e| e.path.clone())
            .collect();
        let now: std::collections::BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        paths.extend(prior.keys().filter(|p| !now.contains(p.as_str())).cloned());
        let sizes: std::collections::HashMap<&str, u64> = entries.iter().map(|e| (e.path.as_str(), e.size)).collect();
        self.track_pointers(which, &to, &paths, &sizes).await?;
        self.write_tree(which, &entries)?;
        self.set_meta(MetaKey::pin(which), &to)?;
        self.event(
            "git.refresh",
            &format!("{which}: {} → {} ({} files, {} changed)", short(from.as_deref()), short(Some(&to)), entries.len(), paths.len()),
            json!({ "ref": which, "from": from, "to": to, "changed": paths.iter().take(50).collect::<Vec<_>>() }),
        );
        Ok(PinMove { changed: true, to: Some(to), paths })
    }

    fn write_tree(&self, which: &str, entries: &[TreeEntry]) -> CellResult<()> {
        self.exec("DELETE FROM tree WHERE ref = ?", vec![which.into()])?;
        for batch in entries.chunks(TREE_BATCH) {
            let tuples = vec!["(?, ?, ?, ?, ?)"; batch.len()].join(", ");
            let mut binds: Vec<SqlStorageValue> = Vec::with_capacity(batch.len() * 5);
            for e in batch {
                binds.extend([which.into(), e.path.as_str().into(), SqlStorageValue::Integer(e.size as i64), e.mode.as_str().into(), e.last_commit_sha.as_str().into()]);
            }
            self.exec(&format!("INSERT INTO tree (ref, path, size, mode, last_commit) VALUES {tuples}"), binds)?;
        }
        Ok(())
    }

    /// What follows a pin: the manifest from main, the code from live.
    /// Each records the commit it was read from, so a read that failed
    /// (code.storage down) is retried by the next refresh, webhook, or poll
    /// even though the pin itself already moved.
    async fn follow(&self, which: &str) -> CellResult<()> {
        let done_key = MetaKey::read_at(which);
        let [pin, done] = self.metas([MetaKey::pin(which), done_key])?;
        if done == pin {
            return Ok(());
        }
        match (which, pin.as_deref()) {
            ("main", Some(sha)) => self.read_manifest(&self.must(MetaKey::Repo)?, sha).await?,
            ("main", None) => self.del_meta(MetaKey::ManifestMain)?,
            (_, live) => self.install_code(live).await?,
        }
        match pin {
            Some(sha) => self.set_meta(done_key, &sha),
            None => self.del_meta(done_key),
        }
    }

    /// Keeps `fragment.json` at main for `GET manifest`, and says when it
    /// asks for access the platform no longer takes from git.
    async fn read_manifest(&self, repo: &str, sha: &str) -> CellResult<()> {
        if self.tree_row("main", "fragment.json")?.is_none() {
            return self.del_meta(MetaKey::ManifestMain);
        }
        let Some(bytes) = self.cs()?.read(repo, sha, "fragment.json", limits::MANIFEST_MAX_BYTES).await? else { return self.del_meta(MetaKey::ManifestMain) };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => self.set_meta(MetaKey::ManifestMain, &v.to_string())?,
            Err(e) => {
                self.del_meta(MetaKey::ManifestMain)?;
                self.event("manifest.invalid", &format!("fragment.json at main is not JSON: {e}"), json!({ "sha": sha }));
                return Ok(());
            }
        }
        if let Ok(m) = manifest::parse(&bytes) {
            if !m.ignored.is_empty() {
                self.event(
                    "manifest.ignored",
                    &format!("fragment.json's {} no longer grant access: use `fragment members` and `fragment visibility`", m.ignored.join(", ")),
                    json!({ "keys": m.ignored }),
                );
            }
        }
        Ok(())
    }

    /// Installs the app from the live commit: `fragment.json` declares the
    /// operations and `app.mjs` is the code. A live commit without
    /// `app.mjs` has no app (its schedules go; pauses and breakers stay for
    /// the next code to judge); one with an invalid manifest keeps the last
    /// good code and records why.
    async fn install_code(&self, live: Option<&str>) -> CellResult<()> {
        let repo = self.must(MetaKey::Repo)?;
        let Some(sha) = live else {
            self.exec("DELETE FROM code", vec![])?;
            clear_installed(&self.sql())?;
            self.sync_schedules(&[])?;
            self.del_meta(MetaKey::MetaLive)?;
            self.del_meta(MetaKey::CapabilitiesLive)?;
            self.del_meta(MetaKey::CodeError)?;
            js::abort_app_facet(&self.raw, "live is gone")?;
            return Ok(());
        };
        let cs = self.cs()?;
        let manifest = match self.tree_row("live", "fragment.json")? {
            None => manifest::Manifest::default(),
            Some(_) => {
                let bytes = cs.read(&repo, sha, "fragment.json", limits::MANIFEST_MAX_BYTES).await?.unwrap_or_default();
                match manifest::parse(&bytes) {
                    Ok(m) => m,
                    Err(e) => return self.code_refused(sha, &format!("fragment.json: {e}")),
                }
            }
        };
        match &manifest.meta {
            Some(meta) => self.set_meta(MetaKey::MetaLive, &serde_json::to_string(meta).expect("meta serializes"))?,
            None => self.del_meta(MetaKey::MetaLive)?,
        }
        self.set_meta(MetaKey::CapabilitiesLive, &serde_json::to_string(&manifest.capabilities).expect("a list serializes"))?;
        if self.tree_row("live", "app.mjs")?.is_none() {
            self.exec("DELETE FROM code", vec![])?;
            clear_installed(&self.sql())?;
            self.sync_schedules(&[])?;
            self.del_meta(MetaKey::CodeError)?;
            js::abort_app_facet(&self.raw, "live has no app.mjs")?;
            self.event("code.none", &format!("live {} has no app.mjs", short(Some(sha))), json!({ "sha": sha }));
            return Ok(());
        }
        let source = match cs.read(&repo, sha, "app.mjs", limits::SOURCE_MAX_BYTES).await {
            Ok(Some(b)) => match String::from_utf8(b) {
                Ok(s) => s,
                Err(_) => return self.code_refused(sha, "app.mjs is not UTF-8"),
            },
            Ok(None) => return self.code_refused(sha, "app.mjs vanished between the listing and the read"),
            Err(e) if e.code == ErrorCode::TooLarge => return self.code_refused(sha, &e.message),
            Err(e) => return Err(e),
        };
        // applib/: the modules app.mjs imports, read with it from the same commit
        let libs: Vec<String> = self
            .tree_rows("live")?
            .into_iter()
            .map(|r| r.path)
            .filter(|p| p.starts_with("applib/") && (p.ends_with(".mjs") || p.ends_with(".js")))
            .collect();
        if libs.len() > limits::APPLIB_FILES_MAX {
            return self.code_refused(sha, &format!("applib/ has {} modules; the limit is {}", libs.len(), limits::APPLIB_FILES_MAX));
        }
        let mut modules = BTreeMap::new();
        let mut total = source.len();
        // the app's own modules; the platform code joins the id when it is
        // loaded (ops.rs `facet`), since a cell deploy changes it, not this
        let mut hasher = Sha256::new();
        hasher.update(b"app.js\0");
        hasher.update(source.as_bytes());
        for path in libs {
            let room = limits::APP_MODULES_MAX_BYTES.saturating_sub(total);
            let text = match cs.read(&repo, sha, &path, room).await {
                Ok(Some(b)) => String::from_utf8(b).map_err(|_| format!("{path} is not UTF-8")),
                Ok(None) => Err(format!("{path} vanished between the listing and the read")),
                Err(e) if e.code == ErrorCode::TooLarge => Err(format!("app.mjs and applib/ are over {} bytes", limits::APP_MODULES_MAX_BYTES)),
                Err(e) => return Err(e),
            };
            let text = match text {
                Ok(t) => t,
                Err(why) => return self.code_refused(sha, &why),
            };
            total += text.len();
            hasher.update(b"\0");
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(text.as_bytes());
            modules.insert(path, text);
        }
        let loader_id = format!("app:{}", hex::encode(hasher.finalize()));
        let notify = serde_json::to_string(&manifest.notify_urls).expect("urls serialize");
        let module_count = modules.len();
        // The code row and its tables, in one step: no await until they are all written.
        self.exec(
            "INSERT INTO code (id, sha, loader_id, source, cpu_ms, installed_at, modules, notify) VALUES (1, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, loader_id = excluded.loader_id, source = excluded.source,
               cpu_ms = excluded.cpu_ms, installed_at = excluded.installed_at, modules = excluded.modules, notify = excluded.notify",
            vec![
                sha.into(),
                loader_id.into(),
                source.into(),
                SqlStorageValue::Integer(limits::APP_CPU_MS.into()),
                SqlStorageValue::Integer(js::now_ms()),
                serde_json::to_string(&modules).expect("modules serialize").into(),
                notify.into(),
            ],
        )?;
        let installed = Installed { operations: &manifest.operations, channels: &manifest.channels, triggers: &manifest.triggers };
        store_installed(&self.sql(), &installed)?;
        self.sync_schedules(&manifest.triggers)?;
        self.forget_undeclared_pauses()?;
        self.del_meta(MetaKey::CodeError)?;
        js::abort_app_facet(&self.raw, "new code from live")?;
        self.event(
            "code.installed",
            &format!(
                "app.mjs from live {} ({} operations, {} channels, {} triggers, {module_count} applib modules)",
                short(Some(sha)),
                manifest.operations.len(),
                manifest.channels.len(),
                manifest.triggers.len()
            ),
            json!({ "sha": sha, "operations": manifest.operations.keys().collect::<Vec<_>>() }),
        );
        Ok(())
    }

    fn code_refused(&self, sha: &str, why: &str) -> CellResult<()> {
        self.set_meta(MetaKey::CodeError, &format!("live {}: {why}", short(Some(sha))))?;
        self.event("code.refused", &format!("live {} not installed: {why}", short(Some(sha))), json!({ "sha": sha }));
        Ok(())
    }

    /// Refreshes pins; a move of main notifies the change feed and starts
    /// the runs its file triggers name.
    pub(crate) async fn interpret(&self, refs: &[&str]) -> CellResult<Vec<(String, PinMove)>> {
        let out = self.interpret_locked(refs).await?;
        // the file triggers' runs, the notifyUrls, and the alarm for newly installed schedules
        self.launch_queued().await;
        for (which, moved) in &out {
            if which == "main" && moved.changed {
                if let Err(e) = self.notify_urls(moved.to.as_deref(), &moved.paths).await {
                    self.event("notify.failed", &e.message, json!({ "code": e.code }));
                }
            }
        }
        self.schedule().await?;
        Ok(out)
    }

    async fn interpret_locked(&self, refs: &[&str]) -> CellResult<Vec<(String, PinMove)>> {
        let _held = self.plane.lock().await;
        let mut out = vec![];
        for which in refs {
            let moved = self.refresh_pin(which).await?;
            if *which == "main" && moved.changed {
                self.broadcast_change(moved.to.as_deref(), &moved.paths);
                let depth = self.commit_depth(moved.to.as_deref())?;
                self.fire_files(moved.to.as_deref(), &moved.paths, depth)?;
            }
            self.follow(which).await?;
            out.push((which.to_string(), moved));
        }
        Ok(out)
    }

    /// Pins read before either branch has been announced (a fragment
    /// whose first push predates its webhook) are fetched on the first
    /// request, once: after that, a branch that is still absent is left to
    /// the webhook, `refresh`, and the poll backstop, so a fragment with
    /// nothing deployed (or with data only) asks code.storage nothing, and
    /// takes no plane lock, per request. A check that fails is not
    /// recorded, so the next request asks again. `facts` follows what moved.
    pub(crate) async fn ensure_pins(&self, facts: &mut Facts) -> CellResult<()> {
        if facts.pins_checked {
            return Ok(());
        }
        let missing: Vec<&str> = REFS.into_iter().filter(|w| facts.pin(w).is_none()).collect();
        if !missing.is_empty() {
            for (which, moved) in self.interpret(&missing).await? {
                facts.set_pin(&which, moved.to);
            }
        }
        self.set_meta(MetaKey::PinsCheckedAt, &js::now_ms().to_string())?;
        facts.pins_checked = true;
        Ok(())
    }

    fn broadcast_change(&self, sha: Option<&str>, paths: &[String]) {
        let frame = json!({ "type": "changed", "ref": "main", "sha": sha, "paths": paths.iter().take(200).collect::<Vec<_>>() }).to_string();
        for ws in self.state.get_websockets_with_tag("watch") {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// The backstop for lost webhooks, from the alarm.
    pub(crate) async fn poll(&self) {
        if let Err(e) = self.interpret(&REFS).await {
            self.event("git.poll-failed", &e.message, json!({ "code": e.code }));
        }
    }

    pub(crate) async fn refresh(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let moved = self.interpret(&REFS).await?;
        let refs: serde_json::Map<String, Value> = moved
            .into_iter()
            .map(|(which, m)| {
                let v = match m.to {
                    Some(pin) => json!({ "pin": pin, "moved": m.changed }),
                    None => json!({ "absent": true }),
                };
                (which, v)
            })
            .collect();
        json_response(&json!({ "ok": true, "refs": refs }))
    }

    /// A code.storage push delivery: validate, then remember (redeliveries
    /// are acknowledged, not interpreted twice), then interpret. The pin
    /// moves to the branch's head as read now, so a late or out-of-order
    /// delivery never moves it backwards.
    pub(crate) async fn webhook(&self, event: &str, signature: &str, body: &[u8]) -> CellResult<Response> {
        self.name()?;
        let secret = self.must(MetaKey::WebhookSecret)?;
        if let Err(why) = webhook::verify(body, signature, &secret, js::now_ms() / 1000, limits::WEBHOOK_WINDOW_S) {
            self.event("webhook.rejected", &format!("{event}: {why}"), Value::Null);
            return Err(CellError::new(ErrorCode::Unauthenticated, why));
        }
        let payload: Value = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("webhook body: {e}")))?;
        let Some(push) = webhook::parse_push(event, &payload) else {
            return json_response(&json!({ "ok": true, "ignored": event }));
        };
        let key = webhook::dedupe_key(event, &payload);
        if !self.rows("SELECT key FROM deliveries WHERE key = ?", vec![key.as_str().into()])?.is_empty() {
            self.event("webhook.redelivery", &format!("{}: {} (already seen)", push.branch, short(Some(&push.after))), Value::Null);
            return json_response(&json!({ "ok": true, "interpreted": false, "redelivery": true }));
        }
        let now = js::now_ms();
        self.exec("INSERT INTO deliveries (key, at) VALUES (?, ?)", vec![key.into(), SqlStorageValue::Integer(now)])?;
        self.exec("DELETE FROM deliveries WHERE at < ?", vec![SqlStorageValue::Integer(now - DELIVERY_TTL_MS)])?;
        self.exec(
            "DELETE FROM deliveries WHERE key NOT IN (SELECT key FROM deliveries ORDER BY at DESC LIMIT ?)",
            vec![SqlStorageValue::Integer(DELIVERIES_KEPT)],
        )?;
        self.event(
            "webhook.push",
            &format!("{}: {} → {}", push.branch, short(Some(&push.before)), short(Some(&push.after))),
            json!({ "ref": push.branch }),
        );
        if !REFS.contains(&push.branch.as_str()) {
            return json_response(&json!({ "ok": true, "interpreted": false, "redelivery": false }));
        }
        self.interpret(&[push.branch.as_str()]).await?;
        json_response(&json!({ "ok": true, "interpreted": true, "redelivery": false }))
    }

    pub(crate) async fn storage_token(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let who = self.caller_id(caller)?;
        let repo = self.must(MetaKey::Repo)?;
        let token = self.cs()?.storage_token(&repo, who).await?;
        self.event(
            "storage-token.minted",
            &format!("{} → repo {repo}, git:read+git:write, {}s", npub::display(who), limits::STORAGE_TOKEN_TTL_S),
            json!({ "actor": npub::display(who), "key": caller.key().map(npub::display), "repo": repo, "expiresAt": token.expires_at }),
        );
        json_response(&token)
    }

    pub(crate) fn manifest(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        match self.meta(MetaKey::ManifestMain)? {
            Some(text) => json_response(&serde_json::from_str::<Value>(&text).map_err(|e| CellError::host(e.to_string()))?),
            None => Err(CellError::new(ErrorCode::NotFound, "main has no fragment.json")),
        }
    }

    pub(crate) async fn files(&self, caller: &Caller) -> CellResult<Response> {
        let mut facts = self.facts()?;
        self.admit(&facts, caller, false, Role::Viewer)?;
        self.ensure_pins(&mut facts).await?;
        let blobs = self.pointer_sizes("main")?;
        let files: Vec<Value> = self
            .tree_rows("main")?
            .into_iter()
            .map(|r| {
                let mut f = json!({ "path": r.path, "size": r.size, "mode": r.mode, "lastCommitSha": r.last_commit, "machinery": site::is_machinery(&r.path) });
                if let Some(size) = blobs.get(&r.path) {
                    f["size"] = json!(size);
                    f["blob"] = json!(true);
                }
                f
            })
            .collect();
        json_response(&json!({ "ref": facts.pin_main, "files": files }))
    }

    /// Streams a file from a pin (`main` for the API, `live` for the site);
    /// a pointer's bytes come from the blob store. Only a file of a
    /// pointer's size can be one, so only those look for a pointer row.
    pub(crate) async fn stream_file(&self, facts: &Facts, which: &str, row: &TreeRow, range: Option<&str>) -> CellResult<Response> {
        if crate::blobs::maybe_pointer(row.size) {
            if let Some(resp) = self.serve_pointer(which, &row.path, range).await? {
                return Ok(resp);
            }
        }
        self.stream_git(facts, which, &row.path).await
    }

    /// Streams a file's bytes from git at a pin.
    pub(crate) async fn stream_git(&self, facts: &Facts, which: &str, path: &str) -> CellResult<Response> {
        let pin = facts.pin(which).ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no {which} pin yet")))?;
        let upstream = self.cs()?.stream(&facts.repo, pin, path).await?;
        let headers = Headers::new();
        headers.set("content-type", site::mime_for_path(path))?;
        headers.set("x-fragment-ref", pin)?;
        if let Ok(Some(len)) = upstream.headers().get("content-length") {
            headers.set("content-length", &len)?;
        }
        let (_, body) = upstream.into_parts();
        Ok(Response::from_body(body)?.with_headers(headers))
    }

    pub(crate) async fn file(&self, caller: &Caller, path: &str) -> CellResult<Response> {
        let mut facts = self.facts()?;
        self.admit(&facts, caller, false, Role::Viewer)?;
        if !valid_repo_path(path) {
            return Err(CellError::invalid("path must be a relative repo path"));
        }
        self.ensure_pins(&mut facts).await?;
        let Some(row) = self.tree_row("main", path)? else {
            return Err(CellError::new(ErrorCode::NotFound, format!("no file {path} at main")));
        };
        let mut resp = self.stream_file(&facts, "main", &row, None).await?;
        resp.headers_mut().set("cache-control", "no-store")?;
        Ok(resp)
    }

    pub(crate) async fn stat(&self, caller: &Caller, path: &str) -> CellResult<Response> {
        let mut facts = self.facts()?;
        self.admit(&facts, caller, false, Role::Viewer)?;
        if !valid_repo_path(path) {
            return Err(CellError::invalid("path must be a relative repo path"));
        }
        self.ensure_pins(&mut facts).await?;
        let absent = json!({ "path": path, "size": 0, "blobSha": "", "lastCommitSha": "", "present": false });
        let stat = match (&facts.pin_main, self.tree_row("main", path)?) {
            (Some(pin), Some(row)) => match self.cs()?.head(&facts.repo, pin, path).await? {
                Some(h) => json!({ "path": path, "size": h.size, "blobSha": h.blob_sha, "lastCommitSha": h.last_commit_sha, "present": true }),
                None => json!({ "path": path, "size": row.size, "blobSha": "", "lastCommitSha": row.last_commit, "present": true }),
            },
            _ => absent,
        };
        json_response(&json!({ "stat": stat, "ref": facts.pin_main }))
    }

    /// The installed operations (from the live commit); none without code.
    pub(crate) fn operations(&self) -> CellResult<BTreeMap<String, OpDecl>> {
        self.typed::<OpRow>("SELECT op, kind, role, input FROM code_ops ORDER BY op", vec![])?.into_iter().map(op_decl).collect()
    }

    /// The declared operation, or why there is none: no code, or no such
    /// operation in it. One keyed read, whatever the manifest holds.
    pub(crate) fn declared(&self, op: &str) -> CellResult<OpDecl> {
        #[derive(Deserialize)]
        struct Found {
            op: Option<String>,
            kind: Option<String>,
            role: Option<String>,
            input: Option<String>,
        }
        let rows: Vec<Found> =
            self.typed("SELECT o.op, o.kind, o.role, o.input FROM code c LEFT JOIN code_ops o ON o.op = ? WHERE c.id = 1", vec![op.into()])?;
        let Some(found) = rows.into_iter().next() else {
            return Err(CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"));
        };
        match (found.op, found.kind, found.role) {
            (Some(op), Some(kind), Some(role)) => Ok(op_decl(OpRow { op, kind, role, input: found.input })?.1),
            (None, None, None) => Err(CellError::new(ErrorCode::UnknownOperation, format!("no operation named {op:?}"))),
            _ => Err(stored(&format!("operation {op}"), "a partial row")),
        }
    }

    /// The app channels the live code declares.
    pub(crate) fn declared_channels(&self) -> CellResult<BTreeMap<String, ChannelDecl>> {
        self.typed::<ChannelRow>("SELECT channel, read FROM code_channels ORDER BY channel", vec![])?.into_iter().map(channel_decl).collect()
    }

    /// Who may read an app channel the live code declares; `None` when it declares no such channel.
    pub(crate) fn declared_channel(&self, channel: &str) -> CellResult<Option<Role>> {
        let rows: Vec<ChannelRow> = self.typed("SELECT channel, read FROM code_channels WHERE channel = ?", vec![channel.into()])?;
        Ok(rows.into_iter().next().map(channel_decl).transpose()?.map(|(_, d)| d.read))
    }

    /// The installed triggers, in their manifest's order (a cron trigger's
    /// schedule is keyed by that index).
    pub(crate) fn triggers(&self) -> CellResult<Vec<TriggerDecl>> {
        let rows: Vec<TriggerRow> = self.typed("SELECT idx, kind, target, run FROM code_triggers ORDER BY idx", vec![])?;
        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.into_iter().enumerate() {
            if row.idx != i64::try_from(i).expect("triggers are few") {
                return Err(stored(&format!("trigger {i}"), format!("it is stored at index {}", row.idx)));
            }
            let on = TriggerOn::from_parts(&row.kind, row.target).ok_or_else(|| stored(&format!("trigger {i}"), format!("kind {:?}", row.kind)))?;
            out.push(TriggerDecl { on, run: row.run });
        }
        Ok(out)
    }

    /// The operations a record on `channel` starts: each once (two triggers
    /// that run one operation start it once), in their first trigger's order.
    pub(crate) fn channel_triggers(&self, channel: &str) -> CellResult<Vec<String>> {
        #[derive(Deserialize)]
        struct Run {
            run: String,
        }
        let on = TriggerOn::Channel(channel.to_string());
        let (kind, target) = on.parts();
        let rows: Vec<Run> = self.typed("SELECT run FROM code_triggers WHERE kind = ? AND target = ? GROUP BY run ORDER BY MIN(idx)", vec![kind.into(), target.into()])?;
        Ok(rows.into_iter().map(|r| r.run).collect())
    }

    /// A test hook (fleets with test hooks only): puts the installed code
    /// back into the shape fragments stored before the code tables (JSON
    /// columns of the code row), so the e2e can restart the node and watch
    /// `migrate_code` move it. With `fill`, placeholder operations are
    /// stored beside the real ones until there are `fill`, so the e2e can
    /// store more than a manifest may and watch the move fail closed.
    pub(crate) fn code_before_tables(&self, fill: Option<u64>) -> CellResult<()> {
        assert!(self.cfg.test_hooks, "the hook answers only on fleets with test hooks");
        let (mut operations, channels, triggers) = (self.operations()?, self.declared_channels()?, self.triggers()?);
        if let Some(fill) = fill {
            let fill_max = 2 * limits::OPERATIONS_MAX as u64;
            if fill > fill_max {
                return Err(CellError::invalid(format!("fill is at most {fill_max}")));
            }
            // Bounded by `fill`, just checked. Of the `fill` names tried, at
            // most as many as there are operations are taken, so the rest
            // are enough.
            for i in 0..fill {
                if operations.len() as u64 >= fill {
                    break;
                }
                operations.entry(format!("filler_{i}")).or_insert(OpDecl { kind: OpKind::Mutation, role: Role::Editor, input: None });
            }
            assert!(operations.len() as u64 >= fill, "filled to the count asked");
        }
        for (col, empty) in CODE_JSON_COLUMNS.into_iter().zip(["{}", "{}", "[]"]) {
            self.exec(&format!("ALTER TABLE code ADD COLUMN {col} TEXT NOT NULL DEFAULT '{empty}'"), vec![])?;
        }
        let text = |v: serde_json::Result<String>| v.map_err(|e| CellError::host(e.to_string()));
        self.exec(
            "UPDATE code SET operations = ?, channels = ?, triggers = ? WHERE id = 1",
            vec![
                text(serde_json::to_string(&operations))?.into(),
                text(serde_json::to_string(&channels))?.into(),
                text(serde_json::to_string(&triggers))?.into(),
            ],
        )?;
        clear_installed(&self.sql())?;
        Ok(())
    }
}

fn short(sha: Option<&str>) -> String {
    sha.map(|s| s.chars().take(12).collect()).unwrap_or_else(|| "(none)".into())
}
