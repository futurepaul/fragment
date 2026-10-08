//! The skills each agent's goose finds (goose's `skills` builtin: a skill's
//! name and description in every session's system prompt, its whole text
//! loaded with `load_skill`), installed per agent at each turn's start into
//! that agent's goose's own skills directory (`<GOOSE_PATH_ROOT>/config/
//! skills`, under `/tmp`: never in a save):
//!
//! - **The platform skill, `fragment`**: what this computer is, then the
//!   `fragment` CLI's own skill (`fragment skill`), so an agent knows it
//!   makes and changes its owner's fragments (apps, sites, brains) with the
//!   CLI in its shell.
//! - **`web-search`**: the web tools, in the place of goose's own bundled
//!   skill of that name (which runs a tool the image lacks).
//! - **The managed skills** (decision 17): the owner's skills fragment's
//!   `skills/` (the blessed `skills` template's release, under any file of
//!   the fragment's own), read as the agent acting for its owner, adapted
//!   for goose: `${SKILL_DIR}` is the skill's directory here, Hermes'
//!   `web_extract` is `web_read`. A skill built on a tool goose lacks is
//!   left out (`LEFT_OUT`), and so is a provider's skill while the agent
//!   has no credential of that provider (`NEEDS`).
//!
//! An agent's own skills (in its work, `~/.agents/skills`) come before
//! these in goose's search, so one of the same name wins. The fetches are
//! the changed files only (`plan`, against what this computer's life
//! installed); an install that does not finish in `INSTALL_MS_MAX` leaves
//! the turn with what is there, and the next turn finishes it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::api::{Api, ApiError, FileEntry, FragmentEntry};
use crate::runtime::Agent;

/// The managed set's files in the skills fragment.
pub const PREFIX: &str = "skills/";
/// Bounds on what one install takes (the platform's own are below these:
/// crates/templates `blessed::DATA_*`).
pub const FILES_MAX: usize = 1_000;
pub const FILE_MAX_BYTES: usize = 256 * 1024;
pub const TOTAL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Files fetched at once.
pub const FETCH_AT_ONCE: usize = 8;
/// A turn waits this long for its agent's skills, at most.
pub const INSTALL_MS_MAX: u64 = 8_000;
/// A path under the managed directory is at most this long, and this deep.
const PATH_MAX: usize = 300;
const DEPTH_MAX: usize = 8;
/// Where, under an agent's skills directory, each part goes.
pub const MANAGED: &str = "managed";
pub const PLATFORM: &str = "fragment";
pub const WEB: &str = "web-search";
/// The platform skill is a page or two, not a manual.
pub const PLATFORM_MAX_BYTES: usize = 24 * 1024;

/// Managed skills left out on goose, and why: each is built on a tool of
/// Hermes' that goose does not have, or done better by goose's own.
pub const LEFT_OUT: [(&str, &str); 3] = [
    ("subagent-driven-development-finite", "built on Hermes' delegate_task"),
    ("requesting-code-review-finite", "built on Hermes' delegate_task"),
    ("duckduckgo-search-finite", "the web tools' web_search and web_read do it"),
];

/// A provider's skill, offered only while the agent holds a credential of
/// that provider (any of these environment variables: docs/computers.md,
/// Connections and operator keys). Unlisted skills need none.
pub const NEEDS: [(&str, &[&str]); 12] = [
    ("google-workspace-finite", &["GOOGLE_OAUTH_ACCESS_TOKEN"]),
    ("perplexity-research-finite", &["PERPLEXITY_API_KEY", "FIRECRAWL_API_KEY"]),
    ("goplaces-finite", &["GOOGLE_PLACES_API_KEY"]),
    ("x-search-finite", &["XAI_API_KEY"]),
    ("x-api-finite", &["X_API_BEARER_TOKEN"]),
    ("music-generation-finite", &["ELEVENLABS_API_KEY", "FAL_KEY"]),
    ("linear-finite", &["LINEAR_API_KEY"]),
    ("notion-finite", &["NOTION_TOKEN"]),
    ("monday-com-finite", &["MONDAY_API_TOKEN"]),
    ("parallel-cli-finite", &["PARALLEL_API_KEY"]),
    ("nano-pdf-finite", &["GEMINI_API_KEY"]),
    ("pdf-workbench-finite", &["GEMINI_API_KEY"]),
];

