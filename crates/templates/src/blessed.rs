//! Blessed templates (docs/cloudflare-v1.md, decision 40): a chat, an agent
//! or a brain fragment names its template in `fragment.json` (`template`), and
//! the platform's release serves that template's manifest, its site, and
//! its app code (`app.mjs`, with `applib/`), so one deploy of the platform
//! updates every chat, with no copies to drift. The fragment's own repo
//! holds only its face (`meta`) and its data (an agent's `SOUL.md`). A
//! fragment that wants other code forks: its files become its own, and its
//! `fragment.json` names no template.
//!
//! The cell installs a blessed fragment's code from here (cell/src/plane.rs:
//! the release's code under the release's identity, `loader_id`, so a
//! platform deploy that changes it starts a fresh worker, as a new commit
//! would) and serves its site (cell/src/serve.rs).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use fragment_core::manifest::{self, Manifest};
use fragment_proto::limits;
use crate::Template;
use sha2::{Digest, Sha256};

/// The templates the platform serves from its release.
pub const BLESSED: [&str; 4] = ["agent", "chat", "brain", "skills"];

/// A blessed template's files, when `name` is one: named statics, never
/// `crate::ALL`, so a binary links only the templates it names (build.rs:
/// the cell carries the blessed ones and not `notes`).
pub fn template(name: &str) -> Option<Template> {
    match name {
        "agent" => Some(crate::AGENT),
        "chat" => Some(crate::CHAT),
        "brain" => Some(crate::BRAIN),
        "skills" => Some(crate::SKILLS),
        _ => None,
    }
}

/// A blessed template's data, all of its files at most this many and this
/// large, each file at most `DATA_FILE_MAX_BYTES`: a fragment lists it whole
/// in one answer, and a computer installs it file by file.
pub const DATA_FILES_MAX: usize = 1_000;
pub const DATA_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const DATA_FILE_MAX_BYTES: usize = 256 * 1024;

/// Whether `path` of a blessed template is its data: what every fragment on
/// it lists and reads from the release (`GET …/files`, `…/file`, `__files`,
/// `__file`), beneath its own files at the same path. Everything but its
/// manifest, its README, its code and its site, which the release serves
/// otherwise (decision 40: no copies to drift).
pub fn is_data(path: &str) -> bool {
    path != "fragment.json" && path != "README.md" && !is_code(path) && !path.starts_with("site/")
}

/// One data file of a blessed template, as a fragment on it lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFile {
    pub path: &'static str,
    pub bytes: &'static [u8],
    /// `release:<24 hex>`, a hash of its bytes: its `lastCommitSha` in a
    /// listing, so a reader fetches again only what a release changed.
    pub version: String,
}

/// A data file's version: a hash of its bytes.
pub fn data_version(bytes: &[u8]) -> String {
    format!("release:{}", hex::encode(&Sha256::digest(bytes)[..12]))
}

/// A blessed template's data files, in path order (none for a template
/// that is not blessed). Made once per isolate.
pub fn data(name: &str) -> &'static [DataFile] {
    static DATA: OnceLock<BTreeMap<&'static str, Vec<DataFile>>> = OnceLock::new();
    let all = DATA.get_or_init(|| {
        BLESSED
            .iter()
            .filter_map(|n| {
                let files = template(n)?.iter().filter(|(p, _)| is_data(p)).map(|(p, b)| DataFile { path: p, bytes: b, version: data_version(b) }).collect();
                Some((*n, files))
            })
            .collect()
    });
    all.get(name).map_or(&[], Vec::as_slice)
}

/// One data file of a blessed template, when `path` is one.
pub fn data_file(name: &str, path: &str) -> Option<&'static DataFile> {
    let files = data(name);
    files.binary_search_by(|f| f.path.cmp(path)).ok().map(|i| &files[i])
}

