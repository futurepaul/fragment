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
    for name in ["basic", "vault", "gen", "todo", "inbox"] {
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

    // notes (the Rust cell's vault) = its own files + the vault's built
    // viewer, served from site/assets/ instead of assets/
    let mut notes = Vec::new();
    collect(&tdir.join("notes"), &tdir, "notes", &mut notes);
    let mut viewer = Vec::new();
    collect(&tdir.join("vault/assets"), &tdir, "vault", &mut viewer);
    let mut files: Vec<(String, String)> = notes.into_iter().map(|(rel, origin)| (rel.clone(), format!("{origin}/{rel}"))).collect();
    files.extend(viewer.into_iter().map(|(rel, origin)| (format!("site/{rel}"), format!("{origin}/{rel}"))));
    files.sort();

    // (scaffold path, source path under templates/)
    let mut sourced: Vec<(&str, Vec<(String, String)>)> =
        groups.iter().map(|(name, files)| (*name, files.iter().map(|(rel, origin)| (rel.clone(), format!("{origin}/{rel}"))).collect())).collect();
    sourced.push(("notes", files));

    let mut src = String::from("#[allow(clippy::type_complexity)] // generated table of template files\npub static TEMPLATES: &[(&str, &[(&str, &[u8])])] = &[\n");
    for (name, files) in &sourced {
        src.push_str(&format!("    ({name:?}, &[\n"));
        for (rel, source) in files {
            src.push_str(&format!(
                "        ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../templates/{source}\"))),\n"
            ));
        }
        src.push_str("    ]),\n");
    }
    src.push_str("];\n");
    fs::write(&dest, src).expect("write templates.rs");
}
