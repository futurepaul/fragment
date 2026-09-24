// Generates OUT_DIR/templates.rs: one static per directory under
// templates/ (every file but src/, which holds sources of committed
// bundles) and `ALL`. A binary that names only some statics links only
// those (the cell leaves out `notes`).
use std::{env, fs, path::Path};

fn collect(dir: &Path, root: &Path, out: &mut Vec<String>) {
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
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let tdir = Path::new(&manifest).join("../../templates");
    println!("cargo:rerun-if-changed={}", tdir.display());
    let mut names: Vec<String> = fs::read_dir(&tdir)
        .expect("templates/")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    let mut src = String::new();
    for name in &names {
        let mut files = Vec::new();
        collect(&tdir.join(name), &tdir.join(name), &mut files);
        files.sort();
        src.push_str(&format!("pub static {}: Template = &[\n", name.to_uppercase()));
        for rel in files {
            src.push_str(&format!("    ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../templates/{name}/{rel}\"))),\n"));
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
