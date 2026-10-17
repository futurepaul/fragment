//! The files the release serves from the cell's Static Assets: the
//! templates under `templates/` (`fragment new` scaffolds them, and the
//! cell makes a fragment from one: docs/api.md, Control API), the shell's
//! files (`cell/shell/`), and the agent docs (`cli/SKILL.md`,
//! `cli/GUIDE.md`, the platform's `/llms.txt` and `/llms-full.txt`, their
//! blanks filled as they are shown: `agent_doc`).
//!
//! The cell holds only their index: each file's path, SHA-256 and size,
//! and each template's release (build.rs), and reads a file's bytes from
//! its Static Assets by its hash (cell/src/assets.rs), so an isolate pays
//! for a file only when it reads one. The `embed` feature adds each file's
//! bytes (the CLI scaffolds them, xtask writes them into the assets
//! directory a cell build uploads: `write_assets`). The `platform` feature
//! (on by default) adds the platform's own files: the blessed templates
//! (`blessed`) and the shell's. The CLI builds without it, so its inputs
//! are the templates it scaffolds and the agent docs alone.

/// One file, as the build found it.
#[derive(Debug, PartialEq, Eq)]
pub struct File {
    /// Its path within its template (relative to a fragment's root), or
    /// its name in the shell.
    pub path: &'static str,
    /// Its bytes' SHA-256, hex: its name among the cell's Static Assets.
    pub sha256: &'static str,
    pub size: u64,
    /// Its bytes, in a binary built with `embed`.
    #[cfg(feature = "embed")]
    pub bytes: &'static [u8],
}

/// A template: its files in path order, and its release.
#[derive(Debug, PartialEq, Eq)]
pub struct Template {
    pub name: &'static str,
    /// A hash of its files' paths and bytes, made by the build (blessed.rs
    /// `release`): a deploy that changes any file changes it.
    pub release: &'static str,
    pub files: &'static [File],
}

impl Template {
    /// Its file at `path`, if it holds one.
    pub fn file(&self, path: &str) -> Option<&'static File> {
        let files: &'static [File] = self.files;
        files.binary_search_by(|f| f.path.cmp(path)).ok().map(|i| &files[i])
    }
}

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

/// The agent docs' blanks. cli/SKILL.md and cli/GUIDE.md name no host:
/// whoever shows them fills these with the platform it speaks for, the
/// cell with its own deployment's (`/llms.txt`, `/llms-full.txt`) and the
/// CLI with its host's (`fragment skill`, `fragment guide`). The
/// platform's origin (`https://finite.chat`), and a fragment's on it,
/// `<name>` standing for the fragment's name.
pub const PLATFORM_BLANK: &str = "{{platform}}";
pub const FRAGMENT_BLANK: &str = "{{fragment}}";
/// A fragment's origin as the CLI's docs name it: the host knows its
/// fragments' domain and the CLI does not (the host's own `/llms.txt`
/// names it).
pub const ANY_FRAGMENT: &str = "https://<name>.<the platform's domain for fragments>";

/// An agent doc's text with its blanks filled.
pub fn agent_doc(text: &str, platform: &str, fragment: &str) -> String {
    text.replace(PLATFORM_BLANK, platform).replace(FRAGMENT_BLANK, fragment)
}

/// The templates a fragment can start from, the simplest first (a create
/// that names none of them lists them in this order), each copied into
/// the fragment's repo (cell/src/publish.rs). `notes` stays with the CLI
/// (`fragment new --template notes`).
pub static CATALOG: [&Template; 5] = [&BLANK, &TODO, &INBOX, &CALORIES, &DOC];

/// A file of cell/shell/ by its path there: a published one, or a page
/// (`index.html`, `admin.html`).
#[cfg(feature = "platform")]
pub fn shell(name: &str) -> Option<&'static File> {
    SHELL.binary_search_by(|f| f.path.cmp(name)).ok().map(|i| &SHELL[i])
}

