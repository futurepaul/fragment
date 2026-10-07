//! The skills every profile finds beyond its own (docs/computers.md):
//!
//! - The managed skills (decision 17): the owner's skills fragment's
//!   `skills/`, the blessed `skills` template's release beneath any file of
//!   the fragment's own at the same path. The boot installs them read-only
//!   at `MANAGED_DIR`. The owner's skills fragment is found as the
//!   computer's first agent acting for its owner (`for`): of the owner's
//!   fragments, the one of kind `skills` under the owner's username. None
//!   means no managed skills, and what was installed goes: the skills a
//!   person's settings list are their agents' (the shell reads the same
//!   fragment).
//! - The platform skill, `fragment`, unless a managed skill takes its name:
//!   the `fragment` CLI's own skill (`fragment skill`) after a page of what
//!   the computer adds (`computer.md`), written at the image's build into
//!   `PLATFORM_DIR` (`hermes-boot build-info`), so it is the binary's in the
//!   image, and shown to the profiles in `PLATFORM_VIEW` (`settle_platform`,
//!   at each start and after each install).
//!
//! Every profile names both in `skills.external_dirs` (hermes.rs). Hermes
//! ranks a profile's own `skills/` (its agent fragment's, synced: sync.rs)
//! above its external dirs, so an agent's own skill wins over a managed one
//! or the platform's. Its external dirs are one rank: two skills there of
//! one name are ambiguous, and Hermes finds neither by it (since its main
//! of 2026-10; v0.21.5 took the first dir's). So a managed `fragment` wins
//! over the platform skill by the platform skill leaving the view.
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
/// While the owner has no skills fragment, it is looked for this often
/// rather than every `SKILLS_EVERY_MS` (main.rs): a person who lacks one
/// adds it from the shell's settings, and their agents have it within
/// about this.
pub const ABSENT_EVERY_MS: u64 = 60_000;

/// Where the platform skill is: in the image, read-only to the agents.
pub const PLATFORM_DIR: &str = "/opt/fragment/skills";
/// Where the profiles find it: a copy of it while no managed skill takes
/// its name (`settle_platform`), the boot's, read-only to the agents. In
/// the boot's run directory: made at each start, never saved.
pub const PLATFORM_VIEW: &str = "/var/lib/fragment-run/platform-skills";
/// Its name, which a managed or an agent's own skill of the same name
/// shadows; and its path under `PLATFORM_DIR` (`<category>/<name>/`).
pub const PLATFORM_NAME: &str = "fragment";
pub const PLATFORM_PATH: &str = "platform/fragment/SKILL.md";
/// The external dirs every profile names: the managed set, and the
/// platform skill's view.
pub const EXTERNAL_DIRS: [&str; 2] = [MANAGED_DIR, PLATFORM_VIEW];
/// The page the computer adds to the CLI's skill.
const COMPUTER_PAGE: &str = include_str!("computer.md");
/// The platform skill's description, which Hermes lists in every turn's
/// prompt (its skills index): what makes an agent load it.
const PLATFORM_DESCRIPTION: &str = "You are an agent on a Fragment computer: what that is, and the `fragment` CLI in your terminal, which acts as you. Load it before you list, read, make, change, deploy or share your owner's fragments (apps, sites, brains, chats), when asked what you can do here, and for your owner's connections (Google: GOOGLE_OAUTH_ACCESS_TOKEN) or your desktop.";
/// The platform skill is a page or two, not a manual.
pub const PLATFORM_MAX_BYTES: usize = 16 * 1024;

/// The platform skill: `cli_skill` (what `fragment skill` prints, a skill
/// named `fragment`) with the computer's page before its body, and the
/// computer's description. Refused unless the CLI's skill is one named
/// `fragment`, and the whole is within `PLATFORM_MAX_BYTES`.
pub fn platform_skill(cli_skill: &str) -> Result<String, String> {
    let (front, body) = cli_skill.strip_prefix("---\n").and_then(|rest| rest.split_once("\n---\n")).ok_or("`fragment skill` printed no skill: no frontmatter")?;
    if !front.lines().any(|l| l.trim_end() == format!("name: {PLATFORM_NAME}")) {
        return Err(format!("`fragment skill` printed a skill not named {PLATFORM_NAME}: {front:?}"));
    }
    let description = serde_json::to_string(PLATFORM_DESCRIPTION).expect("a string serializes");
    let skill = format!("---\nname: {PLATFORM_NAME}\ndescription: {description}\n---\n\n{}\n\n{}\n", COMPUTER_PAGE.trim(), body.trim());
    if skill.len() > PLATFORM_MAX_BYTES {
        return Err(format!("the platform skill is {} bytes, past {PLATFORM_MAX_BYTES}", skill.len()));
    }
    Ok(skill)
}