/// A blessed template's manifest, as it runs.
pub fn manifest(name: &str) -> Result<Manifest, String> {
    let t = template(name).ok_or_else(|| format!("no blessed template {name:?} (the platform serves {})", BLESSED.join(", ")))?;
    let bytes = t.iter().find(|(p, _)| *p == "fragment.json").map(|(_, b)| *b).ok_or_else(|| format!("the {name} template has no fragment.json"))?;
    manifest::parse(bytes).map_err(|e| format!("the {name} template's fragment.json: {e}"))
}

/// Which release of a blessed template a fragment runs: a hash of its
/// files, so a platform deploy that changes any of them is seen at the
/// fragment's next request (cell/src/plane.rs installs it again). The files
/// are constants of this build, so each template's hash is made once per
/// isolate, when a fragment on it first asks (a chat never hashes the
/// brain's 3 MiB viewer).
pub fn release(name: &str) -> Option<String> {
    static RELEASES: [OnceLock<String>; BLESSED.len()] = [const { OnceLock::new() }; BLESSED.len()];
    let at = BLESSED.iter().position(|n| *n == name)?;
    let t = template(name)?;
    Some(RELEASES[at].get_or_init(|| hash(t)).clone())
}

fn hash(t: Template) -> String {
    let mut h = Sha256::new();
    for (path, bytes) in t {
        h.update(path.as_bytes());
        h.update([0]);
        h.update(bytes);
        h.update([0]);
    }
    hex::encode(&h.finalize()[..12])
}

/// The file a blessed template serves for repo path `path` (`site/…`).
pub fn site_file(name: &str, path: &str) -> Option<&'static [u8]> {
    if !path.starts_with("site/") {
        return None;
    }
    template(name)?.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

/// Whether `path` is app code: what a fragment on a blessed template may
/// not carry of its own (it forks to), and what the release serves it.
pub fn is_code(path: &str) -> bool {
    path == "app.mjs" || (path.starts_with("applib/") && (path.ends_with(".mjs") || path.ends_with(".js")))
}

/// A blessed template's app code as the cell installs it: `app.mjs`, its
/// `applib/` modules by path, and its identity, `blessed:<name>@<release>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub id: String,
    pub source: &'static str,
    pub modules: BTreeMap<String, &'static str>,
}

/// Why a template's code is no code the cell installs (a build that
/// carries it fails its tests: `every_blessed_template_runs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeFault {
    NotUtf8(&'static str),
    TooManyModules(usize),
    TooLarge(usize),
    /// `applib/` without an `app.mjs` to import it.
    LibWithoutApp,
}

impl CodeFault {
    pub fn message(&self) -> String {
        match self {
            CodeFault::NotUtf8(p) => format!("{p} is not UTF-8"),
            CodeFault::TooManyModules(n) => format!("applib/ has {n} modules; the limit is {}", limits::APPLIB_FILES_MAX),
            CodeFault::TooLarge(n) => format!("app.mjs and applib/ are {n} bytes; the limit is {}", limits::APP_MODULES_MAX_BYTES),
            CodeFault::LibWithoutApp => "applib/ without an app.mjs".into(),
        }
    }
}

