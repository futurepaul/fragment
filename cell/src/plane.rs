//! The file plane (docs/api.md): every file is in the fragment's
//! code.storage repo. The cell pins `main` (the working copy) and `live`
//! (what is served and whose `app.mjs` runs), keeps a tree index of each
//! pin (metadata only), and moves a pin when a signed push webhook, an
//! editor's `refresh`, or the poll backstop sees its branch move. Editors
//! write to git directly with a storage token; deploy, preview, and
//! rollback are ref moves the CLI makes. The poll backstop keeps its
//! interval only while something outside the platform may be writing (a
//! token minted, a webhook, in the last day) or other work rides it
//! (`busy`); a fragment nothing touches is polled once a day.

use std::collections::BTreeMap;

use fragment_core::tree::{self, Indexed, TreeDiff};
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
/// How far apart a quiet fragment's passes are (the poll backstop, and the
/// housekeeping it carries): a day. A busy one's come every poll interval
/// (`FragmentCell::busy`).
pub const QUIET_PASS_MS: i64 = 24 * 3600 * 1000;
/// How long a storage token minted, or a webhook, keeps a fragment busy: a
/// token lives 15 minutes, so this is generous for whoever pushes with it.
const OUTSIDE_WRITES_MS: i64 = 24 * 3600 * 1000;
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
        assert!(d.post.is_none_or(|p| p >= d.read), "a checked manifest's post role is never looser than its read");
        let post = d.post.map_or(SqlStorageValue::Null, |p| p.as_str().into());
        sql.exec("INSERT INTO code_channels (channel, read, post) VALUES (?, ?, ?)", vec![channel.as_str().into(), d.read.as_str().into(), post])?;
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

/// Who may post to a channel came after the channel table (phase 7 slice
/// B1): a table from before gains the column, and its channels take no
/// posts. Runs in the constructor, before anything reads the channels.
pub(crate) fn migrate_code(sql: &SqlStorage) {
    let cols: Vec<Value> = sql.exec("PRAGMA table_info(code_channels)", None).and_then(|c| c.to_array()).expect("the channel table's columns read");
    if !cols.iter().any(|c| c["name"] == "post") {
        sql.exec("ALTER TABLE code_channels ADD COLUMN post TEXT", None).expect("the channel table migrates");
    }
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
    post: Option<String>,
}

