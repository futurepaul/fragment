//! An agent's repo in its Hermes profile (decision 15): the agent
//! fragment's `SOUL.md`, `memories/` and `skills/` checked out into the
//! profile, and what Hermes changes there committed back through the files
//! route, as the agent. Three ways: main, the profile, and what both agreed
//! on at the last sync (`/data/hermes-sync/<agent>.json`), so neither side's
//! change is lost to the other's silence. When both changed a file, main
//! wins (its owner edited it) and the profile's copy is kept beside it as
//! `<file>.local-<ms>`, never pushed.
//!
//! The plan is a pure function (`plan`, `resolve`); `round` carries it out.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use fragment_bridge::api::{Api, ApiError, FileChange};
use fragment_bridge::ev;
use fragment_bridge::records::hex;
use fragment_bridge::runtime::Agent;

/// A synced file is at most this large (the files route commits at most
/// 256 KiB at once).
pub const FILE_MAX_BYTES: usize = 192 * 1024;
/// Files one agent's sync looks at, at most, on each side.
pub const FILES_MAX: usize = 500;
/// One commit carries at most this many files and bytes (the route's 16
/// files and 256 KiB, with room for base64).
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

/// The sync's steps, given main, what was agreed, and the profile.
pub fn plan(main: &[MainFile], known: &Manifest, local: &[Local]) -> Vec<Action> {
    let main: BTreeMap<&str, &MainFile> = main.iter().map(|m| (m.path.as_str(), m)).collect();
    let local: BTreeMap<&str, &Local> = local.iter().map(|l| (l.path.as_str(), l)).collect();
    let paths: BTreeSet<&str> = main.keys().copied().chain(local.keys().copied()).chain(known.keys().map(String::as_str)).collect();
    let mut out = Vec::new();
    for p in paths {
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

/// The names of the skills Hermes copied in from its own bundle (its
/// `skills/.bundled_manifest`, `name:hash` lines): not the agent's to keep.
pub fn bundled(profile: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(profile.join("skills/.bundled_manifest")) else { return BTreeSet::new() };
    text.lines().filter_map(|l| l.split(':').next()).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect()
}

/// The profile's synced files and their hashes (bundled skills left out).
pub fn scan(profile: &Path) -> Vec<Local> {
    let skip = bundled(profile);
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![profile.join("SOUL.md"), profile.join("memories"), profile.join("skills")];
    // bounded by FILES_MAX files and the profile's tree
    while let Some(p) = stack.pop() {
        if out.len() >= FILES_MAX {
            ev!("sync.bounded", { "profile": profile.display().to_string(), "max": FILES_MAX });
            break;
        }
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        let Ok(rel) = p.strip_prefix(profile) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if meta.is_dir() {
            let bundled_skill = rel.starts_with("skills/") && rel.split('/').any(|c| skip.contains(c));
            if bundled_skill {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.filter_map(Result::ok).map(|e| e.path()));
            }
            continue;
        }
        if !meta.is_file() || !synced(&rel) || meta.len() as usize > FILE_MAX_BYTES {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&p) {
            out.push(Local { path: rel, hash: hash(&bytes) });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn manifest_path(state: &Path, agent: &Agent) -> PathBuf {
    state.join(format!("{}.json", agent.fragment))
}

pub fn load_manifest(state: &Path, agent: &Agent) -> Manifest {
    std::fs::read(manifest_path(state, agent)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_manifest(state: &Path, agent: &Agent, m: &Manifest) -> std::io::Result<()> {
    std::fs::create_dir_all(state)?;
    let tmp = manifest_path(state, agent).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m).expect("a manifest serializes"))?;
    std::fs::rename(&tmp, manifest_path(state, agent))
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
pub async fn round(api: &Api, agent: &Agent, profile: &Path, state: &Path, own: &(dyn Fn(&Path) + Sync)) -> Result<Done, ApiError> {
    let mut known = load_manifest(state, agent);
    let main: Vec<MainFile> = api.files(&agent.fragment, &agent.fragment).await?.into_iter().filter(|f| synced(&f.path) && f.size as usize <= FILE_MAX_BYTES).take(FILES_MAX).map(|f| MainFile { path: f.path, commit: f.last_commit_sha }).collect();
    let main_commit: BTreeMap<String, String> = main.iter().map(|m| (m.path.clone(), m.commit.clone())).collect();
    let local = scan(profile);
    let local_by: BTreeMap<String, Local> = local.iter().map(|l| (l.path.clone(), l.clone())).collect();
    let mut done = Done::default();
    let mut push: Vec<FileChange> = Vec::new();
    let mut pushed: Vec<(String, Option<String>)> = Vec::new();
    for action in plan(&main, &known, &local) {
        match action {
            Action::Fetch(p) => {
                let bytes = api.file(&agent.fragment, &agent.fragment, &p, FILE_MAX_BYTES).await?;
                let h = hash(&bytes);
                let target = profile.join(&p);
                match resolve(known.get(&p), local_by.get(&p), &h) {
                    Resolution::Same => {}
                    Resolution::Pull => {
                        write(&target, &bytes, own);
                        done.pulled += 1;
                    }
                    Resolution::Conflict => {
                        let keep = target.with_file_name(format!("{}.local-{}", target.file_name().and_then(|n| n.to_str()).unwrap_or("file"), fragment_bridge::log::now_ms()));
                        let _ = std::fs::rename(&target, &keep);
                        write(&target, &bytes, own);
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
                let _ = std::fs::remove_file(profile.join(&p));
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
    if let Err(e) = save_manifest(state, agent, &known) {
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

fn write(target: &Path, bytes: &[u8], own: &(dyn Fn(&Path) + Sync)) {
    if let Some(dir) = target.parent() {
        let _ = std::fs::create_dir_all(dir);
        own(dir);
    }
    if std::fs::write(target, bytes).is_ok() {
        own(target);
    }
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
        let got = plan(&main, &known, &local);
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
    }

    #[test]
    fn replay_a_settled_sync_does_nothing() {
        let known: Manifest = [("SOUL.md".into(), k("c1", "h1"))].into_iter().collect();
        assert!(plan(&[m("SOUL.md", "c1")], &known, &[l("SOUL.md", "h1")]).is_empty());
        assert!(plan(&[], &Manifest::new(), &[]).is_empty());
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
                    assert_eq!(req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()), Some("juniper.paul"), "as the agent");
                    let (method, path, q) = (req.method().clone(), req.uri().path().to_string(), req.uri().query().unwrap_or("").to_string());
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    let mut r = repo.lock().unwrap();
                    match (method, path.as_str()) {
                        (Method::GET, "/api/f/juniper.paul/files") => {
                            let list: Vec<Value> = r.files.iter().map(|(p, (c, b))| json!({ "path": p, "size": b.len(), "lastCommitSha": c })).collect();
                            net::json_answer(StatusCode::OK, &json!({ "files": list }))
                        }
                        (Method::GET, "/api/f/juniper.paul/file") => {
                            let p = q.strip_prefix("path=").unwrap_or("").replace("%2F", "/");
                            match r.files.get(&p) {
                                Some((_, b)) => net::respond(StatusCode::OK, "application/octet-stream", b.clone()),
                                None => net::refusal(StatusCode::NOT_FOUND, "not_found", "no such file"),
                            }
                        }
                        (Method::POST, "/api/f/juniper.paul/files") => {
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
        fragment_bridge::log::set_quiet(true);
        let repo = std::sync::Arc::new(std::sync::Mutex::new(fake::Repo::default()));
        {
            let mut r = repo.lock().unwrap();
            r.put("SOUL.md", "You are Juniper.");
            r.put("memories/MEMORY.md", "Paul likes tomatoes.");
            r.put("site/index.html", "not the agent's to keep");
        }
        let (addr, _stop) = fake::start(repo.clone()).await;
        let api = Api::new(&format!("http://{addr}")).unwrap();
        let agent = Agent { fragment: "juniper.paul".into(), identity: "id:j".into(), name: "Juniper".into(), owner: "id:paul".into(), credentials: vec![] };
        let root = std::env::temp_dir().join(format!("hermes-boot-round-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (profile, state) = (root.join("profile"), root.join("sync"));
        let own = |_: &Path| {};

        let d = round(&api, &agent, &profile, &state, &own).await.unwrap();
        assert_eq!((d.pulled, d.pushed), (2, 0));
        assert_eq!(std::fs::read_to_string(profile.join("SOUL.md")).unwrap(), "You are Juniper.");
        assert!(!profile.join("site").exists(), "only the agent's own files");
        assert_eq!(round(&api, &agent, &profile, &state, &own).await.unwrap(), Done::default(), "replay: settled");

        // Hermes writes a memory and a skill: committed back, once
        std::fs::write(profile.join("memories/MEMORY.md"), "Paul likes tomatoes. And basil.").unwrap();
        std::fs::create_dir_all(profile.join("skills/pesto")).unwrap();
        std::fs::write(profile.join("skills/pesto/SKILL.md"), "# Pesto").unwrap();
        let d = round(&api, &agent, &profile, &state, &own).await.unwrap();
        assert_eq!(d.pushed, 2);
        {
            let r = repo.lock().unwrap();
            assert_eq!(r.files["memories/MEMORY.md"].1, b"Paul likes tomatoes. And basil.");
            assert!(r.files.contains_key("skills/pesto/SKILL.md"));
            assert_eq!(r.keys.len(), 1, "one commit");
        }
        // the next round reads main's new commit of them and finds them the same
        assert_eq!(round(&api, &agent, &profile, &state, &own).await.unwrap(), Done::default());

        // the owner edits SOUL.md while Hermes edits it too: main's wins, the profile's is kept
        repo.lock().unwrap().put("SOUL.md", "You are Juniper, a gardener.");
        std::fs::write(profile.join("SOUL.md"), "You are Juniper, a cook.").unwrap();
        let d = round(&api, &agent, &profile, &state, &own).await.unwrap();
        assert_eq!(d.conflicts, 1);
        assert_eq!(std::fs::read_to_string(profile.join("SOUL.md")).unwrap(), "You are Juniper, a gardener.");
        let kept: Vec<_> = std::fs::read_dir(&profile).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("SOUL.md.local-")).collect();
        assert_eq!(kept.len(), 1, "the profile's version beside it");
        // the kept copy is never pushed
        assert_eq!(round(&api, &agent, &profile, &state, &own).await.unwrap(), Done::default());

        // deleted on main: deleted here (it was unchanged here)
        repo.lock().unwrap().files.remove("skills/pesto/SKILL.md");
        let d = round(&api, &agent, &profile, &state, &own).await.unwrap();
        assert_eq!(d.deleted, 1);
        assert!(!profile.join("skills/pesto/SKILL.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_leaves_out_bundled_skills() {
        let d = std::env::temp_dir().join(format!("hermes-boot-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (p, body) in [("SOUL.md", "me"), ("memories/MEMORY.md", "m"), ("skills/mine/SKILL.md", "s"), ("skills/research/arxiv/SKILL.md", "b"), ("skills/.bundled_manifest", "arxiv:abc\n"), ("sessions/x.json", "{}")] {
            std::fs::create_dir_all(d.join(p).parent().unwrap()).unwrap();
            std::fs::write(d.join(p), body).unwrap();
        }
        let paths: Vec<String> = scan(&d).into_iter().map(|l| l.path).collect();
        assert_eq!(paths, vec!["SOUL.md", "memories/MEMORY.md", "skills/mine/SKILL.md"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