/// The platform skill's description: in every session's prompt, what makes
/// an agent load it.
pub const PLATFORM_DESCRIPTION: &str = "You are an agent on your owner's Fragment computer, and the `fragment` CLI in your shell acts as you. Load this before you make, change, publish, share or look up your owner's fragments (apps, sites, pages, dashboards, brains, chats), when asked what you can do here, or to ask another of your owner's agents.";

/// The page this computer adds before the CLI's skill.
pub const COMPUTER_PAGE: &str = include_str!("computer.md");

/// The skill that stands in for goose's bundled `web-search`.
pub const WEB_SKILL: &str = "---
name: web-search
description: Search the web and read pages. Use your web tools (web_search, web_read) first; the browser tools when a page needs clicking, typing, a login or JavaScript.
---

# Searching and reading the web

- `web_search {query}`: the top results (title, address, snippet).
- `web_read {url}`: a page as Markdown, its main text (`mode: \"page\"` for
  everything on it: lists, tables, search pages). Long pages come in
  parts: `start` names the next. `links: true` lists its links.
- When a page needs JavaScript, a login, or clicking: the browser tools
  (`browser_navigate`, then `browser_snapshot` and the refs it shows). Your
  owner watches your browser on your screen.
- To gather a list from a site: `web_read` its listing page with
  `mode: \"page\"` (and `links: true` to follow items); in the browser,
  one `browser_snapshot` of the page reads it all. Don't scroll and
  re-snapshot what one read already gave you.
";

/// The platform skill: the computer's page before the CLI's skill (what
/// `fragment skill` prints, a skill named `fragment`), under the computer's
/// description. Refused unless the CLI's is a skill named `fragment`.
pub fn platform_skill(cli_skill: &str) -> Result<String, String> {
    let (front, body) = cli_skill.strip_prefix("---\n").and_then(|rest| rest.split_once("\n---\n")).ok_or("`fragment skill` printed no skill: no frontmatter")?;
    if !front.lines().any(|l| l.trim_end() == format!("name: {PLATFORM}")) {
        return Err(format!("`fragment skill` printed a skill not named {PLATFORM}"));
    }
    let description = serde_json::to_string(PLATFORM_DESCRIPTION).expect("a string serializes");
    let skill = format!("---\nname: {PLATFORM}\ndescription: {description}\n---\n\n{}\n\n{}\n", COMPUTER_PAGE.trim(), body.trim());
    if skill.len() > PLATFORM_MAX_BYTES {
        return Err(format!("the platform skill is {} bytes, past {PLATFORM_MAX_BYTES}", skill.len()));
    }
    Ok(skill)
}

/// The owner's skills fragment among the fragments the agent reaches acting
/// for them: kind `skills`, under the owner's username; `skills.<username>`
/// first, else the first by name.
pub fn pick(fragments: &[FragmentEntry], agent_fragment: &str) -> Option<String> {
    let (_, username) = agent_fragment.split_once('.')?;
    let mut theirs: Vec<&str> = fragments.iter().filter(|f| f.kind == "skills" && f.name.split_once('.').is_some_and(|(_, u)| u == username)).map(|f| f.name.as_str()).collect();
    theirs.sort_unstable();
    let preferred = format!("skills.{username}");
    theirs.iter().find(|n| **n == preferred).or(theirs.first()).map(|n| n.to_string())
}

/// Whether `rel` is a path a managed file may have.
pub fn valid_rel(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    !rel.is_empty()
        && rel.len() <= PATH_MAX
        && parts.len() <= DEPTH_MAX
        && parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.starts_with('.') && *p != "__pycache__")
        && rel.bytes().all(|b| b.is_ascii_graphic() && b != b'\\')
}

