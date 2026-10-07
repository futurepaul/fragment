//! Who drives an agent's screen, the agent or one person, as the image's
//! agent runtime keeps it in a file of its own: Hermes v0.21.5's Bot Desktop
//! lease (`tools/bot_desktop/lease.py`), which its screen tools read at
//! every action. `computer_use` refuses while a person holds it
//! (`human_has_control`, captures included: the person may be typing a
//! password), and voids an action during which its epoch moved; its
//! browser tools fence the same way. The screen's Take over is this lease
//! (screen.rs), so the agent's tools know when a person holds its screen.
//!
//! The file is `<desktop>/lease.json`: `{holder: "agent" | "human",
//! viewer_id, since, reason, epoch}`, changed only under an exclusive
//! `flock` of `lease.lock` beside it, written whole (`lease.json.tmp`
//! renamed over it), its epoch one more at every change and never at a
//! change that changes nothing, exactly as Hermes changes it (its
//! `_transition`). Hermes reads it without the lock, as this module does.
//!
//! Hermes reads a file it cannot parse as a person holding the screen
//! (`unreadable-lease`): an unreadable lease must never let the agent act
//! on a screen a person may be using. So does this module, and it is
//! stricter about types (an `epoch` that is no count is unreadable here; to
//! Hermes it fails only its next change), so the screen fails closed where
//! the two could disagree. The next change writes a whole lease again.
//!
//! What the screen writes takes the owner of the lease's directory (the
//! runtime's user: the bridge runs as root in our image), so the runtime can
//! still read and lock what the screen wrote.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The holder that is the agent, and the one that is a person (Hermes'
/// `AGENT`, `HUMAN`).
pub const AGENT: &str = "agent";
pub const HUMAN: &str = "human";
/// The viewer Hermes names on a lease it cannot read.
pub const UNREADABLE: &str = "unreadable-lease";
/// A lease file is at most this many bytes: a whole one is about 130 (a
/// viewer id of 64 bytes, a reason Hermes' Desktop shows). Past it the file
/// is no lease (unreadable), and a person holds.
pub const LEASE_FILE_MAX_BYTES: u64 = 4 * 1024;
/// A viewer id, at most this long (the screen's are 32 hex digits).
pub const VIEWER_MAX_BYTES: usize = 64;

/// Who holds a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    Agent,
    /// A person, by their viewer's id (none in a lease written by hand).
    Human(Option<String>),
}

impl Holder {
    /// Whether `viewer` holds it, and may send input.
    pub fn is(&self, viewer: &str) -> bool {
        matches!(self, Holder::Human(Some(v)) if v == viewer)
    }

    /// What the screen's viewers are told: the holding viewer's id, `null`
    /// for the agent. A person whose viewer the lease does not name is
    /// someone, never no one.
    pub fn said(&self) -> Option<String> {
        match self {
            Holder::Agent => None,
            Holder::Human(v) => Some(v.clone().unwrap_or_else(|| UNREADABLE.to_string())),
        }
    }
}

/// A lease, as the file holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub holder: Holder,
    /// When it last changed hands (seconds since the epoch, as Python's
    /// `time.time()`).
    pub since: f64,
    pub reason: String,
    /// One more at every change.
    pub epoch: u64,
}

impl Default for Lease {
    /// No file: a fresh profile, the agent holds (Hermes' `Lease()`).
    fn default() -> Lease {
        Lease { holder: Holder::Agent, since: 0.0, reason: String::new(), epoch: 0 }
    }
}

/// The file's shape, in Hermes' field order (its `asdict`).
#[derive(Serialize, Deserialize)]
struct Wire {
    holder: String,
    #[serde(default)]
    viewer_id: Option<String>,
    #[serde(default)]
    since: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    epoch: Option<u64>,
}

fn unreadable(why: &str) -> Lease {
    Lease { holder: Holder::Human(Some(UNREADABLE.to_string())), since: 0.0, reason: why.to_string(), epoch: 0 }
}

