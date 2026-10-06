//! The Hermes image's answer to the platform's hold (docs/computers.md,
//! "The hold"; step 1 of docs/durable-computers.md). Before a save the
//! platform touches `/run/computer/hold` and waits for `held`. This image
//! answers once its bridge claims nothing (the bridge's own answer,
//! `BRIDGE_HELD`) and once it has copied every SQLite database under
//! `/data` with SQLite's online backup, each whole and as of one moment,
//! into a staging directory the save keeps. Its answer names exactly what
//! it copied (each database and its `-wal`, `-shm`, `-journal`) as what the
//! save leaves out, so a hot copy of a database Hermes is writing, which
//! could tear (F4 of docs/explorations/pi-durable.md), is never what a wake
//! restores; a database made after the copy is named by none, and is kept
//! hot rather than lost. A restore puts the copies back before Hermes
//! starts, and `PRAGMA quick_check` proves each before the guest takes a
//! turn.
//!
//! Why SQLite's backup API from Rust (rusqlite), not Hermes' own: `hermes
//! backup --quick` copies a fixed list of files under one home, missing a
//! database a plugin or a profile keeps elsewhere; its `_safe_copy_db` is
//! per file, but copies 256 pages a step with 0.1 s between, so a busy
//! database restarts its copy and a large one takes seconds the hold does
//! not have. One `step(-1)` copies a database in one read transaction: a
//! consistent copy whatever writes beside it, never restarted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The platform's hold, and this image's answer to it.
pub const HOLD: &str = "/run/computer/hold";
pub const HELD: &str = "/run/computer/held";
/// The bridge's own answer (its `BRIDGE_HELD`): no claim in flight.
pub const BRIDGE_HELD: &str = "/var/lib/fragment-run/bridge-held";
/// What a computer keeps, and where the copies wait, under names the save
/// keeps (`<n>.sqlite`), with the manifest that names them written last.
pub const DATA: &str = "/data";
pub const STAGING: &str = "/data/held-copies";
const MANIFEST: &str = "manifest.json";
/// The live files beside a database that are its, never a copy's.
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
/// Directory entries one walk of `/data` reads at most.
const WALK_ENTRIES_MAX: usize = 200_000;
/// Databases one hold copies at most (the platform's answer bound: four
/// lines each).
pub const DATABASES_MAX: usize = 128;
/// A line the answer names is at most this long (the platform's bound).
const LINE_MAX_BYTES: usize = 256;
/// A copy's tries while its database is locked (a checkpoint, a recovery),
/// and the pause between.
const COPY_TRIES_MAX: u32 = 40;
const COPY_RETRY_MS: u64 = 50;
/// The 16 bytes every SQLite database begins with.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Why a hold could not be answered, or a copy put back or checked.
#[derive(Debug)]
pub enum HeldError {
    /// A directory could not be read, or a path is not what its name says.
    Walk { path: PathBuf, why: String },
    /// More entries or databases than a hold reads.
    TooMany { what: &'static str, max: usize },
    /// A directory, or a link, named as a database: the answer would leave
    /// it out whole, and no copy would keep it.
    NotAFile(PathBuf),
    Copy { path: PathBuf, why: String },
    Staging(String),
    /// The manifest does not read, or names what is no copy of ours.
    Manifest(String),
    /// A database's check failed: the save that restored it is unusable.
    Check { path: PathBuf, why: String },
}

impl std::fmt::Display for HeldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeldError::Walk { path, why } => write!(f, "{}: {why}", path.display()),
            HeldError::TooMany { what, max } => write!(f, "more than {max} {what}"),
            HeldError::NotAFile(p) => write!(f, "{} is named as a database and is no file", p.display()),
            HeldError::Copy { path, why } => write!(f, "copying {}: {why}", path.display()),
            HeldError::Staging(why) => write!(f, "the staging: {why}"),
            HeldError::Manifest(why) => write!(f, "the manifest: {why}"),
            HeldError::Check { path, why } => write!(f, "{} failed its check: {why}", path.display()),
        }
    }
}

