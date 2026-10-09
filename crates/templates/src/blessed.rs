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
//! would) and serves its site (cell/src/serve.rs), each file's bytes read
//! from its Static Assets by the hash this index names (cell/src/assets.rs).

use std::collections::BTreeMap;

use crate::{File, Template};
use fragment_core::manifest::{self, Manifest};
use fragment_proto::limits;

/// The templates the platform serves from its release.
pub const BLESSED: [&str; 4] = ["agent", "chat", "brain", "skills"];

/// A blessed template, when `name` is one: named statics, never
/// `crate::ALL`, so a binary links only the index of the templates it
/// names (the cell carries the blessed ones and not `notes`).
pub fn template(name: &str) -> Option<&'static Template> {
    match name {
        "agent" => Some(&crate::AGENT),
        "chat" => Some(&crate::CHAT),
        "brain" => Some(&crate::BRAIN),
        "skills" => Some(&crate::SKILLS),
        _ => None,
    }
}

/// A blessed template's data, all of its files at most this many and this
/// large, each file at most `DATA_FILE_MAX_BYTES`: a fragment lists it whole
/// in one answer, and a computer installs it file by file.
pub const DATA_FILES_MAX: usize = 1_000;
pub const DATA_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const DATA_FILE_MAX_BYTES: u64 = 256 * 1024;

/// Whether `path` of a blessed template is its data: what every fragment on
/// it lists and reads from the release (`GET …/files`, `…/file`, `__files`,
/// `__file`), beneath its own files at the same path. Everything but its
/// manifest, its README, its code and its site, which the release serves
/// otherwise (decision 40: no copies to drift).
pub fn is_data(path: &str) -> bool {
    path != "fragment.json" && path != "README.md" && !is_code(path) && !path.starts_with("site/")
}

/// A data file's version, `release:<24 hex>` from its bytes' hash: its
/// `lastCommitSha` in a listing, so a reader fetches again only what a
/// release changed.
pub fn data_version(f: &File) -> String {
    format!("release:{}", &f.sha256[..24])
}

/// A blessed template's data files, in path order (none for a template
/// that is not blessed).
pub fn data(name: &str) -> Vec<&'static File> {
    template(name).map(|t| t.files.iter().filter(|f| is_data(f.path)).collect()).unwrap_or_default()
}

/// One data file of a blessed template, when `path` is one.
pub fn data_file(name: &str, path: &str) -> Option<&'static File> {
    template(name)?.file(path).filter(|f| is_data(f.path))
}

/// A blessed template's manifest file, or why there is none.
pub fn manifest_file(name: &str) -> Result<&'static File, String> {
    let t = template(name).ok_or_else(|| format!("no blessed template {name:?} (the platform serves {})", BLESSED.join(", ")))?;
    t.file("fragment.json").ok_or_else(|| format!("the {name} template has no fragment.json"))
}

/// A blessed template's manifest, as it runs, from its file's bytes
/// (`manifest_file`'s).
pub fn manifest(name: &str, bytes: &[u8]) -> Result<Manifest, String> {
    manifest::parse(bytes).map_err(|e| format!("the {name} template's fragment.json: {e}"))
}

/// Which release of a blessed template a fragment runs: a hash of its
/// files, made by the build, so a platform deploy that changes any of them
/// is seen at the fragment's next request (cell/src/plane.rs installs it
/// again).
pub fn release(name: &str) -> Option<&'static str> {
    template(name).map(|t| t.release)
}

/// The file a blessed template serves for repo path `path` (`site/…`).
pub fn site_file(name: &str, path: &str) -> Option<&'static File> {
    if !path.starts_with("site/") {
        return None;
    }
    template(name)?.file(path)
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
    pub source: String,
    pub modules: BTreeMap<String, String>,
}

