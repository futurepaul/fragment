// Bidirectional folder sync, v2 (hard cut):
// - state v2 with size+mtime_ns cache → O(changes) scans (verify for truth)
// - atomic writes everywhere (state, pulled files); flock single-watcher guard
// - three-way merge on conflicts (server keeps history); markers or copies
// - append-only prefixes respected; mass-deletion guard; push/pull/mirror
// - two-tier storage (docs/blob-tier.md): rows are pointers; bytes >64 KiB
//   go CLI-direct to the blob tier below, pulls materialize via
//   .fragment/cache/<sha> + streamed tmp + atomic rename
use crate::api::{encode_q, Client};
use crate::blob;
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[derive(Debug)]
pub enum Mode {
    Push,
    Pull,
    Mirror,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictStrategy {
    Markers,
    Copy,
}

#[derive(Clone)]
pub struct SyncOptions {
    pub mode: Mode,
    /// overlay a read-only source folder into dir before each pass: new
    /// and changed files copy in (source never written, nothing deleted —
    /// dir can hold app code and drops alongside the mirrored content)
    pub mirror_from: Option<PathBuf>,
    pub strategy: ConflictStrategy,
    pub apply_mass_delete: bool,
    pub prune: bool,
    pub verify: bool,
    pub writer_id: String, // 8 hex of our pubkey, for conflict-copy names
    /// resolved blob-tier handle for THIS pass (memoized uploads across the
    /// parallel warm pass and the serial commit loop); attached internally by
    /// sync_once — callers construct options without it
    pub tiers: Option<Arc<blob::TierShared>>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            mode: Mode::Mirror,
            mirror_from: None,
            strategy: ConflictStrategy::Markers,
            apply_mass_delete: false,
            prune: false,
            verify: false,
            writer_id: "anon".into(),
            tiers: None,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct SyncState {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32, // 2; anything else reads as fresh
    pub name: String,
    pub root_dev: u64,
    pub root_ino: u64,
    pub files: HashMap<String, FileState>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileState {
    pub rev: u64,
    pub sha256: String,
    pub size: u64,
    pub mtime_ns: i128,
}

#[derive(Default, Serialize)]
pub struct Report {
    pub pushed: Vec<String>,
    pub pulled: Vec<String>,
    pub merged: Vec<String>,
    pub conflicts: Vec<String>,
    pub deleted_remote: Vec<String>,
    pub deleted_local: Vec<String>,
    pub withheld_deletions: Vec<String>,
    pub skipped_append_only: Vec<String>,
    pub mass_delete_guard: Option<usize>,
    #[serde(rename = "appendOnly")]
    pub append_only: Vec<String>,
    pub scan: ScanStats,
    pub mode: String,
}

#[derive(Default, Serialize, Clone, Copy)]
pub struct ScanStats {
    pub files: usize,
    pub hashed: usize,
}

impl Report {
    /// exit code: 0 clean/merged, 3 conflicts present, 4 mass-deletion guard
    pub fn exit_code(&self) -> i32 {
        if self.mass_delete_guard.is_some() {
            4
        } else if !self.conflicts.is_empty() {
            3
        } else {
            0
        }
    }

    pub fn print(&self) {
        let p = |label: &str, v: &Vec<String>| {
            if !v.is_empty() {
                println!("  {label}: {}", v.len());
                for f in v {
                    println!("    {f}");
                }
            }
        };
        p("pushed", &self.pushed);
        p("pulled", &self.pulled);
        p("merged (auto, three-way)", &self.merged);
        p("deleted remotely", &self.deleted_remote);
        p("deleted locally", &self.deleted_local);
        if !self.withheld_deletions.is_empty() {
            println!("  deletions withheld (pull mode; --prune to apply): {})", self.withheld_deletions.len());
        }
        p("append-only drift (not pushed; edit remotely or remove local)", &self.skipped_append_only);
        if !self.conflicts.is_empty() {
            println!("  CONFLICTS ({}): local keeps yours; see the markers/copies", self.conflicts.len());
            for f in &self.conflicts {
                println!("    {f}");
            }
        }
        if let Some(n) = self.mass_delete_guard {
            println!("  REFUSING to propagate {n} deletions — folder looks unmounted/reset.");
            println!("  re-run with --apply-mass-delete if this is intended");
        }
        let clean = self.pushed.is_empty()
            && self.pulled.is_empty()
            && self.merged.is_empty()
            && self.conflicts.is_empty()
            && self.deleted_remote.is_empty()
            && self.deleted_local.is_empty()
            && self.skipped_append_only.is_empty();
        if clean {
            println!("all {} files match host", self.scan.files);
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join(".fragment").join("state.json")
}

/// The single-watcher lock: an advisory flock on .fragment/sync.lock, held
/// for the process lifetime. Stale by construction (the OS drops it when
/// the holder dies), unlike pid-file guessing.
pub struct SyncLock(File);
impl SyncLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        let p = dir.join(".fragment").join("sync.lock");
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let f = File::create(&p)?;
        f.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("another fragment sync holds this folder ({}). If that's wrong, no process should own it; otherwise stop it or use a different folder.", p.display()))?;
        Ok(SyncLock(f))
    }
}

pub fn load_state(dir: &Path, name: &str) -> Result<SyncState> {
    let p = state_path(dir);
    if !p.exists() {
        return Ok(SyncState { schema_version: 2, name: name.to_string(), root_dev: 0, root_ino: 0, files: HashMap::new() });
    }
    // Hard cut: anything that isn't clean v2 reads as missing (one full
    // re-scan) — corrupt or old states never wedge the folder
    match serde_json::from_slice::<SyncState>(&fs::read(&p)?) {
        Ok(s) if s.schema_version == 2 => {
            if s.name != name {
                bail!("directory is synced to fragment '{}', not '{}'", s.name, name);
            }
            Ok(s)
        }
        _ => {
            eprintln!("warning: {} unreadable or old format — rebuilding state", p.display());
            Ok(SyncState { schema_version: 2, name: name.to_string(), root_dev: 0, root_ino: 0, files: HashMap::new() })
        }
    }
}

/// atomic write: tmp in the same directory (unique name), fsync, rename
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("fragment-partial-{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_state(dir: &Path, state: &SyncState) -> Result<()> {
    atomic_write(&state_path(dir), serde_json::to_string_pretty(state)?.as_bytes())
}

struct LocalFile {
    sha256: String,
    size: u64,
    mtime_ns: i128,
}

/// Walk the folder. With state, a size+mtime match adopts the cached hash
/// (O(changes)); with verify, everything is read and hashed. Skips
/// dotfiles, .fragment/, symlinks, and sync artifacts. Honors .gitignore
/// (nested, parent-dir, global, and .git/info/exclude) exactly when git
/// would: only inside a repo — a plain folder with a stray .gitignore
/// still syncs everything.
pub fn scan_local(dir: &Path, state: Option<&SyncState>, verify: bool) -> Result<(HashMap<String, LocalFile>, ScanStats)> {
    let mut out = HashMap::new();
    let mut stats = ScanStats::default();
    // ripgrep's walker: hidden-file skipping (the old dotfile rules) plus
    // full gitignore semantics; require_git stays at its default (true) so
    // ignore rules apply only where git itself would apply them
    let walker = ignore::WalkBuilder::new(dir)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build();
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().map_or(false, |t| t.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // editor droppings and sync artifacts the walker doesn't know about
        if name.ends_with("~")
            || name.starts_with("~$")
            || name.starts_with(".#")
            || name.ends_with(".swp")
        {
            continue;
        }
        let rel = entry.path().strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
        if rel.contains(".remote-") || rel.contains(".conflict-") || rel.contains(".fragment-partial") {
            continue;
        }
        let md = fs::metadata(entry.path())?;
        let size = md.len();
        let mtime_ns = md
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0);
        let cached = state.and_then(|s| s.files.get(&rel)).filter(|st| st.size == size && st.mtime_ns == mtime_ns);
        let sha = if cached.is_some() && !verify {
            cached.unwrap().sha256.clone()
        } else {
            stats.hashed += 1;
            sha256_hex(&fs::read(entry.path())?)
        };
        stats.files += 1;
        out.insert(rel, LocalFile { sha256: sha, size, mtime_ns });
    }
    Ok((out, stats))
}

struct RemoteFile {
    rev: u64,
    sha256: String,
    deleted: bool,
}

/// copy new/changed files from src into dir (never writes src, never
/// deletes in dir); preserves mtimes so the scan shortcut stays valid
fn mirror_overlay(src: &Path, dir: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.ends_with("~") || name.starts_with(".#") || name.ends_with(".swp") {
            continue;
        }
        let rel = entry.path().strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/");
        if rel.split('/').any(|seg| seg.starts_with('.') && seg != ".") {
            continue;
        }
        // the target's own identity and state are never overlaid: a source
        // folder carrying its own fragment.json must not stomp the
        // corrected one (restore agents hit exactly this)
        if rel == "fragment.json" || rel.starts_with(".fragment/") {
            continue;
        }
        let target = dir.join(&rel);
        let src_meta = fs::metadata(entry.path())?;
        if let Ok(t) = fs::metadata(&target) {
            if t.len() == src_meta.len() {
                let sm = src_meta.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
                let tm = t.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
                if sm == tm {
                    continue;
                }
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &target)?; // fs::copy preserves mtime
    }
    Ok(())
}

pub fn sync_once(client: &Client, name: &str, dir: &Path, opts: &SyncOptions) -> Result<Report> {
    if let Some(src) = &opts.mirror_from {
        mirror_overlay(src, dir)?;
    }
    let mut state = load_state(dir, name)?;

    // two-tier plumbing for this pass: FRAGMENT_BLOB_URL overrides config
    // blob_url; without a tier the <=64 KiB inline path still works but any
    // oversized push hard-fails with guidance (never a silent fallback)
    let mut opts_owned = opts.clone();
    opts_owned.tiers = Some(blob::TierShared::new(blob::BlobTier::resolve(&client.id)));
    let opts = &opts_owned;

    // root identity: a different (dev,ino) with recorded identity means the
    // folder was moved/replaced — refuse rather than "sync" against a stranger
    let md = fs::metadata(dir)?;
    use std::os::unix::fs::MetadataExt;
    if state.root_dev != 0 && (state.root_dev != md.dev() || state.root_ino != md.ino()) {
        bail!(
            "folder moved or replaced since the last sync (root identity changed).\n  if this is really the same folder, run `fragment sync {name} --dir {} --rebuild-state`",
            dir.display()
        );
    }
    state.root_dev = md.dev();
    state.root_ino = md.ino();

    let (mut local, stats) = scan_local(dir, Some(&state), opts.verify)?;

    // beyond bloasd's own max-blob size nothing can ever land in the tier:
    // those are warn-skipped (state keeps the last-good copy) instead of
    // blocking the whole publish. Everything between the 64 KiB inline limit
    // and this cap takes the blob-first path — an unconfigured tier fails
    // THAT push loudly (blob::no_tier_error), it does not degrade to raw.
    const OVER_TIER_CAP_SKIP: u64 = blob::MAX_BLOB_BYTES;
    let mut oversized: Vec<String> = Vec::new();
    let mut kept: HashMap<String, LocalFile> = HashMap::new();
    for (path, file) in local.iter() {
        if file.size > OVER_TIER_CAP_SKIP {
            oversized.push(format!("  {} ({:.1} MB over the 64 MiB tier cap)", path, (file.size - OVER_TIER_CAP_SKIP) as f64 / 1_000_000.0));
        }
    }
    if !oversized.is_empty() {
        eprintln!("warning: skipped {} oversized file(s) — even the blob tier caps out above here; host big media on a bucket/CDN and link it:\n{}", oversized.len(), oversized.join("\n"));
        let skipped: Vec<String> = local
            .iter()
            .filter(|(_, f)| f.size > OVER_TIER_CAP_SKIP)
            .map(|(p, _)| p.clone())
            .collect();
        for path in &skipped {
            if let Some(last_good) = state.files.get(path) {
                kept.insert(
                    path.clone(),
                    LocalFile { sha256: last_good.sha256.clone(), size: last_good.size, mtime_ns: last_good.mtime_ns },
                );
            }
        }
        local.retain(|_, f| f.size <= OVER_TIER_CAP_SKIP);
        for (path, file) in kept {
            local.insert(path, file);
        }
    }

    let remote_resp = client.call(client.get(&format!("/api/f/{name}/files?since_rev=0"))?)?;
    let mut remote: HashMap<String, RemoteFile> = HashMap::new();
    for f in remote_resp["files"].as_array().cloned().unwrap_or_default() {
        remote.insert(
            f["path"].as_str().unwrap_or("").to_string(),
            RemoteFile { rev: f["rev"].as_u64().unwrap_or(0), sha256: f["sha256"].as_str().unwrap_or("").to_string(), deleted: f["deleted"].as_bool().unwrap_or(false) },
        );
    }
    let manifest = client.call(client.get(&format!("/api/f/{name}/manifest"))?)?;
    // the manifest-set guard: a fragment.json in the folder that disagrees
    // with the live manifest means the files deployed but the MACHINERY
    // (workflows, triggers, visibility, appendOnly) never armed — the #1
    // silent-deploy trap observed across three agent-eval runs
    let local_manifest_path = dir.join("fragment.json");
    if local_manifest_path.exists() {
        if let Ok(raw) = fs::read_to_string(&local_manifest_path) {
            if let Ok(local) = serde_json::from_str::<Value>(&raw) {
                if let Ok(resp) = client.call(client.post_json(&format!("/api/f/{name}/manifest/check"), &local)?) {
                    if resp["differs"].as_bool() == Some(true) {
                        eprintln!(
                            "WARNING: fragment.json in the folder differs from the live manifest — files synced but the machinery (workflows/triggers/visibility) is NOT live.\n  fix: fragment manifest-set {name} {}",
                            local_manifest_path.display()
                        );
                    }
                }
            }
        }
    }
    let append_only: Vec<String> = manifest["appendOnly"].as_array().cloned().unwrap_or_default().iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
    let is_append_only = |p: &str| append_only.iter().any(|pre| p == pre.trim_end_matches('/') || p.starts_with(pre.as_str()));

    let mut report = Report { scan: stats, mode: format!("{:?}", opts.mode).to_lowercase(), append_only: append_only.clone(), ..Default::default() };

    let mut paths: Vec<String> = local.keys().chain(remote.keys()).chain(state.files.keys()).cloned().collect();
    paths.sort();
    paths.dedup();

    // mass-deletion guard: count what this pass would delete locally
    if !opts.apply_mass_delete {
        let known = state.files.len().max(1);
        let pending_deletes = paths.iter().filter(|p| {
            local.get(*p).is_none()
                && remote.get(*p).map(|r| !r.deleted).unwrap_or(false)
                && state.files.get(*p).map(|s| s.rev == remote.get(*p).unwrap().rev).unwrap_or(false)
        }).count();
        if pending_deletes > 10 && pending_deletes * 10 > known * 3 {
            report.mass_delete_guard = Some(pending_deletes);
            save_state(dir, &state)?;
            return Ok(report);
        }
    }

    // blob-first warm pass: every big file this pass plausibly pushes gets
    // its tier upload done here, <=UPLOAD_CONCURRENCY-wide, while the
    // row-commit loop below stays serial (base_rev conflicts need order).
    prewarm_tier(dir, &local, &append_only, &remote, opts.mode, opts.tiers.as_ref().expect("tiers attached"));

    for path in paths {
        let l = local.get(&path);
        let r = remote.get(&path).filter(|r| !r.deleted);
        let r_tomb = remote.get(&path).filter(|r| r.deleted);
        let s = state.files.get(&path).cloned();

        match (l, r) {
            (Some(lf), Some(rf)) => {
                let local_changed = s.as_ref().map(|st| st.sha256 != lf.sha256).unwrap_or(true);
                let remote_changed = s.as_ref().map(|st| st.rev != rf.rev).unwrap_or(true);
                match (local_changed, remote_changed) {
                    (false, false) => {
                        // keep mtime cache fresh
                        if let Some(st) = state.files.get_mut(&path) {
                            st.size = lf.size;
                            st.mtime_ns = lf.mtime_ns;
                        }
                    }
                    (true, false) => {
                        if opts.mode == Mode::Pull {
                            continue;
                        }
                        if is_append_only(&path) {
                            // our local edit can never overwrite the remote
                            // append-only file; keep both, say so once
                            state.files.insert(path.clone(), FileState { rev: rf.rev, sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns });
                            report.skipped_append_only.push(path.clone());
                            continue;
                        }
                        push_file(client, name, dir, &path, s.as_ref().map(|x| x.rev).unwrap_or(0), lf, &mut state, &mut report, opts)?;
                    }
                    (false, true) => {
                        if opts.mode == Mode::Push {
                            continue;
                        }
                        pull_file(client, name, dir, &path, &mut state, &mut report, Some((rf.rev, rf.sha256.as_str())))?;
                    }
                    (true, true) => {
                        if rf.sha256 == lf.sha256 {
                            // racing sync already pushed our content: adopt
                            state.files.insert(path.clone(), FileState { rev: rf.rev, sha256: rf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns });
                        } else if is_append_only(&path) {
                            state.files.insert(path.clone(), FileState { rev: rf.rev, sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns });
                            report.skipped_append_only.push(path.clone());
                        } else {
                            resolve_conflict(client, name, dir, &path, lf, rf, s.as_ref(), &mut state, &mut report, opts)?;
                        }
                    }
                }
            }
            (Some(lf), None) => {
                if opts.mode == Mode::Pull {
                    continue;
                }
                if let (Some(tomb), Some(st)) = (r_tomb, &s) {
                    if tomb.rev == st.rev && st.sha256 == lf.sha256 {
                        fs::remove_file(dir.join(&path)).ok();
                        state.files.remove(&path);
                        report.deleted_local.push(path.clone());
                        continue;
                    }
                }
                let base = r_tomb.map(|t| t.rev).unwrap_or(0);
                push_file(client, name, dir, &path, base, lf, &mut state, &mut report, opts)?;
            }
            (None, Some(rf)) => match &s {
                Some(st) if st.rev == rf.rev => {
                    // deleted locally, unchanged remotely → delete remotely
                    if opts.mode == Mode::Pull {
                        if !opts.prune {
                            report.withheld_deletions.push(path.clone());
                            state.files.remove(&path); // stop re-reporting every pass
                            continue;
                        }
                        // --prune in pull mode: pull it back instead of deleting remote
                        pull_file(client, name, dir, &path, &mut state, &mut report, Some((rf.rev, rf.sha256.as_str())))?;
                        continue;
                    }
                    if is_append_only(&path) {
                        // non-owner deletes under append-only are refused
                        // server-side; keep the local file, say so once
                        state.files.remove(&path);
                        report.skipped_append_only.push(path.clone());
                        continue;
                    }
                    let resp = client.delete(&format!("/api/f/{name}/file?path={}", encode_q(&path)))?;
                    if resp.ok() {
                        state.files.remove(&path);
                        report.deleted_remote.push(path.clone());
                    } else {
                        bail!("delete {} failed: {}", path, resp.err_summary());
                    }
                }
                _ => {
                    if opts.mode == Mode::Push {
                        continue;
                    }
                    pull_file(client, name, dir, &path, &mut state, &mut report, Some((rf.rev, rf.sha256.as_str())))?;
                }
            },
            (None, None) => {
                state.files.remove(&path);
            }
        }
    }

    save_state(dir, &state)?;
    Ok(report)
}

fn push_file(
    client: &Client,
    name: &str,
    dir: &Path,
    path: &str,
    base_rev: u64,
    lf: &LocalFile,
    state: &mut SyncState,
    report: &mut Report,
    opts: &SyncOptions,
) -> Result<()> {
    let bytes = fs::read(dir.join(path))?;
    let qs = format!("/api/f/{name}/file?path={}&base_rev={base_rev}", encode_q(path));
    let resp = commit_push(client, opts.tiers.as_ref(), &qs, path, bytes.clone())?;
    if resp.status == 409 {
        // lost a race (or append-only): treat as a conflict to resolve
        let st = state.files.get(path).cloned();
        let remote = fetch_remote(client, name, path)?;
        if let Some(rf) = remote {
            resolve_conflict(client, name, dir, path, lf, &rf, st.as_ref(), state, report, opts)?;
        }
        return Ok(());
    }
    if !resp.ok() {
        bail!("push {} failed: {}", path, resp.err_summary());
    }
    let v = resp.json()?;
    let sha = sha256_hex(&bytes);
    state.files.insert(path.to_string(), FileState { rev: v["rev"].as_u64().unwrap_or(0), sha256: sha, size: lf.size, mtime_ns: lf.mtime_ns });
    report.pushed.push(path.to_string());
    Ok(())
}

/// One file → one row commit through the two-tier contract:
/// `bytes.len() <= 65536` rides the inline carve-out as a raw body;
/// anything larger is uploaded to the tier FIRST (kind-24242 auth event,
/// HEAD-gated, descriptor hash verified) and then committed in the ref
/// form {"ref":{"sha256","size","mime"}}. No tier + oversized = hard error.
fn commit_push(
    client: &Client,
    tiers: Option<&Arc<blob::TierShared>>,
    qs: &str,
    path: &str,
    bytes: Vec<u8>,
) -> Result<crate::api::Resp> {
    if bytes.len() <= blob::INLINE_MAX_BYTES {
        return client.put_bytes(qs, bytes);
    }
    match tiers {
        Some(tiers) => blob::blob_ensure(tiers, &sha256_hex(&bytes), &bytes, blob::mime_for(path))?,
        None => return Err(blob::no_tier_error(path, bytes.len())),
    }
    client.put_ref(qs, &sha256_hex(&bytes), bytes.len() as u64, blob::mime_for(path))
}

/// Warm the tier for every big file this pass plausibly pushes so the
/// uploads fan out (bounded by blob::UPLOAD_CONCURRENCY) instead of running
/// one-at-a-time inside the commit loop. The candidate predicate is a
/// deliberate SUPERSET of the real push decision: a spurious warm-up costs
/// one HEAD probe against an idempotent store, while any missed warm-up
/// still self-heals when push_file calls blob_ensure (same memo).
/// Candidates are deduped by sha first — two files with identical content
/// share one memo entry, so they can never double-PUT.
fn prewarm_tier(
    dir: &Path,
    local: &HashMap<String, LocalFile>,
    append_only: &[String],
    remote: &HashMap<String, RemoteFile>,
    mode: Mode,
    tiers: &Arc<blob::TierShared>,
) {
    const MAX_IN_FLIGHT: usize = blob::UPLOAD_CONCURRENCY; // hard concurrency ceiling
    if tiers.blob.is_none() || mode == Mode::Pull {
        return; // nothing to warm without a tier or without pushes
    }
    let mut cands: Vec<(String, PathBuf)> = Vec::new();
    for (path, lf) in local.iter() {
        if lf.size <= blob::INLINE_MAX_BYTES as u64 {
            continue;
        }
        let is_append_only = |p: &str| append_only.iter().any(|pre| p == pre.trim_end_matches('/') || p.starts_with(pre.as_str()));
        if is_append_only(path) {
            continue;
        }
        // will it actually change rows? unchanged-content files would only
        // cost a HEAD; conservatively include them anyway except when the
        // remote listing already shows this exact sha (pure no-op)
        let remote_already_has_it =
            remote.get(path).is_some_and(|r| !r.deleted && r.sha256 == lf.sha256);
        if remote_already_has_it {
            continue;
        }
        cands.push((lf.sha256.clone(), dir.join(path)));
    }
    if cands.is_empty() {
        return;
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0));
    cands.dedup_by(|a, b| a.0 == b.0);
    for chunk in cands.chunks(MAX_IN_FLIGHT) {
        std::thread::scope(|scope| {
            for (sha, p) in chunk {
                let tiers = tiers.clone();
                let sha = sha.clone();
                let p = p.clone();
                scope.spawn(move || {
                    // best-effort: failures are recorded in the shared memo and
                    // replayed loudly at the row-commit site; nothing printed here
                    let _ = std::fs::read(&p).and_then(|bytes| {
                        blob::blob_ensure(&tiers, &sha, &bytes, crate::blob::mime_for(p.to_string_lossy().as_ref())).map_err(|e| {
                            std::io::Error::other(format!("{e:#}"))
                        })
                    });
                });
            }
        });
    }
}