/// The skill a managed path is of: its `SKILL.md`'s directory's name, for
/// every file under that directory (`research/arxiv-finite/scripts/x.py`
/// is `arxiv-finite`'s), given the set's skill directories.
fn skill_of<'a>(rel: &str, skill_dirs: &'a BTreeSet<String>) -> Option<&'a str> {
    skill_dirs.iter().filter(|d| rel.starts_with(&format!("{d}/"))).max_by_key(|d| d.len()).map(|d| d.rsplit('/').next().unwrap_or(d))
}

/// Whether the agent is offered `skill`, and why not.
pub fn offered(skill: &str, credential_env: &BTreeSet<String>) -> Result<(), &'static str> {
    if let Some((_, why)) = LEFT_OUT.iter().find(|(n, _)| *n == skill) {
        return Err(why);
    }
    match NEEDS.iter().find(|(n, _)| *n == skill) {
        Some((_, any)) if !any.iter().any(|v| credential_env.contains(*v)) => Err("its provider is not among the agent's credentials"),
        _ => Ok(()),
    }
}

/// Installed files: path under the managed directory → its version.
pub type Installed = BTreeMap<String, String>;

/// One install's steps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// (path under the managed directory, its path in the fragment, its version)
    pub fetch: Vec<(String, String, String)>,
    /// paths to delete
    pub remove: Vec<String>,
    /// what is installed once it is done
    pub wanted: Installed,
    /// files of the listing left out, and why
    pub refused: Vec<(String, &'static str)>,
}

/// The steps from what is installed to the listing's managed set, as this
/// agent is offered it.
pub fn plan(listing: Option<&[FileEntry]>, installed: &Installed, credential_env: &BTreeSet<String>) -> Plan {
    let mut out = Plan::default();
    let mut total = 0usize;
    let listing = listing.unwrap_or_default();
    let skill_dirs: BTreeSet<String> = listing.iter().filter_map(|f| f.path.strip_prefix(PREFIX)?.strip_suffix("/SKILL.md")).map(str::to_string).collect();
    for f in listing {
        let Some(rel) = f.path.strip_prefix(PREFIX) else { continue };
        let refused = if !valid_rel(rel) {
            Some("not a safe path")
        } else if let Some(Err(why)) = skill_of(rel, &skill_dirs).map(|s| offered(s, credential_env)) {
            Some(why)
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

/// A managed file's text as goose reads it: its skill's directory named
/// where it says `${SKILL_DIR}`, Hermes' `web_extract` read as `web_read`.
/// Bytes that are no UTF-8 (an image beside a skill) are kept as they are.
pub fn adapt(bytes: &[u8], skill_dir: &Path) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) if text.contains("${SKILL_DIR}") || text.contains("web_extract") => text.replace("${SKILL_DIR}", &skill_dir.display().to_string()).replace("web_extract", "web_read").into_bytes(),
        _ => bytes.to_vec(),
    }
}

/// Writes `bytes` at `dir/rel` whole (a temporary file renamed over it),
/// so goose never reads half a skill.
fn write_file(dir: &Path, rel: &str, bytes: &[u8]) -> std::io::Result<()> {
    let target = dir.join(rel);
    let parent = target.parent().expect("a file has a directory");
    std::fs::create_dir_all(parent)?;
    let tmp = target.with_file_name(format!(".{}.fragment-tmp", target.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    std::fs::write(&tmp, bytes)?;
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

fn load_manifest(path: &Path) -> Installed {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_manifest(path: &Path, m: &Installed) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m).expect("a manifest serializes"))?;
    std::fs::rename(&tmp, path)
}

/// What one install did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub fragment: Option<String>,
    pub fetched: u32,
    pub removed: u32,
    pub refused: u32,
    pub skills: u32,
}

/// Why an install stopped: what is installed stays whole either way.
#[derive(Debug)]
pub enum InstallError {
    Api(ApiError),
    Disk(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Api(e) => write!(f, "{e}"),
            InstallError::Disk(e) => write!(f, "writing the skills: {e}"),
        }
    }
}

impl From<ApiError> for InstallError {
    fn from(e: ApiError) -> InstallError {
        InstallError::Api(e)
    }
}

