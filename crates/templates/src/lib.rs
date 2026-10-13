//! The files the release serves from the cell's Static Assets: the
//! templates under `templates/` (`fragment new` scaffolds them, and the
//! cell makes a fragment from one: docs/api.md, Control API), the shell's
//! files (`cell/shell/`), and the agent docs (`cli/SKILL.md`,
//! `cli/GUIDE.md`, the platform's `/llms.txt` and `/llms-full.txt`).
//!
//! The cell holds only their index: each file's path, SHA-256 and size,
//! and each template's release (build.rs), and reads a file's bytes from
//! its Static Assets by its hash (cell/src/assets.rs), so an isolate pays
//! for a file only when it reads one. The `embed` feature adds each file's
//! bytes (the CLI scaffolds them, xtask writes them into the assets
//! directory a cell build uploads: `write_assets`).

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

/// The templates a fragment can start from, the simplest first (a create
/// that names none of them lists them in this order), each copied into
/// the fragment's repo (cell/src/publish.rs). `notes` stays with the CLI
/// (`fragment new --template notes`).
pub static CATALOG: [&Template; 4] = [&BLANK, &TODO, &INBOX, &CALORIES];

/// A file of cell/shell/ by its path there: a published one, or a page
/// (`index.html`, `admin.html`).
pub fn shell(name: &str) -> Option<&'static File> {
    SHELL.binary_search_by(|f| f.path.cmp(name)).ok().map(|i| &SHELL[i])
}

/// The shell's files it publishes at `/__shell/<name>` (cell/src/shell.rs),
/// each with its content type: of the rest of cell/shell/, its pages are
/// served at their own paths, and its credits and licenses not at all.
const SHELL_PUBLISHED: [(&str, &str); 16] = [
    ("shell.js", "text/javascript; charset=utf-8"),
    ("shell.css", "text/css; charset=utf-8"),
    ("layout.js", "text/javascript; charset=utf-8"),
    ("viewer.js", "text/javascript; charset=utf-8"),
    ("agent-identity.js", "text/javascript; charset=utf-8"),
    ("app-icons.js", "text/javascript; charset=utf-8"),
    ("lucide-icons.js", "text/javascript; charset=utf-8"),
    ("tooltips.js", "text/javascript; charset=utf-8"),
    // Settings' Billing: a seat, credit, an org (docs/billing.md)
    ("billing.js", "text/javascript; charset=utf-8"),
    ("vendor/split-grid.js", "text/javascript; charset=utf-8"),
    ("manifest.webmanifest", "application/manifest+json"),
    ("icon.svg", "image/svg+xml"),
    // the viewer's wallpaper: Teo Badini's photograph on Pexels (cell/shell/CREDITS.md)
    ("wallpaper.jpg", "image/jpeg"),
    // every agent's image, tinted to its colour (shell.css; CREDITS.md)
    ("agent.png", "image/png"),
    // the operators' admin page (`/admin`)
    ("admin.js", "text/javascript; charset=utf-8"),
    ("admin.css", "text/css; charset=utf-8"),
];

/// A file the shell publishes, by its name, and its content type.
pub fn shell_published(name: &str) -> Option<(&'static File, &'static str)> {
    let (_, content_type) = SHELL_PUBLISHED.iter().find(|(n, _)| *n == name)?;
    Some((shell(name).expect("every file the shell publishes is indexed (a test)"), content_type))
}

pub mod blessed;

/// Every file the cell may read from its Static Assets, each once: the
/// blessed and catalog templates', the shell's and the agent docs.
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
#[cfg(feature = "embed")]
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

#[cfg(test)]
#[path = "../../../images/hermes/boot/src/skill_references.rs"]
mod skill_references;

#[cfg(test)]
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
        assert_eq!(CATALOG.map(|t| t.name), ["blank", "todo", "inbox", "calories"]);
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
}