/// A lease from the file's bytes; one that is not a lease is a person
/// holding (Hermes' `_read`: fail closed).
pub fn parse(bytes: &[u8]) -> Lease {
    if bytes.len() as u64 > LEASE_FILE_MAX_BYTES {
        return unreadable("lease file corrupt");
    }
    // unknown fields are Hermes' to add: it keeps only its own
    let Ok(w) = serde_json::from_slice::<Wire>(bytes) else { return unreadable("lease file corrupt") };
    let holder = match w.holder.as_str() {
        AGENT => Holder::Agent,
        HUMAN => Holder::Human(w.viewer_id),
        _ => return unreadable("lease file corrupt"),
    };
    Lease { holder, since: w.since.unwrap_or(0.0), reason: w.reason.unwrap_or_default(), epoch: w.epoch.unwrap_or(0) }
}

/// The file's bytes for `lease`, as Hermes writes them (`json.dumps`).
pub fn render(lease: &Lease) -> String {
    let (holder, viewer_id) = match &lease.holder {
        Holder::Agent => (AGENT, None),
        Holder::Human(v) => (HUMAN, v.clone()),
    };
    let w = Wire { holder: holder.to_string(), viewer_id, since: Some(lease.since), reason: Some(lease.reason.clone()), epoch: Some(lease.epoch) };
    serde_json::to_string(&w).expect("a lease serializes")
}

/// Now, as the lease's `since` counts it.
pub fn now_s() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `viewer` takes the screen (Hermes' `acquire`): the last to take it wins;
/// one who holds it already changes nothing. `None`: no change.
pub fn take(lease: &Lease, viewer: &str, now: f64) -> Option<Lease> {
    assert!(!viewer.is_empty() && viewer.len() <= VIEWER_MAX_BYTES, "a viewer is named");
    if lease.holder.is(viewer) {
        return None;
    }
    Some(Lease { holder: Holder::Human(Some(viewer.to_string())), since: now, reason: String::new(), epoch: lease.epoch.wrapping_add(1) })
}

/// The screen goes back to the agent (Hermes' `release`): given back by
/// `viewer`, which changes nothing unless that viewer holds it (a viewer
/// leaving never takes the screen from the one who took over after it); or,
/// with no viewer, from whoever holds it (a lease an earlier life of the
/// screen left: no viewer of its survives it). The agent's already changes
/// nothing (a bump would void the agent's action under way). `None`: no
/// change.
pub fn give(lease: &Lease, viewer: Option<&str>, now: f64) -> Option<Lease> {
    if lease.holder == Holder::Agent {
        return None;
    }
    if let Some(v) = viewer {
        if !lease.holder.is(v) {
            return None;
        }
    }
    Some(Lease { holder: Holder::Agent, since: now, reason: String::new(), epoch: lease.epoch.wrapping_add(1) })
}

/// What a lease file's change could not do: the screen keeps the lease as
/// it was, and says so.
#[derive(Debug)]
pub enum LeaseError {
    /// The lease's directory is not there (the runtime has not made the
    /// agent's desktop's directory): nothing is made for it.
    NoDirectory(PathBuf),
    Io(String),
    /// Read back after the rename, the file was not what was written: a
    /// writer that does not take the lock (never Hermes).
    NotAsWritten { wrote: String, read: String },
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::NoDirectory(d) => write!(f, "{}: the lease's directory is not there", d.display()),
            LeaseError::Io(e) => write!(f, "{e}"),
            LeaseError::NotAsWritten { wrote, read } => write!(f, "the lease read back {read:?}, not the {wrote:?} written"),
        }
    }
}

impl std::error::Error for LeaseError {}

fn io(what: &Path, e: std::io::Error) -> LeaseError {
    LeaseError::Io(format!("{}: {e}", what.display()))
}

/// What tells one version of the file from the next: a rename makes a new
/// inode, and a write in place a new length or time.
pub type Signature = Option<(u64, u64, i64, i64)>;

/// A lease file: read as it is now, changed under its lock.
#[derive(Debug, Clone)]
pub struct LeaseFile {
    path: PathBuf,
}

