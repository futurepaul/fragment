//! An agent's repo in its Hermes profile (decision 15): the agent
//! fragment's `SOUL.md`, `memories/` and `skills/` checked out into the
//! profile, and what Hermes changes there committed back through the files
//! route, as the agent. Three ways: main, the profile, and what both agreed
//! on at the last sync (the profile's `MANIFEST`: it goes where the profile
//! goes, so a profile made again starts from nothing), so neither side's
//! change is lost to the other's silence. When both changed a file, main
//! wins (its owner edited it) and the profile's copy is kept beside it as
//! `<file>.local-<ms>`, never pushed.
//!
//! A file is deleted on one side only when the other side was read whole
//! and lacks it. A file either side has but does not sync (past
//! `FILE_MAX_BYTES`, or not a regular file) is left as it is on both sides.
//! A side past `FILES_MAX` files, or a profile not read or written whole,
//! fails the round, which deletes nothing for it.
//!
//! The plan is a pure function (`plan`, `resolve`); `round` carries it out.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use fragment_bridge::api::{Api, ApiError, FileChange, FileEntry};
use fragment_bridge::ev;
use fragment_bridge::records::hex;
use fragment_bridge::runtime::Agent;

/// A synced file is at most this large; one past it is left as it is. The
/// files route (`POST /api/f/{name}/files`, cell/src/publish.rs) commits 16
/// files and 1 MiB of content at once, in a body of at most 2 MiB.
pub const FILE_MAX_BYTES: usize = 192 * 1024;
/// Files one agent's sync takes, at most, on each side; past it, none.
pub const FILES_MAX: usize = 500;
/// What was agreed at the last sync, in the profile.
pub const MANIFEST: &str = ".fragment-sync.json";
/// One commit carries at most this many files (the route's 16), and is cut
/// before its text and base64 pass this many bytes unless it carries a
/// single file (at most 256 KiB as base64): well within the route's 1 MiB.
pub const COMMIT_FILES_MAX: usize = 16;
pub const COMMIT_BYTES_MAX: usize = 180 * 1024;

/// What both sides agreed on for one file at the last sync: main's last
/// commit of it (empty once the bridge pushed it, until main is read
/// again), and its content's hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub commit: String,
    pub hash: String,
}

pub type Manifest = BTreeMap<String, Known>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainFile {
    pub path: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub path: String,
    pub hash: String,
}

/// One step of a sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Main changed it (or it is new there): read it, then `resolve`.
    Fetch(String),
    /// The profile changed it (or it is new there): commit it.
    Push(String),
    /// The profile deleted it, main did not change it: delete it on main.
    PushDelete(String),
    /// Main deleted it, the profile did not change it: delete it here.
    DeleteLocal(String),
    /// Gone on both sides: forget it.
    Forget(String),
}

/// Whether a path of the agent's repo is one Hermes keeps in its profile.
pub fn synced(path: &str) -> bool {
    let in_scope = path == "SOUL.md" || path.starts_with("memories/") || path.starts_with("skills/");
    let parts_ok = path.split('/').all(|p| !p.is_empty() && !p.starts_with('.') && p != "__pycache__" && !p.contains(".local-"));
    in_scope && parts_ok && path.len() <= 300
}

/// The sync's steps, given main, what was agreed, the profile, and the
/// paths either side has but does not sync (left as they are).
pub fn plan(main: &[MainFile], known: &Manifest, local: &[Local], unsynced: &BTreeSet<String>) -> Vec<Action> {
    let main: BTreeMap<&str, &MainFile> = main.iter().map(|m| (m.path.as_str(), m)).collect();
    let local: BTreeMap<&str, &Local> = local.iter().map(|l| (l.path.as_str(), l)).collect();
    let paths: BTreeSet<&str> = main.keys().copied().chain(local.keys().copied()).chain(known.keys().map(String::as_str)).collect();
    let mut out = Vec::new();
    for p in paths.into_iter().filter(|p| !unsynced.contains(*p)) {
        let (m, k, l) = (main.get(p), known.get(p), local.get(p));
        let action = match (m, k, l) {
            (Some(m), Some(k), Some(l)) if m.commit == k.commit => (l.hash != k.hash).then(|| Action::Push(p.into())),
            (Some(m), Some(k), None) if m.commit == k.commit => Some(Action::PushDelete(p.into())),
            (Some(_), _, _) => Some(Action::Fetch(p.into())),
            (None, Some(k), Some(l)) if l.hash == k.hash => Some(Action::DeleteLocal(p.into())),
            (None, Some(_), Some(_)) => Some(Action::Push(p.into())),
            (None, Some(_), None) => Some(Action::Forget(p.into())),
            (None, None, Some(_)) => Some(Action::Push(p.into())),
            (None, None, None) => unreachable!("a path comes from one of the three"),
        };
        out.extend(action);
    }
    out
}