/// The shell's files it publishes at `/__shell/<name>` (cell/src/shell.rs),
/// each with its content type: of the rest of cell/shell/, its pages are
/// served at their own paths, and its credits and licenses not at all.
#[cfg(feature = "platform")]
const SHELL_PUBLISHED: [(&str, &str); 17] = [
    ("shell.js", "text/javascript; charset=utf-8"),
    ("shell.css", "text/css; charset=utf-8"),
    ("layout.js", "text/javascript; charset=utf-8"),
    ("viewer.js", "text/javascript; charset=utf-8"),
    ("agent-identity.js", "text/javascript; charset=utf-8"),
    // an agent's colour: the agent template's, linked (templates/agent/site)
    ("agent-color.js", "text/javascript; charset=utf-8"),
    // its icons (Lucide's among them: vendor/lucide-LICENSE) and an app's
    ("icons.js", "text/javascript; charset=utf-8"),
    ("tooltips.js", "text/javascript; charset=utf-8"),
    // Settings' Billing: a seat, credit, an org (docs/billing.md)
    ("billing.js", "text/javascript; charset=utf-8"),
    ("vendor/split-grid.js", "text/javascript; charset=utf-8"),
    ("manifest.webmanifest", "application/manifest+json"),
    ("icon.svg", "image/svg+xml"),
    // the desktop's and the launcher's light and dark landscapes (cell/shell/CREDITS.md)
    ("wallpaper.jpg", "image/jpeg"),
    ("wallpaper-dark.jpg", "image/jpeg"),
    // every agent's image, tinted to its colour (shell.css; CREDITS.md)
    ("agent.png", "image/png"),
    // the operators' admin page (`/admin`)
    ("admin.js", "text/javascript; charset=utf-8"),
    ("admin.css", "text/css; charset=utf-8"),
];

/// A file the shell publishes, by its name, and its content type.
#[cfg(feature = "platform")]
pub fn shell_published(name: &str) -> Option<(&'static File, &'static str)> {
    let (_, content_type) = SHELL_PUBLISHED.iter().find(|(n, _)| *n == name)?;
    Some((shell(name).expect("every file the shell publishes is indexed (a test)"), content_type))
}

#[cfg(feature = "platform")]
pub mod blessed;

/// Every file the cell may read from its Static Assets, each once: the
/// blessed and catalog templates', the shell's and the agent docs.
#[cfg(feature = "platform")]
pub fn release_files() -> Vec<&'static File> {
    let templates = blessed::BLESSED.iter().filter_map(|n| blessed::template(n)).chain(CATALOG);
    let all = templates.flat_map(|t| t.files.iter()).chain(SHELL).chain([&SKILL_MD, &GUIDE_MD]);
    let mut seen = std::collections::BTreeSet::new();
    all.filter(|f| seen.insert(f.sha256)).collect()
}

/// Writes every file of `release_files` into `dir`, each named by its
/// SHA-256 (as cell/src/assets.rs reads it), and removes anything else
/// there: the directory a cell build uploads as its Static Assets
/// (cell/wrangler.jsonc, `assets`). Each file's bytes are checked against
/// its hash first. Answers how many files, and their bytes.
#[cfg(all(feature = "embed", feature = "platform"))]
pub fn write_assets(dir: &std::path::Path) -> std::io::Result<(usize, u64)> {
    use sha2::{Digest, Sha256};
    use std::fs;
    let files = release_files();
    let tmp = dir.with_extension("tmp");
    fs::create_dir_all(dir)?;
    fs::create_dir_all(&tmp)?;
    let mut total = 0u64;
    for f in &files {
        assert_eq!(hex::encode(Sha256::digest(f.bytes)), f.sha256, "{}: its bytes are its hash's", f.path);
        assert_eq!(f.bytes.len() as u64, f.size, "{}: its bytes are its size", f.path);
        total += f.size;
        let at = dir.join(f.sha256);
        // named by its hash: a file there of its size is its bytes
        if fs::metadata(&at).is_ok_and(|m| m.len() == f.size) {
            continue;
        }
        // whole or not at all: a node watching the directory never reads half a file
        let part = tmp.join(f.sha256);
        fs::write(&part, f.bytes)?;
        fs::rename(&part, &at)?;
    }
    fs::remove_dir_all(&tmp)?;
    let names: std::collections::BTreeSet<&str> = files.iter().map(|f| f.sha256).collect();
    // bounded: what the directory holds
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(|n| names.contains(n)) {
            match entry.file_type()?.is_dir() {
                true => fs::remove_dir_all(entry.path())?,
                false => fs::remove_file(entry.path())?,
            }
        }
    }
    Ok((files.len(), total))
}