impl LeaseFile {
    pub fn new(path: PathBuf) -> LeaseFile {
        assert!(path.is_absolute() && path.parent().is_some(), "a lease file is named by an absolute path");
        LeaseFile { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `lease.lock` beside it (Hermes' `path.with_suffix(".lock")`).
    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    /// `lease.json.tmp` beside it (Hermes' `path.with_suffix(".json.tmp")`).
    fn tmp_path(&self) -> PathBuf {
        self.path.with_extension("json.tmp")
    }

    pub fn signature(&self) -> Signature {
        let m = std::fs::metadata(&self.path).ok()?;
        Some((m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
    }

    /// The lease now, without the lock (as Hermes reads it): a missing file
    /// is the agent's, one that does not read a person's.
    pub fn read(&self) -> Lease {
        let mut bytes = Vec::new();
        let opened = File::open(&self.path).and_then(|f| f.take(LEASE_FILE_MAX_BYTES + 1).read_to_end(&mut bytes));
        match opened {
            Ok(_) => parse(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Lease::default(),
            Err(_) => unreadable("lease file unreadable"),
        }
    }

    /// Changes the lease as `change` says, under the lock (Hermes'
    /// `_transition`): the lease as it is after, and whether it changed.
    pub fn change(&self, change: impl FnOnce(&Lease) -> Option<Lease>) -> Result<(Lease, bool), LeaseError> {
        let dir = self.path.parent().expect("an absolute path has a parent");
        let owner = match std::fs::metadata(dir) {
            Ok(m) if m.is_dir() => (m.uid(), m.gid()),
            Ok(_) => return Err(LeaseError::NoDirectory(dir.to_path_buf())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LeaseError::NoDirectory(dir.to_path_buf())),
            Err(e) => return Err(io(dir, e)),
        };
        let lock_path = self.lock_path();
        let lock = OpenOptions::new().create(true).append(true).mode(0o600).open(&lock_path).map_err(|e| io(&lock_path, e))?;
        own(&lock, owner).map_err(|e| io(&lock_path, e))?;
        // Hermes holds it only for a read and a write: never long
        lock.lock().map_err(|e| io(&lock_path, e))?;
        let before = self.read();
        let Some(after) = change(&before) else { return Ok((before, false)) };
        // a count, as Python's never overflows; one at u64's end was written by hand
        assert_eq!(after.epoch, before.epoch.wrapping_add(1), "a change is one epoch on");
        let text = render(&after);
        let tmp = self.tmp_path();
        let written = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| {
            own(&f, owner)?;
            f.write_all(text.as_bytes())
        });
        written.map_err(|e| io(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io(&self.path, e))?;
        let read = self.read();
        drop(lock);
        if read == after {
            Ok((after, true))
        } else {
            Err(LeaseError::NotAsWritten { wrote: text, read: render(&read) })
        }
    }
}

/// `f` takes `owner` (the lease's directory's), when it has another.
fn own(f: &File, (uid, gid): (u32, u32)) -> std::io::Result<()> {
    let m = f.metadata()?;
    if (m.uid(), m.gid()) == (uid, gid) {
        return Ok(());
    }
    std::os::unix::fs::fchown(f, Some(uid), Some(gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(v: &str, epoch: u64) -> Lease {
        Lease { holder: Holder::Human(Some(v.into())), since: 1.0, reason: String::new(), epoch }
    }

    /// Valid: a person takes the screen and gives it back, each a change of
    /// one epoch, the agent's again after.
    #[test]
    fn take_and_give_are_one_epoch_each() {
        let fresh = Lease::default();
        let taken = take(&fresh, "v1", 10.0).expect("a change");
        assert_eq!((taken.holder.clone(), taken.epoch, taken.since), (Holder::Human(Some("v1".into())), 1, 10.0));
        assert!(taken.holder.is("v1") && !taken.holder.is("v2"));
        let given = give(&taken, Some("v1"), 11.0).expect("a change");
        assert_eq!((given.holder, given.epoch), (Holder::Agent, 2));
        // another viewer takes it from the first: the last to take it wins
        let stolen = take(&taken, "v2", 12.0).expect("a change");
        assert_eq!((stolen.holder.said(), stolen.epoch), (Some("v2".into()), 2));
    }

    /// Replay: taking what one holds, or giving back what the agent holds,
    /// changes nothing, so no epoch moves under an action admitted before.
    #[test]
    fn a_change_that_changes_nothing_is_none() {
        assert_eq!(take(&human("v1", 4), "v1", 20.0), None);
        assert_eq!(give(&Lease::default(), Some("v1"), 20.0), None);
        assert_eq!(give(&Lease::default(), None, 20.0), None);
    }

    /// Invalid: a viewer gives back only what it holds (one leaving never
    /// takes the screen from the one who took over after it); a lease no
    /// viewer of this life holds goes back to the agent only when named so.
    #[test]
    fn only_the_holder_gives_back() {
        assert_eq!(give(&human("v2", 3), Some("v1"), 20.0), None);
        assert_eq!(give(&unreadable("lease file corrupt"), Some("v1"), 20.0), None, "a viewer never gives back what no viewer holds");
        let stale = give(&unreadable("lease file corrupt"), None, 20.0).expect("an earlier life's lease, let go");
        assert_eq!((stale.holder, stale.epoch), (Holder::Agent, 1));
        assert_eq!(give(&human("v2", 3), None, 20.0).map(|l| l.epoch), Some(4));
    }

    /// The file as Hermes writes it reads back; what is no lease is a person
    /// holding, never the agent.
    #[test]
    fn a_file_reads_as_hermes_reads_it() {
        let hermes = br#"{"holder": "human", "viewer_id": "abc", "since": 1759800000.25, "reason": "log in to X", "epoch": 7}"#;
        assert_eq!(parse(hermes), Lease { holder: Holder::Human(Some("abc".into())), since: 1_759_800_000.25, reason: "log in to X".into(), epoch: 7 });
        assert_eq!(parse(br#"{"holder": "agent", "viewer_id": null, "since": 1.5, "reason": "", "epoch": 2, "later": true}"#).holder, Holder::Agent, "a field Hermes adds later is its own");
        let round = human("v1", 9);
        assert_eq!(parse(render(&round).as_bytes()), round);
        assert!(render(&round).starts_with(r#"{"holder":"human","viewer_id":"v1","since":"#), "Hermes' field order");
        for bad in [&b""[..], b"[]", b"{}", br#"{"holder": "nobody"}"#, br#"{"holder": "agent", "epoch": "3"}"#, br#"{"holder": "agent", "epoch": -1}"#, br#"{"holder": "human", "viewer_id": 5}"#] {
            assert_eq!(parse(bad).holder, Holder::Human(Some(UNREADABLE.into())), "{:?} is no lease", String::from_utf8_lossy(bad));
        }
        let long = format!(r#"{{"holder": "agent", "reason": "{}"}}"#, "x".repeat(LEASE_FILE_MAX_BYTES as usize));
        assert_eq!(parse(long.as_bytes()).holder, Holder::Human(Some(UNREADABLE.into())));
        assert_eq!(human("v1", 1).holder.said(), Some("v1".into()));
        assert_eq!(Holder::Human(None).said(), Some(UNREADABLE.into()), "a person the lease names no viewer of is someone");
        assert_eq!(Holder::Agent.said(), None);
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bridge-lease-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("bot-desktop")).unwrap();
        d
    }

    /// The file changed under its lock: written whole, read back, its epoch
    /// one on per change; a change that changes nothing writes nothing; a
    /// file that does not read is let go of whole by the next change.
    #[test]
    fn a_lease_file_changes_under_its_lock() {
        let d = dir("change");
        let f = LeaseFile::new(d.join("bot-desktop/lease.json"));
        assert_eq!(f.read(), Lease::default(), "no file: the agent's");
        let (taken, changed) = f.change(|l| take(l, "v1", now_s())).unwrap();
        assert!(changed && taken.holder.is("v1") && taken.epoch == 1);
        assert_eq!(f.read(), taken);
        assert!(d.join("bot-desktop/lease.lock").exists() && !d.join("bot-desktop/lease.json.tmp").exists());
        let sig = f.signature();
        assert_eq!(f.change(|l| take(l, "v1", now_s())).unwrap(), (taken, false), "taken again: no change");
        assert_eq!(f.signature(), sig, "and nothing written");
        let (given, _) = f.change(|l| give(l, Some("v1"), now_s())).unwrap();
        assert_eq!((given.holder, given.epoch), (Holder::Agent, 2));
        std::fs::write(f.path(), b"{torn").unwrap();
        assert!(f.read().holder.is(UNREADABLE), "a torn lease is a person's");
        let (repaired, _) = f.change(|l| give(l, None, now_s())).unwrap();
        assert_eq!((repaired.holder, repaired.epoch), (Holder::Agent, 1));
        let mode = std::fs::metadata(f.path()).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "the runtime's own, as Hermes makes it");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Invalid: a lease whose directory is not there makes nothing (the
    /// runtime makes its desktop's directory).
    #[test]
    fn no_directory_makes_nothing() {
        let d = dir("nodir");
        let f = LeaseFile::new(d.join("missing/lease.json"));
        assert!(matches!(f.change(|l| take(l, "v1", 1.0)), Err(LeaseError::NoDirectory(_))));
        assert!(!d.join("missing").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