/// What to do with a file main changed, now that its content is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Both sides hold it already: remember it.
    Same,
    /// Write main's into the profile (the profile did not change it).
    Pull,
    /// Both changed it: main's wins, the profile's is kept beside it.
    Conflict,
}

pub fn resolve(known: Option<&Known>, local: Option<&Local>, main_hash: &str) -> Resolution {
    match (known, local) {
        (_, None) => Resolution::Pull,
        (_, Some(l)) if l.hash == main_hash => Resolution::Same,
        (Some(k), Some(l)) if l.hash == k.hash => Resolution::Pull,
        _ => Resolution::Conflict,
    }
}

pub fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// What one side has: the files it syncs, and the paths it has but does
/// not sync.
#[derive(Debug)]
pub struct Side<F> {
    pub files: Vec<F>,
    pub unsynced: BTreeSet<String>,
}

/// Main's synced files, from its whole listing (the files route answers
/// every file or fails).
pub fn listed(listing: Vec<FileEntry>) -> Result<Side<MainFile>, SyncError> {
    let mut out = Side { files: Vec::new(), unsynced: BTreeSet::new() };
    for f in listing.into_iter().filter(|f| synced(&f.path)) {
        if f.size as usize > FILE_MAX_BYTES {
            out.unsynced.insert(f.path);
        } else {
            out.files.push(MainFile { path: f.path, commit: f.last_commit_sha });
        }
    }
    if out.files.len() + out.unsynced.len() > FILES_MAX {
        return Err(SyncError::TooMany("main"));
    }
    Ok(out)
}

/// The profile's synced files and their hashes, read whole: what cannot be
/// read fails it, and so do more than `FILES_MAX` files.
pub fn scan(profile: &Path) -> Result<Side<Local>, SyncError> {
    let mut out = Side { files: Vec::new(), unsynced: BTreeSet::new() };
    let mut stack: Vec<PathBuf> = vec![profile.join("SOUL.md"), profile.join("memories"), profile.join("skills")];
    // bounded by the profile's tree, and FILES_MAX files in it
    while let Some(p) = stack.pop() {
        let unread = |e: std::io::Error| SyncError::Profile(format!("reading {}: {e}", p.display()));
        let meta = match std::fs::symlink_metadata(&p) {
            Ok(meta) => meta,
            // never there, or gone since its directory was read
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(unread(e)),
        };
        let rel = p.strip_prefix(profile).expect("a path under the profile").to_string_lossy().replace('\\', "/");
        if meta.is_dir() {
            // a directory no synced file is under is not read
            if rel == "memories" || rel == "skills" || synced(&rel) {
                for e in std::fs::read_dir(&p).map_err(unread)? {
                    stack.push(e.map_err(unread)?.path());
                }
            }
            continue;
        }
        if !synced(&rel) {
            continue;
        }
        if out.files.len() + out.unsynced.len() == FILES_MAX {
            return Err(SyncError::TooMany("the profile"));
        }
        if !meta.is_file() || meta.len() > FILE_MAX_BYTES as u64 {
            out.unsynced.insert(rel);
            continue;
        }
        match std::fs::read(&p) {
            Ok(bytes) => out.files.push(Local { path: rel, hash: hash(&bytes) }),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(unread(e)),
        }
    }
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// What was agreed at the last sync: nothing, when there is none to read
/// (the safe side: with nothing agreed, nothing is deleted).
pub fn load_manifest(profile: &Path) -> Manifest {
    std::fs::read(profile.join(MANIFEST)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_manifest(profile: &Path, m: &Manifest) -> std::io::Result<()> {
    let tmp = profile.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(m).expect("a manifest serializes"))?;
    std::fs::rename(&tmp, profile.join(MANIFEST))
}

/// Why a round stopped: the platform did not answer as it should, the
/// profile was not read or written whole, or a side (`main`, `the
/// profile`) has more than `FILES_MAX` files. What was agreed is saved only
/// by a round that ends, so the next starts again from both sides.
#[derive(Debug)]
pub enum SyncError {
    Api(ApiError),
    Profile(String),
    TooMany(&'static str),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Api(e) => write!(f, "{e}"),
            SyncError::Profile(e) => write!(f, "the profile: {e}"),
            SyncError::TooMany(side) => write!(f, "{side} has more than {FILES_MAX} synced files: none synced"),
        }
    }
}

impl From<ApiError> for SyncError {
    fn from(e: ApiError) -> SyncError {
        SyncError::Api(e)
    }
}

/// What one round did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub pulled: u32,
    pub pushed: u32,
    pub conflicts: u32,
    pub deleted: u32,
}

