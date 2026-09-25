// Thin sync over the code.storage commit builder — commit without a local
// clone (ROADMAP workstream B):
//   push  = scan folder, diff against the branch-head listing, ONE
//           commit-pack with expected-parent CAS; conflict -> refetch head,
//           rebuild the diff, retry (bounded, explicit error after)
//   pull  = fetch tree/files from main into the folder
//   mirror= push then pull
// No local .git, no git2, no host file API. `.fragment/state.json` is a
// stat cache only (path -> content sha + mtime + last-seen commit); the
// repo is the truth, the folder is a disposable working copy.
//
// Hard cuts vs the old engine: the three-way merge/conflict-marker
// machinery and the blob tier are gone (remote content survives in git
// history; conflicts keep local and save a `.conflict-` copy). The
// root-identity (dev/ino) check is subsumed by the mass-deletion guard,
// which now also refuses a total wipe regardless of file count.
use crate::api::Client;
use crate::api::CodedError;
use crate::codestorage::{Author, Change, CodeStorage, CsError, MAIN, MAX_CAS_ATTEMPTS};
use fragment_core::codestorage::TreeEntry;
use anyhow::{anyhow, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Push,
    Pull,
    Mirror,
}

#[derive(Clone)]
pub struct SyncOptions {
    pub mode: Mode,
    /// overlay a read-only source folder into dir before each pass: new
    /// and changed files copy in (source never written, nothing deleted —
    /// dir can hold app code and drops alongside the mirrored content).
    /// Non-git sources (fbrain) depend on this; git sources would collapse
    /// to a fetch, but that is not the Brain case.
    pub mirror_from: Option<PathBuf>,
    pub apply_mass_delete: bool,
    /// in pull mode, delete local files that were deleted remotely
    /// (pull never deletes without it; mirror always propagates)
    pub prune: bool,
    pub writer_id: String, // 8 hex of our pubkey, for conflict-copy names
    /// FRAGMENT_CODESTORAGE_URL/config override for the code.storage
    /// server (backend-swap knob; else the storage-token response wins)
    pub codestorage: Option<String>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            mode: Mode::Mirror,
            mirror_from: None,
            apply_mass_delete: false,
            prune: false,
            writer_id: "anon".into(),
            codestorage: None,
        }
    }
}