fn pull_file(
    client: &Client,
    name: &str,
    dir: &Path,
    path: &str,
    state: &mut SyncState,
    report: &mut Report,
    src: Option<(u64, &str)>,
) -> Result<()> {
    let target = dir.join(path);

    // cache short-circuit BEFORE any network traffic: a present cache entry
    // for this exact sha is content-addressed truth, so repeated pulls of an
    // unchanged hash stay offline (and instant). Read errors degrade to a
    // normal network pull.
    let cached = src
        .as_ref()
        .and_then(|(_, sha)| blob::cache_lookup(dir, sha))
        .and_then(|p| fs::read(&p).ok());
    if let (Some((rev, sha)), Some(bytes)) = (&src, cached) {
        atomic_write(&target, &bytes)?;
        let md = fs::metadata(&target)?;
        let mtime_ns = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
        state.files.insert(
            path.to_string(),
            FileState { rev: *rev, sha256: (*sha).to_string(), size: md.len(), mtime_ns },
        );
        report.pulled.push(path.to_string());
        return Ok(());
    }

    // network fetch: follows redirects (the new public-read wire form answers
    // with a 302 to `${BLOBSD_PUBLIC_URL}/${sha}`), streams to tmp next to
    // the target (heap-flat for big blobs), fsyncs, then atomically renames;
    // Last-Modified is honored best-effort so later scans keep their shortcut
    let mut sr = client.get_stream(&format!("/api/f/{name}/file?path={}", encode_q(path)))?;
    if !(200..300).contains(&sr.status) {
        let st = sr.status;
        bail!("pull {} failed: http {} {}", path, st, sr.err_summary());
    }
    let mtime = sr.last_modified();
    let mut reader = sr.into_read();
    let tmp = target.with_extension(format!("fragment-partial-{}", std::process::id()));
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let sha;
    {
        let f = File::create(&tmp)?;
        let mut hw = HashWriter { inner: f, hasher: Sha256::new() };
        std::io::copy(&mut reader, &mut hw).with_context(|| format!("streaming {path}"))?;
        hw.inner.sync_all()?;
        sha = hex::encode(hw.hasher.finalize());
    }
    if let Some((_, want)) = &src {
        if !want.is_empty() && !want.eq_ignore_ascii_case(&sha) {
            fs::remove_file(&tmp).ok();
            bail!("pull {} failed: fetched sha256 {sha} but the listing promised {want}", path);
        }
    }
    // write-through into the cache BEFORE publishing the file: hygiene errors
    // never block a pull, so treat them as warnings only
    if let Err(e) = blob::cache_store(dir, &sha, &tmp) {
        eprintln!("warning: could not cache {}: {e}", sha);
    }
    if let Some(t) = mtime {
        let f = File::options().write(true).open(&tmp)?;
        f.set_times(fs::FileTimes::new().set_modified(t))?;
    }
    fs::rename(&tmp, &target)?;

    let md = fs::metadata(&target)?;
    let mtime_ns = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
    state.files.insert(
        path.to_string(),
        FileState { rev: src.map(|(r, _)| r).unwrap_or(0), sha256: sha, size: md.len(), mtime_ns },
    );
    report.pulled.push(path.to_string());
    Ok(())
}

