//! The managed skills (decision 17) for every profile. They are the
//! owner's skills fragment's `skills/`: the blessed `skills` template's
//! release, beneath any file of the fragment's own at the same path
//! (docs/computers.md). The boot installs them read-only at `MANAGED_DIR`,
//! which every profile names in `skills.external_dirs` (hermes.rs). Hermes
//! scans a profile's own `skills/` (its agent fragment's, synced: sync.rs)
//! before its external dirs, and the first skill of a name wins, so an
//! agent's own skill wins over a managed one of the same name.
//!
//! The owner's skills fragment is found as the computer's first agent
//! acting for its owner (`for`): of the owner's fragments, the one of kind
//! `skills` under the owner's username. None means no managed skills, and
//! what was installed goes: the skills a person's settings list are their
//! agents' (the shell reads the same fragment).
//!
//! The plan is a pure function (`pick`, `plan`); `install` carries it out.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use fragment_bridge::api::{Api, ApiError, FileEntry, FragmentEntry};
use fragment_bridge::ev;
use fragment_bridge::runtime::Agent;

/// Where the managed skills are installed: under `/data`, so a wake finds
/// the last install and fetches only what changed since.
pub const MANAGED_DIR: &str = "/data/hermes/managed-skills";
/// What the installed files were, by path under `MANAGED_DIR`: the
/// listing's version of each (`lastCommitSha`).
pub const MANIFEST: &str = "/data/hermes-sync/managed-skills.json";
/// The managed set's files in the skills fragment.
pub const PREFIX: &str = "skills/";
/// Bounds on what one install takes (the platform's own are below these:
/// crates/templates `blessed::DATA_*`): files, each file, and all of them.
pub const FILES_MAX: usize = 1_000;
pub const FILE_MAX_BYTES: usize = 256 * 1024;
pub const TOTAL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Files fetched at once.
pub const FETCH_AT_ONCE: usize = 8;
/// A path under `MANAGED_DIR` is at most this long, and this deep.
const PATH_MAX: usize = 300;
const DEPTH_MAX: usize = 8;

/// The owner's skills fragment among the fragments the agent reaches acting
/// for them: kind `skills`, named under the owner's username (an agent
/// fragment is its owner's, so its name's username is theirs: a skills
/// fragment someone else shared with them is not theirs to install).
/// `skills.<username>` first, else the first by name.
pub fn pick(fragments: &[FragmentEntry], agent_fragment: &str) -> Option<String> {
    let (_, username) = agent_fragment.split_once('.')?;
    let mut theirs: Vec<&str> = fragments.iter().filter(|f| f.kind == "skills" && f.name.split_once('.').is_some_and(|(_, u)| u == username)).map(|f| f.name.as_str()).collect();
    theirs.sort_unstable();
    let preferred = format!("skills.{username}");
    theirs.iter().find(|n| **n == preferred).or(theirs.first()).map(|n| n.to_string())
}

/// Installed files: path under `MANAGED_DIR` → the version installed.
pub type Installed = BTreeMap<String, String>;

/// One install's steps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// (path under `MANAGED_DIR`, its path in the fragment, its version)
    pub fetch: Vec<(String, String, String)>,
    /// paths under `MANAGED_DIR` to delete
    pub remove: Vec<String>,
    /// what is installed once it is done
    pub wanted: Installed,
    /// files of the listing left out, and why
    pub refused: Vec<(String, &'static str)>,
}

/// Whether `rel` is a path a managed file may have under `MANAGED_DIR`.
pub fn valid_rel(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    !rel.is_empty()
        && rel.len() <= PATH_MAX
        && parts.len() <= DEPTH_MAX
        && parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.starts_with('.') && *p != "__pycache__")
        && rel.bytes().all(|b| b.is_ascii_graphic() && b != b'\\')
}