/// Typed sync errors (engineering style: no anyhow in the sync module).
#[derive(Debug)]
pub enum SyncError {
    Cs(CsError),
    /// the fragment host refused a blob's upload or download: its code
    /// (budget, size, role) is the one to act on
    Host(CodedError),
    Io(String),
    /// the folder's journal belongs to a different repo than the fragment
    /// being synced — refuse before touching anything
    Repo(String),
}
impl From<CsError> for SyncError {
    fn from(e: CsError) -> Self {
        SyncError::Cs(e)
    }
}
impl From<std::io::Error> for SyncError {
    fn from(e: std::io::Error) -> Self {
        SyncError::Io(e.to_string())
    }
}
impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Cs(e) => write!(f, "{e}"),
            SyncError::Host(e) => write!(f, "{e}"),
            SyncError::Io(e) => write!(f, "io: {e}"),
            SyncError::Repo(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for SyncError {}

impl SyncError {
    /// A blob's failure for `path`: the host's refusal keeps its code.
    fn blob(path: &str, e: anyhow::Error) -> SyncError {
        match e.downcast::<CodedError>() {
            Ok(refused) => SyncError::Host(CodedError { code: refused.code, msg: format!("{path}: {}", refused.msg) }),
            Err(e) => SyncError::Io(format!("{path}: {e:#}")),
        }
    }
}

/// stat cache v3: content sha + last-seen remote commit per path.
/// Anything else (or a v2 file from the old engine) reads as fresh —
/// corrupt or old states never wedge the folder.
#[derive(Serialize, Deserialize, Default)]
pub struct SyncState {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32, // 3
    pub name: String,
    /// url-form repo identity this folder last synced against, plus the
    /// fragment host it synced through. The mirror trusts the journal to
    /// mean "same world": without the binding, a journal from ANOTHER
    /// world (a wiped+recreated fragment, or the same name on a different
    /// host — dev vs prod bite equally, and repo ids alone can't tell
    /// them apart when the service reports name-form identities) makes
    /// every file look remotely-changed and the pull phase faithfully
    /// mirrors the wrong world over the folder — silent local data loss
    /// (found live, twice in one day). None on old journals; bound on the
    /// first sync.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    /// by path, in order: the journal serializes the same for the same
    /// state, so an unchanged one is recognized and not written again
    pub files: BTreeMap<String, FileState>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileState {
    pub sha256: String,
    pub size: u64,
    pub mtime_ns: i128,
    /// last commit SHA we saw touch this path (the listing's
    /// last_commit_sha) — our per-file remote version marker
    pub commit: String,
}

#[derive(Default, Serialize, Debug)]
pub struct Report {
    pub pushed: Vec<String>,
    pub pulled: Vec<String>,
    pub deleted_remote: Vec<String>,
    pub deleted_local: Vec<String>,
    pub withheld_deletions: Vec<String>,
    pub conflicts: Vec<String>,
    pub mass_delete_guard: Option<usize>,
    pub scan: ScanStats,
    pub mode: String,
    /// main's head as the pass left it (its own commit, when one landed)
    #[serde(skip)]
    pub head: Option<String>,
    /// a commit of this pass landed (or may have: its answer was lost)
    #[serde(skip)]
    pub landed: bool,
}

#[derive(Default, Serialize, Clone, Copy, Debug)]
pub struct ScanStats {
    pub files: usize,
    pub hashed: usize,
}

impl Report {
    /// exit code: 0 clean, 3 conflicts present, 4 mass-deletion guard
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
        p("deleted remotely", &self.deleted_remote);
        p("deleted locally", &self.deleted_local);
        if !self.withheld_deletions.is_empty() {
            println!("  deletions withheld (pull mode; --prune to apply): {})", self.withheld_deletions.len());
        }
        if !self.conflicts.is_empty() {
            println!("  CONFLICTS ({}): local keeps yours; the remote copy is saved beside it (.conflict-…)", self.conflicts.len());
            for f in &self.conflicts {
                println!("    {f}");
            }
        }
        if let Some(n) = self.mass_delete_guard {
            println!("  REFUSING to propagate {n} deletion(s) — folder looks unmounted/reset.");
            println!("  re-run with --apply-mass-delete if this is intended");
        }
        let clean = self.pushed.is_empty()
            && self.pulled.is_empty()
            && self.conflicts.is_empty()
            && self.deleted_remote.is_empty()
            && self.deleted_local.is_empty();
        if clean {
            println!("all {} files match the repo", self.scan.files);
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
/// the holder dies).
pub struct SyncLock(#[allow(dead_code)] File); // the field IS the lock (RAII)
impl SyncLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        let p = dir.join(".fragment").join("sync.lock");
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let f = File::create(&p)?;
        f.try_lock_exclusive()
            .map_err(|_| anyhow!("another fragment sync holds this folder ({}). If that's wrong, no process should own it; otherwise stop it or use a different folder.", p.display()))?;
        Ok(SyncLock(f))
    }
}

pub fn load_state(dir: &Path, name: &str) -> Result<SyncState> {
    Ok(read_state(dir, name)?.0)
}

/// The journal, and the bytes it was read from (`None` when there was
/// none, or it was unreadable and starts fresh).
fn read_state(dir: &Path, name: &str) -> Result<(SyncState, Option<Vec<u8>>)> {
    let fresh = || SyncState { schema_version: 3, name: name.to_string(), host: None, repo: None, files: BTreeMap::new() };
    let p = state_path(dir);
    if !p.exists() {
        return Ok((fresh(), None));
    }
    let bytes = fs::read(&p)?;
    match serde_json::from_slice::<SyncState>(&bytes) {
        Ok(s) if s.schema_version == 3 => {
            if s.name != name {
                anyhow::bail!("directory is synced to fragment '{}', not '{}'", s.name, name);
            }
            Ok((s, Some(bytes)))
        }
        _ => {
            eprintln!("warning: {} unreadable or old format — rebuilding state", p.display());
            Ok((fresh(), None))
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

/// Writes the journal, unless it is byte for byte what was read (`before`):
/// a pass that changed nothing leaves the file alone, with no fsync'd
/// rewrite.
fn save_state(dir: &Path, state: &SyncState, before: Option<&[u8]>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    if before == Some(bytes.as_slice()) {
        return Ok(());
    }
    if let Some(parent) = state_path(dir).parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&state_path(dir), &bytes)
}

/// The folder as a pass starts: its journal (with the bytes it was read
/// from) and a scan of the folder against it.
pub struct Local {
    state: SyncState,
    journal: Option<Vec<u8>>,
    files: BTreeMap<String, LocalFile>,
    stats: ScanStats,
}

impl Local {
    /// Whether the folder is what the journal says: the same paths, with
    /// the same content. A watcher woken by the OS needs no pass when it is
    /// (the event was the pass's own write, or touched nothing that syncs).
    pub fn matches_journal(&self) -> bool {
        self.files.len() == self.state.files.len()
            && self.files.iter().all(|(path, lf)| self.state.files.get(path).is_some_and(|st| st.sha256 == lf.sha256))
    }
}

/// Reads the folder for a pass: the mirror source overlaid first (its new
/// and changed files copy in), then the journal and a scan against it. No
/// network.
pub fn read_local(dir: &Path, name: &str, opts: &SyncOptions) -> Result<Local, SyncError> {
    if let Some(src) = &opts.mirror_from {
        mirror_overlay(src, dir).map_err(|e| SyncError::Io(format!("mirror-from {}: {e}", src.display())))?;
    }
    let (state, journal) = read_state(dir, name).map_err(|e| SyncError::Io(e.to_string()))?;
    let (files, stats) = scan_local(dir, Some(&state), false).map_err(|e| SyncError::Io(e.to_string()))?;
    Ok(Local { state, journal, files, stats })
}

pub(crate) struct LocalFile {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) mtime_ns: i128,
}

/// Whether a path in the folder (relative, `/`-separated) takes part in
/// sync, in either direction. One rule for the scan, the watcher, the
/// mirror source, and the repo's listing (a repo file that is out is never
/// pulled, and never deleted for being absent here), checked on every
/// segment, so a folder that is out takes everything under it:
/// - dot files and folders: sync's own `.fragment/`, `.git/`, an editor's
///   workspace state (`.obsidian/`), `.DS_Store`, `.#` lock files;
/// - the top-level `node_modules/`: the platform never loads or serves it
///   (an app's modules are `applib/`; `site::is_machinery`), and one
///   install writes thousands of files. One deeper down is content (the
///   site serves `site/node_modules/…`), so it syncs;
/// - editor droppings: `~` backups, `~$` lock files, `.swp` swap files;
/// - sync's own `.conflict-` copies and `.fragment-partial` temp files.
pub fn syncable(rel: &str) -> bool {
    // bounded by the path's segments
    for (depth, seg) in rel.split('/').enumerate() {
        let hidden = seg.starts_with('.');
        let modules = depth == 0 && seg == "node_modules";
        let dropping = seg.ends_with('~') || seg.starts_with("~$") || seg.ends_with(".swp");
        let ours = seg.contains(".conflict-") || seg.contains(".fragment-partial");
        if hidden || modules || dropping || ours {
            return false;
        }
    }
    true
}

/// `path` relative to `root`, `/`-separated ("" for the root itself).
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map(|rel| rel.to_string_lossy().replace('\\', "/")).unwrap_or_default()
}

/// Walk the folder. With state, a size+mtime match adopts the cached hash
/// (O(changes)); with verify, everything is read and hashed. Takes the
/// `syncable` paths, never following symlinks, and honors .gitignore
/// exactly when git would (only inside a repo).
pub fn scan_local(dir: &Path, state: Option<&SyncState>, verify: bool) -> Result<(BTreeMap<String, LocalFile>, ScanStats)> {
    let mut out = BTreeMap::new();
    let mut stats = ScanStats::default();
    let root = dir.to_path_buf();
    let walker = ignore::WalkBuilder::new(dir)
        // `syncable` owns the dotfile rule, as it does for the watcher
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        // a folder that is out is never entered
        .filter_entry(move |e| e.depth() == 0 || syncable(&relative(&root, e.path())))
        .build();
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = relative(dir, entry.path());
        let md = fs::metadata(entry.path())?;
        let size = md.len();
        let mtime_ns = md
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0);
        let cached = state.and_then(|s| s.files.get(&rel)).filter(|st| st.size == size && st.mtime_ns == mtime_ns);
        let sha = if let (Some(st), false) = (cached, verify) {
            st.sha256.clone()
        } else {
            stats.hashed += 1;
            sha256_hex(&fs::read(entry.path())?)
        };
        stats.files += 1;
        out.insert(rel, LocalFile { sha256: sha, size, mtime_ns });
    }
    Ok((out, stats))
}

/// copy new/changed files from src into dir (never writes src, never
/// deletes in dir); preserves mtimes so the scan shortcut stays valid.
/// The target's own identity is never overlaid: a source folder carrying
/// its own fragment.json must not stomp the corrected one.
fn mirror_overlay(src: &Path, dir: &Path) -> Result<()> {
    let walker = walkdir::WalkDir::new(src).follow_links(false).into_iter().filter_entry(|e| e.depth() == 0 || syncable(&relative(src, e.path())));
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = relative(src, entry.path());
        if rel == "fragment.json" {
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

/// What one push pass wants to do, computed purely from (local, remote,
/// state) so CAS retries can rebuild it against a fresh listing.
struct PushPlan {
    upserts: Vec<String>,
    deletes: Vec<String>, // paths to delete remotely (deleted locally)
    conflicts: Vec<String>,
}

fn push_plan(
    local: &BTreeMap<String, LocalFile>,
    remote: &HashMap<String, TreeEntry>,
    state: &SyncState,
) -> PushPlan {
    let mut plan = PushPlan { upserts: Vec::new(), deletes: Vec::new(), conflicts: Vec::new() };
    let mut paths: Vec<&String> = local.keys().chain(remote.keys()).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        let l = local.get(path);
        let r = remote.get(path);
        let s = state.files.get(path);
        match (l, r, s) {
            (Some(lf), Some(rf), Some(st)) => {
                let local_changed = st.sha256 != lf.sha256;
                let remote_changed = st.commit != rf.last_commit_sha;
                if local_changed && remote_changed {
                    plan.conflicts.push(path.clone());
                } else if local_changed {
                    plan.upserts.push(path.clone());
                }
                // local unchanged: remote-side changes are the pull
                // phase's business, never a push
            }
            (Some(_), Some(_), None) => {
                // stateless bootstrap: equal size is NOT identity — the
                // bootstrap pass in sync_once adopts verified-equal files
                // into the journal, so anything still journal-absent here
                // is content-different (or the pass could not run) and
                // local wins; the prior remote content survives in git
                // history
                plan.upserts.push(path.clone());
            }
            (Some(lf), None, Some(st)) => {
                if st.sha256 != lf.sha256 {
                    plan.upserts.push(path.clone()); // content beats deletion
                }
                // else: deleted remotely, local untouched -> pull phase
                // propagates the deletion (guarded)
            }
            (Some(_), None, None) => plan.upserts.push(path.clone()), // new file
            (None, Some(rf), Some(st)) => {
                // deleted locally, remotely untouched since we saw it -> delete remotely
                if st.commit == rf.last_commit_sha {
                    plan.deletes.push(path.clone());
                }
                // else remote moved on: leave it; pull re-materializes
            }
            (None, Some(_), None) => {} // remote-only: pull fetches
            (None, None, _) => {}       // stale state row; pull phase cleans
        }
    }
    plan
}

/// The mass-deletion guard's floor: a pass may always delete this many
/// files. It is 3, not 10: with a floor of 10, deleting 8 of a 10-file
/// fragment (a whole world change, found live) sailed through because
/// 8 ≤ 10 while being 80% of the folder. Small folders deserve the
/// percent protection most.
pub const MASS_DELETE_FLOOR: usize = 3;
/// Past the floor, the share of the known files (percent) a pass may delete.
pub const MASS_DELETE_PERCENT: usize = 30;

/// Mass-deletion guard (ported): a pass that would delete more than
/// max(MASS_DELETE_FLOOR, MASS_DELETE_PERCENT%) of the known files, or ALL
/// of them (the unmounted-disk / replaced-folder case the old
/// root-identity check covered), is refused until --apply-mass-delete.
/// Counts both directions — deletions pushed remotely and deletions
/// applied locally.
fn mass_delete_trips(push_deletes: usize, local_deletes: usize, known: usize, apply: bool) -> Option<usize> {
    if apply {
        return None;
    }
    let pending = push_deletes + local_deletes;
    let known = known.max(1);
    let over_share = pending > MASS_DELETE_FLOOR && pending * 100 > known * MASS_DELETE_PERCENT;
    if over_share || (pending == known && pending > 0) {
        Some(pending)
    } else {
        None
    }
}

/// `fragment sync`: connects, runs one pass, and nudges the cell's pins
/// when a commit landed.
pub fn sync_once(client: &Client, name: &str, dir: &Path, opts: &SyncOptions) -> Result<Report, SyncError> {
    let storage = CodeStorage::connect(client, name, opts.codestorage.as_deref())?;
    let report = pass(client, &storage, name, dir, opts)?;
    if report.landed {
        refresh_pins(client, name);
    }
    Ok(report)
}

/// One pass over the folder with a connected `storage`: a command's own
/// (deploy's is also its live move's), or a watcher's held one. It does
/// not nudge the cell's pins: a caller whose pass `landed` calls
/// `refresh_pins`, once for everything it moved.
pub fn pass(client: &Client, storage: &CodeStorage, name: &str, dir: &Path, opts: &SyncOptions) -> Result<Report, SyncError> {
    pass_over(client, storage, name, dir, opts, read_local(dir, name, opts)?)
}

/// `pass`, over a folder already read (`read_local`): a watcher reads it
/// first to learn whether it needs a pass at all.
pub fn pass_over(client: &Client, storage: &CodeStorage, name: &str, dir: &Path, opts: &SyncOptions, local: Local) -> Result<Report, SyncError> {
    let Local { mut state, journal, files: local, stats } = local;
    // World binding: this folder's journal must belong to THIS host+repo.
    // A mismatch means the fragment was recreated, or this folder last
    // synced the same name somewhere else (dev vs prod) — mirroring now
    // would faithfully copy the wrong world over the folder. Refuse
    // before any read or write.
    let bound_host = client.host.trim_end_matches('/').to_string();
    match (&state.host, &state.repo) {
        (Some(h), Some(r)) if h != &bound_host || r != storage.repo() => {
            return Err(SyncError::Repo(format!(
                "this folder is bound to {} repo {r}, but fragment '{name}' is {} repo {} — the fragment was likely recreated or this folder synced a different host.\n  to sync here anyway: rm {}/.fragment/state.json (rebinds; remote content survives in git history)",
                h, bound_host, storage.repo(), dir.display()
            )));
        }
        _ => {
            state.host = Some(bound_host);
            state.repo = Some(storage.repo().to_string());
        }
    }
    let author = Author::writer(&opts.writer_id);
    let blobs = crate::blobs::Blobs::new(client, name);
    let mut report = Report { scan: stats, mode: format!("{:?}", opts.mode).to_lowercase(), ..Default::default() };
    let mut recorded: HashSet<String> = HashSet::new();

    // main as this pass sees it: read once, reused by the push's first
    // attempt and by the pull, and after our own commit derived rather than
    // listed again
    let mut listing = list_main(storage)?;
    adopt_identical(storage, &local, &listing, &mut state)?;
    // candidate local deletions: known remotely before, gone from the
    // listing now, local copy untouched since we saw it
    let local_delete_candidates: Vec<String> = state
        .files
        .iter()
        .filter(|(p, st)| {
            !listing.files.contains_key(*p)
                && local.get(*p).is_some_and(|lf| lf.sha256 == st.sha256)
        })
        .map(|(p, _)| p.clone())
        .collect();
    let pre = push_plan(&local, &listing.files, &state);
    // pull mode never deletes remotely, so would-be push deletions must not
    // trip the guard there (a wiped folder in pull mode just re-downloads)
    let push_side = if opts.mode == Mode::Pull { 0 } else { pre.deletes.len() };
    if let Some(n) = mass_delete_trips(push_side, local_delete_candidates.len(), state.files.len(), opts.apply_mass_delete) {
        report.mass_delete_guard = Some(n);
        // dropping stale rows keeps the guard from re-tripping forever on
        // state that no longer matches either side
        for p in local_delete_candidates {
            state.files.remove(&p);
        }
        save_state(dir, &state, journal.as_deref()).map_err(|e| SyncError::Io(e.to_string()))?;
        report.head = listing.head;
        return Ok(report);
    }

    // ---- push: one commit with expected-parent CAS, bounded rebuilds ----
    if opts.mode != Mode::Pull {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            if attempt > 1 {
                listing = list_main(storage)?;
                // a commit whose answer was lost may have landed: what it
                // wrote now equals the folder, and adopting it (here and in
                // record_conflicts) is what keeps it from reading as a new
                // file to push again or as a conflict with our own bytes
                adopt_identical(storage, &local, &listing, &mut state)?;
            }
            let plan = push_plan(&local, &listing.files, &state);
            record_conflicts(
                ConflictCtx { storage, blobs: &blobs, dir, local: &local, listing: &listing, state: &mut state, report: &mut report, recorded: &mut recorded, writer_id: &opts.writer_id },
                &plan.conflicts,
            )?;
            if plan.upserts.is_empty() && plan.deletes.is_empty() {
                break; // nothing to commit (clean, or a replay absorbed)
            }
            let mut changes: Vec<Change> = Vec::with_capacity(plan.upserts.len() + plan.deletes.len());
            for p in &plan.upserts {
                let bytes = fs::read(dir.join(p)).map_err(|e| SyncError::Io(format!("read {p}: {e}")))?;
                let bytes = blobs.store(bytes).map_err(|e| SyncError::blob(p, e))?;
                changes.push(Change::Upsert { path: p.clone(), bytes });
            }
            for p in &plan.deletes {
                changes.push(Change::Delete { path: p.clone() });
            }
            let msg = format!(
                "fragment sync {}: +{} -{}",
                name,
                plan.upserts.len(),
                plan.deletes.len()
            );
            match storage.commit(listing.head.as_deref(), &msg, &author, &changes) {
                Ok(tip) => {
                    for p in &plan.upserts {
                        if let Some(lf) = local.get(p) {
                            state.files.insert(
                                p.clone(),
                                FileState { sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns, commit: tip.clone() },
                            );
                        }
                        report.pushed.push(p.clone());
                    }
                    for p in &plan.deletes {
                        state.files.remove(p);
                        report.deleted_remote.push(p.clone());
                    }
                    listing = listing.landed(tip, &changes);
                    report.landed = true;
                    break;
                }
                Err(CsError::CasRejected { .. }) if attempt < MAX_CAS_ATTEMPTS => {
                    eprintln!("  branch moved under us — refetching head and rebuilding the diff (attempt {attempt}/{MAX_CAS_ATTEMPTS})");
                    continue;
                }
                Err(CsError::OutcomeUnknown(detail)) if attempt < MAX_CAS_ATTEMPTS => {
                    // never resent blind: the rebuilt diff is empty if it
                    // landed, and the same commit on a fresh head if not
                    eprintln!("  the commit's answer was lost ({detail}) — re-reading the branch to see whether it landed (attempt {attempt}/{MAX_CAS_ATTEMPTS})");
                    report.landed = true; // perhaps: the refresh nudge is harmless either way
                    continue;
                }
                Err(CsError::CasRejected { detail }) => {
                    return Err(SyncError::Cs(CsError::CasRejected {
                        detail: format!("branch kept moving after {MAX_CAS_ATTEMPTS} attempts ({detail}); re-run fragment sync"),
                    }));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    // ---- pull: make the folder match main (fetch new/changed; propagate
    // remote deletions per mode) ----
    if opts.mode != Mode::Push {
        for rf in listing.files.values() {
            let l = local.get(&rf.path);
            let s = state.files.get(&rf.path);
            let fetch = match (l, s) {
                (None, _) => true, // remote-only
                (Some(_), None) => {
                    // journal-absent but present locally: the bootstrap
                    // pass adopted every content-verified match, so what's
                    // left here is content-different — fetch (remote wins
                    // in pull/mirror; push modes already pushed local)
                    true
                }
                (Some(lf), Some(st)) => {
                    let local_changed = st.sha256 != lf.sha256;
                    let remote_changed = st.commit != rf.last_commit_sha;
                    if local_changed && remote_changed {
                        // both changed and push didn't resolve it (push
                        // modes off, or a race) — same conflict treatment
                        record_conflicts(
                            ConflictCtx { storage, blobs: &blobs, dir, local: &local, listing: &listing, state: &mut state, report: &mut report, recorded: &mut recorded, writer_id: &opts.writer_id },
                            std::slice::from_ref(&rf.path),
                        )?;
                        false
                    } else {
                        remote_changed && !local_changed
                    }
                }
            };
            if fetch {
                pull_file(Pull { storage, blobs: &blobs, dir, rev: listing.rev() }, rf, &mut state, &mut report)?;
            }
        }
        // remote deletions: known before, gone now, local copy untouched
        let mut paths: Vec<String> = state.files.keys().cloned().collect();
        paths.sort();
        for p in paths {
            if listing.files.contains_key(&p) {
                continue;
            }
            let untouched = local.get(&p).is_some_and(|lf| Some(&lf.sha256) == state.files.get(&p).map(|s| &s.sha256));
            if !untouched {
                continue;
            }
            match opts.mode {
                Mode::Mirror => {
                    fs::remove_file(dir.join(&p)).map_err(|e| SyncError::Io(format!("delete {p}: {e}")))?;
                    state.files.remove(&p);
                    report.deleted_local.push(p.clone());
                }
                Mode::Pull => {
                    if opts.prune {
                        fs::remove_file(dir.join(&p)).map_err(|e| SyncError::Io(format!("delete {p}: {e}")))?;
                        state.files.remove(&p);
                        report.deleted_local.push(p.clone());
                    } else {
                        report.withheld_deletions.push(p.clone());
                        // keep the state row: dropping it here made the
                        // deletion unknowable, so a later --prune pass could
                        // never apply it (found by the e2e withhold-then-prune
                        // sequence). Re-reporting each pass is honest — the
                        // deletion is still pending.
                    }
                }
                Mode::Push => unreachable!("pull phase only runs when mode != Push"),
            }
        }
    }

    save_state(dir, &state, journal.as_deref()).map_err(|e| SyncError::Io(e.to_string()))?;
    report.head = listing.head;
    Ok(report)
}

/// Tells the cell its fragment's git moved. Our commits and ref moves are
/// EXTERNAL pushes from the cell's perspective: without this nudge its
/// pins wait out the 5-minute poll backstop before app reads and serving
/// see them. Best-effort: the poll covers a missed nudge.
pub fn refresh_pins(client: &Client, name: &str) {
    if let Err(e) = client.post_json(&format!("/api/f/{name}/refresh"), &serde_json::json!({})) {
        eprintln!("warning: cell pin refresh failed ({e:#}); the poll backstop will catch up");
    }
}

/// main as one pass reads it: its head, and the files at that head (read
/// at the head's commit, not at `main`, so the two always agree).
struct Listing {
    head: Option<String>,
    files: HashMap<String, TreeEntry>,
}

impl Listing {
    /// The ref a pass reads files at: the listing's own commit, so a read
    /// sees the bytes the listing describes even if main moved since.
    fn rev(&self) -> &str {
        self.head.as_deref().unwrap_or(MAIN)
    }

    /// main once our commit `tip` landed on this listing's head. The
    /// commit named this head as its expected parent, so our changes are
    /// the only difference, and listing main again would only read back
    /// what we just wrote.
    fn landed(mut self, tip: String, changes: &[Change]) -> Listing {
        for change in changes {
            match change {
                Change::Upsert { path, bytes } => {
                    let entry = TreeEntry { path: path.clone(), size: bytes.len() as u64, mode: FILE_MODE.to_string(), last_commit_sha: tip.clone() };
                    self.files.insert(path.clone(), entry);
                }
                Change::Delete { path } => {
                    self.files.remove(path);
                }
            }
        }
        assert!(self.files.values().all(|f| !f.last_commit_sha.is_empty()), "every entry names its commit");
        Listing { head: Some(tip), files: self.files }
    }
}

/// The git mode of every file sync commits (the commit pack's default).
const FILE_MODE: &str = "100644";

fn list_main(storage: &CodeStorage) -> Result<Listing, SyncError> {
    let Some(head) = storage.branch_head(MAIN)? else {
        return Ok(Listing { head: None, files: HashMap::new() }); // empty repo: everything local is new
    };
    let files = storage.list_files(&head)?.into_iter().filter(|f| syncable(&f.path)).map(|f| (f.path.clone(), f)).collect();
    Ok(Listing { head: Some(head), files })
}

/// Brings the journal up to what both sides already agree on, before any
/// plan is made from it:
/// - a journal-absent file present on both sides with the same content is
///   adopted (the stateless bootstrap, and a new file whose commit landed
///   though its answer was lost). Equal size used to be "provisionally
///   same" — but equal size is not identity, and a same-size
///   different-content file silently never pushed (found live). So each
///   equal-size unknown is fetched and hashed; mismatches stay
///   journal-absent and the push plan treats them as local-wins.
/// - a row for a path gone from both sides is dropped (a deletion that
///   landed, whoever made it); no plan reads it, and it would count toward
///   the mass-deletion guard's known files forever.
fn adopt_identical(
    storage: &CodeStorage,
    local: &BTreeMap<String, LocalFile>,
    listing: &Listing,
    state: &mut SyncState,
) -> Result<(), SyncError> {
    for (p, rf) in listing.files.iter() {
        if state.files.contains_key(p) {
            continue;
        }
        let Some(lf) = local.get(p) else { continue };
        if lf.size != rf.size && !crate::blobs::could_point(rf.size, lf.size) {
            continue;
        }
        let bytes = storage.read_file(p, listing.rev())?;
        if crate::blobs::content_sha(&bytes) == lf.sha256 {
            state.files.insert(
                p.clone(),
                FileState { sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns, commit: rf.last_commit_sha.clone() },
            );
        }
    }
    state.files.retain(|p, _| local.contains_key(p) || listing.files.contains_key(p));
    Ok(())
}

/// both sides changed: keep local, save the remote bytes beside it as
/// `<path>.conflict-<ts>-<writer>`, then adopt (remote commit, LOCAL file
/// sha) so the conflict reports once and the next pass pushes local
/// content — the old copy-strategy semantics (theirs survives in the copy
/// file and in git history). Both sides changed to the SAME bytes is no
/// conflict (our own commit whose answer was lost, or two writers agreeing):
/// that adopts silently.
fn record_conflicts(
    ctx: ConflictCtx<'_>,
    conflicts: &[String],
) -> Result<(), SyncError> {
    for path in conflicts {
        // by path, not by report line: `a.md`'s line starts with "a", so
        // matching the text skipped a real conflict on `a` whenever `a.md`
        // was recorded first
        if !ctx.recorded.insert(path.clone()) {
            continue; // already recorded this pass
        }
        let rf_commit = ctx.listing.files.get(path).map(|r| r.last_commit_sha.clone()).unwrap_or_default();
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (stem, ext) = match path.rfind('.') {
            Some(i) if !path[i..].contains('/') => (&path[..i], &path[i..]),
            _ => (path.as_str(), ""),
        };
        let conflict_path = format!("{stem}.conflict-{ts}-{}{ext}", ctx.writer_id);
        let git_bytes = ctx.storage.read_file(path, ctx.listing.rev());
        let same = match (&git_bytes, ctx.local.get(path)) {
            (Ok(bytes), Some(lf)) => crate::blobs::content_sha(bytes) == lf.sha256,
            _ => false,
        };
        if !same {
            match git_bytes.map_err(anyhow::Error::from).and_then(|b| ctx.blobs.resolve(b)) {
                Ok(bytes) => {
                    atomic_write(&ctx.dir.join(&conflict_path), &bytes).map_err(|e| SyncError::Io(e.to_string()))?;
                    ctx.report.conflicts.push(format!("{path} (remote copy: {conflict_path})"));
                }
                Err(e) => {
                    ctx.report.conflicts.push(format!("{path} (remote copy unavailable: {e})"));
                }
            }
        }
        let entry = match ctx.local.get(path) {
            Some(lf) => FileState { sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns, commit: rf_commit },
            None => match ctx.state.files.get(path) {
                Some(st) => FileState { commit: rf_commit, ..st.clone() },
                None => continue,
            },
        };
        ctx.state.files.insert(path.clone(), entry);
    }
    Ok(())
}

/// Everything record_conflicts needs; keeps it at one argument.
struct ConflictCtx<'a> {
    storage: &'a CodeStorage,
    blobs: &'a crate::blobs::Blobs<'a>,
    dir: &'a Path,
    local: &'a BTreeMap<String, LocalFile>,
    listing: &'a Listing,
    state: &'a mut SyncState,
    report: &'a mut Report,
    /// the paths recorded as conflicts this pass (the push plan and the
    /// pull can both name one)
    recorded: &'a mut HashSet<String>,
    writer_id: &'a str,
}

/// Where a pull reads from and writes to.
struct Pull<'a> {
    storage: &'a CodeStorage,
    blobs: &'a crate::blobs::Blobs<'a>,
    dir: &'a Path,
    /// the listing's commit (`Listing::rev`)
    rev: &'a str,
}

fn pull_file(at: Pull<'_>, entry: &TreeEntry, state: &mut SyncState, report: &mut Report) -> Result<(), SyncError> {
    let Pull { storage, blobs, dir, rev } = at;
    let (path, commit) = (entry.path.as_str(), entry.last_commit_sha.as_str());
    let bytes = blobs.resolve(storage.read_file(path, rev)?).map_err(|e| SyncError::blob(path, e))?;
    let sha = sha256_hex(&bytes);
    atomic_write(&dir.join(path), &bytes).map_err(|e| SyncError::Io(e.to_string()))?;
    let md = fs::metadata(dir.join(path)).map_err(|e| SyncError::Io(e.to_string()))?;
    let mtime_ns = md.modified().map(|m| m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0)).unwrap_or(0);
    state.files.insert(
        path.to_string(),
        FileState { sha256: sha, size: md.len(), mtime_ns, commit: commit.to_string() },
    );
    report.pulled.push(path.to_string());
    Ok(())
}

/// Full-content audit: local truth vs the repo listing + fetched bytes
/// (no shortcuts — every remote file is fetched and hashed).
pub fn verify(client: &Client, name: &str, dir: &Path, codestorage: Option<&str>) -> Result<Report, SyncError> {
    let state = load_state(dir, name).map_err(|e| SyncError::Io(e.to_string()))?;
    let (local, stats) = scan_local(dir, Some(&state), true).map_err(|e| SyncError::Io(e.to_string()))?;
    let storage = CodeStorage::connect(client, name, codestorage)?;
    let mut drift = Report { scan: stats, mode: "verify".into(), ..Default::default() };
    let listing = list_main(&storage)?;
    for p in listing.files.keys() {
        match local.get(p) {
            None => drift.conflicts.push(format!("{p}: in the repo, missing locally")),
            Some(lf) => {
                let bytes = storage.read_file(p, listing.rev())?;
                if crate::blobs::content_sha(&bytes) != lf.sha256 {
                    drift.conflicts.push(format!("{p}: content differs from the repo"));
                }
            }
        }
    }
    for p in local.keys() {
        if !listing.files.contains_key(p) {
            drift.conflicts.push(format!("{p}: not in the repo"));
        }
    }
    Ok(drift)
}

/// Commit exactly one file to main with CAS retries (manifest-set).
pub fn commit_single_file(
    client: &Client,
    name: &str,
    path: &str,
    bytes: Vec<u8>,
    message: &str,
    writer_id: &str,
    codestorage: Option<&str>,
) -> Result<String, SyncError> {
    let storage = CodeStorage::connect(client, name, codestorage)?;
    let author = Author::writer(writer_id);
    for attempt in 1..=MAX_CAS_ATTEMPTS {
        let head = storage.branch_head(MAIN)?;
        if let (true, Some(tip)) = (attempt > 1, &head) {
            if storage.read_file(path, MAIN).is_ok_and(|b| b == bytes) {
                // the last attempt's answer was lost, or someone wrote the
                // same bytes: either way main holds them
                return Ok(tip.clone());
            }
        }
        match storage.commit(head.as_deref(), message, &author, &[Change::Upsert { path: path.to_string(), bytes: bytes.clone() }]) {
            Ok(tip) => return Ok(tip),
            Err(CsError::CasRejected { .. } | CsError::OutcomeUnknown(_)) if attempt < MAX_CAS_ATTEMPTS => continue,
            Err(CsError::CasRejected { detail }) => {
                return Err(SyncError::Cs(CsError::CasRejected {
                    detail: format!("branch kept moving after {MAX_CAS_ATTEMPTS} attempts ({detail})"),
                }));
            }
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("bounded loop returns from every arm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;
    use crate::mockcs::MockServer;

    fn client_for(mock: &MockServer) -> Client {
        Client::new(&mock.url, auth::fixed(7))
    }

    fn opts(mode: Mode) -> SyncOptions {
        SyncOptions { mode, writer_id: "deadbeef".into(), ..Default::default() }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fragment-sync3-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scan_uses_cache() {
        let dir = tmpdir("scan");
        fs::create_dir_all(dir.join("site")).unwrap();
        fs::write(dir.join("site/index.html"), b"hi").unwrap();
        let (l1, s1) = scan_local(&dir, None, false).unwrap();
        assert_eq!(s1.hashed, 1);
        let mut st = SyncState { schema_version: 3, name: "x".into(), host: None, repo: None, files: BTreeMap::new() };
        let lf = l1.get("site/index.html").unwrap();
        st.files.insert("site/index.html".into(), FileState { sha256: lf.sha256.clone(), size: lf.size, mtime_ns: lf.mtime_ns, commit: "c".into() });
        let (_, s2) = scan_local(&dir, Some(&st), false).unwrap();
        assert_eq!(s2.hashed, 0);
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: one rule says what syncs. Method: the paths each old list
    /// disagreed on, and ordinary ones.
    #[test]
    fn what_syncs() {
        // a node_modules below the top is the site's content: it serves it
        for rel in ["a.md", "site/index.html", "notes/2026/x.md", "a.b/c.d.md", "applib/x.mjs", "fragment.json", "site/node_modules/y.js"] {
            assert!(syncable(rel), "{rel} syncs");
        }
        for rel in [
            ".obsidian/workspace.json", // the watcher's old rule tested one segment for a two-segment path
            ".DS_Store",
            "site/.DS_Store",
            ".git/HEAD",
            ".fragment/state.json",
            ".trash/old.md",
            "node_modules/x/index.js", // the scan uploaded it while the watcher ignored it
            "a.md~",
            "~$report.docx",
            "notes.swp",
            ".#notes.md",
            "a.conflict-1700000000-deadbeef.md",
            "doc.fragment-partial-4242",
        ] {
            assert!(!syncable(rel), "{rel} does not sync");
        }
    }

    /// Goal: the scan and the mirror source take what `syncable` takes.
    /// Method: a folder with one file of each kind.
    #[test]
    fn the_scan_and_the_mirror_source_share_the_rule() {
        let src = tmpdir("rule-src");
        let dir = tmpdir("rule-dir");
        for (rel, bytes) in [("a.md", "a"), (".obsidian/workspace.json", "{}"), ("node_modules/x/index.js", "x"), ("b.md~", "b"), ("sub/.DS_Store", "d"), ("sub/c.md", "c")] {
            fs::create_dir_all(src.join(rel).parent().unwrap()).unwrap();
            fs::write(src.join(rel), bytes).unwrap();
        }
        let (scanned, _) = scan_local(&src, None, true).unwrap();
        assert_eq!(scanned.keys().collect::<Vec<_>>(), ["a.md", "sub/c.md"]);
        mirror_overlay(&src, &dir).unwrap();
        let (overlaid, _) = scan_local(&dir, None, true).unwrap();
        assert_eq!(overlaid.keys().collect::<Vec<_>>(), ["a.md", "sub/c.md"]);
        assert!(!dir.join("node_modules").exists() && !dir.join(".obsidian").exists(), "the overlay copies only what syncs");
        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: a repo file that does not sync is left alone both ways: never
    /// pulled, and never deleted for being absent from the folder (the
    /// scan skips it). Method: a repo with a dotfile and a node_modules
    /// file; a mirror pass, then another.
    #[test]
    fn repo_files_that_do_not_sync_are_left_alone() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.md", b"a"), (".env.example", b"X=1"), ("node_modules/x.js", b"x")]);
        let c = client_for(&mock);
        let dir = tmpdir("left-alone");
        let first = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!(first.pulled, ["a.md"]);
        let second = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(second.deleted_remote.is_empty() && second.pushed.is_empty(), "{second:?}");
        assert_eq!(mock.paths("t", "main"), [".env.example", "a.md", "node_modules/x.js"]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_leaves_no_partial() {
        let dir = tmpdir("atomic");
        atomic_write(&dir.join("a.txt"), b"content").unwrap();
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"content");
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains("partial")).collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_state_reads_fresh() {
        let dir = tmpdir("oldstate");
        fs::create_dir_all(dir.join(".fragment")).unwrap();
        fs::write(dir.join(".fragment/state.json"), b"{\"name\":\"x\",\"files\":{\"a\":{\"rev\":1}}}").unwrap();
        let st = load_state(&dir, "x").unwrap();
        assert!(st.files.is_empty()); // v2 → fresh
        assert_eq!(st.schema_version, 3);
        fs::remove_dir_all(&dir).ok();
    }

    /// The guide states the guard with the code's own numbers.
    #[test]
    fn the_guide_names_the_guards_numbers() {
        let guide = include_str!("../GUIDE.md").split_whitespace().collect::<Vec<_>>().join(" ");
        let rule = format!("more than max({MASS_DELETE_FLOOR}, {MASS_DELETE_PERCENT}%) of known files, or all of them");
        assert!(guide.contains(&rule), "GUIDE.md should say: {rule}");
    }

    /// Goal: a pass tells a script what happened by its exit code alone,
    /// as GUIDE.md says, and a tripped guard is the code a script sees even
    /// beside conflicts (it needs someone to look before anything more
    /// moves). Method: a report of each outcome, the codes written out.
    /// The e2e's sync section checks one of them through the process.
    #[test]
    fn a_report_exits_with_its_outcome() {
        let paths = |ps: &[&str]| ps.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        let busy = Report {
            pushed: paths(&["a.md"]),
            pulled: paths(&["b.md"]),
            deleted_remote: paths(&["c.md"]),
            deleted_local: paths(&["d.md"]),
            withheld_deletions: paths(&["e.md"]),
            ..Report::default()
        };
        let conflicted = Report { conflicts: paths(&["f.md (remote copy: f.conflict-1-deadbeef.md)"]), ..Report::default() };
        let guarded = Report { mass_delete_guard: Some(15), ..Report::default() };
        let both = Report { conflicts: paths(&["f.md"]), mass_delete_guard: Some(15), ..Report::default() };
        assert_eq!(Report::default().exit_code(), 0);
        assert_eq!(busy.exit_code(), 0, "work done, withheld deletions too, is a clean pass");
        assert_eq!(conflicted.exit_code(), 3);
        assert_eq!(guarded.exit_code(), 4);
        assert_eq!(both.exit_code(), 4);
        let guide = include_str!("../GUIDE.md");
        assert!(guide.contains("**Exit codes**: 0 clean, 1 failure, 3 conflicts, 4 guard tripped."), "GUIDE.md names the codes");
    }

    #[test]
    fn mass_delete_guard_semantics() {
        // rule: pending > max(3, 30% of known), or ALL of them
        assert!(mass_delete_trips(11, 0, 30, false).is_some());
        assert!(mass_delete_trips(11, 0, 100, false).is_none()); // 11% only
        assert!(mass_delete_trips(9, 0, 30, false).is_none()); // 30% exactly
        // the live incident: 8 of a 10-file folder (80%) must trip — the
        // old floor of 10 let a whole world change through (8 ≤ 10)
        assert!(mass_delete_trips(0, 8, 10, false).is_some());
        assert!(mass_delete_trips(8, 0, 10, false).is_some());
        // small legitimate deletions stay quiet: 3 or fewer, or a small
        // share of a big folder
        assert!(mass_delete_trips(3, 0, 10, false).is_none());
        assert!(mass_delete_trips(3, 0, 1000, false).is_none());
        // full wipe trips regardless of count (unmounted-folder case)
        assert!(mass_delete_trips(3, 0, 3, false).is_some());
        assert!(mass_delete_trips(1, 0, 1, false).is_some());
        // override
        assert!(mass_delete_trips(11, 0, 30, true).is_none());
        // pull-side deletions count toward the same guard
        assert!(mass_delete_trips(0, 11, 30, false).is_some());
        // empty state floors `known` at 1: mass deletions without state
        // cannot happen for real, but the wipe rule still catches them
        assert!(mass_delete_trips(100, 0, 1, false).is_some());
    }

    // ---- end-to-end against the mock code.storage ----

    #[test]
    fn push_success_commits_folder() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[]);
        let c = client_for(&mock);
        let dir = tmpdir("push-ok");
        fs::write(dir.join("a.txt"), b"alpha").unwrap();
        fs::create_dir_all(dir.join("site")).unwrap();
        fs::write(dir.join("site/index.html"), b"<h1>hi</h1>").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert_eq!(report.pushed.len(), 2);
        assert_eq!(mock.file_at("t", "main", "a.txt").unwrap(), b"alpha");
        assert_eq!(mock.file_at("t", "main", "site/index.html").unwrap(), b"<h1>hi</h1>");
        fs::remove_dir_all(&dir).ok();
    }

    /// The fake's request counts, as a test states them.
    fn counts(routes: &[(&str, u32)]) -> BTreeMap<String, u32> {
        routes.iter().map(|(route, n)| (route.to_string(), *n)).collect()
    }

    /// Goal: a pass reads main's head and listing once. Method: count the
    /// fake's requests for passes over a synced folder. Before, a pass with
    /// nothing to push read the head three times and listed twice, and one
    /// that pushed listed again after its own commit.
    #[test]
    fn a_pass_reads_the_head_and_the_listing_once() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"a"), ("b.txt", b"b")]);
        let c = client_for(&mock);
        let dir = tmpdir("reads-once");
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        mock.take_requests("");
        let read_once = counts(&[("GET branch", 1), ("GET files/metadata", 1), ("GET storage-token", 1)]);

        let idle = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(idle.pushed.is_empty() && idle.pulled.is_empty() && !idle.landed, "{idle:?}");
        assert_eq!(mock.take_requests(""), read_once, "a mirror pass with nothing to do");
        sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        assert_eq!(mock.take_requests(""), read_once, "a pull pass with nothing to do");

        fs::write(dir.join("a.txt"), b"changed").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        let pushed = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!((pushed.pushed.as_slice(), pushed.deleted_remote.as_slice()), (&["a.txt".to_string()][..], &["b.txt".to_string()][..]));
        assert!(pushed.pulled.is_empty() && pushed.landed, "{pushed:?}");
        assert_eq!(pushed.head, mock.branch("t", "main"), "the pass ends on its own commit");
        assert_eq!(
            mock.take_requests(""),
            counts(&[("GET branch", 1), ("GET files/metadata", 1), ("POST commit-pack", 1), ("GET storage-token", 1), ("POST refresh", 1)]),
            "a pass that pushes lists once, and its pull reads the listing it derived"
        );

        // the derived listing named our commit, so nothing reads as changed
        let again = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(again.pushed.is_empty() && again.pulled.is_empty() && again.conflicts.is_empty(), "{again:?}");
        assert_eq!(mock.take_requests(""), read_once);
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: the journal is written when a pass changed it, and only
    /// then. Method: its inode across an idle pass and a pushing one (the
    /// write is a rename, so a new inode). Before, every pass rewrote and
    /// fsynced it.
    #[cfg(unix)]
    #[test]
    fn the_journal_is_written_only_when_it_changed() {
        use std::os::unix::fs::MetadataExt;
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"a")]);
        let c = client_for(&mock);
        let dir = tmpdir("journal-writes");
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        let journal = || fs::metadata(dir.join(".fragment/state.json")).unwrap().ino();
        let first = journal();
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!(journal(), first, "an idle pass leaves it alone");
        fs::write(dir.join("b.txt"), b"b").unwrap();
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_ne!(journal(), first, "a pass that pushed wrote it");
        assert!(load_state(&dir, "t").unwrap().files.contains_key("b.txt"));
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: the listing a pass derives after its own commit is the one
    /// main lists. Method: commit changes (an edit, a new file, a
    /// deletion) on a listing's head, derive, list main, and compare every
    /// entry.
    #[test]
    fn a_derived_listing_is_the_listing_of_main() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"a"), ("b.txt", b"b"), ("c/d.txt", b"d")]);
        let c = client_for(&mock);
        let storage = CodeStorage::connect(&c, "t", None).unwrap();
        let before = list_main(&storage).unwrap();
        let changes = [
            Change::Upsert { path: "a.txt".into(), bytes: b"a, longer now".to_vec() },
            Change::Upsert { path: "c/new.txt".into(), bytes: b"new".to_vec() },
            Change::Delete { path: "b.txt".into() },
        ];
        let tip = storage.commit(before.head.as_deref(), "edit", &Author::writer("deadbeef"), &changes).unwrap();
        let derived = before.landed(tip, &changes);
        let listed = list_main(&storage).unwrap();
        let entries = |l: &Listing| -> BTreeMap<String, (u64, String, String)> {
            l.files.values().map(|f| (f.path.clone(), (f.size, f.mode.clone(), f.last_commit_sha.clone()))).collect()
        };
        assert_eq!(derived.head, listed.head);
        assert_eq!(entries(&derived), entries(&listed));
        assert_eq!(entries(&listed).len(), 3, "a.txt, c/d.txt, c/new.txt");
    }

    #[test]
    fn push_replay_commits_exactly_once() {
        // the idempotency replay: the same folder re-synced must send NO
        // further commit-packs (content equality short-circuits)
        let mock = MockServer::start();
        mock.seed_repo("t", &[]);
        let c = client_for(&mock);
        let dir = tmpdir("push-replay");
        fs::write(dir.join("a.txt"), b"alpha").unwrap();
        sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert_eq!(mock.commit_pack_count(), 1);
        let r2 = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert!(r2.pushed.is_empty());
        assert_eq!(mock.commit_pack_count(), 1, "replayed sync must not commit again");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn push_conflicting_parent_retries_then_succeeds() {
        // competitor moves the tip once: our pack 409s, we refetch,
        // rebuild, and land on the new tip — both changes survive
        let mock = MockServer::start();
        mock.seed_repo("t", &[]);
        mock.sabotage_commit_packs(1);
        let c = client_for(&mock);
        let dir = tmpdir("push-conflict-once");
        fs::write(dir.join("a.txt"), b"alpha").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert_eq!(report.pushed.len(), 1);
        assert!(mock.file_at("t", "main", "competitor.txt").is_some(), "competitor commit survives");
        assert_eq!(mock.file_at("t", "main", "a.txt").unwrap(), b"alpha");
        assert!(mock.commit_pack_count() >= 2, "one rejected attempt plus one landing");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn push_conflicting_parent_forever_is_bounded() {
        // every attempt is sabotaged: explicit error after MAX_CAS_ATTEMPTS
        let mock = MockServer::start();
        mock.seed_repo("t", &[]);
        mock.sabotage_commit_packs(99);
        let c = client_for(&mock);
        let dir = tmpdir("push-conflict-forever");
        fs::write(dir.join("a.txt"), b"alpha").unwrap();
        let err = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap_err();
        assert!(err.to_string().contains("branch kept moving"), "got: {err}");
        assert_eq!(mock.commit_pack_count(), MAX_CAS_ATTEMPTS, "exactly the bounded number of attempts");
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: a sync whose commit landed but lost its answer ends clean.
    /// Method: the fake applies the pass's commit and drops the answer. The
    /// old client sent the pack again (a 409), then recorded a conflict
    /// with a copy of its own bytes and exited 3. Now: one pack, no
    /// conflict, exit 0, and the next pass has nothing to do.
    #[test]
    fn a_lost_commit_answer_adopts_what_landed() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("doc.md", b"base"), ("gone.md", b"g"), ("kept.md", b"k")]);
        let c = client_for(&mock);
        let dir = tmpdir("lost-answer");
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        fs::write(dir.join("doc.md"), b"ours, changed").unwrap();
        fs::write(dir.join("new.md"), b"new").unwrap();
        fs::remove_file(dir.join("gone.md")).unwrap();
        mock.drop_commit_answers("t", 1);
        let report = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
        assert_eq!(report.exit_code(), 0);
        assert_eq!(mock.commit_pack_count(), 1, "the pack was sent once");
        let copies = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".conflict-")).count();
        assert_eq!(copies, 0, "no conflict copy");
        assert_eq!(mock.file_at("t", "main", "doc.md").unwrap(), b"ours, changed");
        assert_eq!(mock.file_at("t", "main", "new.md").unwrap(), b"new");
        assert!(mock.file_at("t", "main", "gone.md").is_none());
        assert_eq!(fs::read(dir.join("doc.md")).unwrap(), b"ours, changed");
        let st = load_state(&dir, "t").unwrap();
        assert!(!st.files.contains_key("gone.md"), "the landed deletion leaves no row");
        let again = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(again.pushed.is_empty() && again.pulled.is_empty() && again.conflicts.is_empty(), "{again:?}");
        assert_eq!(mock.commit_pack_count(), 1, "and nothing is left to send");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_set_with_a_lost_answer_lands_once() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("fragment.json", br#"{"name":"t"}"#)]);
        let c = client_for(&mock);
        mock.drop_commit_answers("t", 1);
        let tip = commit_single_file(&c, "t", "fragment.json", br#"{"name":"t","visibility":"public"}"#.to_vec(), "manifest-set", "deadbeef", None).unwrap();
        assert_eq!(mock.commit_pack_count(), 1, "sent once");
        assert_eq!(mock.branch("t", "main").as_deref(), Some(tip.as_str()));
        assert_eq!(mock.file_at("t", "main", "fragment.json").unwrap(), br#"{"name":"t","visibility":"public"}"#);
    }

    #[test]
    fn pull_materializes_repo() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"remote-a"), ("site/index.html", b"<p>x</p>")]);
        let c = client_for(&mock);
        let dir = tmpdir("pull");
        let report = sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        assert_eq!(report.pulled.len(), 2);
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"remote-a");
        assert_eq!(fs::read(dir.join("site/index.html")).unwrap(), b"<p>x</p>");
        // second pull: nothing to do
        let r2 = sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        assert!(r2.pulled.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mirror_pushes_and_pulls() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("remote-only.txt", b"r")]);
        let c = client_for(&mock);
        let dir = tmpdir("mirror");
        fs::write(dir.join("local-only.txt"), b"l").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!(report.pushed, vec!["local-only.txt"]);
        assert_eq!(report.pulled, vec!["remote-only.txt"]);
        assert!(dir.join("remote-only.txt").exists());
        assert!(mock.file_at("t", "main", "local-only.txt").is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deletion_pushes_and_propagates_back() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("gone.txt", b"g"), ("kept.txt", b"k")]);
        let c = client_for(&mock);
        let dir = tmpdir("delete");
        // first pass: adopt both files locally
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        // delete one locally, push the deletion
        fs::remove_file(dir.join("gone.txt")).unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert_eq!(report.deleted_remote, vec!["gone.txt"]);
        assert!(mock.file_at("t", "main", "gone.txt").is_none());
        assert!(mock.file_at("t", "main", "kept.txt").is_some());
        // a second folder (fresh state) pulling sees the deletion
        let dir2 = tmpdir("delete-other");
        sync_once(&c, "t", &dir2, &opts(Mode::Pull)).unwrap();
        assert!(dir2.join("kept.txt").exists());
        assert!(!dir2.join("gone.txt").exists(), "pull of an empty path must not create it");
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn remote_deletion_propagates_in_mirror_withheld_in_pull() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("gone.txt", b"g"), ("kept.txt", b"k")]);
        let c = client_for(&mock);
        // three folders adopt both files
        let dir = tmpdir("rdelete");
        let dir2 = tmpdir("rdelete-other");
        let dir3 = tmpdir("rdelete-third");
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        sync_once(&c, "t", &dir3, &opts(Mode::Pull)).unwrap();
        // dir2 pulls, deletes gone.txt, pushes the deletion
        sync_once(&c, "t", &dir2, &opts(Mode::Pull)).unwrap();
        fs::remove_file(dir2.join("gone.txt")).unwrap();
        sync_once(&c, "t", &dir2, &opts(Mode::Push)).unwrap();
        // mirror folder: local untouched → deletion propagates locally
        let report = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!(report.deleted_local, vec!["gone.txt"]);
        assert!(!dir.join("gone.txt").exists());
        // pull-only folder with untouched local copy: withheld without --prune
        let report = sync_once(&c, "t", &dir3, &opts(Mode::Pull)).unwrap();
        assert_eq!(report.withheld_deletions, vec!["gone.txt"]);
        assert!(dir3.join("gone.txt").exists(), "pull without --prune never deletes");
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&dir2).ok();
        fs::remove_dir_all(&dir3).ok();
    }

    #[test]
    fn remote_deletion_applies_in_pull_with_prune() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("gone.txt", b"g"), ("kept.txt", b"k")]);
        let c = client_for(&mock);
        let dir = tmpdir("prune");
        let other = tmpdir("prune-other");
        sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        sync_once(&c, "t", &other, &opts(Mode::Pull)).unwrap();
        fs::remove_file(other.join("gone.txt")).unwrap();
        sync_once(&c, "t", &other, &opts(Mode::Push)).unwrap();
        let o = SyncOptions { prune: true, ..opts(Mode::Pull) };
        let report = sync_once(&c, "t", &dir, &o).unwrap();
        assert_eq!(report.deleted_local, vec!["gone.txt"]);
        assert!(!dir.join("gone.txt").exists());
        assert!(dir.join("kept.txt").exists());
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&other).ok();
    }

    #[test]
    fn withheld_deletion_still_applies_on_a_later_prune_pass() {
        // pull without --prune withholds; the FOLLOWING pass with --prune
        // must apply it — dropping the state row on withhold made the
        // deletion unknowable (e2e: filesync modes sequence)
        let mock = MockServer::start();
        mock.seed_repo("t", &[("gone.txt", b"g"), ("kept.txt", b"k")]);
        let c = client_for(&mock);
        let dir = tmpdir("prune-late");
        let other = tmpdir("prune-late-other");
        sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        sync_once(&c, "t", &other, &opts(Mode::Pull)).unwrap();
        fs::remove_file(other.join("gone.txt")).unwrap();
        sync_once(&c, "t", &other, &opts(Mode::Push)).unwrap();
        // withhold first
        let withheld = sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        assert_eq!(withheld.withheld_deletions, vec!["gone.txt"]);
        assert!(dir.join("gone.txt").exists());
        // then apply
        let report = sync_once(&c, "t", &dir, &SyncOptions { prune: true, ..opts(Mode::Pull) }).unwrap();
        assert_eq!(report.deleted_local, vec!["gone.txt"]);
        assert!(!dir.join("gone.txt").exists());
        assert!(dir.join("kept.txt").exists());
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&other).ok();
    }

    #[test]
    fn mass_deletion_guard_blocks_push() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"1"), ("b", b"2"), ("c", b"3")]);
        let c = client_for(&mock);
        let dir = tmpdir("guard");
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        // wipe the folder locally: full-wipe rule trips (3 known, 3 pending)
        fs::remove_file(dir.join("a")).unwrap();
        fs::remove_file(dir.join("b")).unwrap();
        fs::remove_file(dir.join("c")).unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert!(report.mass_delete_guard.is_some());
        assert_eq!(report.exit_code(), 4);
        assert!(mock.file_at("t", "main", "a").is_some(), "nothing deleted while guarded");
        // override applies it
        let report = sync_once(&c, "t", &dir, &SyncOptions { apply_mass_delete: true, ..opts(Mode::Push) }).unwrap();
        assert_eq!(report.deleted_remote.len(), 3);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wiped_folder_in_pull_mode_re_downloads_not_guard() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"1"), ("b", b"2"), ("c", b"3")]);
        let c = client_for(&mock);
        let dir = tmpdir("wipe-pull");
        sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        fs::remove_file(dir.join("a")).unwrap();
        fs::remove_file(dir.join("b")).unwrap();
        fs::remove_file(dir.join("c")).unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        assert!(report.mass_delete_guard.is_none(), "pull must not trip on local deletions");
        assert_eq!(report.pulled.len(), 3, "everything re-downloads");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn conflict_both_changed_saves_remote_copy() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("shared.txt", b"base")]);
        let c = client_for(&mock);
        let dir = tmpdir("conflict");
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        // remote side changes it
        let other = tmpdir("conflict-other");
        sync_once(&c, "t", &other, &opts(Mode::Pull)).unwrap();
        fs::write(other.join("shared.txt"), b"theirs").unwrap();
        sync_once(&c, "t", &other, &opts(Mode::Push)).unwrap();
        // local side changes it too
        fs::write(dir.join("shared.txt"), b"ours").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.exit_code(), 3);
        assert_eq!(fs::read(dir.join("shared.txt")).unwrap(), b"ours", "local keeps ours");
        // the copy name splits stem/ext like the old engine: shared.conflict-<ts>-<w>.txt
        let copy = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.starts_with("shared.conflict-"));
        assert!(copy.is_some(), "remote copy saved beside it");
        let copy_bytes = fs::read(dir.join(copy.unwrap())).unwrap();
        assert_eq!(copy_bytes, b"theirs");
        assert_eq!(mock.file_at("t", "main", "shared.txt").unwrap(), b"theirs", "remote untouched by our conflict");
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&other).ok();
    }

    /// Goal: a conflict on `a` is recorded even when one on `a.md` was
    /// recorded first. Method: record them in that order within one pass
    /// (the pull walks a HashMap, so in a real pass the order is chance);
    /// matching report text found "a.md (remote copy: …)" starting with "a"
    /// and skipped `a` silently, on every pass.
    #[test]
    fn conflicts_are_tracked_by_path_not_report_text() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"theirs a"), ("a.md", b"theirs a.md")]);
        let c = client_for(&mock);
        let dir = tmpdir("conflict-paths");
        fs::write(dir.join("a"), b"ours a").unwrap();
        fs::write(dir.join("a.md"), b"ours a.md").unwrap();
        let storage = CodeStorage::connect(&c, "t", None).unwrap();
        let blobs = crate::blobs::Blobs::new(&c, "t");
        let listing = list_main(&storage).unwrap();
        let (local, _) = scan_local(&dir, None, true).unwrap();
        let mut state = SyncState { schema_version: 3, name: "t".into(), host: None, repo: None, files: BTreeMap::new() };
        let mut report = Report::default();
        let mut recorded = HashSet::new();
        for path in ["a.md", "a", "a.md", "a"] {
            let ctx = ConflictCtx { storage: &storage, blobs: &blobs, dir: &dir, local: &local, listing: &listing, state: &mut state, report: &mut report, recorded: &mut recorded, writer_id: "deadbeef" };
            record_conflicts(ctx, &[path.to_string()]).unwrap();
        }
        assert_eq!(report.conflicts.len(), 2, "each path once: {:?}", report.conflicts);
        assert!(report.conflicts[0].starts_with("a.md (remote copy: a.conflict-"), "{:?}", report.conflicts);
        assert!(report.conflicts[1].starts_with("a (remote copy: a.conflict-"), "{:?}", report.conflicts);
        let copies: Vec<Vec<u8>> = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".conflict-")).map(|e| fs::read(e.path()).unwrap()).collect();
        assert!(copies.contains(&b"theirs a".to_vec()) && copies.contains(&b"theirs a.md".to_vec()), "both remote copies saved");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pull_records_both_a_and_a_md_conflicts() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"base a"), ("a.md", b"base a.md")]);
        let c = client_for(&mock);
        let dir = tmpdir("pull-conflict-paths");
        sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        mock.external_commit("t", "main", &[("a", Some(b"theirs a")), ("a.md", Some(b"theirs a.md"))], "theirs");
        // a different size, so the scan re-hashes whatever the mtime says
        fs::write(dir.join("a"), b"ours, a").unwrap();
        fs::write(dir.join("a.md"), b"ours, a.md").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        let mut paths: Vec<&str> = report.conflicts.iter().filter_map(|c| c.split(' ').next()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a", "a.md"], "{:?}", report.conflicts);
        assert_eq!(fs::read(dir.join("a")).unwrap(), b"ours, a");
        assert_eq!(fs::read(dir.join("a.md")).unwrap(), b"ours, a.md");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_catches_drift() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"same"), ("b.txt", b"will-drift")]);
        let c = client_for(&mock);
        let dir = tmpdir("verify");
        sync_once(&c, "t", &dir, &opts(Mode::Pull)).unwrap();
        let clean = verify(&c, "t", &dir, None).unwrap();
        assert!(clean.conflicts.is_empty(), "{:?}", clean.conflicts);
        assert_eq!(clean.exit_code(), 0, "a folder in sync exits 0");
        // same size, different content — the exact lie the audit exists for
        fs::write(dir.join("b.txt"), b"went-drft").unwrap();
        let report = verify(&c, "t", &dir, None).unwrap();
        assert!(report.conflicts.iter().any(|c| c.starts_with("b.txt")), "{:?}", report.conflicts);
        assert!(report.conflicts.iter().all(|c| !c.starts_with("a.txt")));
        assert_eq!(report.exit_code(), 3, "drift exits as a conflict does");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stateless_bootstrap_verifies_by_content() {
        // no state, equal size, DIFFERENT content ("12345" vs "67890"):
        // bootstrap verifies by content — no adoption, local wins. The old
        // size-provisional rule silently never pushed these (found live);
        // verify audits nothing if the first sync already guessed wrong.
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"12345")]);
        let c = client_for(&mock);
        let dir = tmpdir("bootstrap");
        fs::write(dir.join("a.txt"), b"67890").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert_eq!(report.pushed, vec!["a.txt".to_string()], "content-different must push, not adopt");
        let st = load_state(&dir, "t").unwrap();
        assert!(st.files.contains_key("a.txt"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mirror_from_overlays_before_push() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[]);
        let c = client_for(&mock);
        let src = tmpdir("mf-src");
        fs::create_dir_all(src.join("notes")).unwrap();
        fs::write(src.join("notes/x.md"), b"note body").unwrap();
        fs::write(src.join("fragment.json"), b"{}").unwrap(); // never overlaid
        let dir = tmpdir("mf");
        fs::write(dir.join("app.mjs"), b"// app").unwrap();
        let o = SyncOptions { mirror_from: Some(src.clone()), ..opts(Mode::Push) };
        let report = sync_once(&c, "t", &dir, &o).unwrap();
        assert_eq!(report.pushed.len(), 2); // app.mjs + notes/x.md
        assert!(mock.file_at("t", "main", "notes/x.md").is_some());
        assert!(mock.file_at("t", "main", "app.mjs").is_some());
        assert!(mock.file_at("t", "main", "fragment.json").is_none(), "source fragment.json must not stomp the target's");
        fs::remove_dir_all(&src).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_single_file_lands_with_cas_retry() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("fragment.json", br#"{"name":"t"}"#)]);
        let c = client_for(&mock);
        let tip = commit_single_file(&c, "t", "fragment.json", br#"{"name":"t","visibility":"public"}"#.to_vec(), "manifest-set", "deadbeef", None).unwrap();
        assert!(!tip.is_empty());
        let v: serde_json::Value = serde_json::from_slice(&mock.file_at("t", "main", "fragment.json").unwrap()).unwrap();
        assert_eq!(v["visibility"], "public");
        // replay (same expected parent now stale) still succeeds via retry
        let tip2 = commit_single_file(&c, "t", "fragment.json", br#"{"name":"t","visibility":"token"}"#.to_vec(), "manifest-set", "deadbeef", None).unwrap();
        assert_ne!(tip, tip2);
    }

    // ---- the world-change regressions (both found live in one day) ----

    #[test]
    fn journal_bound_to_other_world_refuses() {
        // a journal from a DIFFERENT world must refuse before mirroring
        // that world over the folder. Two dimensions, two incidents:
        // (a) different repo, same host — a recreated fragment
        // (b) same repo name, different host — dev vs prod (repo ids
        //     alone can't tell these apart when the service reports
        //     name-form identities)
        let mock = MockServer::start();
        mock.seed_repo("t", &[("old.txt", b"remote world")]);
        let c = client_for(&mock);
        for (tag, journal) in [
            (
                "other repo, same host",
                format!("{{\"schemaVersion\":3,\"name\":\"t\",\"host\":\"{}\",\"repo\":\"urn:some-other-repo\",\"files\":{{}}}}", mock.url),
            ),
            (
                "same repo, other host",
                r#"{"schemaVersion":3,"name":"t","host":"https://fragment.club","repo":"t","files":{}}"#.to_string(),
            ),
        ] {
            let dir = tmpdir("world-mismatch");
            fs::write(dir.join("local.txt"), b"local world").unwrap();
            fs::create_dir_all(dir.join(".fragment")).unwrap();
            fs::write(dir.join(".fragment/state.json"), journal).unwrap();
            let err = match sync_once(&c, "t", &dir, &opts(Mode::Mirror)) {
                Err(SyncError::Repo(msg)) => msg,
                other => panic!("{tag}: expected Repo error, got {other:?}"),
            };
            assert!(err.contains("rebinds"), "{tag}: error offers the remedy: {err}");
            // nothing touched: local intact, no commits sent
            assert_eq!(fs::read(dir.join("local.txt")).unwrap(), b"local world", "{tag}");
            assert_eq!(mock.commit_pack_count(), 0, "{tag}");
            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn recreated_remote_world_guard_holds_local_files() {
        // same repo identity, but the remote was reset (fresh seed, empty
        // tree): the journal's files look remotely-deleted and the mirror
        // would delete them locally — the guard must refuse instead
        let mock = MockServer::start();
        mock.seed_repo("t", &[]);
        let c = client_for(&mock);
        let dir = tmpdir("world-reset");
        for i in 0..5 {
            fs::write(dir.join(format!("f{i}.txt")), format!("content {i}").as_bytes()).unwrap();
        }
        sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap(); // binds the journal
        assert_eq!(mock.refresh_count(), 1, "landing sync nudges the pin refresh");
        mock.seed_repo("t", &[]); // server-side world reset, same repo id
        let report = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(report.mass_delete_guard.is_some(), "5 of 5 remotely-deleted must trip the guard");
        for i in 0..5 {
            assert!(dir.join(format!("f{i}.txt")).exists(), "f{i}.txt must survive the refusal");
        }
        // the escape hatch applies it
        let o = SyncOptions { apply_mass_delete: true, ..opts(Mode::Mirror) };
        let report = sync_once(&c, "t", &dir, &o).unwrap();
        assert!(report.mass_delete_guard.is_none());
        assert_eq!(report.deleted_local.len() + report.pushed.len(), 5, "re-seeded local files push back");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bootstrap_same_size_different_content_pushes() {
        // stateless bootstrap used to treat equal SIZE as same FILE — a
        // same-size different-content file silently never pushed (found
        // live). Content must decide.
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"aaaa")]);
        let c = client_for(&mock);
        let dir = tmpdir("bootstrap-diff");
        fs::write(dir.join("a.txt"), b"bbbb").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert_eq!(report.pushed, vec!["a.txt".to_string()], "equal size, different content must push");
        assert_eq!(mock.file_at("t", "main", "a.txt").unwrap(), b"bbbb");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bootstrap_identical_content_adopts_without_commit() {
        // the flip side: byte-identical files adopt into the journal with
        // no commit and no re-fetch on the next pass
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.txt", b"same")]);
        let c = client_for(&mock);
        let dir = tmpdir("bootstrap-same");
        fs::write(dir.join("a.txt"), b"same").unwrap();
        let report = sync_once(&c, "t", &dir, &opts(Mode::Push)).unwrap();
        assert!(report.pushed.is_empty(), "identical content is not a push");
        assert_eq!(mock.commit_pack_count(), 0, "no commit-packs sent — the seed commit is server-side");
        let r2 = sync_once(&c, "t", &dir, &opts(Mode::Mirror)).unwrap();
        assert!(r2.pushed.is_empty() && r2.pulled.is_empty(), "adopted file stays quiet");
        let st = load_state(&dir, "t").unwrap();
        assert_eq!(st.repo.as_deref(), Some("t"), "journal binds to the repo identity");
        assert_eq!(st.host.as_deref(), Some(mock.url.as_str()), "journal binds to the host identity");
        fs::remove_dir_all(&dir).ok();
    }
}