/// A blessed template's app code, `None` when it carries none (its
/// fragments are channels and a site), held to an app's own limits.
pub fn code(name: &str) -> Result<Option<Code>, CodeFault> {
    let Some(t) = template(name) else { return Ok(None) };
    let text = |path: &'static str, bytes: &'static [u8]| std::str::from_utf8(bytes).map_err(|_| CodeFault::NotUtf8(path));
    let mut source = None;
    let mut modules = BTreeMap::new();
    let mut total = 0usize;
    for (path, bytes) in t.iter().filter(|(p, _)| is_code(p)) {
        total += bytes.len();
        match *path {
            "app.mjs" => source = Some(text(path, bytes)?),
            lib => {
                modules.insert(lib.to_string(), text(path, bytes)?);
            }
        }
    }
    if modules.len() > limits::APPLIB_FILES_MAX {
        return Err(CodeFault::TooManyModules(modules.len()));
    }
    if total > limits::APP_MODULES_MAX_BYTES || source.is_some_and(|s| s.len() > limits::SOURCE_MAX_BYTES) {
        return Err(CodeFault::TooLarge(total));
    }
    match source {
        None if modules.is_empty() => Ok(None),
        None => Err(CodeFault::LibWithoutApp),
        Some(source) => {
            let release = release(name).expect("a template found here has a release");
            Ok(Some(Code { id: format!("blessed:{name}@{release}"), source, modules }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every blessed template parses, names its kind, has a page, and its
    /// code, when it carries some, is within an app's limits and runs the
    /// operations its manifest declares (a chat's push, from its trigger).
    #[test]
    fn every_blessed_template_runs() {
        for name in BLESSED {
            let Some(t) = template(name) else { panic!("{name} is blessed but not under templates/") };
            let m = manifest(name).unwrap();
            assert!(m.kind.is_some(), "{name} names its kind");
            assert!(m.template.is_none(), "{name} names no template itself");
            assert!(site_file(name, "site/index.html").is_some(), "{name} has a page");
            assert_eq!(release(name), release(name));
            assert_eq!(release(name), Some(hash(t)), "the cached release is the files' hash");
            match code(name).unwrap_or_else(|e| panic!("{name}: {}", e.message())) {
                Some(c) => {
                    assert_eq!(c.id, format!("blessed:{name}@{}", release(name).unwrap()), "its code's identity is the release");
                    assert!(c.source.contains("export class App"), "{name}'s app.mjs exports App");
                    assert!(!m.operations.is_empty(), "{name} declares the operations its code runs");
                    for op in m.operations.keys() {
                        assert!(c.source.contains(&format!("{op}(")), "{name}'s app.mjs has a method for {op}");
                    }
                    for trigger in &m.triggers {
                        assert!(m.operations.contains_key(&trigger.run), "{name}'s trigger runs an operation it declares");
                    }
                }
                None => assert!(m.operations.is_empty() && m.triggers.is_empty(), "{name} declares no operations without code"),
            }
        }
        assert!(code("chat").unwrap().is_some(), "the chat carries its push (decision 9)");
        assert!(template("todo").is_none(), "a template not blessed is only ever copied");
        assert_eq!(code("todo"), Ok(None), "and its code is no release's");
    }

    /// Goal: the notes viewer's bundle (the brain's too) carries no chunk
    /// it never loads (templates/notes/src/viewer.mjs, its recipe). Method:
    /// from viewer.js, every `"./…"` a file names, statically imported or
    /// not, is followed; every chunk is reached.
    #[test]
    fn the_viewer_carries_no_chunk_it_never_loads() {
        let files: std::collections::BTreeMap<&str, &[u8]> = crate::NOTES.iter().filter_map(|(p, b)| Some((p.strip_prefix("site/assets/")?, *b))).collect();
        let (mut reached, mut queue) = (std::collections::BTreeSet::from(["viewer.js"]), vec!["viewer.js"]);
        // bounded: each file is queued once
        while let Some(f) = queue.pop() {
            let dir = f.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
            for named in std::str::from_utf8(files[f]).expect("JavaScript is UTF-8").split("\"./").skip(1).filter_map(|s| s.split_once('"')).map(|(n, _)| format!("{dir}{n}")) {
                if let Some((&path, _)) = files.get_key_value(named.as_str()) {
                    if reached.insert(path) {
                        queue.push(path);
                    }
                }
            }
        }
        let unread: Vec<&&str> = files.keys().filter(|p| p.starts_with("chunks/") && !reached.contains(*p)).collect();
        assert!(unread.is_empty(), "chunks nothing imports: {unread:?}");
    }

    /// Goal: every agent's image is one file (cell/shell/CREDITS.md).
    /// Method: the chat's is the agent's, one copy in the binary.
    #[test]
    fn the_agent_image_is_one_file() {
        let (chat, agent) = (site_file("chat", "site/agent.png").expect("the chat's"), site_file("agent", "site/agent.png").expect("the agent's"));
        assert!(std::ptr::eq(chat.as_ptr(), agent.as_ptr()), "embedded once");
    }

    /// Goal: a brain (decision 30) is the notes viewer and the brain's own
    /// code, all of it the template's. Method: its page loads the viewer and
    /// its search; the viewer's files are the notes template's very bytes
    /// (one copy in the binary, through the symlink build.rs follows); its
    /// code carries its sections and its guide, which is one template
    /// literal; its file trigger keeps the index.
    #[test]
    fn a_brain_is_the_notes_viewer_and_its_own_search() {
        let page = std::str::from_utf8(site_file("brain", "site/index.html").expect("a brain has a page")).unwrap();
        assert!(page.contains("assets/viewer.js") && page.contains("brain.js"), "its page is the viewer and its search");
        for (path, bytes) in crate::NOTES.iter().filter(|(p, _)| p.starts_with("site/assets/")) {
            let brain = site_file("brain", path).unwrap_or_else(|| panic!("the brain serves the viewer's {path}"));
            assert!(std::ptr::eq(brain.as_ptr(), bytes.as_ptr()), "{path} is embedded once");
        }
        let m = manifest("brain").unwrap();
        assert_eq!(m.kind(), fragment_proto::FragmentKind::Brain);
        assert!(m.triggers.iter().any(|t| t.run == "changed"), "a file trigger keeps its index");
        let c = code("brain").unwrap().expect("a brain carries code");
        assert!(c.modules.contains_key("applib/sections.mjs") && c.modules.contains_key("applib/guide.mjs"), "{:?}", c.modules.keys());
        let guide = c.modules["applib/guide.mjs"];
        let text = guide.split_once("String.raw`").map(|(_, t)| t).expect("the guide is String.raw");
        let body = text.strip_suffix("`;\n").expect("one template literal, to the end");
        assert!(!body.contains('`') && !body.contains("${"), "the guide holds no backtick or substitution");
        assert!(body.contains("raw/") && body.contains("fragment call <brain> search"), "it says how to ingest and search");
    }

    /// Goal: what counts as code is what a fragment on a template may not
    /// carry of its own. Method: app.mjs and applib modules are code; data,
    /// a page's script, and a module outside applib/ are not.
    #[test]
    fn code_is_app_mjs_and_applib() {
        for p in ["app.mjs", "applib/x.mjs", "applib/a/b.js"] {
            assert!(is_code(p), "{p}");
        }
        for p in ["SOUL.md", "site/chat.js", "lib/x.mjs", "applib/notes.md", "app.js", "x/app.mjs"] {
            assert!(!is_code(p), "{p}");
        }
    }

    /// The named statics are the templates under templates/ of those names,
    /// and only the blessed ones are named.
    #[test]
    fn blessed_templates_are_the_named_ones() {
        for (name, files) in crate::ALL {
            match template(name) {
                Some(t) => assert!(std::ptr::eq(t, *files), "{name} is its own template"),
                None => assert!(!BLESSED.contains(name), "{name} is blessed but not named"),
            }
        }
        assert!(BLESSED.iter().all(|n| template(n).is_some()));
        assert!(template("notes").is_none() && template("nope").is_none());
    }

    /// Goal: a blessed template's data is everything its fragments do not get
    /// some other way. Method: the manifest, the README, code and the site are
    /// not data; any other path is.
    #[test]
    fn data_is_everything_but_manifest_readme_code_and_site() {
        for p in ["skills/research/arxiv-finite/SKILL.md", "skills/x/scripts/a.py", "LICENSE", "docs/README.md", "x/app.mjs"] {
            assert!(is_data(p), "{p}");
        }
        for p in ["fragment.json", "README.md", "app.mjs", "applib/a.mjs", "site/index.html", "site/skills/x.md"] {
            assert!(!is_data(p), "{p}");
        }
        assert!(data("agent").is_empty() && data("chat").is_empty(), "the agent and chat templates carry no data");
        assert!(data("todo").is_empty() && data("nope").is_empty(), "a template that is not blessed has none to serve");
        assert_eq!(data_version(b"a"), data_version(b"a"));
        assert_ne!(data_version(b"a"), data_version(b"b"));
        assert!(data_version(b"").starts_with("release:") && data_version(b"").len() == "release:".len() + 24);
    }

    /// Goal: decision 17's managed set, as the skills template carries it.
    /// Method: every skill is `skills/<category>/<name>/SKILL.md` with a
    /// frontmatter naming it and saying what it is for; names are unique; the
    /// rewritten skills are there and what they replace is not; nothing
    /// Finite-only or generated rides along; and it fits a listing.
    #[test]
    fn the_skills_template_is_the_managed_set() {
        let files = data("skills");
        assert!(!files.is_empty());
        assert!(files.windows(2).all(|w| w[0].path < w[1].path), "in path order, as data_file searches it");
        let total: usize = files.iter().map(|f| f.bytes.len()).sum();
        assert!(files.len() <= DATA_FILES_MAX && total <= DATA_MAX_BYTES, "{} files, {total} bytes", files.len());
        let mut names = std::collections::BTreeSet::new();
        for f in files {
            assert!(f.path.starts_with("skills/"), "the managed set is under skills/: {}", f.path);
            assert!(f.bytes.len() <= DATA_FILE_MAX_BYTES, "{} is {} bytes", f.path, f.bytes.len());
            assert!(!f.path.contains("__pycache__") && !f.path.ends_with(".pyc") && !f.path.ends_with(".DS_Store"), "generated: {}", f.path);
            assert_eq!(data_file("skills", f.path), Some(f));
            if let Ok(text) = std::str::from_utf8(f.bytes) {
                for finite_only in ["/profile-assets/", "~/.finite/", "FINITECHAT_HOME", "/home/node/", ".hermes/.env", ".hermes/venv", "git.finite.chat"] {
                    assert!(!text.contains(finite_only), "{} names {finite_only}", f.path);
                }
            }
            // a skill is skills/<category>/<name>/ or, uncategorized, skills/<name>/
            let parts: Vec<&str> = f.path.split('/').collect();
            if parts.last() == Some(&"SKILL.md") {
                assert!(matches!(parts.len(), 3 | 4), "{}: a skill sits at skills/<category>/<name>/ or skills/<name>/", f.path);
                let text = std::str::from_utf8(f.bytes).expect("a SKILL.md is UTF-8");
                let front = text.strip_prefix("---\n").and_then(|t| t.split_once("\n---")).map(|(f, _)| f).unwrap_or_else(|| panic!("{} has a frontmatter", f.path));
                let name = front.lines().find_map(|l| l.strip_prefix("name:")).map(str::trim).unwrap_or_else(|| panic!("{} names itself", f.path));
                assert_eq!(name, parts[parts.len() - 2], "{}: a skill's name is its directory's", f.path);
                assert!(front.lines().any(|l| l.starts_with("description:")), "{} says what it is for", f.path);
                assert!(names.insert(name.to_string()), "{name} twice");
            }
        }
        for rewritten in ["apps-finite", "git-finite", "brain-finite", "google-workspace-finite", "image-generation-finite"] {
            assert!(names.contains(rewritten), "{rewritten}");
        }
        for gone in ["shared-skills-finite", "finite-sites-publishing-finite", "publish-web-apps-finite", "website-building-finite", "finitebrain", "llm-wiki-finite", "fal-image-editing-finite", "ml-paper-writing-finite"] {
            assert!(!names.contains(gone), "{gone} is replaced");
        }
        assert_eq!(names.len(), 42, "finite-skills' 47, less shared-skills, with sites, publishing and website building one skill, brain and llm-wiki one, and the two paper-writing skills one");
        assert!(data_file("skills", "fragment.json").is_none() && data_file("skills", "skills/nope/SKILL.md").is_none());
    }

    #[test]
    fn faults_say_why() {
        assert!(CodeFault::TooManyModules(65).message().contains("64"));
        assert!(CodeFault::NotUtf8("app.mjs").message().contains("app.mjs"));
    }
}
