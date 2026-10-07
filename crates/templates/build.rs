// Generates OUT_DIR/templates.rs: one static per directory under
// templates/ (every file but src/, which holds sources of committed
// bundles) and `ALL`. Each distinct file is embedded once, as a static of
// its own that every template holding it names: a template may hold
// another's files through a symlink (the brain's viewer is the notes
// template's, `templates/brain/site/assets`; the chat's agent.png is the
// agent's), and its bytes are in a binary once. A symlink that leaves templates/ fails the build.
use std::collections::BTreeMap;
use std::{env, fs, path::Path, path::PathBuf};

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

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let tdir = Path::new(&manifest).join("../../templates");
    println!("cargo:rerun-if-changed={}", tdir.display());
    let troot = fs::canonicalize(&tdir).expect("templates/");
    let mut names: Vec<String> = fs::read_dir(&tdir)
        .expect("templates/")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    // each file once, by where it really is under templates/
    let mut statics: BTreeMap<String, usize> = BTreeMap::new();
    let mut lists = Vec::new();
    for name in &names {
        let mut files = Vec::new();
        collect(&tdir.join(name), &tdir.join(name), &mut files);
        files.sort();
        let mut list = Vec::new();
        for (rel, path) in files {
            let real = fs::canonicalize(&path).unwrap_or_else(|e| panic!("templates/{name}/{rel}: {e}"));
            let real = real
                .strip_prefix(&troot)
                .unwrap_or_else(|_| panic!("templates/{name}/{rel} is a symlink out of templates/"))
                .to_string_lossy()
                .replace('\\', "/");
            let next = statics.len();
            let id = *statics.entry(real).or_insert(next);
            list.push((rel, id));
        }
        lists.push((name, list));
    }
    let mut src = String::new();
    for (real, id) in &statics {
        src.push_str(&format!("static F{id}: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../templates/{real}\"));\n"));
    }
    for (name, list) in &lists {
        src.push_str(&format!("pub static {}: Template = &[\n", name.to_uppercase()));
        for (rel, id) in list {
            src.push_str(&format!("    ({rel:?}, F{id}),\n"));
        }
        src.push_str("];\n");
    }
    src.push_str("pub static ALL: &[(&str, Template)] = &[\n");
    for name in &names {
        src.push_str(&format!("    ({name:?}, {}),\n", name.to_uppercase()));
    }
    src.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("templates.rs"), src).expect("write templates.rs");
}