/// Writes the platform skill whole under `dir` (the image's build: root's,
/// readable by all).
pub fn write_platform_skill(dir: &Path, skill: &str) -> std::io::Result<()> {
    write_file(dir, PLATFORM_PATH, skill.as_bytes())
}

/// When the next install of the managed skills is due after one that found
/// the owner's skills fragment (`Some(true)`), found none (`Some(false)`),
/// or did not answer (`None`): sooner while there is none.
pub fn next_install_ms(found: Option<bool>, every_ms: u64) -> u64 {
    match found {
        Some(false) => every_ms.min(ABSENT_EVERY_MS),
        Some(true) | None => every_ms,
    }
}

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

/// The name Hermes gives the skill whose `SKILL.md` reads `text`, in a
/// directory named `dir_name`: its frontmatter's `name` (quoted or not),
/// else the directory's.
pub fn skill_name(text: &str, dir_name: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n");
    let front = text.strip_prefix("---\n").and_then(|r| r.split_once("\n---").map(|(f, _)| f.to_string())).unwrap_or_default();
    let declared = front.lines().find_map(|l| l.strip_prefix("name:")).map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string()).filter(|v| !v.is_empty());
    declared.unwrap_or_else(|| dir_name.to_string())
}

/// Whether a managed skill takes the platform skill's name: a `SKILL.md`
/// installed under `dir` (as `installed` lists them) that Hermes names
/// `PLATFORM_NAME`.
pub fn shadows_platform(dir: &Path, installed: &Installed) -> bool {
    installed.keys().filter(|rel| *rel == "SKILL.md" || rel.ends_with("/SKILL.md")).any(|rel| {
        let path = dir.join(rel);
        let dir_name = path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // bounded: a managed file is at most FILE_MAX_BYTES
        std::fs::read_to_string(&path).is_ok_and(|text| skill_name(&text, &dir_name) == PLATFORM_NAME)
    })
}