/// The steps from what is installed to the listing's managed set: fetch
/// what is new or changed, remove what the listing no longer has. A file
/// past a bound or at a path that is no safe path is refused, never
/// fetched; the rest of the set still installs.
pub fn plan(listing: Option<&[FileEntry]>, installed: &Installed) -> Plan {
    let mut out = Plan::default();
    let mut total = 0usize;
    for f in listing.unwrap_or_default() {
        let Some(rel) = f.path.strip_prefix(PREFIX) else { continue };
        let refused = if !valid_rel(rel) {
            Some("not a safe path")
        } else if f.size as usize > FILE_MAX_BYTES {
            Some("larger than a managed file may be")
        } else if out.wanted.len() >= FILES_MAX {
            Some("past the managed set's files")
        } else if total + f.size as usize > TOTAL_MAX_BYTES {
            Some("past the managed set's bytes")
        } else {
            None
        };
        if let Some(why) = refused {
            out.refused.push((f.path.clone(), why));
            continue;
        }
        total += f.size as usize;
        if installed.get(rel) != Some(&f.last_commit_sha) || f.last_commit_sha.is_empty() {
            out.fetch.push((rel.to_string(), f.path.clone(), f.last_commit_sha.clone()));
        }
        out.wanted.insert(rel.to_string(), f.last_commit_sha.clone());
    }
    out.remove = installed.keys().filter(|p| !out.wanted.contains_key(*p)).cloned().collect();
    out
}

/// Which installed paths are a skill's `SKILL.md`: what the profiles see.
pub fn skill_names(installed: &Installed) -> BTreeSet<String> {
    installed.keys().filter_map(|p| p.strip_suffix("/SKILL.md")).filter_map(|d| d.rsplit('/').next()).map(str::to_string).collect()
}

pub fn load_manifest(path: &Path) -> Installed {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_manifest(path: &Path, m: &Installed) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m).expect("a manifest serializes"))?;
    std::fs::rename(&tmp, path)
}

/// Writes `bytes` at `dir/rel` whole (a temporary file renamed over it), so
/// Hermes never reads half a skill. Owned by the boot (root), readable by
/// all: the managed set is read-only to the agents.
fn write_file(dir: &Path, rel: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let target = dir.join(rel);
    let parent = target.parent().expect("a managed file has a directory");
    std::fs::create_dir_all(parent)?;
    let tmp = target.with_file_name(format!(".{}.fragment-tmp", target.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    std::fs::write(&tmp, bytes)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, &target)
}

/// Removes `dir/rel`, then each directory it leaves empty up to `dir`.
fn remove_file(dir: &Path, rel: &str) {
    let target = dir.join(rel);
    let _ = std::fs::remove_file(&target);
    let mut parent = target.parent().map(Path::to_path_buf);
    // bounded by the path's depth
    while let Some(p) = parent {
        if p == dir || std::fs::remove_dir(&p).is_err() {
            break;
        }
        parent = p.parent().map(Path::to_path_buf);
    }
}

/// What one install did, for the log.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub fragment: Option<String>,
    pub fetched: u32,
    pub removed: u32,
    pub refused: u32,
    pub skills: u32,
}

/// Why an install stopped: the platform did not answer as it should, or a
/// file could not be written. Either way what is installed stays whole.
#[derive(Debug)]
pub enum InstallError {
    Api(ApiError),
    Disk(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Api(e) => write!(f, "{e}"),
            InstallError::Disk(e) => write!(f, "writing the managed skills: {e}"),
        }
    }
}

impl From<ApiError> for InstallError {
    fn from(e: ApiError) -> InstallError {
        InstallError::Api(e)
    }
}