/// Write-forwarding wrapper tallying sha256 + byte count while streaming a
/// pull body to its tmp file.
struct HashWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}
impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // hash only the bytes actually consumed (partial writes are legal)
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn fetch_remote(client: &Client, name: &str, path: &str) -> Result<Option<RemoteFile>> {
    let resp = client.call(client.get(&format!("/api/f/{name}/files?since_rev=0"))?)?;
    for f in resp["files"].as_array().cloned().unwrap_or_default() {
        if f["path"].as_str() == Some(path) {
            return Ok(Some(RemoteFile { rev: f["rev"].as_u64().unwrap_or(0), sha256: f["sha256"].as_str().unwrap_or("").to_string(), deleted: f["deleted"].as_bool().unwrap_or(false) }));
        }
    }
    Ok(None)
}

/// both sides changed: try a three-way merge against the recorded ancestor
/// (the server keeps history), falling back to markers or conflict copies
fn resolve_conflict(
    client: &Client,
    name: &str,
    dir: &Path,
    path: &str,
    lf: &LocalFile,
    rf: &RemoteFile,
    st: Option<&FileState>,
    state: &mut SyncState,
    report: &mut Report,
    opts: &SyncOptions,
) -> Result<()> {
    let ours = fs::read(dir.join(path))?;
    let theirs_resp = client.get(&format!("/api/f/{name}/file?path={}", encode_q(path)))?;
    if !theirs_resp.ok() {
        bail!("conflict fetch {} failed: {}", path, theirs_resp.err_summary());
    }
    let theirs = theirs_resp.body;

    // ancestor: the revision we last synced, from server history
    let base: Option<Vec<u8>> = match st {
        Some(s) if s.rev > 0 => {
            let a = client.get(&format!("/api/f/{name}/file/at?path={}&rev={}", encode_q(path), s.rev))?;
            if a.ok() && sha256_hex(&a.body) == s.sha256 { Some(a.body) } else { None }
        }
        _ => None,
    };

    match try_merge(base.as_deref(), &ours, &theirs) {
        MergeOutcome::Clean(merged) => {
            atomic_write(&dir.join(path), &merged)?;
            let sha = sha256_hex(&merged);
            let qs = format!("/api/f/{name}/file?path={}&base_rev={}", encode_q(path), rf.rev);
            // merged pushes follow the same two-tier contract as plain pushes
            let resp = commit_push(client, opts.tiers.as_ref(), &qs, path, merged.clone())?;
            if resp.ok() {
                let v = resp.json()?;
                let md = fs::metadata(dir.join(path))?;
                let mtime_ns = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
                state.files.insert(path.to_string(), FileState { rev: v["rev"].as_u64().unwrap_or(0), sha256: sha, size: md.len(), mtime_ns });
                report.merged.push(path.to_string());
            } else {
                // push lost a race; local merge stands, next pass re-resolves
                report.conflicts.push(format!("{path} (merged locally, push lost a race: {})", resp.err_summary()));
            }
        }
        MergeOutcome::Markers(merged) if opts.strategy == ConflictStrategy::Markers => {
            atomic_write(&dir.join(path), &merged)?;
            let sha = sha256_hex(&merged);
            let md = fs::metadata(dir.join(path))?;
            let mtime_ns = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
            // stable until the user resolves: local sha adopted at remote rev
            state.files.insert(path.to_string(), FileState { rev: rf.rev, sha256: sha, size: md.len(), mtime_ns });
            report.conflicts.push(path.to_string());
        }
        _ => {
            // copy strategy (or markers of a non-mergeable): save theirs
            // beside ours, keep ours local, adopt (remote rev, local sha)
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
            let (stem, ext) = match path.rfind('.') {
                Some(i) if !path[i..].contains('/') => (&path[..i], &path[i..]),
                _ => (path, ""),
            };
            let conflict_path = format!("{stem}.conflict-{ts}-{}{ext}", opts.writer_id);
            atomic_write(&dir.join(&conflict_path), &theirs)?;
            state.files.insert(path.to_string(), FileState { rev: rf.rev, sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns });
            report.conflicts.push(format!("{path} (remote copy: {conflict_path})"));
        }
    }
    Ok(())
}

