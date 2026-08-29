//! `fragment build` — compile a fragment folder authored as a normal web
//! app into the shape the platform serves. TypeScript sources become
//! runnable files, import specifiers get the extensions the runtime's
//! module loader expects, and site assets get content-hashed names with
//! their references rewritten (immutable caching). A parse gate refuses to
//! produce broken output — the "served bytes must parse" promise, enforced
//! at build time instead of discovered by users.
//!
//! The compiled outputs are plain, inspectable files, committed to the
//! folder and synced like any other content: same contract as the
//! platform's own `runtime/src` (generated, but readable).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_transformer::TransformOptions;

pub fn run(dir: &Path) -> Result<()> {
    let mut built = BuildReport::default();
    compile_typescript(dir, &mut built)?;
    // gate BEFORE hashing: a broken file must fail the build before any
    // hashed artifact of it exists
    parse_gate(dir, &mut built)?;
    hash_site_assets(dir, &mut built)?;
    built.print();
    Ok(())
}

#[derive(Default)]
struct BuildReport {
    compiled: Vec<String>,
    hashed: Vec<(String, String)>,
    gated: usize,
}

impl BuildReport {
    fn print(&self) {
        if !self.compiled.is_empty() {
            println!("  compiled (ts -> js): {}", self.compiled.len());
            for f in &self.compiled {
                println!("    {f}");
            }
        }
        for (from, to) in &self.hashed {
            println!("  hashed: {from} -> {to}");
        }
        if self.compiled.is_empty() && self.hashed.is_empty() {
            println!("  nothing to build (no .ts sources, no unhashed site assets)");
        }
        println!("  parse gate: {} served files ok", self.gated);
    }
}

/// Files/dirs that never take part in a build.
fn skip(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == ".fragment" || s == "node_modules" || s == ".git" || s.starts_with(".celld")
    })
}

/// One TypeScript source -> stripped JavaScript, via oxc. Returns the
/// emitted text.
fn strip_types(rel: &str, src: &str) -> Result<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(rel).unwrap_or_default().with_typescript(true);
    let parsed = Parser::new(&allocator, src, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        let first = parsed.diagnostics.first().map(|e| e.to_string()).unwrap_or_default();
        bail!("{rel}: parse error: {first}");
    }
    let mut program = parsed.program;
    // TS stripping needs scope information: semantic pass, then transform
    let semantic = oxc_semantic::SemanticBuilder::new().build(&program);
    let scoping = semantic.semantic.into_scoping();
    let source_path = std::path::Path::new(rel);
    let transformer = oxc_transformer::Transformer::new(
        &allocator,
        source_path,
        &TransformOptions::default(),
    );
    let ret = transformer.build_with_scoping(scoping, &mut program);
    if !ret.diagnostics.is_empty() {
        let first = ret.diagnostics.first().map(|e| e.to_string()).unwrap_or_default();
        bail!("{rel}: transform error: {first}");
    }
    Ok(Codegen::new().build(&program).code)
}

/// `./util.ts` -> `./util.mjs`, `"./x.ts"` -> `"./x.mjs"`: emitted sibling
/// files keep their stem; the loader's module map is keyed by real paths,
/// so the specifier must name the compiled file.
fn fix_relative_specifiers(src: &str) -> String {
    fn sub(spec: &str) -> String {
        if let Some(stem) = spec.strip_suffix(".ts") {
            format!("{stem}.mjs")
        } else if let Some(stem) = spec.strip_suffix(".tsx") {
            format!("{stem}.mjs")
        } else {
            spec.to_string()
        }
    }
    let re = |pat: &str| -> String { pat.to_string() };
    let _ = re;
    // covering: from "…", import "…", import("…"), export … from "…"
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            let spec = &src[start..j.min(src.len())];
            let fixed = if spec.starts_with('.') && (spec.ends_with(".ts") || spec.ends_with(".tsx")) {
                sub(spec)
            } else {
                spec.to_string()
            };
            out.push(quote as char);
            out.push_str(&fixed);
            if j < bytes.len() {
                out.push(quote as char);
            }
            i = j + 1;
        } else {
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn compile_typescript(dir: &Path, report: &mut BuildReport) -> Result<()> {
    let mut sources: Vec<PathBuf> = Vec::new();
    collect_ts(dir, dir, &mut sources)?;
    for src in sources {
        let rel = src.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
        if rel.ends_with(".d.ts") {
            continue; // declarations are for the editor, not the runtime
        }
        let text = std::fs::read_to_string(&src).with_context(|| format!("read {rel}"))?;
        let emitted = strip_types(&rel, &text)?;
        let emitted = fix_relative_specifiers(&emitted);
        let out_rel = if rel == "app.ts" {
            "app.mjs".to_string()
        } else if let Some(stem) = rel.strip_suffix(".ts") {
            format!("{stem}.mjs")
        } else if let Some(stem) = rel.strip_suffix(".tsx") {
            bail!("{rel}: .tsx is not supported yet — author jsx-free or wait for the jsx step");
        } else {
            unreachable!()
        };
        let out = dir.join(&out_rel);
        std::fs::write(&out, emitted).with_context(|| format!("write {out_rel}"))?;
        // the .ts source STAYS beside its compiled sibling: the folder is
        // the author's project and the source of truth — builds are
        // repeatable, editors keep their types, and the runtime only ever
        // loads the compiled path the manifest names
        report.compiled.push(out_rel);
    }
    Ok(())
}

fn collect_ts(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if skip(&path.strip_prefix(root).unwrap_or(&path)) {
            continue;
        }
        if path.is_dir() {
            collect_ts(root, &path, out)?;
        } else if name.ends_with(".ts") || name.ends_with(".tsx") {
            out.push(path);
        }
    }
    Ok(())
}