/// An agent's goose's skills directory, and its install's manifest.
pub fn dirs(goose_root: &Path, agent: &str) -> (PathBuf, PathBuf) {
    let root = goose_root.join(agent);
    (root.join("config").join("skills"), root.join("skills-installed.json"))
}

/// One install of `agent`'s skills under `dir`: the platform skill (when
/// the CLI's is given), `web-search`, and the owner's managed set as the
/// agent is offered it.
pub async fn install(api: &Api, agent: &Agent, dir: &Path, manifest: &Path, platform: Option<&str>) -> Result<Done, InstallError> {
    let disk = |e: std::io::Error| InstallError::Disk(e.to_string());
    std::fs::create_dir_all(dir).map_err(disk)?;
    if let Some(skill) = platform {
        write_file(dir, &format!("{PLATFORM}/SKILL.md"), skill.as_bytes()).map_err(disk)?;
    }
    write_file(dir, &format!("{WEB}/SKILL.md"), WEB_SKILL.as_bytes()).map_err(disk)?;
    let managed = dir.join(MANAGED);
    let mut installed: Installed = load_manifest(manifest).into_iter().filter(|(rel, _)| valid_rel(rel) && managed.join(rel).is_file()).collect();
    let fragments = api.fragments_for(&agent.fragment, &agent.owner).await?;
    let fragment = pick(&fragments, &agent.fragment);
    let listing = match &fragment {
        Some(f) => Some(api.files_for(&agent.fragment, f, &agent.owner).await?),
        None => None,
    };
    let credential_env: BTreeSet<String> = agent.credentials.iter().flat_map(|c| c.env.iter().cloned()).collect();
    let steps = plan(listing.as_deref(), &installed, &credential_env);
    let mut done = Done { fragment: fragment.clone(), refused: steps.refused.len() as u32, ..Done::default() };
    if let Some(f) = &fragment {
        let skill_dirs: BTreeSet<String> = steps.wanted.keys().filter_map(|p| p.strip_suffix("/SKILL.md")).map(str::to_string).collect();
        for batch in steps.fetch.chunks(FETCH_AT_ONCE) {
            let reads = batch.iter().map(|(_, path, _)| api.file_for(&agent.fragment, f, &agent.owner, path, FILE_MAX_BYTES));
            let got = futures_util::future::join_all(reads).await;
            for ((rel, _, version), bytes) in batch.iter().zip(got) {
                let bytes = bytes?;
                let skill_dir = skill_dirs.iter().filter(|d| rel.starts_with(&format!("{d}/"))).max_by_key(|d| d.len()).map(|d| managed.join(d)).unwrap_or_else(|| managed.clone());
                write_file(&managed, rel, &adapt(&bytes, &skill_dir)).map_err(disk)?;
                installed.insert(rel.clone(), version.clone());
                done.fetched += 1;
            }
            // saved as it goes: a cut install fetches only what it lacks next time
            save_manifest(manifest, &installed).map_err(disk)?;
        }
    }
    for rel in &steps.remove {
        remove_file(&managed, rel);
        installed.remove(rel);
        done.removed += 1;
    }
    assert_eq!(installed, steps.wanted, "an install that answered whole installs exactly the listing's set");
    save_manifest(manifest, &installed).map_err(disk)?;
    done.skills = installed.keys().filter(|p| p.ends_with("/SKILL.md")).count() as u32;
    Ok(done)
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

    #[test]
    fn the_owners_skills_fragment_is_found_by_kind_and_username() {
        let list = [frag("garden.paul", "app"), frag("skills.skyler", "skills"), frag("extra.paul", "skills"), frag("skills.paul", "skills")];
        assert_eq!(pick(&list, "juniper.paul").as_deref(), Some("skills.paul"));
        assert_eq!(pick(&list[..3], "juniper.paul").as_deref(), Some("extra.paul"), "another label of theirs");
        assert_eq!(pick(&list[..2], "juniper.paul"), None, "skyler's is not paul's");
    }

    #[test]
    fn only_safe_paths_are_installed() {
        for ok in ["research/arxiv-finite/SKILL.md", "grill-me/SKILL.md", "a/b/c/d.py"] {
            assert!(valid_rel(ok), "{ok}");
        }
        for bad in ["", "../x", "a/../b", "/etc/passwd", "a//b", "a/.hidden", "a/__pycache__/x.pyc", "a/b c", "a\\b"] {
            assert!(!valid_rel(bad), "{bad}");
        }
    }

    /// Valid: the managed set as the agent is offered it: a Hermes-only
    /// skill left out with all its files, a provider's skill only with its
    /// credential. Replay: a settled install fetches nothing. A change
    /// fetches what changed and removes what went.
    #[test]
    fn a_plan_is_the_set_this_agent_is_offered() {
        let listing = vec![
            file("fragment.json", 40, "c1"),
            file("skills/research/arxiv-finite/SKILL.md", 100, "release:a"),
            file("skills/research/arxiv-finite/scripts/x.py", 200, "release:b"),
            file("skills/software-development/subagent-driven-development-finite/SKILL.md", 50, "release:s"),
            file("skills/productivity/linear-finite/SKILL.md", 50, "release:l"),
            file("skills/productivity/linear-finite/scripts/linear.py", 50, "release:l2"),
            file("skills/grill-me/SKILL.md", 50, "c2"),
        ];
        let none = BTreeSet::new();
        let first = plan(Some(&listing), &Installed::new(), &none);
        assert_eq!(first.fetch.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["research/arxiv-finite/SKILL.md", "research/arxiv-finite/scripts/x.py", "grill-me/SKILL.md"]);
        assert_eq!(first.refused.len(), 3, "the Hermes-only skill, and linear's two files without its key: {:?}", first.refused);
        let linear: BTreeSet<String> = ["LINEAR_API_KEY".to_string()].into();
        let with_key = plan(Some(&listing), &Installed::new(), &linear);
        assert!(with_key.wanted.contains_key("productivity/linear-finite/scripts/linear.py"), "with its credential, the provider's skill whole");
        let settled = plan(Some(&listing), &first.wanted, &none);
        assert!(settled.fetch.is_empty() && settled.remove.is_empty(), "{settled:?}");
        let next = vec![file("skills/research/arxiv-finite/SKILL.md", 120, "release:a2"), file("skills/grill-me/SKILL.md", 50, "c2")];
        let changed = plan(Some(&next), &first.wanted, &none);
        assert_eq!(changed.fetch.len(), 1);
        assert_eq!(changed.remove, vec!["research/arxiv-finite/scripts/x.py".to_string()]);
        assert!(plan(None, &first.wanted, &none).wanted.is_empty(), "no skills fragment: none");
    }

    /// `${SKILL_DIR}` is the skill's directory; Hermes' reader is goose's.
    #[test]
    fn a_skill_is_adapted_for_goose() {
        let dir = Path::new("/tmp/goose/a.paul/config/skills/managed/research/arxiv-finite");
        let out = adapt(b"python3 ${SKILL_DIR}/scripts/search_arxiv.py; web_extract(urls=[x])", dir);
        assert_eq!(String::from_utf8(out).unwrap(), "python3 /tmp/goose/a.paul/config/skills/managed/research/arxiv-finite/scripts/search_arxiv.py; web_read(urls=[x])");
        let png = [0x89, b'P', b'N', b'G', 0xff];
        assert_eq!(adapt(&png, dir), png.to_vec(), "bytes that are no text, as they are");
    }

    /// The platform skill is the computer's page and the CLI's skill, under
    /// its own description; anything that is not the CLI's skill is refused.
    #[test]
    fn the_platform_skill() {
        let cli = "---\nname: fragment\ndescription: the cli\n---\n\n# fragment\n\nMake apps.\n";
        let s = platform_skill(cli).unwrap();
        assert!(s.starts_with("---\nname: fragment\ndescription: \"You are an agent on your owner's Fragment computer"), "{s}");
        assert!(s.contains("# Your computer") && s.ends_with("Make apps.\n"));
        assert!(platform_skill("no frontmatter").is_err());
        assert!(platform_skill("---\nname: other\n---\nx").is_err());
        assert!(WEB_SKILL.starts_with("---\nname: web-search\n"));
    }
}