fn channel_decl(row: ChannelRow) -> CellResult<(String, ChannelDecl)> {
    let role = |key: &str, text: &str| Role::parse(text).ok_or_else(|| stored(&format!("channel {}", row.channel), format!("{key} {text:?}")));
    let read = role("read", &row.read)?;
    let post = row.post.as_deref().map(|p| role("post", p)).transpose()?;
    Ok((row.channel, ChannelDecl { read, post }))
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
        let prior: BTreeMap<String, Indexed> =
            self.tree_rows(which)?.into_iter().map(|r| (r.path, Indexed { size: r.size, mode: r.mode, last_commit: r.last_commit })).collect();
        let diff = tree::diff(&prior, &entries);
        let paths = diff.paths();
        let sizes: std::collections::HashMap<&str, u64> = entries.iter().map(|e| (e.path.as_str(), e.size)).collect();
        self.track_pointers(which, &to, &paths, &sizes).await?;
        self.write_tree(which, &diff, entries.len())?;
        self.set_meta(MetaKey::pin(which), &to)?;
        self.event(
            "git.refresh",
            &format!("{which}: {} → {} ({} files, {} changed)", short(from.as_deref()), short(Some(&to)), entries.len(), paths.len()),
            json!({ "ref": which, "from": from, "to": to, "changed": paths.iter().take(50).collect::<Vec<_>>() }),
        );
        Ok(PinMove { changed: true, to: Some(to), paths })
    }

    /// Writes a pin move's changes to its index (`tree::diff`): deletes
    /// the paths gone and upserts the ones added or changed, so a one-file
    /// commit writes one row whatever the tree's size. The index then
    /// holds one row per file of the listing, `files`, or the listing
    /// named a path twice: then the pin does not move, and the next
    /// refresh diffs again from what was written.
    fn write_tree(&self, which: &str, diff: &TreeDiff<'_>, files: usize) -> CellResult<()> {
        for batch in diff.removed.chunks(TREE_BATCH) {
            let marks = vec!["?"; batch.len()].join(", ");
            let mut binds: Vec<SqlStorageValue> = Vec::with_capacity(batch.len() + 1);
            binds.push(which.into());
            binds.extend(batch.iter().map(|p| SqlStorageValue::from(*p)));
            self.exec(&format!("DELETE FROM tree WHERE ref = ? AND path IN ({marks})"), binds)?;
        }
        for batch in diff.upserts.chunks(TREE_BATCH) {
            let tuples = vec!["(?, ?, ?, ?, ?)"; batch.len()].join(", ");
            let mut binds: Vec<SqlStorageValue> = Vec::with_capacity(batch.len() * 5);
            for e in batch {
                binds.extend([which.into(), e.path.as_str().into(), SqlStorageValue::Integer(e.size as i64), e.mode.as_str().into(), e.last_commit_sha.as_str().into()]);
            }
            self.exec(
                &format!(
                    "INSERT INTO tree (ref, path, size, mode, last_commit) VALUES {tuples}
                     ON CONFLICT (ref, path) DO UPDATE SET size = excluded.size, mode = excluded.mode, last_commit = excluded.last_commit"
                ),
                binds,
            )?;
        }
        let held = self.rows("SELECT COUNT(*) AS n FROM tree WHERE ref = ?", vec![which.into()])?.first().and_then(|r| r["n"].as_u64()).expect("COUNT answers a row");
        if held != files as u64 {
            return Err(CellError::host(format!("{which}'s index holds {held} files after the move, not the listing's {files} (a path listed twice?)")));
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
            (_, live) => {
                self.install_code(live).await?;
                self.tell_computer().await;
            }
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
    /// the next code to judge), and keeps only its manifest's channels:
    /// people post to them with no app code at all (a chat). One with an
    /// invalid manifest keeps the last good code and records why.
    async fn install_code(&self, live: Option<&str>) -> CellResult<()> {
        let repo = self.must(MetaKey::Repo)?;
        let Some(sha) = live else {
            self.exec("DELETE FROM code", vec![])?;
            clear_installed(&self.sql())?;
            self.sync_schedules(&[])?;
            self.del_meta(MetaKey::MetaLive)?;
            self.del_meta(MetaKey::CapabilitiesLive)?;
            self.want_computer(false)?;
            self.del_meta(MetaKey::CodeError)?;
            self.set_agent_live(None)?;
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
        // its agent's instructions are read here, so one that cannot be is refused with the rest
        let agent = match &manifest.agent {
            None => None,
            Some(decl) => match self.agent_at_live(&repo, sha, decl).await? {
                Ok(agent) => Some(agent),
                Err(why) => return self.code_refused(sha, &why),
            },
        };
        match &manifest.meta {
            Some(meta) => self.set_meta(MetaKey::MetaLive, &serde_json::to_string(meta).expect("meta serializes"))?,
            None => self.del_meta(MetaKey::MetaLive)?,
        }
        self.set_meta(MetaKey::CapabilitiesLive, &serde_json::to_string(&manifest.capabilities).expect("a list serializes"))?;
        self.set_agent_live(agent.as_ref())?;
        self.want_computer(manifest.computer)?;
        if self.tree_row("live", "app.mjs")?.is_none() {
            self.exec("DELETE FROM code", vec![])?;
            // no operations to run, so nothing for a trigger to start
            store_installed(&self.sql(), &Installed { operations: &BTreeMap::new(), channels: &manifest.channels, triggers: &[] })?;
            self.sync_schedules(&[])?;
            self.del_meta(MetaKey::CodeError)?;
            js::abort_app_facet(&self.raw, "live has no app.mjs")?;
            let summary = format!("live {} has no app.mjs ({} channels)", short(Some(sha)), manifest.channels.len());
            self.event("code.none", &summary, json!({ "sha": sha, "channels": manifest.channels.keys().collect::<Vec<_>>() }));
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

    /// The backstop for lost webhooks, from the alarm's pass.
    pub(crate) async fn poll(&self) {
        if let Err(e) = self.interpret(&REFS).await {
            self.event("git.poll-failed", &e.message, json!({ "code": e.code }));
        }
    }

    /// Whether the fragment's next pass (the poll backstop and the
    /// housekeeping it carries) comes within the poll interval rather than
    /// a day after the last: something outside the platform may have
    /// written its repo in the last day (a storage token was minted for it,
    /// or a webhook arrived), a run is in flight (each pass checks it
    /// against its Workflow) or held with a video's reservation to give
    /// back, or a template or a declared agent is still to land (each pass
    /// tries again). The rest of the alarm's work has due times of its own.
    /// A fragment nothing touches (a chat, from its second day) is woken
    /// once a day, and asks code.storage twice.
    pub(crate) fn busy(&self) -> CellResult<bool> {
        #[derive(Deserialize)]
        struct Busy {
            outside_at: Option<String>,
            pending: i64,
            running: i64,
            videos: i64,
        }
        let rows: Vec<Busy> = self.typed(
            "SELECT (SELECT value FROM meta WHERE key = ?) AS outside_at,
               EXISTS (SELECT 1 FROM meta WHERE key IN (?, ?, ?)) AS pending,
               EXISTS (SELECT 1 FROM runs WHERE status = 'running') AS running,
               EXISTS (SELECT 1 FROM spend WHERE video IS NOT NULL AND run IN (SELECT id FROM runs WHERE status = 'held')) AS videos",
            vec![MetaKey::OutsideAt.key().into(), MetaKey::TemplatePending.key().into(), MetaKey::AgentPending.key().into(), MetaKey::ComputerPending.key().into()],
        )?;
        let b = rows.into_iter().next().expect("a SELECT without FROM answers one row");
        let outside = b.outside_at.and_then(|at| at.parse::<i64>().ok()).is_some_and(|at| js::now_ms() - at < OUTSIDE_WRITES_MS);
        Ok(outside || b.pending != 0 || b.running != 0 || b.videos != 0)
    }

    /// Something outside the platform may write the repo from now on (a
    /// storage token was minted for it, or a webhook arrived): for the next
    /// day the poll backstop comes within its interval (`busy`).
    async fn outside_writer(&self) -> CellResult<()> {
        self.set_meta(MetaKey::OutsideAt, &js::now_ms().to_string())?;
        self.schedule().await
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
        self.outside_writer().await?;
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
        self.outside_writer().await?;
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
        self.typed::<ChannelRow>("SELECT channel, read, post FROM code_channels ORDER BY channel", vec![])?.into_iter().map(channel_decl).collect()
    }

    /// An app channel the live code declares (who reads it, who may post
    /// to it); `None` when it declares no such channel.
    pub(crate) fn declared_channel(&self, channel: &str) -> CellResult<Option<ChannelDecl>> {
        let rows: Vec<ChannelRow> = self.typed("SELECT channel, read, post FROM code_channels WHERE channel = ?", vec![channel.into()])?;
        Ok(rows.into_iter().next().map(channel_decl).transpose()?.map(|(_, d)| d))
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
}

fn short(sha: Option<&str>) -> String {
    sha.map(|s| s.chars().take(12).collect()).unwrap_or_else(|| "(none)".into())
}