fn sha8(bytes: &[u8]) -> String {
    let d = <sha2::Sha256 as sha2::Digest>::digest(bytes);
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn already_hashed(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    let before_ext = base.split('.').collect::<Vec<_>>();
    // name.<8hex>.ext or name.<7-8hex>.ext — the platform's hashInName shape
    before_ext.len() >= 3
        && before_ext[before_ext.len() - 2].len() == 8
        && before_ext[before_ext.len() - 2].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Content-hash every buildable site asset and rewrite references in
/// site html + js. The un-hashed file stays as the rebuildable source
/// (idempotent: `fragment build` twice produces the same pair).
fn hash_site_assets(dir: &Path, report: &mut BuildReport) -> Result<()> {
    let site = dir.join("site");
    if !site.is_dir() {
        return Ok(());
    }
    let mut renames: BTreeMap<String, String> = BTreeMap::new(); // "/old" -> "/new"
    let mut assets: Vec<PathBuf> = Vec::new();
    collect_assets(&site, &mut assets)?;
    for asset in &assets {
        let rel = asset.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap().to_string();
        if already_hashed(&name) {
            continue;
        }
        let bytes = std::fs::read(asset)?;
        let h = sha8(&bytes);
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), e.to_string()),
            None => (name.clone(), String::new()),
        };
        let hashed_name = if ext.is_empty() { format!("{stem}.{h}") } else { format!("{stem}.{h}.{ext}") };
        let hashed_rel = format!("site/{hashed_name}");
        std::fs::write(dir.join(&hashed_rel), &bytes)?;
        // references may spell the asset with either extension — an
        // index.html says /main.js while the compiled sibling is main.mjs;
        // both spellings map to the hashed name
        let mut variants = vec![name.clone()];
        if let Some((stem, ext)) = name.rsplit_once('.') {
            let other = if ext == "js" { "mjs" } else { "js" };
            variants.push(format!("{stem}.{other}"));
        }
        for v in variants {
            let (url_old, dir_old) = (format!("/{v}"), format!("./{v}"));
            if v == name {
                renames.insert(url_old, format!("/{}", hashed_name));
                renames.insert(dir_old, format!("./{}", hashed_name));
            } else if !dir.join("site").join(&v).exists() {
                // only remap the alternate spelling when no real file
                // claims it — never shadow a genuine asset
                renames.insert(url_old, format!("/{}", hashed_name));
                renames.insert(dir_old, format!("./{}", hashed_name));
            }
        }
        report.hashed.push((rel, hashed_rel));
    }
    if renames.is_empty() {
        return Ok(());
    }
    // rewrite references across site html + js
    let mut texts: Vec<PathBuf> = Vec::new();
    collect_texts(&site, &mut texts)?;
    for t in &texts {
        let mut text = std::fs::read_to_string(t)?;
        let before = text.clone();
        for (old, new) in &renames {
            // quoted occurrences only — never rewrite inside prose
            for q in ['"', '\''] {
                text = text.replace(&format!("{q}{old}{q}"), &format!("{q}{new}{q}"));
            }
        }
        if text != before {
            std::fs::write(t, text)?;
        }
    }
    Ok(())
}

fn collect_assets(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_assets(&path, out)?;
        } else {
            let name = entry.file_name().to_string_lossy().to_string();
            if matches!(name.rsplit('.').next(), Some("js" | "mjs" | "css")) {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn collect_texts(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_texts(&path, out)?;
        } else {
            let name = entry.file_name().to_string_lossy().to_string();
            if matches!(name.rsplit('.').next(), Some("html" | "js" | "mjs" | "css")) {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Everything the browser or the loader will execute must parse. This is
/// the gate that turns "works on my machine" into "deploys at all": a
/// served file with a syntax error fails here, with file and message.
fn parse_gate(dir: &Path, report: &mut BuildReport) -> Result<()> {
    let mut targets: Vec<PathBuf> = Vec::new();
    let app = dir.join("app.mjs");
    if app.is_file() {
        targets.push(app);
    }
    if dir.join("site").is_dir() {
        collect_assets(&dir.join("site"), &mut targets)?;
    }
    for t in targets {
        let rel = t.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&t)?;
        let allocator = Allocator::default();
        let source_type = SourceType::from_path(&rel).unwrap_or_default();
        let parsed = Parser::new(&allocator, &text, source_type).parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            let first = parsed.diagnostics.first().map(|e| e.to_string()).unwrap_or_default();
            bail!("parse gate FAILED: {rel}: {first}\n  fix the syntax error — this file would have served broken otherwise");
        }
        report.gated += 1;
    }
    Ok(())
}
