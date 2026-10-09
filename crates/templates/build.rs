// Generates OUT_DIR/templates.rs: the files the release serves from the
// cell's Static Assets, as an index of each one's path, SHA-256 and size
// (the cell reads a file's bytes by its hash: cell/src/assets.rs), and,
// with the `embed` feature, their bytes (the CLI, xtask, which writes them
// into the assets, and the e2e): one `Template` per directory under
// templates/ (every file but src/, which holds sources of committed
// bundles), and `ALL`. Each template's release, a hash of its paths and
// bytes, is made here, so a cell that never holds the bytes names it all
// the same.
//
// A template may hold another's files through a symlink (the brain's
// viewer is the notes template's, `templates/brain/site/assets`; the
// chat's agent.png is the agent's): one asset, and embedded, its bytes
// are in a binary once. A symlink that leaves templates/ fails the build.
use std::collections::BTreeMap;
use std::{env, fs, path::Path, path::PathBuf};

use sha2::{Digest, Sha256};

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "src" || name == ".DS_Store" {
            continue;
        }
        if p.is_dir() {
            collect(&p, root, out);
        } else {
            out.push((p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"), p));
        }
    }
}

/// The files the generated code names: each distinct real file once, as
/// the static that embeds it.
struct Statics {
    ids: BTreeMap<PathBuf, usize>,
}

impl Statics {
    fn id(&mut self, real: PathBuf) -> usize {
        let next = self.ids.len();
        *self.ids.entry(real).or_insert(next)
    }
}

/// One file's index entry (and, embedded, its bytes), as Rust.
fn entry(path: &str, bytes: &[u8], id: usize, embed: bool) -> String {
    let sha = hex::encode(Sha256::digest(bytes));
    let embedded = if embed { format!(", bytes: B{id}") } else { String::new() };
    format!("File {{ path: {path:?}, sha256: {sha:?}, size: {}{embedded} }}", bytes.len())
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let embed = env::var_os("CARGO_FEATURE_EMBED").is_some();
    let repo = fs::canonicalize(Path::new(&manifest).join("../..")).expect("the repo");
    let tdir = repo.join("templates");
    println!("cargo:rerun-if-changed={}", tdir.display());
    let mut names: Vec<String> = fs::read_dir(&tdir)
        .expect("templates/")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    let mut statics = Statics { ids: BTreeMap::new() };
    let mut src = String::new();
    for name in &names {
        let mut files = Vec::new();
        collect(&tdir.join(name), &tdir.join(name), &mut files);
        files.sort();
        let mut release = Sha256::new();
        let mut entries = Vec::new();
        for (rel, path) in files {
            let real = fs::canonicalize(&path).unwrap_or_else(|e| panic!("templates/{name}/{rel}: {e}"));
            assert!(real.starts_with(&tdir), "templates/{name}/{rel} is a symlink out of templates/");
            let bytes = fs::read(&real).unwrap_or_else(|e| panic!("templates/{name}/{rel}: {e}"));
            // which release of the template a fragment runs (blessed.rs `release`)
            release.update(rel.as_bytes());
            release.update([0]);
            release.update(&bytes);
            release.update([0]);
            entries.push(entry(&rel, &bytes, statics.id(real), embed));
        }
        let release = hex::encode(&release.finalize()[..12]);
        src.push_str(&format!("pub static {}: Template = Template {{\n    name: {name:?},\n    release: {release:?},\n    files: &[\n", name.to_uppercase()));
        for e in entries {
            src.push_str(&format!("        {e},\n"));
        }
        src.push_str("    ],\n};\n");
    }
    src.push_str("pub static ALL: &[&Template] = &[\n");
    for name in &names {
        src.push_str(&format!("    &{},\n", name.to_uppercase()));
    }
    src.push_str("];\n");
    if embed {
        for (real, id) in &statics.ids {
            src.push_str(&format!("static B{id}: &[u8] = include_bytes!({:?});\n", real.display().to_string()));
        }
    }
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("templates.rs"), src).expect("write templates.rs");
}
