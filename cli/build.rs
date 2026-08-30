// Generate the template registry (OUT_DIR/templates.rs): every file under
// templates/<name>/ (minus src/, package files) as include_bytes! entries.
// "dropzone" is composed: the vault tree plus the dropzone overlay, so the
// viewer ships once in the binary. Paths record each file's origin dir.
use std::{env, fs, path::Path};

// (rel path within the template, origin template dir name)
fn collect(root: &Path, tdir: &Path, origin: &str, out: &mut Vec<(String, String)>) {
    let prefix = tdir.join(origin);
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "node_modules" || name == "src" || name == ".DS_Store" {
            continue;
        }
        if p.is_dir() {
            collect(&p, tdir, origin, out);
        } else {
            let rel = p.strip_prefix(&prefix).unwrap().to_string_lossy().replace('\\', "/");
            if rel == "package.json" || rel == "package-lock.json" {
                continue;
            }
            out.push((rel, origin.to_string()));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=../templates");
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let tdir = Path::new(&manifest).parent().unwrap().join("templates");
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("templates.rs");

    let mut groups: Vec<(&str, Vec<(String, String)>)> = Vec::new();
    for name in ["basic", "vault", "gen"] {
        let mut files = Vec::new();
        collect(&tdir.join(name), &tdir, name, &mut files);
        files.sort();
        groups.push((name, files));
    }
    // dropzone = vault + overlay (overlay wins on same path)
    let mut dz: Vec<(String, String)> = groups.iter().find(|(n, _)| *n == "vault").unwrap().1.clone();
    let mut overlay = Vec::new();
    collect(&tdir.join("dropzone"), &tdir, "dropzone", &mut overlay);
    for (rel, origin) in overlay {
        dz.retain(|(r, _)| *r != rel);
        dz.push((rel, origin));
    }
    dz.sort();
    groups.push(("dropzone", dz));

    let mut src = String::from("pub static TEMPLATES: &[(&str, &[(&str, &[u8])])] = &[\n");
    for (name, files) in &groups {
        src.push_str(&format!("    ({name:?}, &[\n"));
        for (rel, origin) in files {
            src.push_str(&format!(
                "        ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../templates/{origin}/{rel}\"))),\n"
            ));
        }
        src.push_str("    ]),\n");
    }
    src.push_str("];\n");
    fs::write(&dest, src).expect("write templates.rs");
}