/// Why a template's code is no code the cell installs (a build that
/// carries it fails its tests: `every_blessed_template_runs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeFault {
    NotUtf8(&'static str),
    TooManyModules(usize),
    TooLarge(u64),
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

/// A blessed template's code files, `app.mjs` and its `applib/` modules in
/// path order, `None` when it carries none (its fragments are channels and
/// a site), held to an app's own limits by the sizes the index names.
pub fn code_files(name: &str) -> Result<Option<Vec<&'static File>>, CodeFault> {
    let Some(t) = template(name) else { return Ok(None) };
    let files: Vec<&'static File> = t.files.iter().filter(|f| is_code(f.path)).collect();
    let source = files.iter().find(|f| f.path == "app.mjs");
    let modules = files.len() - usize::from(source.is_some());
    let total: u64 = files.iter().map(|f| f.size).sum();
    if modules > limits::APPLIB_FILES_MAX {
        return Err(CodeFault::TooManyModules(modules));
    }
    if total > limits::APP_MODULES_MAX_BYTES as u64 || source.is_some_and(|s| s.size > limits::SOURCE_MAX_BYTES as u64) {
        return Err(CodeFault::TooLarge(total));
    }
    match source {
        None if modules == 0 => Ok(None),
        None => Err(CodeFault::LibWithoutApp),
        Some(_) => Ok(Some(files)),
    }
}