/// One sync of an agent's repo and its profile. `own` makes a written
/// file Hermes' (its uid and gid).
pub async fn round(api: &Api, agent: &Agent, profile: &Path, own: &(dyn Fn(&Path) + Sync)) -> Result<Done, SyncError> {
    let mut known = load_manifest(profile);
    let main = listed(api.files(&agent.fragment, &agent.fragment).await?)?;
    let local = scan(profile)?;
    let unsynced: BTreeSet<String> = main.unsynced.union(&local.unsynced).cloned().collect();
    let main_commit: BTreeMap<String, String> = main.files.iter().map(|m| (m.path.clone(), m.commit.clone())).collect();
    let local_by: BTreeMap<String, Local> = local.files.iter().map(|l| (l.path.clone(), l.clone())).collect();
    let mut done = Done::default();
    let mut push: Vec<FileChange> = Vec::new();
    let mut pushed: Vec<(String, Option<String>)> = Vec::new();
    for action in plan(&main.files, &known, &local.files, &unsynced) {
        match action {
            Action::Fetch(p) => {
                let bytes = api.file(&agent.fragment, &agent.fragment, &p, FILE_MAX_BYTES).await?;
                let h = hash(&bytes);
                let target = profile.join(&p);
                let wrote = |e: std::io::Error| SyncError::Profile(format!("writing {p}: {e}"));
                match resolve(known.get(&p), local_by.get(&p), &h) {
                    Resolution::Same => {}
                    Resolution::Pull => {
                        write(profile, &target, &bytes, own).map_err(wrote)?;
                        done.pulled += 1;
                    }
                    Resolution::Conflict => {
                        let keep = target.with_file_name(format!("{}.local-{}", target.file_name().and_then(|n| n.to_str()).unwrap_or("file"), fragment_bridge::log::now_ms()));
                        std::fs::rename(&target, &keep).map_err(wrote)?;
                        write(profile, &target, &bytes, own).map_err(wrote)?;
                        done.conflicts += 1;
                        ev!("sync.conflict", { "agent": agent.fragment, "path": p, "kept": keep.display().to_string() });
                    }
                }
                known.insert(p.clone(), Known { commit: main_commit[&p].clone(), hash: h });
            }
            Action::Push(p) => {
                let Ok(bytes) = std::fs::read(profile.join(&p)) else { continue };
                let change = match String::from_utf8(bytes.clone()) {
                    Ok(text) => FileChange::Text { path: p.clone(), text },
                    Err(_) => FileChange::Base64 { path: p.clone(), base64: base64::engine::general_purpose::STANDARD.encode(&bytes) },
                };
                push.push(change);
                pushed.push((p, Some(hash(&bytes))));
            }
            Action::PushDelete(p) => {
                push.push(FileChange::Delete { path: p.clone(), delete: true });
                pushed.push((p, None));
            }
            Action::DeleteLocal(p) => {
                std::fs::remove_file(profile.join(&p)).map_err(|e| SyncError::Profile(format!("deleting {p}: {e}")))?;
                known.remove(&p);
                done.deleted += 1;
            }
            Action::Forget(p) => {
                known.remove(&p);
            }
        }
    }
    // Commit what changed here, in batches the route takes; each batch's key
    // is its content's hash, so a retried round commits nothing twice.
    let mut batch: Vec<FileChange> = Vec::new();
    let mut batch_paths: Vec<(String, Option<String>)> = Vec::new();
    let mut bytes = 0usize;
    for (change, path) in push.into_iter().zip(pushed) {
        let size = match &change {
            FileChange::Text { text, .. } => text.len(),
            FileChange::Base64 { base64, .. } => base64.len(),
            FileChange::Delete { .. } => 0,
        };
        if !batch.is_empty() && (batch.len() >= COMMIT_FILES_MAX || bytes + size > COMMIT_BYTES_MAX) {
            commit(api, agent, std::mem::take(&mut batch), std::mem::take(&mut batch_paths), &mut known, &mut done).await?;
            bytes = 0;
        }
        bytes += size;
        batch.push(change);
        batch_paths.push(path);
    }
    commit(api, agent, batch, batch_paths, &mut known, &mut done).await?;
    if let Err(e) = save_manifest(profile, &known) {
        ev!("sync.unsaved", { "agent": agent.fragment, "error": e.to_string() });
    }
    Ok(done)
}