/// Shows the platform skill to the profiles: in `view`, a copy of
/// `platform`'s (written whole when it differs) unless a managed skill
/// installed in `dir` (as `manifest` lists them) takes its name, when it
/// leaves the view. The view is there either way: Hermes skips an external
/// dir it does not find until the profile's config changes. Whether it is
/// shown.
pub fn settle_platform(dir: &Path, manifest: &Path, platform: &Path, view: &Path) -> std::io::Result<bool> {
    std::fs::create_dir_all(view)?;
    if shadows_platform(dir, &load_manifest(manifest)) {
        remove_file(view, PLATFORM_PATH);
        return Ok(false);
    }
    let skill = std::fs::read(platform.join(PLATFORM_PATH))?;
    if std::fs::read(view.join(PLATFORM_PATH)).ok().as_deref() != Some(&skill[..]) {
        write_file(view, PLATFORM_PATH, &skill)?;
    }
    Ok(true)
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
        let a = |f: &str, o: &str| Agent { fragment: f.into(), identity: format!("id:{f}"), name: f.into(), owner: o.into(), credentials: vec![] };
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
        let (addr, _stop) = fake::start(files.clone(), down.clone(), Arc::new(Mutex::new(true))).await;
        let api = Api::new(&format!("http://{addr}")).unwrap();
        let agent = Agent { fragment: "juniper.paul".into(), identity: "id:j".into(), name: "Juniper".into(), owner: "id:paul".into(), credentials: vec![] };
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

    /// Hermes' rule for a profile's skills (its `resolve_skill_catalog`,
    /// `agent/skill_utils.py`, since its main of 2026-10): the profile's own
    /// dir ranks above its external dirs, which are one rank. A skill is
    /// known by its name, its directory's name and its path under its dir;
    /// the best rank holding a name wins it, and two skills of that rank
    /// holding it (not copies of one) are ambiguous: neither is found by it.
    /// Each name found → the file that is its skill.
    fn hermes_finds(own: &Path, external: &[PathBuf]) -> BTreeMap<String, PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.filter_map(Result::ok) {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().is_some_and(|n| n == "SKILL.md") {
                    out.push(p);
                }
            }
        }
        // (rank, name, aliases, file)
        let mut skills = vec![];
        for (rank, dir) in std::iter::once((0, own)).chain(external.iter().map(|d| (1, d.as_path()))) {
            let mut files = vec![];
            walk(dir, &mut files);
            for f in files {
                let dir_name = f.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
                let name = skill_name(&std::fs::read_to_string(&f).unwrap(), &dir_name);
                let rel = f.parent().unwrap().strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                skills.push((rank, name.clone(), BTreeSet::from([name, dir_name, rel]), f));
            }
        }
        let mut found = BTreeMap::new();
        for (rank, name, _, file) in &skills {
            let holders: Vec<_> = skills.iter().filter(|s| s.2.contains(name)).collect();
            let best = holders.iter().map(|s| s.0).min().unwrap();
            let at_best: Vec<_> = holders.iter().filter(|s| s.0 == best).collect();
            if *rank == best && at_best.len() == 1 {
                found.insert(name.clone(), file.clone());
            }
        }
        found
    }

    #[test]
    fn a_skills_name_is_its_frontmatters_else_its_directorys() {
        assert_eq!(skill_name("---\nname: fragment\ndescription: x\n---\n# body\n", "other"), "fragment");
        assert_eq!(skill_name("---\nname: \"fragment\"\n---\n", "other"), "fragment");
        assert_eq!(skill_name("\u{feff}---\r\nname: 'fragment'\r\n---\r\n", "other"), "fragment");
        assert_eq!(skill_name("---\ndescription: no name\n---\n", "fragment"), "fragment");
        assert_eq!(skill_name("# no frontmatter\nname: fragment\n", "notes"), "notes", "a body's line is no name");
    }

    /// Hermes' skill guard: a skill whose text holds one of these is
    /// flagged as a prompt injection (its `_INJECTION_PATTERNS`).
    const HERMES_INJECTION_PATTERNS: [&str; 9] = ["ignore previous instructions", "ignore all previous", "you are now", "disregard your", "forget your instructions", "new instructions:", "system prompt:", "<system>", "]]>"];

    /// Valid: the platform skill is named `fragment`, says what the
    /// computer adds before the CLI's own skill, whole, and reads to Hermes
    /// as a skill (one frontmatter, a one-line description it lists, nothing
    /// its guard flags or its preprocessing would run). Invalid: a CLI skill
    /// with no frontmatter, or another name, is refused.
    #[test]
    fn the_platform_skill_is_the_clis_after_the_computers_page() {
        let cli = include_str!("../../../../cli/SKILL.md");
        let skill = platform_skill(cli).unwrap();
        assert!(skill.starts_with("---\nname: fragment\ndescription: \"You are an agent on a Fragment computer"), "{skill}");
        let (front, body) = skill.strip_prefix("---\n").and_then(|r| r.split_once("\n---\n")).unwrap();
        assert_eq!(front.lines().count(), 2, "a name and a description: {front}");
        let cli_body = cli.strip_prefix("---\n").and_then(|r| r.split_once("\n---\n")).unwrap().1.trim();
        assert!(body.contains(cli_body), "the CLI's skill, whole");
        let page = body.find("# Your computer").unwrap();
        assert!(page < body.find("# fragment").unwrap(), "the computer's page first");
        for said in ["FRAGMENT_AS_AGENT", "skip its Install and Pair", "apps-finite", "brain-finite", "GOOGLE_OAUTH_ACCESS_TOKEN", "google-workspace-finite", "\"Its computer's screen\"", "take over", "fragment create", "fragment write", "fragment deploy", "fragment call", "fragment list"] {
            assert!(body.contains(said), "it says {said:?}");
        }
        let lower = skill.to_lowercase();
        assert!(HERMES_INJECTION_PATTERNS.iter().all(|p| !lower.contains(p)), "nothing Hermes' guard flags");
        assert!(!skill.contains("!`") && !skill.contains("${HERMES_"), "nothing Hermes' preprocessing runs or fills");
        assert!(skill.len() <= PLATFORM_MAX_BYTES && PLATFORM_DESCRIPTION.len() <= 1024, "a page, and a description Hermes lists whole");
        assert!(platform_skill(cli_body).is_err(), "no frontmatter");
        assert!(platform_skill(&cli.replacen("name: fragment", "name: other", 1)).is_err(), "another name");
        assert!(platform_skill(&format!("{cli}{}", "x".repeat(PLATFORM_MAX_BYTES))).is_err(), "past its bound");
    }

    #[test]
    fn the_skills_fragment_is_looked_for_sooner_while_there_is_none() {
        assert_eq!(next_install_ms(Some(true), 600_000), 600_000);
        assert_eq!(next_install_ms(None, 600_000), 600_000, "an install that failed waits its cadence");
        assert_eq!(next_install_ms(Some(false), 600_000), ABSENT_EVERY_MS);
        assert_eq!(next_install_ms(Some(false), 2_000), 2_000, "a test's cadence is never slowed");
    }

    /// Goal: the platform skill is every profile's unless a managed skill
    /// takes its name. Valid: with no skills fragment it is the `fragment`
    /// Hermes finds; a managed `fragment` shadows it (the platform skill
    /// leaves the view). Invalid: both in the external dirs, Hermes finds
    /// no `fragment` at all. Replay: the managed one gone, it is found
    /// again, as it was written. Method: the managed set installed against
    /// the fake API, the view settled after each install as the boot does,
    /// and Hermes' rule over a profile's own dir and `EXTERNAL_DIRS`.
    #[tokio::test]
    async fn the_platform_skill_is_there_with_no_skills_fragment_and_a_managed_one_shadows_it() {
        use std::sync::{Arc, Mutex};
        fragment_bridge::log::set_quiet(true);
        let files: fake::Files = Arc::new(Mutex::new(BTreeMap::new()));
        let listed = Arc::new(Mutex::new(false));
        let (addr, _stop) = fake::start(files.clone(), Arc::new(Mutex::new(false)), listed.clone()).await;
        let api = Api::new(&format!("http://{addr}")).unwrap();
        let agent = Agent { fragment: "juniper.paul".into(), identity: "id:j".into(), name: "Juniper".into(), owner: "id:paul".into(), credentials: vec![] };
        let root = std::env::temp_dir().join(format!("hermes-boot-platform-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (own, manifest) = (root.join("profile/skills"), root.join("sync/managed.json"));
        // the profile's dirs as its config names them, under the test's root
        let at = |d: &str| root.join(d.trim_start_matches('/'));
        let external: Vec<PathBuf> = EXTERNAL_DIRS.iter().map(|d| at(d)).collect();
        let (managed, platform, view) = (at(MANAGED_DIR), at(PLATFORM_DIR), at(PLATFORM_VIEW));
        let skill = platform_skill(include_str!("../../../../cli/SKILL.md")).unwrap();
        write_platform_skill(&platform, &skill).unwrap();
        std::fs::create_dir_all(own.join("garden-notes")).unwrap();
        std::fs::write(own.join("garden-notes/SKILL.md"), "---\nname: garden-notes\n---\n").unwrap();
        let settle = || settle_platform(&managed, &manifest, &platform, &view).unwrap();

        // no skills fragment: no managed skills, the platform's `fragment`
        let d = install(&api, &agent, "id:paul", &managed, &manifest).await.unwrap();
        assert_eq!((d.fragment.as_deref(), d.skills), (None, 0));
        assert!(settle(), "shown");
        let found = hermes_finds(&own, &external);
        assert_eq!(found.get("fragment"), Some(&view.join(PLATFORM_PATH)), "{found:?}");
        assert!(found.contains_key("garden-notes"));

        // the skills fragment appears, holding a `fragment` of its own: it wins
        *listed.lock().unwrap() = true;
        files.lock().unwrap().insert("skills/fragment/SKILL.md".into(), ("release:f".into(), b"---\nname: fragment\ndescription: The managed one.\n---\n".to_vec()));
        files.lock().unwrap().insert("skills/grill-me/SKILL.md".into(), ("release:g".into(), b"---\nname: grill-me\n---\n".to_vec()));
        let d = install(&api, &agent, "id:paul", &managed, &manifest).await.unwrap();
        assert_eq!((d.fragment.as_deref(), d.skills), (Some("skills.paul"), 2));
        // invalid: both beside each other in one rank, Hermes finds neither
        assert_eq!(hermes_finds(&own, &external).get("fragment"), None, "ambiguous");
        assert!(!settle(), "shadowed");
        assert!(view.is_dir() && !view.join(PLATFORM_PATH).exists(), "the view stays, without it");
        let found = hermes_finds(&own, &external);
        assert_eq!(found.get("fragment"), Some(&managed.join("fragment/SKILL.md")), "the managed one shadows it: {found:?}");
        assert!(found.contains_key("grill-me") && found.contains_key("garden-notes"));
        assert!(!settle(), "replay: still shadowed");

        // the managed one gone: the platform's again, untouched
        files.lock().unwrap().remove("skills/fragment/SKILL.md");
        let d = install(&api, &agent, "id:paul", &managed, &manifest).await.unwrap();
        assert_eq!((d.removed, d.skills), (1, 1));
        assert!(settle());
        assert_eq!(hermes_finds(&own, &external).get("fragment"), Some(&view.join(PLATFORM_PATH)));
        assert_eq!(std::fs::read_to_string(view.join(PLATFORM_PATH)).unwrap(), skill, "the image's, as written");
        assert_eq!(std::fs::read_to_string(platform.join(PLATFORM_PATH)).unwrap(), skill, "no install touches it");
        // an agent's own `fragment` wins over the platform's (its own rank)
        std::fs::create_dir_all(own.join("fragment")).unwrap();
        std::fs::write(own.join("fragment/SKILL.md"), "---\nname: fragment\n---\n").unwrap();
        assert_eq!(hermes_finds(&own, &external).get("fragment"), Some(&own.join("fragment/SKILL.md")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The fragment list and a skills fragment's files, as the platform
    /// answers an agent acting for its owner (`for`); `listed` says whether
    /// the owner has the skills fragment.
    mod fake {
        use std::collections::BTreeMap;
        use std::net::SocketAddr;
        use std::sync::{Arc, Mutex};

        use fragment_bridge::net;
        use hyper::{Method, StatusCode};
        use serde_json::{json, Value};

        pub type Files = Arc<Mutex<BTreeMap<String, (String, Vec<u8>)>>>;

        pub async fn start(files: Files, down: Arc<Mutex<bool>>, listed: Arc<Mutex<bool>>) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (stop, rx) = tokio::sync::watch::channel(false);
            let handler = move |req: hyper::Request<hyper::body::Incoming>, _: SocketAddr| {
                let (files, down, listed) = (files.clone(), down.clone(), listed.clone());
                async move {
                    if *down.lock().unwrap() {
                        return net::refusal(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "down");
                    }
                    assert_eq!(req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()), Some("juniper.paul"), "as the agent");
                    let q = req.uri().query().unwrap_or("").to_string();
                    assert!(q.split('&').any(|kv| kv == "for=id%3Apaul"), "acting for its owner: {q}");
                    let skills = *listed.lock().unwrap();
                    match (req.method().clone(), req.uri().path()) {
                        (Method::GET, "/api/fragments") => {
                            let mut list = vec![json!({ "name": "garden.paul", "role": "editor", "kind": "app" })];
                            if skills {
                                list.insert(0, json!({ "name": "skills.paul", "role": "editor", "kind": "skills" }));
                            }
                            net::json_answer(StatusCode::OK, &json!({ "fragments": list }))
                        }
                        (_, path) if !skills && path.starts_with("/api/f/skills.paul/") => net::refusal(StatusCode::NOT_FOUND, "not_found", "no such fragment"),
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