/// A blessed template's code from its code files' bytes (`code_files`'s,
/// each with the bytes read for it).
pub fn code(name: &str, read: Vec<(&'static File, Vec<u8>)>) -> Result<Code, CodeFault> {
    let release = release(name).expect("code files are a blessed template's");
    let mut source = None;
    let mut modules = BTreeMap::new();
    for (f, bytes) in read {
        assert!(is_code(f.path), "{} is code", f.path);
        let text = String::from_utf8(bytes).map_err(|_| CodeFault::NotUtf8(f.path))?;
        match f.path {
            "app.mjs" => source = Some(text),
            lib => {
                modules.insert(lib.to_string(), text);
            }
        }
    }
    let source = source.ok_or(CodeFault::LibWithoutApp)?;
    Ok(Code { id: format!("blessed:{name}@{release}"), source, modules })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::bytes_of;

    fn text(t: &Template, path: &str) -> String {
        String::from_utf8(bytes_of(t, t.file(path).unwrap_or_else(|| panic!("{} has {path}", t.name)))).expect("UTF-8")
    }

    /// Every blessed template parses, names its kind, has a page, and its
    /// code, when it carries some, is within an app's limits and runs the
    /// operations its manifest declares (a chat's push, from its trigger).
    #[test]
    fn every_blessed_template_runs() {
        for name in BLESSED {
            let Some(t) = template(name) else { panic!("{name} is blessed but not under templates/") };
            let m = manifest(name, &bytes_of(t, manifest_file(name).unwrap())).unwrap();
            assert!(m.kind.is_some(), "{name} names its kind");
            assert!(m.template.is_none(), "{name} names no template itself");
            assert!(site_file(name, "site/index.html").is_some(), "{name} has a page");
            assert_eq!(release(name), Some(t.release), "its release is the build's hash of its files");
            match code_files(name).unwrap_or_else(|e| panic!("{name}: {}", e.message())) {
                Some(files) => {
                    let c = code(name, files.into_iter().map(|f| (f, bytes_of(t, f))).collect()).unwrap_or_else(|e| panic!("{name}: {}", e.message()));
                    assert_eq!(c.id, format!("blessed:{name}@{}", t.release), "its code's identity is the release");
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
        assert!(code_files("chat").unwrap().is_some(), "the chat carries its push (decision 9)");
        assert!(template("todo").is_none(), "a template not blessed is only ever copied");
        assert_eq!(code_files("todo"), Ok(None), "and its code is no release's");
        assert!(manifest_file("todo").unwrap_err().contains("agent, chat, brain, skills"), "a template not blessed is named so");
        let chat = template("chat").unwrap();
        let files = code_files("chat").unwrap().unwrap();
        let read = |f: &'static File| (f, if f.path == "app.mjs" { vec![0xff] } else { bytes_of(chat, f) });
        assert_eq!(code("chat", files.into_iter().map(read).collect()), Err(CodeFault::NotUtf8("app.mjs")), "bytes that are not UTF-8 are no code");
    }

    /// Goal: the notes viewer's bundle (the brain's too) carries no chunk
    /// it never loads (templates/notes/src/viewer.mjs, its recipe). Method:
    /// from viewer.js, every `"./…"` a file names, statically imported or
    /// not, is followed; every chunk is reached.
    #[test]
    fn the_viewer_carries_no_chunk_it_never_loads() {
        let files: BTreeMap<&str, Vec<u8>> = crate::NOTES.files.iter().filter_map(|f| Some((f.path.strip_prefix("site/assets/")?, bytes_of(&crate::NOTES, f)))).collect();
        let (mut reached, mut queue) = (std::collections::BTreeSet::from(["viewer.js"]), vec!["viewer.js"]);
        // bounded: each file is queued once
        while let Some(f) = queue.pop() {
            let dir = f.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
            for named in std::str::from_utf8(&files[f]).expect("JavaScript is UTF-8").split("\"./").skip(1).filter_map(|s| s.split_once('"')).map(|(n, _)| format!("{dir}{n}")) {
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
    /// Method: the chat's and the shell's are the agent's: one asset.
    #[test]
    fn the_agent_image_is_one_file() {
        let (chat, agent) = (site_file("chat", "site/agent.png").expect("the chat's"), site_file("agent", "site/agent.png").expect("the agent's"));
        assert_eq!(chat.sha256, agent.sha256, "one asset");
        assert_eq!(crate::shell("agent.png").map(|f| f.sha256), Some(agent.sha256), "the shell's too");
    }

    /// Goal: a brain (decision 30) is the notes viewer and the brain's own
    /// code, all of it the template's. Method: its page loads the viewer and
    /// its search; the viewer's files are the notes template's very bytes
    /// (one asset, through the symlink build.rs follows); its code carries
    /// its sections and its guide, which is one template literal; its file
    /// trigger keeps the index.
    #[test]
    fn a_brain_is_the_notes_viewer_and_its_own_search() {
        let brain = template("brain").unwrap();
        let page = text(brain, "site/index.html");
        assert!(page.contains("assets/viewer.js") && page.contains("brain.js"), "its page is the viewer and its search");
        for f in crate::NOTES.files.iter().filter(|f| f.path.starts_with("site/assets/")) {
            let mine = site_file("brain", f.path).unwrap_or_else(|| panic!("the brain serves the viewer's {}", f.path));
            assert_eq!(mine.sha256, f.sha256, "{} is one asset", f.path);
        }
        let m = manifest("brain", &bytes_of(brain, manifest_file("brain").unwrap())).unwrap();
        assert_eq!(m.kind(), fragment_proto::FragmentKind::Brain);
        assert!(m.triggers.iter().any(|t| t.run == "changed"), "a file trigger keeps its index");
        let files = code_files("brain").unwrap().expect("a brain carries code");
        let c = code("brain", files.into_iter().map(|f| (f, bytes_of(brain, f))).collect()).unwrap();
        assert!(c.modules.contains_key("applib/sections.mjs") && c.modules.contains_key("applib/guide.mjs"), "{:?}", c.modules.keys());
        let guide = &c.modules["applib/guide.mjs"];
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
        for t in crate::ALL {
            match template(t.name) {
                Some(b) => assert!(std::ptr::eq(b, *t), "{} is its own template", t.name),
                None => assert!(!BLESSED.contains(&t.name), "{} is blessed but not named", t.name),
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
        let skills = data("skills");
        let (a, b) = (skills[0], skills[1]);
        assert_eq!(data_version(a), format!("release:{}", &a.sha256[..24]), "a hash of its bytes");
        assert_ne!(data_version(a), data_version(b));
        assert!(data_version(a).len() == "release:".len() + 24);
    }

    /// Goal: decision 17's managed set, as the skills template carries it.
    /// Method: every skill is `skills/<category>/<name>/SKILL.md` with a
    /// frontmatter naming it and saying what it is for; names are unique; the
    /// rewritten skills are there and what they replace is not; nothing
    /// Finite-only, generated, or ours not to redistribute rides along; and
    /// it fits a listing.
    #[test]
    fn the_skills_template_is_the_managed_set() {
        let skills = template("skills").unwrap();
        let files = data("skills");
        assert!(!files.is_empty());
        assert!(files.windows(2).all(|w| w[0].path < w[1].path), "in path order, as data_file searches it");
        let total: u64 = files.iter().map(|f| f.size).sum();
        assert!(files.len() <= DATA_FILES_MAX && total <= DATA_MAX_BYTES, "{} files, {total} bytes", files.len());
        let mut names = std::collections::BTreeSet::new();
        for f in files {
            assert!(f.path.starts_with("skills/"), "the managed set is under skills/: {}", f.path);
            assert!(f.size <= DATA_FILE_MAX_BYTES, "{} is {} bytes", f.path, f.size);
            assert!(!f.path.contains("__pycache__") && !f.path.ends_with(".pyc") && !f.path.ends_with(".DS_Store"), "generated: {}", f.path);
            assert_eq!(data_file("skills", f.path), Some(f));
            let bytes = bytes_of(skills, f);
            if let Ok(text) = std::str::from_utf8(&bytes) {
                // the repo is MIT, and the release hands this to every person
                assert!(!text.contains("All rights reserved"), "{} is not ours to redistribute", f.path);
                for finite_only in ["/profile-assets/", "~/.finite/", "FINITECHAT_HOME", "/home/node/", ".hermes/.env", ".hermes/venv", "git.finite.chat"] {
                    assert!(!text.contains(finite_only), "{} names {finite_only}", f.path);
                }
            }
            // a skill is skills/<category>/<name>/ or, uncategorized, skills/<name>/
            let parts: Vec<&str> = f.path.split('/').collect();
            if parts.last() == Some(&"SKILL.md") {
                assert!(matches!(parts.len(), 3 | 4), "{}: a skill sits at skills/<category>/<name>/ or skills/<name>/", f.path);
                let text = std::str::from_utf8(&bytes).expect("a SKILL.md is UTF-8");
                let front = text.strip_prefix("---\n").and_then(|t| t.split_once("\n---")).map(|(f, _)| f).unwrap_or_else(|| panic!("{} has a frontmatter", f.path));
                let name = front.lines().find_map(|l| l.strip_prefix("name:")).map(str::trim).unwrap_or_else(|| panic!("{} names itself", f.path));
                assert_eq!(name, parts[parts.len() - 2], "{}: a skill's name is its directory's", f.path);
                assert!(front.lines().any(|l| l.starts_with("description:")), "{} says what it is for", f.path);
                assert!(names.insert(name.to_string()), "{name} twice");
            }
        }
        for rewritten in ["apps-finite", "git-finite", "brain-finite", "google-workspace-finite", "image-generation-finite", "model-council-finite", "cocod-finite", "nostr-agent-interface-cli-finite", "x-api-finite", "music-generation-finite", "trading-agent-finite", "polymarket-finite"] {
            assert!(names.contains(rewritten), "{rewritten}");
        }
        for gone in ["shared-skills-finite", "finite-sites-publishing-finite", "publish-web-apps-finite", "website-building-finite", "finitebrain", "llm-wiki-finite", "fal-image-editing-finite", "ml-paper-writing-finite", "powerpoint-finite"] {
            assert!(!names.contains(gone), "{gone} is replaced");
        }
        assert_eq!(names.len(), 12, "Fragment contracts and the six operator integrations Paul retains");
        assert!(data_file("skills", "fragment.json").is_none() && data_file("skills", "skills/nope/SKILL.md").is_none());
    }

    #[test]
    fn faults_say_why() {
        assert!(CodeFault::TooManyModules(65).message().contains("64"));
        assert!(CodeFault::NotUtf8("app.mjs").message().contains("app.mjs"));
    }
}