/// One commit of the profile's changes; its key is its content's hash, so
/// a retried round commits nothing twice.
async fn commit(api: &Api, agent: &Agent, batch: Vec<FileChange>, paths: Vec<(String, Option<String>)>, known: &mut Manifest, done: &mut Done) -> Result<(), ApiError> {
    if batch.is_empty() {
        return Ok(());
    }
    assert!(batch.len() <= COMMIT_FILES_MAX, "a commit carries at most {COMMIT_FILES_MAX} files");
    let key = format!("hermes-{}", &hash(serde_json::to_string(&batch).expect("changes serialize").as_bytes())[..32]);
    let commit = api.commit(&agent.fragment, &agent.fragment, &batch, &format!("{}: memories and skills, from its computer", agent.name), &key).await?;
    ev!("sync.pushed", { "agent": agent.fragment, "files": batch.len(), "commit": commit });
    for (path, h) in paths {
        done.pushed += 1;
        match h {
            Some(h) => {
                known.insert(path, Known { commit: String::new(), hash: h });
            }
            None => {
                known.remove(&path);
            }
        }
    }
    Ok(())
}

/// Writes a file of the profile. It and each directory made for it, up to
/// the profile, are Hermes' (`own`): Hermes writes beside what is pulled.
fn write(profile: &Path, target: &Path, bytes: &[u8], own: &(dyn Fn(&Path) + Sync)) -> std::io::Result<()> {
    let dir = target.parent().expect("a file under the profile");
    let made: Vec<&Path> = dir.ancestors().take_while(|d| d.starts_with(profile) && *d != profile && !d.exists()).collect();
    std::fs::create_dir_all(dir)?;
    for d in made.into_iter().rev() {
        own(d);
    }
    std::fs::write(target, bytes)?;
    own(target);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(path: &str, commit: &str) -> MainFile {
        MainFile { path: path.into(), commit: commit.into() }
    }
    fn l(path: &str, hash: &str) -> Local {
        Local { path: path.into(), hash: hash.into() }
    }
    fn k(commit: &str, hash: &str) -> Known {
        Known { commit: commit.into(), hash: hash.into() }
    }

    #[test]
    fn only_the_agents_own_files_sync() {
        for yes in ["SOUL.md", "memories/MEMORY.md", "skills/cook/SKILL.md"] {
            assert!(synced(yes), "{yes}");
        }
        for no in ["fragment.json", "site/index.html", "agent.json", "memories/.hidden", "skills/x/__pycache__/a.pyc", "memories/a.md.local-17", "memories//a", "soul.md"] {
            assert!(!synced(no), "{no}");
        }
    }

    /// Valid: every combination of who changed what since the last sync.
    #[test]
    fn a_plan_takes_each_sides_changes() {
        let known: Manifest = [
            ("SOUL.md".into(), k("c1", "h1")),
            ("memories/a.md".into(), k("c1", "ha")),
            ("memories/b.md".into(), k("c1", "hb")),
            ("memories/c.md".into(), k("c1", "hc")),
            ("memories/d.md".into(), k("c1", "hd")),
            ("memories/gone.md".into(), k("c1", "hg")),
        ]
        .into_iter()
        .collect();
        let main = vec![m("SOUL.md", "c1"), m("memories/a.md", "c1"), m("memories/b.md", "c2"), m("memories/d.md", "c1"), m("memories/new-main.md", "c3")];
        let local = vec![l("SOUL.md", "h1"), l("memories/a.md", "ha2"), l("memories/b.md", "hb"), l("memories/c.md", "hc"), l("memories/new-local.md", "hn")];
        let got = plan(&main, &known, &local, &BTreeSet::new());
        assert_eq!(
            got,
            vec![
                Action::Push("memories/a.md".into()),        // the profile changed it
                Action::Fetch("memories/b.md".into()),       // main changed it
                Action::DeleteLocal("memories/c.md".into()), // main deleted it, untouched here
                Action::PushDelete("memories/d.md".into()),  // deleted here, untouched on main
                Action::Forget("memories/gone.md".into()),   // gone on both sides
                Action::Push("memories/new-local.md".into()),
                Action::Fetch("memories/new-main.md".into()),
            ]
        );
        // Invalid: a path either side does not sync is left as it is, whatever the rest
        let every: BTreeSet<String> = known.keys().chain(main.iter().map(|m| &m.path)).chain(local.iter().map(|l| &l.path)).cloned().collect();
        assert!(plan(&main, &known, &local, &every).is_empty());
    }

    #[test]
    fn replay_a_settled_sync_does_nothing() {
        let known: Manifest = [("SOUL.md".into(), k("c1", "h1"))].into_iter().collect();
        assert!(plan(&[m("SOUL.md", "c1")], &known, &[l("SOUL.md", "h1")], &BTreeSet::new()).is_empty());
        assert!(plan(&[], &Manifest::new(), &[], &BTreeSet::new()).is_empty());
    }

    #[test]
    fn resolving_main_changes() {
        let kn = k("c1", "h1");
        assert_eq!(resolve(Some(&kn), None, "h2"), Resolution::Pull, "missing here: take main's");
        assert_eq!(resolve(Some(&kn), Some(&l("x", "h2")), "h2"), Resolution::Same);
        assert_eq!(resolve(Some(&kn), Some(&l("x", "h1")), "h2"), Resolution::Pull, "unchanged here");
        assert_eq!(resolve(Some(&kn), Some(&l("x", "h3")), "h2"), Resolution::Conflict, "both changed: main wins, the profile's kept");
        // the first sync: Hermes' default SOUL.md loses to the agent's own
        assert_eq!(resolve(None, Some(&l("SOUL.md", "default")), "agents"), Resolution::Conflict);
        // a file the bridge pushed comes back with main's new commit: the same
        assert_eq!(resolve(Some(&k("", "h5")), Some(&l("x", "h5")), "h5"), Resolution::Same);
    }

    /// A files route as the platform's (`GET files`, `GET file?path=`,
    /// `POST files`), each change a new commit of the files it names.
    mod fake {
        use std::collections::BTreeMap;
        use std::net::SocketAddr;
        use std::sync::{Arc, Mutex};

        use base64::Engine;
        use fragment_bridge::net;
        use http_body_util::BodyExt;
        use hyper::{Method, StatusCode};
        use serde_json::{json, Value};

        #[derive(Default)]
        pub struct Repo {
            pub files: BTreeMap<String, (String, Vec<u8>)>,
            pub commits: u32,
            pub keys: Vec<String>,
        }

        impl Repo {
            pub fn put(&mut self, path: &str, body: &str) {
                self.commits += 1;
                self.files.insert(path.into(), (format!("c{}", self.commits), body.as_bytes().to_vec()));
            }
        }

        pub async fn start(repo: Arc<Mutex<Repo>>) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (stop, rx) = tokio::sync::watch::channel(false);
            let handler = move |req: hyper::Request<hyper::body::Incoming>, _: SocketAddr| {
                let repo = repo.clone();
                async move {
                    assert_eq!(req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()), Some("juniper--k3x9"), "as the agent");
                    let (method, path, q) = (req.method().clone(), req.uri().path().to_string(), req.uri().query().unwrap_or("").to_string());
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    let mut r = repo.lock().unwrap();
                    match (method, path.as_str()) {
                        (Method::GET, "/api/f/juniper--k3x9/files") => {
                            let list: Vec<Value> = r.files.iter().map(|(p, (c, b))| json!({ "path": p, "size": b.len(), "lastCommitSha": c })).collect();
                            net::json_answer(StatusCode::OK, &json!({ "files": list }))
                        }
                        (Method::GET, "/api/f/juniper--k3x9/file") => {
                            let p = q.strip_prefix("path=").unwrap_or("").replace("%2F", "/");
                            match r.files.get(&p) {
                                Some((_, b)) => net::respond(StatusCode::OK, "application/octet-stream", b.clone()),
                                None => net::refusal(StatusCode::NOT_FOUND, "not_found", "no such file"),
                            }
                        }
                        (Method::POST, "/api/f/juniper--k3x9/files") => {
                            let v: Value = serde_json::from_slice(&body).unwrap();
                            r.keys.push(v["key"].as_str().unwrap().to_string());
                            r.commits += 1;
                            let c = format!("c{}", r.commits);
                            for f in v["files"].as_array().unwrap() {
                                let p = f["path"].as_str().unwrap().to_string();
                                if f["delete"] == json!(true) {
                                    r.files.remove(&p);
                                } else if let Some(t) = f["text"].as_str() {
                                    r.files.insert(p, (c.clone(), t.as_bytes().to_vec()));
                                } else {
                                    let b = base64::engine::general_purpose::STANDARD.decode(f["base64"].as_str().unwrap()).unwrap();
                                    r.files.insert(p, (c.clone(), b));
                                }
                            }
                            net::json_answer(StatusCode::OK, &json!({ "commit": c }))
                        }
                        _ => net::refusal(StatusCode::NOT_FOUND, "not_found", &path),
                    }
                }
            };
            tokio::spawn(net::serve(listener, handler, rx));
            (addr, stop)
        }
    }

    /// Goal: a round brings main's files into the profile, then commits the
    /// profile's changes back as the agent; a file both sides changed keeps
    /// main's, with the profile's beside it; a settled round does nothing.
    #[tokio::test]
    async fn a_round_against_the_files_route() {
        let w = World::new("round", &[("SOUL.md", "You are Juniper."), ("memories/MEMORY.md", "Paul likes tomatoes."), ("site/index.html", "not the agent's to keep")]).await;
        let d = w.round().await.unwrap();
        assert_eq!((d.pulled, d.pushed), (2, 0));
        assert_eq!(w.here("SOUL.md").as_deref(), Some(&b"You are Juniper."[..]));
        assert!(!w.profile.join("site").exists(), "only the agent's own files");
        assert_eq!(w.round().await.unwrap(), Done::default(), "replay: settled");

        // Hermes writes a memory and a skill: committed back, once
        w.write("memories/MEMORY.md", b"Paul likes tomatoes. And basil.");
        w.write("skills/pesto/SKILL.md", b"# Pesto");
        assert_eq!(w.round().await.unwrap().pushed, 2);
        assert_eq!(w.main("memories/MEMORY.md").as_deref(), Some(&b"Paul likes tomatoes. And basil."[..]));
        assert!(w.main("skills/pesto/SKILL.md").is_some());
        assert_eq!(w.repo.lock().unwrap().keys.len(), 1, "one commit");
        // the next round reads main's new commit of them and finds them the same
        assert_eq!(w.round().await.unwrap(), Done::default());

        // the owner edits SOUL.md while Hermes edits it too: main's wins, the profile's is kept
        w.repo.lock().unwrap().put("SOUL.md", "You are Juniper, a gardener.");
        w.write("SOUL.md", b"You are Juniper, a cook.");
        assert_eq!(w.round().await.unwrap().conflicts, 1);
        assert_eq!(w.here("SOUL.md").as_deref(), Some(&b"You are Juniper, a gardener."[..]));
        let kept: Vec<_> = std::fs::read_dir(&w.profile).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("SOUL.md.local-")).collect();
        assert_eq!(kept.len(), 1, "the profile's version beside it");
        // the kept copy is never pushed
        assert_eq!(w.round().await.unwrap(), Done::default());

        // deleted on main: deleted here (it was unchanged here)
        w.repo.lock().unwrap().files.remove("skills/pesto/SKILL.md");
        assert_eq!(w.round().await.unwrap().deleted, 1);
        assert!(w.here("skills/pesto/SKILL.md").is_none());
        // deleted here: deleted on main (it was unchanged there)
        std::fs::remove_file(w.profile.join("memories/MEMORY.md")).unwrap();
        assert_eq!(w.round().await.unwrap().pushed, 1);
        assert!(w.main("memories/MEMORY.md").is_none());
    }

    #[test]
    fn scan_reads_the_synced_files_and_leaves_the_rest() {
        let d = std::env::temp_dir().join(format!("hermes-boot-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let big = "x".repeat(FILE_MAX_BYTES + 1);
        for (p, body) in [("SOUL.md", "me"), ("memories/MEMORY.md", "m"), ("memories/big.md", &big), ("skills/mine/SKILL.md", "s"), ("skills/mine/__pycache__/a.pyc", "c"), ("skills/.git/HEAD", "h"), ("sessions/x.json", "{}")] {
            std::fs::create_dir_all(d.join(p).parent().unwrap()).unwrap();
            std::fs::write(d.join(p), body).unwrap();
        }
        std::os::unix::fs::symlink("MEMORY.md", d.join("memories/link.md")).unwrap();
        let s = scan(&d).unwrap();
        assert_eq!(s.files.iter().map(|l| l.path.as_str()).collect::<Vec<_>>(), ["SOUL.md", "memories/MEMORY.md", "skills/mine/SKILL.md"]);
        assert_eq!(s.unsynced.iter().map(String::as_str).collect::<Vec<_>>(), ["memories/big.md", "memories/link.md"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A pulled file and every directory made for it are Hermes'; what was
    /// there before (the profile, its `skills/`) is left as it was.
    #[test]
    fn a_pull_gives_hermes_each_directory_it_makes() {
        let profile = std::env::temp_dir().join(format!("hermes-boot-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::create_dir_all(profile.join("skills")).unwrap();
        let owned = std::sync::Mutex::new(Vec::new());
        let own = |p: &Path| owned.lock().unwrap().push(p.strip_prefix(&profile).unwrap().to_string_lossy().into_owned());
        write(&profile, &profile.join("skills/research/pesto/references/a.md"), b"a", &own).unwrap();
        assert_eq!(*owned.lock().unwrap(), ["skills/research", "skills/research/pesto", "skills/research/pesto/references", "skills/research/pesto/references/a.md"]);
        // replay: the directories are there now, only the file is written
        owned.lock().unwrap().clear();
        write(&profile, &profile.join("skills/research/pesto/references/a.md"), b"b", &own).unwrap();
        assert_eq!(*owned.lock().unwrap(), ["skills/research/pesto/references/a.md"]);
        let _ = std::fs::remove_dir_all(&profile);
    }

    /// A profile and its agent's repo behind the fake files route.
    struct World {
        repo: std::sync::Arc<std::sync::Mutex<fake::Repo>>,
        api: Api,
        agent: Agent,
        root: PathBuf,
        profile: PathBuf,
        _stop: tokio::sync::watch::Sender<bool>,
    }

    impl World {
        async fn new(name: &str, files: &[(&str, &str)]) -> World {
            fragment_bridge::log::set_quiet(true);
            let repo = std::sync::Arc::new(std::sync::Mutex::new(fake::Repo::default()));
            for (p, body) in files {
                repo.lock().unwrap().put(p, body);
            }
            let (addr, _stop) = fake::start(repo.clone()).await;
            let api = Api::new(&format!("http://{addr}")).unwrap();
            let agent = Agent { fragment: "juniper--k3x9".into(), identity: "npub1j".into(), name: "Juniper".into(), owner: "npub1paul".into(), credentials: vec![] };
            let root = std::env::temp_dir().join(format!("hermes-boot-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            // the boot makes the profile before its first round (main.rs, `write_profile`)
            let profile = root.join("profile");
            std::fs::create_dir_all(&profile).unwrap();
            World { repo, api, agent, root, profile, _stop }
        }
        async fn round(&self) -> Result<Done, SyncError> {
            round(&self.api, &self.agent, &self.profile, &|_: &Path| {}).await
        }
        fn main(&self, path: &str) -> Option<Vec<u8>> {
            self.repo.lock().unwrap().files.get(path).map(|(_, b)| b.clone())
        }
        fn here(&self, path: &str) -> Option<Vec<u8>> {
            std::fs::read(self.profile.join(path)).ok()
        }
        fn write(&self, path: &str, body: &[u8]) {
            std::fs::create_dir_all(self.profile.join(path).parent().unwrap()).unwrap();
            std::fs::write(self.profile.join(path), body).unwrap();
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Invalid: a file grown past `FILE_MAX_BYTES` (or made a link) on either
    /// side is left as it is: never deleted, never written over, on either side.
    #[tokio::test]
    async fn a_file_past_the_bound_is_left_alone_on_both_sides() {
        let w = World::new("unsynced", &[("memories/MEMORY.md", "Paul likes tomatoes."), ("skills/pesto/SKILL.md", "# Pesto"), ("SOUL.md", "You are Juniper.")]).await;
        w.round().await.unwrap();
        let big = vec![b'x'; FILE_MAX_BYTES + 1];
        // grown past the bound here: main keeps its copy
        w.write("memories/MEMORY.md", &big);
        // made a link here: main keeps its copy
        std::fs::remove_file(w.profile.join("SOUL.md")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", w.profile.join("SOUL.md")).unwrap();
        // grown past the bound on main: the profile keeps its copy
        w.repo.lock().unwrap().put("skills/pesto/SKILL.md", &"y".repeat(FILE_MAX_BYTES + 1));
        // new on both sides at once, past the bound on one: neither is written over
        w.write("memories/notes.md", &big);
        w.repo.lock().unwrap().put("memories/notes.md", "the owner's notes");
        w.repo.lock().unwrap().put("skills/bread/data.bin", &"z".repeat(FILE_MAX_BYTES + 1));
        w.write("skills/bread/data.bin", b"the agent's");
        for _ in 0..2 {
            w.round().await.unwrap();
            assert_eq!(w.main("memories/MEMORY.md").as_deref(), Some(&b"Paul likes tomatoes."[..]));
            assert_eq!(w.main("SOUL.md").as_deref(), Some(&b"You are Juniper."[..]));
            assert_eq!(w.here("skills/pesto/SKILL.md").as_deref(), Some(&b"# Pesto"[..]));
            assert_eq!(w.here("memories/notes.md").as_deref(), Some(&big[..]));
            assert_eq!(w.main("skills/bread/data.bin").map(|b| b.len()), Some(FILE_MAX_BYTES + 1));
        }
        // back within the bound: it syncs again
        w.write("memories/MEMORY.md", b"Paul likes tomatoes. And basil.");
        w.round().await.unwrap();
        assert_eq!(w.main("memories/MEMORY.md").as_deref(), Some(&b"Paul likes tomatoes. And basil."[..]));
    }

    /// Invalid: more than `FILES_MAX` files on either side refuse the round;
    /// nothing is deleted on either side for the ones past it.
    #[tokio::test]
    async fn more_files_than_the_bound_refuse_the_round() {
        let names: Vec<String> = (0..FILES_MAX).map(|i| format!("memories/m{i:03}.md")).collect();
        let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "m")).collect();
        let w = World::new("bound", &files).await;
        w.round().await.unwrap();
        // one more on main, sorted first
        w.repo.lock().unwrap().put("memories/a.md", "a");
        let got = w.round().await;
        assert!(names.iter().all(|n| w.here(n).is_some()), "nothing deleted here");
        assert!(matches!(got, Err(SyncError::TooMany("main"))), "refused: {got:?}");
        w.repo.lock().unwrap().files.remove("memories/a.md");
        w.round().await.unwrap();
        // one more here
        w.write("memories/z.md", b"z");
        let got = w.round().await;
        assert!(names.iter().all(|n| w.main(n).is_some()), "nothing deleted on main");
        assert!(matches!(got, Err(SyncError::TooMany("the profile"))), "refused: {got:?}");
    }

    /// Invalid: a directory the scan cannot read fails the round; its files
    /// are not read as deleted.
    #[tokio::test]
    async fn an_unreadable_directory_fails_the_round() {
        use std::os::unix::fs::PermissionsExt;
        let w = World::new("unreadable", &[("skills/pesto/SKILL.md", "# Pesto")]).await;
        w.round().await.unwrap();
        let dir = w.profile.join("skills/pesto");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&dir).is_ok(); // root reads it anyway
        let got = w.round().await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !readable {
            assert!(matches!(got, Err(SyncError::Profile(_))), "fails: {got:?}");
        }
        assert!(w.main("skills/pesto/SKILL.md").is_some(), "not deleted on main");
    }

    /// Restart: a profile retired (moved aside) and made again for the same
    /// agent starts from nothing: main's files come back, none is deleted.
    #[tokio::test]
    async fn a_fresh_profile_deletes_nothing() {
        let w = World::new("fresh", &[("SOUL.md", "You are Juniper."), ("memories/MEMORY.md", "Paul likes tomatoes.")]).await;
        w.round().await.unwrap();
        std::fs::rename(&w.profile, w.root.join("retired")).unwrap();
        std::fs::create_dir_all(&w.profile).unwrap();
        for _ in 0..2 {
            w.round().await.unwrap();
            assert_eq!(w.main("SOUL.md").as_deref(), Some(&b"You are Juniper."[..]));
            assert_eq!(w.main("memories/MEMORY.md").as_deref(), Some(&b"Paul likes tomatoes."[..]));
        }
        assert_eq!(w.here("memories/MEMORY.md").as_deref(), Some(&b"Paul likes tomatoes."[..]));
    }

    /// Valid: a skill of the agent's named as one of Hermes' own is the
    /// agent's: fetched, and never deleted from main.
    #[tokio::test]
    async fn a_skill_named_as_a_bundled_one_is_the_agents() {
        let w = World::new("bundled", &[("skills/arxiv/SKILL.md", "# Mine")]).await;
        w.write("skills/.bundled_manifest", b"arxiv:abc\n");
        for _ in 0..2 {
            w.round().await.unwrap();
        }
        assert_eq!(w.main("skills/arxiv/SKILL.md").as_deref(), Some(&b"# Mine"[..]));
    }
}