#[cfg(all(test, feature = "platform"))]
#[path = "../../../images/hermes/boot/src/skill_references.rs"]
mod skill_references;

#[cfg(all(test, feature = "platform"))]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// A template's file's bytes, as the repo holds them.
    pub(crate) fn bytes_of(t: &Template, f: &File) -> Vec<u8> {
        std::fs::read(repo().join("templates").join(t.name).join(f.path)).unwrap_or_else(|e| panic!("templates/{}/{}: {e}", t.name, f.path))
    }

    /// Goal: the index is the files: what the cell names by hash is what
    /// the build uploads and the repo holds, and a template's release is
    /// the hash of its paths and bytes it always was (a fragment's
    /// installed release, and its code's identity, do not change with
    /// where the bytes live). Method: every file of every template, the
    /// shell and the docs read from the repo, hashed and measured; each
    /// release made again as it was made at run time; every file under
    /// each directory listed, in path order.
    #[test]
    fn the_index_is_the_files() {
        let check = |what: &str, f: &File, bytes: &[u8]| {
            assert_eq!(hex::encode(Sha256::digest(bytes)), f.sha256, "{what}: its hash");
            assert_eq!(bytes.len() as u64, f.size, "{what}: its size");
            #[cfg(feature = "embed")]
            assert_eq!(f.bytes, bytes, "{what}: its embedded bytes");
        };
        for t in ALL {
            let mut release = Sha256::new();
            for f in t.files {
                let bytes = bytes_of(t, f);
                check(&format!("templates/{}/{}", t.name, f.path), f, &bytes);
                release.update(f.path.as_bytes());
                release.update([0]);
                release.update(&bytes);
                release.update([0]);
            }
            assert_eq!(t.release, hex::encode(&release.finalize()[..12]), "{}'s release", t.name);
            assert!(t.files.windows(2).all(|w| w[0].path < w[1].path), "{}'s files are in path order", t.name);
            assert!(t.files.iter().all(|f| t.file(f.path) == Some(f)), "{}'s files are found by path", t.name);
            assert!(!t.files.iter().any(|f| f.path.starts_with("src/") || f.path.ends_with(".DS_Store")), "{} carries no sources", t.name);
            // the cell commits each at its path (publish.rs `stamped`)
            assert!(t.files.iter().all(|f| fragment_proto::RepoPath::parse(f.path).is_ok()), "{}'s files are at repo paths", t.name);
        }
        for f in SHELL {
            check(&format!("cell/shell/{}", f.path), f, &std::fs::read(repo().join("cell/shell").join(f.path)).unwrap());
            assert_eq!(shell(f.path), Some(f));
        }
        assert!(SHELL.windows(2).all(|w| w[0].path < w[1].path), "the shell's files are in path order");
        check("cli/SKILL.md", &SKILL_MD, &std::fs::read(repo().join("cli/SKILL.md")).unwrap());
        check("cli/GUIDE.md", &GUIDE_MD, &std::fs::read(repo().join("cli/GUIDE.md")).unwrap());
        assert!(shell("index.html").is_some() && shell("shell.js").is_some() && shell("nope.js").is_none());
        let count = |dir: &str| walkdir(&repo().join(dir));
        assert_eq!(SHELL.len(), count("cell/shell"), "every file of the shell is indexed");
    }

    /// Goal: the agent docs name no host of their own: a deployment's
    /// fills its own in, and none says fragment.club or invite-only. Valid:
    /// each, filled, names that platform and its fragments, with no blank
    /// left; the secrets' `{{NAME}}` a job's fetch fills is no blank.
    /// Invalid: a doc that writes a host in.
    #[test]
    fn the_agent_docs_name_the_platform_that_shows_them() {
        let read = |path: &str| std::fs::read_to_string(repo().join(path)).unwrap();
        let (skill, guide) = (read("cli/SKILL.md"), read("cli/GUIDE.md"));
        for (path, text) in [("cli/SKILL.md", &skill), ("cli/GUIDE.md", &guide)] {
            for gone in ["fragment.club", "fragment.boats", "invite-only", "finite.chat", "finite.place"] {
                assert!(!text.contains(gone), "{path} names {gone}: it fills in its host");
            }
            assert!(text.contains(PLATFORM_BLANK) && text.contains(FRAGMENT_BLANK), "{path} has both blanks");
            let filled = agent_doc(text, "https://finite.chat", "https://<name>.finite.place");
            assert!(!filled.contains(PLATFORM_BLANK) && !filled.contains(FRAGMENT_BLANK), "{path}: a blank left");
            assert!(filled.contains("fragment login --host https://finite.chat") && filled.contains("https://<name>.finite.place"), "{path}");
        }
        let filled = agent_doc(&skill, "http://127.0.0.1:8790", "http://<name>.fragment.localhost:8790");
        assert!(filled.contains("http://127.0.0.1:8790/llms-full.txt"), "the short doc points to the long one, on its own platform");
        assert!(agent_doc(&guide, "https://p", "https://f").contains("{{NAME}}"), "a secret's placeholder stays");
        assert_eq!(agent_doc("sign in at {{platform}}; see {{fragment}}/", "https://a", "https://<name>.b"), "sign in at https://a; see https://<name>.b/");
    }

    /// Goal: the shell publishes its own files and nothing else of
    /// cell/shell/. Method: every file it names is indexed, with its type;
    /// its pages, credits and licenses are indexed but not published.
    #[test]
    fn the_shell_publishes_its_files_alone() {
        for (name, content_type) in SHELL_PUBLISHED {
            assert_eq!(shell_published(name), Some((shell(name).unwrap(), content_type)), "{name}");
        }
        for name in ["index.html", "admin.html", "CREDITS.md", "vendor/lucide-LICENSE", "vendor/split-grid.LICENSE.txt"] {
            assert!(shell(name).is_some() && shell_published(name).is_none(), "{name}");
        }
        assert!(shell_published("nope.js").is_none() && shell_published("../templates/chat/app.mjs").is_none());
    }

    fn walkdir(dir: &std::path::Path) -> usize {
        let files = std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name() != ".DS_Store");
        files.map(|e| if e.path().is_dir() { walkdir(&e.path()) } else { 1 }).sum()
    }

    /// Goal: what a cell build uploads is what the cell may read, once
    /// each, and nothing else. Method: the blessed and catalog templates',
    /// the shell's and the docs' files are all there; a file two hold (the
    /// agent's image) is one; a template the cell does not serve adds only
    /// what another it serves holds too (notes' viewer is the brain's).
    #[test]
    fn the_release_files_are_what_the_cell_reads() {
        let files = release_files();
        let hashes: std::collections::BTreeSet<&str> = files.iter().map(|f| f.sha256).collect();
        assert_eq!(hashes.len(), files.len(), "each once");
        for t in blessed::BLESSED.iter().filter_map(|n| blessed::template(n)).chain(CATALOG) {
            assert!(t.files.iter().all(|f| hashes.contains(f.sha256)), "{}", t.name);
        }
        assert!(SHELL.iter().chain([&SKILL_MD, &GUIDE_MD]).all(|f| hashes.contains(f.sha256)));
        assert!(NOTES.files.iter().any(|f| f.path == "app.mjs" && !hashes.contains(f.sha256)), "notes is the CLI's");
        assert!(NOTES.files.iter().filter(|f| f.path.starts_with("site/assets/")).all(|f| hashes.contains(f.sha256)), "its viewer is the brain's");
        assert_eq!(CATALOG.map(|t| t.name), ["blank", "todo", "inbox", "calories", "doc"]);
    }

    /// A doc's text with each run of whitespace one space, so a list it
    /// wraps reads as one line.
    fn flat(path: &str) -> String {
        std::fs::read_to_string(repo().join(path)).unwrap().split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Goal: the docs name the templates `fragment create` takes as the
    /// cell takes them: the catalog, copied in, and the blessed ones, not
    /// the CLI's own set (`fragment new --list`, with `notes`). Method:
    /// each doc's list, as it writes it, made from `CATALOG` and
    /// `BLESSED`. Invalid: a template added to either and to no doc.
    #[test]
    fn the_docs_name_the_templates_create_takes() {
        let ticked = |names: &[&str]| names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
        let catalog = CATALOG.map(|t| t.name);
        let (guide, skill, api) = (flat("cli/GUIDE.md"), flat("cli/SKILL.md"), flat("docs/api.md"));
        assert!(guide.contains(&format!("one of {} is copied in", ticked(&catalog))), "cli/GUIDE.md names the catalog");
        assert!(guide.contains(&format!("a blessed one, {}, runs", ticked(&blessed::BLESSED))), "cli/GUIDE.md names the blessed templates");
        assert!(skill.contains(&format!("one with no folder: {}", catalog.join("|"))), "cli/SKILL.md names the catalog");
        assert!(api.contains(&format!("With `template` ({}; any other is 400", ticked(&catalog))), "docs/api.md names the catalog");
        assert!(api.contains(&format!("{} are blessed", ticked(&blessed::BLESSED))), "docs/api.md names the blessed templates");
        assert!(!catalog.contains(&"notes") && NOTES.name == "notes", "notes is the CLI's alone");
    }

    /// Goal: the chat template's README names every file of its page.
    /// Method: each file under `site/` is in it, in backticks.
    #[test]
    fn the_chat_readme_names_its_pages_files() {
        let readme = flat("templates/chat/README.md");
        let chat = blessed::template("chat").expect("chat is blessed");
        for f in chat.files.iter().filter_map(|f| f.path.strip_prefix("site/")) {
            assert!(readme.contains(&format!("`{f}`")), "templates/chat/README.md does not name site/{f}");
        }
    }

    /// Goal: the assets directory a build writes is the release's files by
    /// hash, alone, and writing it again is safe. Method: a stale file and a
    /// directory in it are removed; every file is there under its hash with
    /// its bytes; a second write changes nothing.
    #[cfg(feature = "embed")]
    #[test]
    fn the_assets_directory_is_the_release_files() {
        let dir = std::env::temp_dir().join(format!("fragment-assets-{}", std::process::id())).join("assets");
        std::fs::create_dir_all(dir.join("old")).unwrap();
        std::fs::write(dir.join("stale"), b"x").unwrap();
        let (n, bytes) = write_assets(&dir).unwrap();
        let files = release_files();
        assert_eq!(n, files.len());
        assert_eq!(bytes, files.iter().map(|f| f.size).sum::<u64>());
        assert!(!dir.join("stale").exists() && !dir.join("old").exists(), "nothing else stays");
        for f in &files {
            assert_eq!(std::fs::read(dir.join(f.sha256)).unwrap(), f.bytes, "{}", f.path);
        }
        assert_eq!(write_assets(&dir).unwrap(), (n, bytes));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), n);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// Every text file beneath a managed skill resolves its references
    /// through the release's files, just as Hermes' skill_view does.
    #[test]
    fn every_managed_skill_reference_exists() {
        let mut missing = std::collections::BTreeSet::new();
        for skill in SKILLS.files.iter().filter(|f| f.path.ends_with("/SKILL.md")) {
            let root = skill.path.strip_suffix("SKILL.md").unwrap();
            for f in SKILLS.files.iter().filter(|f| f.path.starts_with(root)) {
                if let Ok(text) = String::from_utf8(bytes_of(&SKILLS, f)) {
                    for reference in super::skill_references::paths(&text) {
                        let target = format!("{root}{reference}");
                        if !SKILLS.files.iter().any(|f| f.path == target || f.path.starts_with(&format!("{}/", target.trim_end_matches('/')))) {
                            missing.insert(format!("{} mentions missing {reference}", f.path));
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "{}", missing.into_iter().collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn every_template_has_a_manifest() {
        for t in ALL {
            assert!(t.file("fragment.json").is_some(), "{} has no fragment.json", t.name);
        }
    }

    /// A template an author starts from (`fragment new`) has a manifest the
    /// platform takes, and says what each of its operations does, as the
    /// guide asks of every author's.
    #[cfg(feature = "embed")]
    #[test]
    fn a_scaffolds_operations_say_what_they_do() {
        mod names {
            include!("blessed_names.rs");
        }
        for t in ALL.iter().filter(|t| !names::BLESSED.contains(&t.name)) {
            let bytes = t.file("fragment.json").expect("every template has one").bytes;
            let m = fragment_core::manifest::parse(bytes).unwrap_or_else(|e| panic!("{}'s fragment.json: {e}", t.name));
            let undescribed: Vec<&String> = m.operations.iter().filter(|(_, d)| d.description.is_none()).map(|(op, _)| op).collect();
            assert!(undescribed.is_empty(), "{} leaves {undescribed:?} undescribed", t.name);
        }
    }

    /// Goal: the doc template's notes hold together, so a fragment that
    /// copies them gets what the README says. Valid: its manifest's
    /// operations are exactly the verbs `applib/notes.mjs` defines and
    /// guards, each public and described, beside the `notes` channel it
    /// publishes to, which takes no posts; `app.mjs` takes the mixin; the
    /// page marks its column and blocks and loads the notes, cursors off;
    /// the README names every verb. Invalid: a verb renamed in one place
    /// and not the others.
    #[cfg(feature = "embed")]
    #[test]
    fn the_doc_templates_notes_hold_together() {
        use fragment_proto::Role;
        let text = |path: &str| std::str::from_utf8(DOC.file(path).unwrap_or_else(|| panic!("doc has {path}")).bytes).expect("utf-8");
        let m = fragment_core::manifest::parse(text("fragment.json").as_bytes()).expect("doc's fragment.json");
        let verbs = ["notes", "annotate", "resolve", "remove_note"];
        let declared: Vec<&str> = m.operations.keys().map(String::as_str).collect();
        let mut sorted = verbs.to_vec();
        sorted.sort_unstable();
        assert_eq!(declared, sorted, "doc declares the notes' verbs, and only them");
        for (name, op) in &m.operations {
            assert_eq!(op.role, Role::Public, "{name}: whoever opens the page; the app decides who writes");
            assert!(op.description.as_deref().is_some_and(|d| !d.is_empty()), "{name} is described");
        }
        let notes = m.channels.get("notes").expect("the notes channel");
        assert!(notes.read == Role::Public && notes.post.is_none(), "the app alone writes the notes channel, which its readers read");
        let mixin = text("applib/notes.mjs");
        assert!(mixin.contains(&format!("const VERBS = {verbs:?};")), "the mixin guards the same verbs");
        for verb in verbs {
            assert!(mixin.contains(&format!("\n    {verb}(")), "the mixin defines {verb}");
            assert!(text("README.md").contains(&format!("`{verb}`")), "the README names {verb}");
        }
        assert!(mixin.contains("call.publish(\"notes\""), "its changes go to the notes channel");
        assert!(text("app.mjs").contains("extends withNotes(DurableObject)"), "app.mjs takes the mixin");
        let page = text("site/index.html");
        for wanted in ["<main data-notes>", "class=\"notes-page\"", "./notes.js", "./notes.css", "./__fragment.css", "data-a=\"lede\""] {
            assert!(page.contains(wanted), "the page has {wanted}");
        }
        assert!(page.contains("<html lang=\"en\">"), "cursors are off by default (the e2e turns them on by this tag)");
    }
}