/// One copy: its name in the staging, the live path it is of, its size,
/// and the owner and mode the live file had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copied {
    pub file: String,
    pub path: String,
    pub bytes: u64,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

/// The staging's manifest, written once every copy is whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub copies: Vec<Copied>,
}

fn named_db(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "db")
}

/// Every SQLite database the answer leaves out: each entry named `*.db`
/// under `root`, outside `skip` (the staging, and what the save keeps
/// whole). A directory or a link so named is an error: the answer would
/// leave it out, and nothing would keep it.
pub fn databases(root: &Path, skip: &[&Path]) -> Result<Vec<PathBuf>, HeldError> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    // bounded by WALK_ENTRIES_MAX
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && dir != root => continue,
            Err(e) => return Err(HeldError::Walk { path: dir, why: e.to_string() }),
        };
        for entry in entries {
            let entry = entry.map_err(|e| HeldError::Walk { path: dir.clone(), why: e.to_string() })?;
            seen += 1;
            if seen > WALK_ENTRIES_MAX {
                return Err(HeldError::TooMany { what: "entries under /data", max: WALK_ENTRIES_MAX });
            }
            let path = entry.path();
            if skip.iter().any(|s| path == *s) {
                continue;
            }
            // a file that went as it was read is no database to keep
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            let file_type = meta.file_type();
            match (named_db(&path), file_type.is_file(), file_type.is_dir()) {
                (true, true, _) => found.push(path),
                (true, false, _) => return Err(HeldError::NotAFile(path)),
                (false, _, true) => stack.push(path),
                (false, _, false) => {}
            }
        }
    }
    if found.len() > DATABASES_MAX {
        return Err(HeldError::TooMany { what: "databases", max: DATABASES_MAX });
    }
    found.sort();
    Ok(found)
}

/// Copies `src` whole into `dst` (a new file), by SQLite's online backup
/// in one step: one read transaction, so the copy is of one moment
/// whatever writes beside it. A locked source is tried again, bounded.
fn copy_one(src: &Path, dst: &Path) -> Result<(), String> {
    use rusqlite::backup::{Backup, StepResult};
    let from = rusqlite::Connection::open_with_flags(src, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| e.to_string())?;
    let mut to = rusqlite::Connection::open(dst).map_err(|e| e.to_string())?;
    {
        let backup = Backup::new(&from, &mut to).map_err(|e| e.to_string())?;
        let mut tries = 0;
        // bounded by COPY_TRIES_MAX
        loop {
            tries += 1;
            match backup.step(-1).map_err(|e| e.to_string())? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked if tries < COPY_TRIES_MAX => std::thread::sleep(std::time::Duration::from_millis(COPY_RETRY_MS)),
                other => return Err(format!("still {other:?} after {tries} tries")),
            }
            if tries >= COPY_TRIES_MAX {
                return Err(format!("not done after {tries} steps"));
            }
        }
    }
    // the copy stands alone: no journal of its own beside it
    to.execute_batch("PRAGMA journal_mode = DELETE").map_err(|e| e.to_string())?;
    drop(to);
    std::fs::File::open(dst).and_then(|f| f.sync_all()).map_err(|e| e.to_string())
}

