// Generate the template registry (OUT_DIR/templates.rs): every file under
// templates/<name>/ (minus src/, which holds sources of committed bundles)
// as include_bytes! entries.
use std::{env, fs, path::Path};

const TEMPLATES: [&str; 4] = ["todo", "inbox", "notes", "chat"];

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
    println!("cargo:rerun-if-changed=../templates");
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let tdir = Path::new(&manifest).parent().unwrap().join("templates");
    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("templates.rs");

    let mut src = String::from("#[allow(clippy::type_complexity)] // generated table of template files\npub static TEMPLATES: &[(&str, &[(&str, &[u8])])] = &[\n");
    for name in TEMPLATES {
        let mut files = Vec::new();
        collect(&tdir.join(name), &tdir.join(name), &mut files);
        files.sort();
        src.push_str(&format!("    ({name:?}, &[\n"));
        for rel in files {
            src.push_str(&format!("        ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../templates/{name}/{rel}\"))),\n"));
        }
        src.push_str("    ]),\n");
    }
    src.push_str("];\n");
    fs::write(&dest, src).expect("write templates.rs");
}