enum MergeOutcome {
    Clean(Vec<u8>),
    Markers(Vec<u8>),
    NotMergeable,
}

fn try_merge(base: Option<&[u8]>, ours: &[u8], theirs: &[u8]) -> MergeOutcome {
    let base = match base {
        Some(b) => b,
        None => return MergeOutcome::NotMergeable, // no ancestor → copy
    };
    const MAX: usize = 5 * 1024 * 1024;
    if ours.len() > MAX || theirs.len() > MAX || base.len() > MAX {
        return MergeOutcome::NotMergeable;
    }
    let (b, o, t) = match (std::str::from_utf8(base), std::str::from_utf8(ours), std::str::from_utf8(theirs)) {
        (Ok(b), Ok(o), Ok(t)) => (b.replace("\r\n", "\n"), o.replace("\r\n", "\n"), t.replace("\r\n", "\n")),
        _ => return MergeOutcome::NotMergeable,
    };
    // diffy 0.5: Err(conflicted) carries the full merge WITH <<<<<<< markers.
    // JSON gets no special treatment: a key-level "smart" merge was cut —
    // zero uses ever, and silently dropping the other side's scalar loses
    // data without a sound. JSON conflicts get loud markers like any text.
    match diffy::merge(&b, &o, &t) {
        Ok(m) => MergeOutcome::Clean(m.into_bytes()),
        Err(conflicted) => MergeOutcome::Markers(conflicted.into_bytes()),
    }
}