/// Copies each of `dbs` into `staging`, then writes the manifest, so a
/// manifest is only ever of copies that are whole. The staging is made,
/// empty and the caller's, by `make_staging` (as root: `/data` is root's);
/// the caller is the user who owns the databases (opening one creates its
/// `-wal` and `-shm` as the opener's).
pub fn copy_all(dbs: &[PathBuf], staging: &Path) -> Result<Manifest, HeldError> {
    use std::os::unix::fs::MetadataExt;
    assert!(dbs.len() <= DATABASES_MAX, "the walk bounds the databases");
    let empty = std::fs::read_dir(staging).map_err(|e| HeldError::Staging(e.to_string()))?.next().is_none();
    if !empty {
        return Err(HeldError::Staging(format!("{} is not empty", staging.display())));
    }
    let mut copies = Vec::with_capacity(dbs.len());
    for (i, db) in dbs.iter().enumerate() {
        let file = format!("{i}.sqlite");
        let dst = staging.join(&file);
        let meta = std::fs::metadata(db).map_err(|e| HeldError::Copy { path: db.clone(), why: e.to_string() })?;
        copy_one(db, &dst).map_err(|why| HeldError::Copy { path: db.clone(), why })?;
        let bytes = std::fs::metadata(&dst).map_err(|e| HeldError::Copy { path: db.clone(), why: e.to_string() })?.len();
        assert!(bytes > 0, "a copy holds at least its header");
        copies.push(Copied { file, path: db.display().to_string(), bytes, uid: meta.uid(), gid: meta.gid(), mode: meta.mode() & 0o7777 });
    }
    let manifest = Manifest { copies };
    let text = serde_json::to_vec(&manifest).map_err(|e| HeldError::Manifest(e.to_string()))?;
    let tmp = staging.join("manifest.json.tmp");
    let written = std::fs::write(&tmp, &text).and_then(|()| std::fs::File::open(&tmp)?.sync_all()).and_then(|()| std::fs::rename(&tmp, staging.join(MANIFEST)));
    written.map_err(|e| HeldError::Manifest(e.to_string()))?;
    Ok(manifest)
}

/// Makes the staging anew, empty, owned by `owner` (the user who copies).
pub fn make_staging(staging: &Path, owner: Option<(u32, u32)>) -> Result<(), HeldError> {
    clear_staging(staging)?;
    std::fs::create_dir_all(staging).map_err(|e| HeldError::Staging(e.to_string()))?;
    if let Some((uid, gid)) = owner {
        let path = std::ffi::CString::new(staging.as_os_str().as_encoded_bytes()).map_err(|e| HeldError::Staging(e.to_string()))?;
        // SAFETY: a valid NUL-terminated path of the directory just made.
        if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
            return Err(HeldError::Staging(format!("chown: {}", std::io::Error::last_os_error())));
        }
    }
    Ok(())
}

/// Removes the staging, whatever it holds.
pub fn clear_staging(staging: &Path) -> Result<(), HeldError> {
    match std::fs::remove_dir_all(staging) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HeldError::Staging(e.to_string())),
    }
}

/// The manifest in `staging`, when one is there and every copy it names is
/// whole (each a file of its size, of a database under `/data`).
pub fn read_manifest(data: &Path, staging: &Path) -> Result<Option<Manifest>, HeldError> {
    let text = match std::fs::read(staging.join(MANIFEST)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(HeldError::Manifest(e.to_string())),
    };
    let manifest: Manifest = serde_json::from_slice(&text).map_err(|e| HeldError::Manifest(e.to_string()))?;
    if manifest.copies.len() > DATABASES_MAX {
        return Err(HeldError::Manifest(format!("{} copies", manifest.copies.len())));
    }
    for c in &manifest.copies {
        let path = Path::new(&c.path);
        let under_data = path.is_absolute() && path.starts_with(data) && !path.starts_with(staging) && path.components().all(|p| matches!(p, std::path::Component::RootDir | std::path::Component::Normal(_)));
        let file_ok = !c.file.contains('/') && c.file.ends_with(".sqlite");
        if !under_data || !named_db(path) || !file_ok {
            return Err(HeldError::Manifest(format!("{} is no copy of a database under {}", c.path, data.display())));
        }
        let bytes = std::fs::metadata(staging.join(&c.file)).map(|m| m.len()).map_err(|e| HeldError::Manifest(format!("{}: {e}", c.file)))?;
        if bytes != c.bytes {
            return Err(HeldError::Manifest(format!("{} is {bytes} bytes, not {}", c.file, c.bytes)));
        }
    }
    Ok(Some(manifest))
}