/// One install of the owner's managed skills into `dir`, read as `reader`
/// acting for `owner`. An answer the platform did not give (a transport
/// error, a refusal) changes nothing installed: the last install stays.
pub async fn install(api: &Api, reader: &Agent, owner: &str, dir: &Path, manifest: &Path) -> Result<Done, InstallError> {
    // what the manifest says is installed and is still on disk (a file gone
    // from under it is fetched again)
    let mut installed: Installed = load_manifest(manifest).into_iter().filter(|(rel, _)| valid_rel(rel) && dir.join(rel).is_file()).collect();
    let fragments = api.fragments_for(&reader.fragment, owner).await?;
    let fragment = pick(&fragments, &reader.fragment);
    let listing = match &fragment {
        Some(f) => Some(api.files_for(&reader.fragment, f, owner).await?),
        None => None,
    };
    let steps = plan(listing.as_deref(), &installed);
    let mut done = Done { fragment: fragment.clone(), refused: steps.refused.len() as u32, ..Done::default() };
    for (path, why) in &steps.refused {
        ev!("skills.refused", { "path": path, "why": why });
    }
    std::fs::create_dir_all(dir).map_err(|e| InstallError::Disk(format!("{}: {e}", dir.display())))?;
    if let Some(f) = &fragment {
        for batch in steps.fetch.chunks(FETCH_AT_ONCE) {
            let reads = batch.iter().map(|(_, path, _)| api.file_for(&reader.fragment, f, owner, path, FILE_MAX_BYTES));
            let got = futures_util::future::join_all(reads).await;
            for ((rel, _, version), bytes) in batch.iter().zip(got) {
                let bytes = bytes?;
                write_file(dir, rel, &bytes).map_err(|e| InstallError::Disk(format!("{rel}: {e}")))?;
                installed.insert(rel.clone(), version.clone());
                done.fetched += 1;
            }
            // saved as it goes: a cut install fetches only what it lacks next time
            if let Err(e) = save_manifest(manifest, &installed) {
                ev!("skills.unsaved", { "error": e.to_string() });
            }
        }
    }
    for rel in &steps.remove {
        remove_file(dir, rel);
        installed.remove(rel);
        done.removed += 1;
    }
    assert_eq!(installed, steps.wanted, "an install that answered whole installs exactly the listing's set");
    if let Err(e) = save_manifest(manifest, &installed) {
        ev!("skills.unsaved", { "error": e.to_string() });
    }
    done.skills = skill_names(&installed).len() as u32;
    Ok(done)
}

/// The agent the boot reads the managed set as: the computer's first agent
/// whose owner is the computer's (one computer per person: decision 13).
pub fn reader<'a>(agents: &'a [Agent], owner: &str) -> Option<&'a Agent> {
    agents.iter().find(|a| a.owner == owner)
}