/// full-hash audit: local truth vs remote listing
pub fn verify(client: &Client, name: &str, dir: &Path) -> Result<Report> {
    let mut opts = SyncOptions::default();
    opts.verify = true;
    let state = load_state(dir, name)?;
    let (local, stats) = scan_local(dir, Some(&state), true)?;
    let remote_resp = client.call(client.get(&format!("/api/f/{name}/files?since_rev=0"))?)?;
    let mut drift = Report { scan: stats, mode: "verify".into(), ..Default::default() };
    for f in remote_resp["files"].as_array().cloned().unwrap_or_default() {
        let p = f["path"].as_str().unwrap_or("").to_string();
        let rsha = f["sha256"].as_str().unwrap_or("").to_string();
        if f["deleted"].as_bool().unwrap_or(false) {
            if local.contains_key(&p) {
                drift.conflicts.push(format!("{p}: deleted remotely, present locally"));
            }
            continue;
        }
        match local.get(&p) {
            Some(lf) if lf.sha256 == rsha => {}
            Some(_) => drift.conflicts.push(format!("{p}: content differs from remote")),
            None => drift.conflicts.push(format!("{p}: missing locally")),
        }
    }
    for p in local.keys() {
        if !remote_resp["files"].as_array().cloned().unwrap_or_default().iter().any(|f| f["path"].as_str() == Some(p.as_str())) {
            drift.conflicts.push(format!("{p}: not on remote"));
        }
    }
    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_uses_cache() {
        let dir = std::env::temp_dir().join(format!("fragment-scan2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("site")).unwrap();
        fs::write(dir.join("site/index.html"), b"hi").unwrap();
        // first scan: hashed
        let (l1, s1) = scan_local(&dir, None, false).unwrap();
        assert_eq!(s1.hashed, 1);
        // seed state, rescan with cache: zero hashes
        let mut st = SyncState { schema_version: 2, name: "x".into(), root_dev: 0, root_ino: 0, files: HashMap::new() };
        let lf = l1.get("site/index.html").unwrap();
        st.files.insert("site/index.html".into(), FileState { rev: 1, sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns });
        let (_, s2) = scan_local(&dir, Some(&st), false).unwrap();
        assert_eq!(s2.hashed, 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_leaves_no_partial() {
        let dir = std::env::temp_dir().join(format!("fragment-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        atomic_write(&dir.join("a.txt"), b"content").unwrap();
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"content");
        assert!(dir.join("a.fragment-partial-1").exists() == false);
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains("partial")).collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge3_non_overlapping() {
        let base = "line1\nline2\nline3\n";
        let ours = "OURS\nline2\nline3\n";
        let theirs = "line1\nline2\nTHEIRS\n";
        match try_merge(Some(base.as_bytes()), ours.as_bytes(), theirs.as_bytes()) {
            MergeOutcome::Clean(m) => {
                let s = String::from_utf8(m).unwrap();
                assert!(s.contains("OURS"));
                assert!(s.contains("THEIRS"));
                assert!(!s.contains("<<<<<<<"));
            }
            _ => panic!("expected clean merge"),
        }
    }

    #[test]
    fn merge3_overlapping_marks() {
        let base = "line\n";
        let ours = "ours-version\n";
        let theirs = "theirs-version\n";
        match try_merge(Some(base.as_bytes()), ours.as_bytes(), theirs.as_bytes()) {
            MergeOutcome::Markers(m) => assert!(String::from_utf8(m).unwrap().contains("<<<<<<<")),
            _ => panic!("expected markers"),
        }
    }

    #[test]
    fn old_state_reads_fresh() {
        let dir = std::env::temp_dir().join(format!("fragment-v1state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".fragment")).unwrap();
        fs::write(dir.join(".fragment/state.json"), b"{\"name\":\"x\",\"files\":{\"a\":{\"rev\":1,\"sha256\":\"beef\"}}}").unwrap();
        let st = load_state(&dir, "x").unwrap();
        assert!(st.files.is_empty()); // v1 → fresh
        assert_eq!(st.schema_version, 2);
        fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod hw_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    // pins the trap this once caused: hex::encode(finalize()) is the digest,
    // while sha256_hex(finalize()) would hash the digest a second time
    fn hash_writer_tallies_streamed_bytes() {
        let payload = b"hello pull";
        let mut hw = HashWriter { inner: Cursor::new(Vec::new()), hasher: Sha256::new() };
        let mut rd = Cursor::new(payload.to_vec());
        std::io::copy(&mut rd, &mut hw).unwrap();
        let inner = hw.inner.into_inner();
        assert_eq!(inner, payload.to_vec(), "bytes forwarded");
        assert_eq!(hex::encode(hw.hasher.finalize()), "6a853b1f10c0c94d79bdd80f7fd724207d05b5174104d67677ff916e1114f096");
        assert_ne!(sha256_hex(&Sha256::digest(payload)), "6a853b1f10c0c94d79bdd80f7fd724207d05b5174104d67677ff916e1114f096");
    }
}