/// At a start, before Hermes: puts back what the staging's manifest names,
/// each copy over its live path, the live file's `-wal`, `-shm` and
/// `-journal` removed (they are of the file the save left out, never of the
/// copy), its owner and mode as they were; then removes the staging. No
/// manifest (no copy, or one cut short) puts nothing back. Idempotent: a
/// second call finds no staging. Answers what it put back.
pub fn put_back(data: &Path, staging: &Path) -> Result<Vec<PathBuf>, HeldError> {
    let Some(manifest) = read_manifest(data, staging)? else {
        clear_staging(staging)?;
        return Ok(vec![]);
    };
    let mut put = Vec::with_capacity(manifest.copies.len());
    for c in &manifest.copies {
        let path = PathBuf::from(&c.path);
        for sidecar in SIDECARS {
            let mut side = path.clone().into_os_string();
            side.push(sidecar);
            match std::fs::remove_file(&side) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(HeldError::Copy { path: PathBuf::from(side), why: e.to_string() }),
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| HeldError::Copy { path: path.clone(), why: e.to_string() })?;
        }
        std::fs::rename(staging.join(&c.file), &path).map_err(|e| HeldError::Copy { path: path.clone(), why: e.to_string() })?;
        let owned = std::ffi::CString::new(c.path.as_bytes()).map_err(|e| HeldError::Copy { path: path.clone(), why: e.to_string() })?;
        // SAFETY: a valid NUL-terminated path of a file this put in place.
        unsafe {
            libc::chown(owned.as_ptr(), c.uid, c.gid);
            libc::chmod(owned.as_ptr(), c.mode as libc::mode_t);
        }
        put.push(path);
    }
    clear_staging(staging)?;
    Ok(put)
}

/// `PRAGMA quick_check` on each of `dbs`: the first that fails, and why. A
/// database that will not open fails it too. The caller is the user who
/// owns them (opening one creates its `-wal` and `-shm` as the opener's).
pub fn quick_check(dbs: &[PathBuf]) -> Result<(), HeldError> {
    for db in dbs {
        let check = || -> Result<String, String> {
            let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| e.to_string())?;
            c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())
        };
        match check() {
            Ok(answer) if answer == "ok" => {}
            Ok(answer) => return Err(HeldError::Check { path: db.clone(), why: answer }),
            Err(why) => return Err(HeldError::Check { path: db.clone(), why }),
        }
    }
    Ok(())
}