/// The managed directory every profile names (hermes.rs).
pub fn managed_dir() -> PathBuf {
    PathBuf::from(MANAGED_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(name: &str, kind: &str) -> FragmentEntry {
        FragmentEntry { name: name.into(), role: "editor".into(), kind: kind.into() }
    }

    fn file(path: &str, size: u64, version: &str) -> FileEntry {
        FileEntry { path: path.into(), size, last_commit_sha: version.into() }
    }

    /// Valid: the owner's skills fragment, `skills.<user>` first. Invalid:
    /// someone else's shared with them, a fragment of another kind.
    #[test]
    fn the_owners_skills_fragment_is_found_by_kind_and_username() {
        let list = [frag("garden.paul", "app"), frag("skills.skyler", "skills"), frag("extra.paul", "skills"), frag("skills.paul", "skills")];
        assert_eq!(pick(&list, "juniper.paul").as_deref(), Some("skills.paul"));
        assert_eq!(pick(&list[..3], "juniper.paul").as_deref(), Some("extra.paul"), "another label of theirs");
        assert_eq!(pick(&list[..2], "juniper.paul"), None, "skyler's is not paul's");
        assert_eq!(pick(&[], "juniper.paul"), None);
        assert_eq!(pick(&list, "nodot"), None, "an agent fragment names its username");
    }

    #[test]
    fn only_safe_paths_are_installed() {
        for ok in ["research/arxiv-finite/SKILL.md", "grill-me/SKILL.md", "a/b/c/d.py"] {
            assert!(valid_rel(ok), "{ok}");
        }
        for bad in ["", "../x", "a/../b", "/etc/passwd", "a//b", "a/.hidden", "a/__pycache__/x.pyc", "a/b c", "a\\b", &"a/".repeat(9), &"x".repeat(301)] {
            assert!(!valid_rel(bad), "{bad}");
        }
    }

    /// Valid: a first install fetches the managed set (and nothing else of
    /// the fragment's); a change fetches what changed and removes what
    /// went. Replay: a settled install does nothing. Invalid: a file past a
    /// bound, or at an unsafe path, is refused and the rest installs.
    #[test]
    fn a_plan_fetches_what_changed_and_removes_what_went() {
        let listing = vec![
            file("fragment.json", 40, "c1"),
            file("skills/research/arxiv-finite/SKILL.md", 100, "release:a"),
            file("skills/research/arxiv-finite/scripts/x.py", 200, "release:b"),
            file("skills/grill-me/SKILL.md", 50, "c2"),
        ];
        let first = plan(Some(&listing), &Installed::new());
        assert_eq!(first.fetch.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["research/arxiv-finite/SKILL.md", "research/arxiv-finite/scripts/x.py", "grill-me/SKILL.md"]);
        assert!(first.remove.is_empty() && first.refused.is_empty());
        assert_eq!(skill_names(&first.wanted), ["arxiv-finite", "grill-me"].map(String::from).into_iter().collect());
        // replay: what is installed is the listing
        let settled = plan(Some(&listing), &first.wanted);
        assert!(settled.fetch.is_empty() && settled.remove.is_empty(), "{settled:?}");
        // a release changed one file and dropped another
        let next = vec![file("skills/research/arxiv-finite/SKILL.md", 120, "release:a2"), file("skills/grill-me/SKILL.md", 50, "c2")];
        let changed = plan(Some(&next), &first.wanted);
        assert_eq!(changed.fetch, vec![("research/arxiv-finite/SKILL.md".to_string(), "skills/research/arxiv-finite/SKILL.md".to_string(), "release:a2".to_string())]);
        assert_eq!(changed.remove, vec!["research/arxiv-finite/scripts/x.py".to_string()]);
        // no skills fragment: everything goes
        let none = plan(None, &first.wanted);
        assert!(none.fetch.is_empty() && none.wanted.is_empty() && none.remove.len() == 3);
        // past a bound, or unsafe: refused, the rest installs
        let bad = vec![file("skills/x/../../etc/passwd", 1, "v"), file("skills/big/SKILL.md", FILE_MAX_BYTES as u64 + 1, "v"), file("skills/ok/SKILL.md", 1, "v")];
        let p = plan(Some(&bad), &Installed::new());
        assert_eq!(p.refused.len(), 2);
        assert_eq!(p.wanted.keys().collect::<Vec<_>>(), ["ok/SKILL.md"]);
        let many: Vec<FileEntry> = (0..FILES_MAX + 5).map(|i| file(&format!("skills/s{i}/SKILL.md"), 1, "v")).collect();
        let p = plan(Some(&many), &Installed::new());
        assert_eq!((p.wanted.len(), p.refused.len()), (FILES_MAX, 5));
    }

    #[test]
    fn the_reader_is_an_agent_of_the_computers_owner() {
        let a = |f: &str, o: &str| Agent { fragment: f.into(), identity: format!("id:{f}"), name: f.into(), owner: o.into() };
        let agents = [a("x.skyler", "id:skyler"), a("juniper.paul", "id:paul")];
        assert_eq!(reader(&agents, "id:paul").map(|r| r.fragment.as_str()), Some("juniper.paul"));
        assert!(reader(&agents[..1], "id:paul").is_none());
    }

    /// Goal: an install against the files route writes the managed set, a
    /// change of it is followed, and what went is removed; a platform that
    /// does not answer changes nothing installed. Method: a fake API with
    /// an owner's skills fragment; the install runs as the agent for its owner.
    #[tokio::test]
    async fn an_install_follows_the_skills_fragment() {
        use std::sync::{Arc, Mutex};
        fragment_bridge::log::set_quiet(true);
        let files: fake::Files = Arc::new(Mutex::new(BTreeMap::new()));
        files.lock().unwrap().insert("skills/research/arxiv-finite/SKILL.md".into(), ("release:a".into(), b"---\nname: arxiv-finite\n---\n".to_vec()));
        files.lock().unwrap().insert("skills/grill-me/SKILL.md".into(), ("release:g".into(), b"---\nname: grill-me\n---\n".to_vec()));
        files.lock().unwrap().insert("fragment.json".into(), ("c1".into(), b"{}".to_vec()));
        let down = Arc::new(Mutex::new(false));
        let (addr, _stop) = fake::start(files.clone(), down.clone()).await;
        let api = Api::new(&format!("http://{addr}")).unwrap();
        let agent = Agent { fragment: "juniper.paul".into(), identity: "id:j".into(), name: "Juniper".into(), owner: "id:paul".into() };
        let root = std::env::temp_dir().join(format!("hermes-boot-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, manifest) = (root.join("managed"), root.join("sync/managed.json"));

        let d = install(&api, &agent, "id:paul", &dir, &manifest).await.unwrap();
        assert_eq!((d.fragment.as_deref(), d.fetched, d.skills), (Some("skills.paul"), 2, 2));
        assert!(dir.join("research/arxiv-finite/SKILL.md").exists() && !dir.join("fragment.json").exists());
        let replay = install(&api, &agent, "id:paul", &dir, &manifest).await.unwrap();
        assert_eq!((replay.fetched, replay.removed), (0, 0), "settled");
        // a file gone from under the manifest is fetched again
        std::fs::remove_file(dir.join("grill-me/SKILL.md")).unwrap();
        let repaired = install(&api, &agent, "id:paul", &dir, &manifest).await.unwrap();
        assert_eq!(repaired.fetched, 1);
        assert!(dir.join("grill-me/SKILL.md").exists());

        // the release changes one skill and drops the other
        files.lock().unwrap().insert("skills/research/arxiv-finite/SKILL.md".into(), ("release:a2".into(), b"---\nname: arxiv-finite\n---\nnew\n".to_vec()));
        files.lock().unwrap().remove("skills/grill-me/SKILL.md");
        let d = install(&api, &agent, "id:paul", &dir, &manifest).await.unwrap();
        assert_eq!((d.fetched, d.removed, d.skills), (1, 1, 1));
        assert!(std::fs::read_to_string(dir.join("research/arxiv-finite/SKILL.md")).unwrap().ends_with("new\n"));
        assert!(!dir.join("grill-me").exists(), "its directory goes with its last file");

        // the platform down: nothing installed changes
        *down.lock().unwrap() = true;
        assert!(install(&api, &agent, "id:paul", &dir, &manifest).await.is_err());
        assert!(dir.join("research/arxiv-finite/SKILL.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The fragment list and a skills fragment's files, as the platform
    /// answers an agent acting for its owner (`for`).
    mod fake {
        use std::collections::BTreeMap;
        use std::net::SocketAddr;
        use std::sync::{Arc, Mutex};

        use fragment_bridge::net;
        use hyper::{Method, StatusCode};
        use serde_json::{json, Value};

        pub type Files = Arc<Mutex<BTreeMap<String, (String, Vec<u8>)>>>;

        pub async fn start(files: Files, down: Arc<Mutex<bool>>) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (stop, rx) = tokio::sync::watch::channel(false);
            let handler = move |req: hyper::Request<hyper::body::Incoming>, _: SocketAddr| {
                let (files, down) = (files.clone(), down.clone());
                async move {
                    if *down.lock().unwrap() {
                        return net::refusal(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "down");
                    }
                    assert_eq!(req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()), Some("juniper.paul"), "as the agent");
                    let q = req.uri().query().unwrap_or("").to_string();
                    assert!(q.split('&').any(|kv| kv == "for=id%3Apaul"), "acting for its owner: {q}");
                    match (req.method().clone(), req.uri().path()) {
                        (Method::GET, "/api/fragments") => net::json_answer(StatusCode::OK, &json!({ "fragments": [{ "name": "skills.paul", "role": "editor", "kind": "skills" }, { "name": "garden.paul", "role": "editor", "kind": "app" }] })),
                        (Method::GET, "/api/f/skills.paul/files") => {
                            let list: Vec<Value> = files.lock().unwrap().iter().map(|(p, (v, b))| json!({ "path": p, "size": b.len(), "lastCommitSha": v })).collect();
                            net::json_answer(StatusCode::OK, &json!({ "files": list }))
                        }
                        (Method::GET, "/api/f/skills.paul/file") => {
                            let path = q.split('&').find_map(|kv| kv.strip_prefix("path=")).unwrap_or("").replace("%2F", "/");
                            match files.lock().unwrap().get(&path) {
                                Some((_, b)) => net::respond(StatusCode::OK, "text/markdown", b.clone()),
                                None => net::refusal(StatusCode::NOT_FOUND, "not_found", "no such file"),
                            }
                        }
                        (_, path) => net::refusal(StatusCode::NOT_FOUND, "not_found", path),
                    }
                }
            };
            tokio::spawn(net::serve(listener, handler, rx));
            (addr, stop)
        }
    }
}