/// Every file under `root` that is a SQLite database by its first bytes,
/// whatever its name (a browser's cookies are one): what the Docker rung
/// checks after a restore.
pub fn sqlite_files(root: &Path) -> Result<Vec<PathBuf>, HeldError> {
    use std::io::Read;
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    // bounded by WALK_ENTRIES_MAX
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(Result::ok) {
            seen += 1;
            if seen > WALK_ENTRIES_MAX {
                return Err(HeldError::TooMany { what: "entries", max: WALK_ENTRIES_MAX });
            }
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() && meta.len() >= 100 {
                let mut head = [0u8; 16];
                if std::fs::File::open(&path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == SQLITE_HEADER {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Whether the answer can name `db` exactly: its path under `data` as an
/// anchored gitignore pattern, of letters, digits and `._-/` only (no
/// character a pattern reads as more), within the platform's bound. One it
/// cannot name is not copied: the save keeps it hot, never leaves it out.
pub fn nameable(data: &Path, db: &Path) -> bool {
    let Ok(rel) = db.strip_prefix(data) else { return false };
    let Some(rel) = rel.to_str() else { return false };
    !rel.is_empty() && rel.len() + 1 + "-journal".len() <= LINE_MAX_BYTES && rel.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
}

/// The answer to a hold: exactly what was copied, each database and its
/// journal files, one anchored pattern a line, relative to `data`. A
/// database Hermes makes after the copy is named by none, so the save
/// keeps it hot rather than leaving it out.
pub fn answer(data: &Path, copies: &[Copied]) -> String {
    let mut out = String::new();
    for c in copies {
        let path = Path::new(&c.path);
        assert!(nameable(data, path), "only a nameable database is copied: {}", c.path);
        let rel = path.strip_prefix(data).expect("under the data").display().to_string();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            out.push_str(&format!("/{rel}{suffix}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("held-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn db(path: &Path, rows: u32) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE IF NOT EXISTS t (n INTEGER, s TEXT);").unwrap();
        for n in 0..rows {
            c.execute("INSERT INTO t VALUES (?1, ?2)", rusqlite::params![n, "x".repeat(200)]).unwrap();
        }
    }

    fn count(path: &Path) -> u32 {
        rusqlite::Connection::open(path).unwrap().query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap()
    }

    /// Valid: every `*.db` under the root is found but those it skips; a
    /// directory or a link so named is an error (it would be left out whole).
    #[test]
    fn the_walk_finds_every_database_and_refuses_what_it_cannot_copy() {
        let root = dir("walk");
        std::fs::create_dir_all(root.join("hermes/profiles/a")).unwrap();
        std::fs::create_dir_all(root.join("work")).unwrap();
        db(&root.join("hermes/state.db"), 1);
        db(&root.join("hermes/profiles/a/state.db"), 1);
        db(&root.join("work/app.db"), 1);
        std::fs::write(root.join("hermes/notes.txt"), "x").unwrap();
        let found = databases(&root, &[&root.join("work")]).unwrap();
        assert_eq!(found, vec![root.join("hermes/profiles/a/state.db"), root.join("hermes/state.db")]);
        std::fs::create_dir_all(root.join("hermes/odd.db")).unwrap();
        assert!(matches!(databases(&root, &[]), Err(HeldError::NotAFile(_))));
        std::fs::remove_dir(root.join("hermes/odd.db")).unwrap();
        std::os::unix::fs::symlink(root.join("hermes/state.db"), root.join("hermes/link.db")).unwrap();
        assert!(matches!(databases(&root, &[]), Err(HeldError::NotAFile(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Valid: a copy taken while another connection writes is whole and of
    /// one moment; put back over a live database (its WAL with it gone), it
    /// opens with every row the copy had and passes its check; a second put
    /// back finds nothing. Invalid: a manifest cut short puts nothing back.
    #[test]
    fn a_copy_taken_while_written_is_whole_and_put_back_before_a_start() {
        let root = dir("copy");
        let live = root.join("hermes/state.db");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        db(&live, 50);
        let writer = rusqlite::Connection::open(&live).unwrap();
        writer.execute_batch("BEGIN; INSERT INTO t VALUES (999, 'uncommitted');").unwrap();
        let staging = root.join("held-copies");
        make_staging(&staging, None).unwrap();
        let manifest = copy_all(std::slice::from_ref(&live), &staging).unwrap();
        writer.execute_batch("ROLLBACK;").unwrap();
        drop(writer);
        assert_eq!(manifest.copies.len(), 1);
        let copy = staging.join(&manifest.copies[0].file);
        assert_eq!(count(&copy), 50, "the copy has what was committed, not what was in flight");
        assert!(!Path::new(&format!("{}-wal", copy.display())).exists(), "the copy stands alone");
        // the live database moves on, a WAL of its own beside it, as the save leaves it out
        db(&live, 10);
        assert_eq!(count(&live), 60);
        let live_conn = rusqlite::Connection::open(&live).unwrap();
        live_conn.execute_batch("PRAGMA wal_autocheckpoint = 0; INSERT INTO t VALUES (7, 'only in the wal');").unwrap();
        assert!(Path::new(&format!("{}-wal", live.display())).exists());
        drop(live_conn);
        // a copy of a path outside the data root is refused, and puts nothing back
        assert!(matches!(read_manifest(&root.join("elsewhere"), &staging), Err(HeldError::Manifest(_))));
        // put back: the copy over the live file, the live WAL gone, as of the copy
        let put = put_back(&root, &staging).unwrap();
        assert_eq!(put, vec![live.clone()]);
        assert!(!staging.exists(), "the staging goes");
        assert!(!Path::new(&format!("{}-wal", live.display())).exists(), "the live file's WAL went with it");
        quick_check(std::slice::from_ref(&live)).unwrap();
        assert_eq!(count(&live), 50, "as of the copy");
        // again: nothing to put back
        assert_eq!(put_back(&root, &staging).unwrap(), Vec::<PathBuf>::new());
        // a manifest cut short (a copy missing) refuses; no manifest puts nothing back
        make_staging(&staging, None).unwrap();
        assert!(copy_all(std::slice::from_ref(&live), &staging).is_ok());
        assert!(matches!(copy_all(std::slice::from_ref(&live), &staging), Err(HeldError::Staging(_))), "a staging that is not empty is refused");
        make_staging(&staging, None).unwrap();
        let manifest = copy_all(std::slice::from_ref(&live), &staging).unwrap();
        std::fs::remove_file(staging.join(&manifest.copies[0].file)).unwrap();
        assert!(matches!(put_back(&root, &staging), Err(HeldError::Manifest(_))));
        std::fs::remove_file(staging.join(MANIFEST)).unwrap();
        assert_eq!(put_back(&root, &staging).unwrap(), Vec::<PathBuf>::new());
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Invalid: a check of a database torn as a hot copy can tear it (its
    /// pages from two moments) fails, as does one that is not a database.
    #[test]
    fn a_torn_database_fails_its_check() {
        let root = dir("torn");
        let live = root.join("state.db");
        db(&live, 400);
        let c = rusqlite::Connection::open(&live).unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        drop(c);
        let mut bytes = std::fs::read(&live).unwrap();
        // a page of the table's b-tree overwritten, as a copy read across a write would leave it
        let page = 4096 * 3;
        assert!(bytes.len() > page + 4096, "{} bytes", bytes.len());
        for b in &mut bytes[page..page + 4096] {
            *b = 0xA5;
        }
        let torn = root.join("torn.db");
        std::fs::write(&torn, &bytes).unwrap();
        assert!(matches!(quick_check(std::slice::from_ref(&torn)), Err(HeldError::Check { .. })));
        std::fs::write(root.join("not.db"), b"no database at all, only words, more than a hundred bytes of them, so it is read as a file").unwrap();
        assert!(quick_check(&[root.join("not.db")]).is_err());
        assert_eq!(sqlite_files(&root).unwrap(), vec![live, torn], "found by their first bytes");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The answer names exactly what was copied, anchored under the data
    /// root; a database it cannot name is never copied.
    #[test]
    fn the_answer_names_exactly_what_was_copied() {
        let data = Path::new("/data");
        let copy = |p: &str| Copied { file: "0.sqlite".into(), path: p.into(), bytes: 4096, uid: 0, gid: 0, mode: 0o600 };
        assert_eq!(
            answer(data, &[copy("/data/hermes/state.db"), copy("/data/hermes/profiles/juniper-paul/state.db")]),
            "/hermes/state.db\n/hermes/state.db-wal\n/hermes/state.db-shm\n/hermes/state.db-journal\n/hermes/profiles/juniper-paul/state.db\n/hermes/profiles/juniper-paul/state.db-wal\n/hermes/profiles/juniper-paul/state.db-shm\n/hermes/profiles/juniper-paul/state.db-journal\n"
        );
        for bad in ["/data/a b.db", "/data/a*.db", "/data/[x].db", "/elsewhere/a.db", "/data/é.db"] {
            assert!(!nameable(data, Path::new(bad)), "{bad}");
        }
        assert!(!nameable(data, &data.join("x".repeat(LINE_MAX_BYTES))));
        assert!(nameable(data, Path::new("/data/hermes/kanban.db")));
    }
}
