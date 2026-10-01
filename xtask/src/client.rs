//! The browser's way to a computer by its key (docs/runtime-seam.md):
//! sandcastle-web built for wasm32 and packed as the cell's static assets
//! (`cell/client/`, wrangler.jsonc's `assets`). celld serves them on every
//! host before the cell runs, so the module (about 4 MB) never enters an
//! isolate:
//!
//!   /__computer/client.js                        the entry: starts the build below
//!   /__computer/<build>/sandcastle_web.js        wasm-bindgen's glue
//!   /__computer/<build>/sandcastle_web_bg.wasm.gz  the module, gzipped
//!
//! `<build>` is the module's digest, so the files under it never change and
//! a browser keeps them (`_headers`); the entry is revalidated on each load.
//! celld serves an asset's bytes as they are (it compresses nothing), hence
//! the gzip, which `_headers` declares; and it takes every `*.wasm` below the
//! cell into the cell's own bundle, hence the `.gz` name.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use sha2::{Digest, Sha256};

use crate::run;

/// wasm-bindgen as sandcastle/Cargo.lock pins it, which the library's
/// version (xtask/Cargo.toml) must equal: its glue reads that module.
const WASM_BINDGEN: &str = "0.2.127";
/// The path every host serves the client under.
const PREFIX: &str = "__computer";
const GLUE: &str = "sandcastle_web.js";
const MODULE: &str = "sandcastle_web_bg.wasm";
const MODULE_GZ: &str = "sandcastle_web_bg.wasm.gz";
/// A digest's first 16 hex: the build's name.
const BUILD_HEX: usize = 16;

/// Builds the client and writes `cell/client/` whole.
pub fn build() -> Result<()> {
    let root = devstack::repo_root();
    let sandcastle = root.join("sandcastle");
    let pinned = locked_version(&std::fs::read_to_string(sandcastle.join("Cargo.lock"))?, "wasm-bindgen");
    anyhow::ensure!(
        pinned.as_deref() == Some(WASM_BINDGEN),
        "sandcastle/Cargo.lock pins wasm-bindgen {pinned:?}, xtask's library is {WASM_BINDGEN}: move both together"
    );
    let work = root.join("target/client");
    std::fs::create_dir_all(&work)?;
    let clang = Clang::locate(&work)?;
    // sandcastle/.cargo/config.toml picks getrandom's WebCrypto backend
    let mut cargo = Command::new("cargo");
    cargo
        .args(["build", "--quiet", "--release", "--locked", "--target", "wasm32-unknown-unknown", "-p", "sandcastle-web", "--target-dir"])
        .arg(work.join("cargo"))
        .current_dir(&sandcastle)
        .env("CC_wasm32_unknown_unknown", &clang.cc)
        .env("AR_wasm32_unknown_unknown", &clang.ar);
    if let Some(flags) = &clang.cflags {
        cargo.env("CFLAGS_wasm32_unknown_unknown", flags);
    }
    run(&mut cargo)?;
    let input = work.join("cargo/wasm32-unknown-unknown/release/sandcastle_web.wasm");
    let raw = std::fs::read(&input).with_context(|| format!("read {}", input.display()))?;
    let name = hex(&Sha256::digest(&raw))[..BUILD_HEX].to_string();
    let pkg = work.join("pkg").join(&name);
    if !pkg.join(GLUE).is_file() || !pkg.join(MODULE_GZ).is_file() {
        bindgen(&input, &work.join("pkg"), &name)?;
    }
    let out = devstack::client_dir();
    match std::fs::remove_dir_all(&out) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("clear {}", out.display())),
        _ => {}
    }
    let dir = out.join(PREFIX).join(&name);
    std::fs::create_dir_all(&dir)?;
    for f in [GLUE, MODULE_GZ] {
        std::fs::copy(pkg.join(f), dir.join(f)).with_context(|| format!("copy {f}"))?;
    }
    std::fs::write(out.join(PREFIX).join("client.js"), entry(&name))?;
    std::fs::write(out.join("_headers"), headers(&name))?;
    let size = std::fs::metadata(dir.join(MODULE_GZ))?.len();
    println!("computer client {name}: {:.2} MB gzipped", size as f64 / 1e6);
    Ok(())
}

/// wasm-bindgen's glue and the module, gzipped, into `<pkg>/<name>/`
/// (made beside it, then renamed: a half-written build is never taken for
/// one); the builds before it go.
fn bindgen(input: &Path, pkg: &Path, name: &str) -> Result<()> {
    let tmp = pkg.join(format!(".{name}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(input)
        .web(true)?
        .typescript(false)
        .out_name("sandcastle_web")
        .generate(&tmp)
        .context("wasm-bindgen")?;
    let module = std::fs::read(tmp.join(MODULE))?;
    // mtime 0 (flate2's default): the same module gzips to the same bytes
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&module)?;
    std::fs::write(tmp.join(MODULE_GZ), gz.finish()?)?;
    std::fs::remove_file(tmp.join(MODULE))?;
    for old in std::fs::read_dir(pkg)? {
        let old = old?.path();
        if old != tmp {
            std::fs::remove_dir_all(&old).with_context(|| format!("remove {}", old.display()))?;
        }
    }
    std::fs::rename(&tmp, pkg.join(name))?;
    Ok(())
}

/// The entry a page imports: the build's exports, once its module runs.
fn entry(name: &str) -> String {
    format!(
        "// The way to a computer by its key (docs/runtime-seam.md): sandcastle-web,\n\
         // started once per page. Everything under ./{name}/ is immutable.\n\
         import init from \"./{name}/{GLUE}\";\n\
         export * from \"./{name}/{GLUE}\";\n\
         await init({{ module_or_path: new URL(\"./{name}/{MODULE_GZ}\", import.meta.url) }});\n"
    )
}

/// The build's files kept for a year (their path names their bytes), and
/// the module declared for what it is.
fn headers(name: &str) -> String {
    format!(
        "/{PREFIX}/{name}/*\n  Cache-Control: public, max-age=31536000, immutable\n\
         /{PREFIX}/{name}/{MODULE_GZ}\n  Content-Type: application/wasm\n  Content-Encoding: gzip\n"
    )
}

/// A package's version in a Cargo.lock, when it holds exactly one.
fn locked_version(lock: &str, package: &str) -> Option<String> {
    let name = format!("name = \"{package}\"");
    let mut lines = lock.lines();
    let mut found = vec![];
    while let Some(line) = lines.next() {
        if line == name {
            if let Some(v) = lines.next().and_then(|l| l.strip_prefix("version = \"")).and_then(|v| v.strip_suffix('"')) {
                found.push(v.to_string());
            }
        }
    }
    match found.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A C compiler for wasm32: `ring` compiles C for the client, and Apple's
/// clang has no wasm32 target; an LLVM clang does.
struct Clang {
    cc: PathBuf,
    ar: PathBuf,
    /// Flags it needs besides (a Nix clang is told where its headers are).
    cflags: Option<String>,
}

const NO_CLANG: &str = "no C compiler for wasm32 (ring compiles C for the computer client, and Apple's clang has no wasm32 target). \
Install LLVM (macOS: `brew install llvm`; Linux: `apt install clang llvm`), or name one with \
CC_wasm32_unknown_unknown and AR_wasm32_unknown_unknown (and CFLAGS_wasm32_unknown_unknown if it needs flags)";

impl Clang {
    /// The one named in the environment, else the first that compiles for
    /// wasm32 of: Homebrew's LLVM, `clang` and `llvm-ar` (versioned or
    /// not) on the PATH, and Nix's LLVM 19 when Nix is there.
    fn locate(work: &Path) -> Result<Clang> {
        if let Some(cc) = std::env::var_os("CC_wasm32_unknown_unknown") {
            let ar = std::env::var_os("AR_wasm32_unknown_unknown").context("CC_wasm32_unknown_unknown is set, AR_wasm32_unknown_unknown is not")?;
            let named = Clang { cc: cc.into(), ar: ar.into(), cflags: std::env::var("CFLAGS_wasm32_unknown_unknown").ok() };
            named.probe(work).with_context(|| format!("CC_wasm32_unknown_unknown ({}) does not compile for wasm32", named.cc.display()))?;
            return Ok(named);
        }
        let mut tried = vec![];
        for c in Self::candidates().into_iter().chain(Self::nix()) {
            match c.probe(work) {
                Ok(()) => return Ok(c),
                Err(e) => tried.push(format!("  {}: {e:#}", c.cc.display())),
            }
        }
        bail!("{NO_CLANG}. Tried:\n{}", if tried.is_empty() { "  none found".into() } else { tried.join("\n") })
    }

    fn candidates() -> Vec<Clang> {
        let mut found = vec![];
        for opt in ["/opt/homebrew/opt", "/usr/local/opt"] {
            let Ok(entries) = std::fs::read_dir(opt) else { continue };
            // `llvm`, then `llvm@N` newest first
            let mut named: Vec<(u32, PathBuf)> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let rank = if n == "llvm" { Some(u32::MAX) } else { n.strip_prefix("llvm@").and_then(|v| v.parse().ok()) };
                    rank.map(|r| (r, e.path().join("bin")))
                })
                .collect();
            named.sort_by_key(|n| std::cmp::Reverse(n.0));
            found.extend(named.into_iter().map(|(_, bin)| Clang { cc: bin.join("clang"), ar: bin.join("llvm-ar"), cflags: None }));
        }
        for suffix in std::iter::once(String::new()).chain((15..=22).rev().map(|v| format!("-{v}"))) {
            if let (Some(cc), Some(ar)) = (on_path(&format!("clang{suffix}")), on_path(&format!("llvm-ar{suffix}"))) {
                found.push(Clang { cc, ar, cflags: None });
            }
        }
        found
    }

    /// Nix's unwrapped LLVM 19, pointed at its own headers (they sit in
    /// another output).
    fn nix() -> Option<Clang> {
        on_path("nix")?;
        let out = |installable: &str| -> Option<PathBuf> {
            let o = Command::new("nix").args(["build", "--no-link", "--print-out-paths", installable]).stderr(Stdio::null()).output().ok()?;
            o.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        };
        let clang = out("nixpkgs#llvmPackages_19.clang-unwrapped")?;
        let lib = out("nixpkgs#llvmPackages_19.clang-unwrapped.lib")?;
        let llvm = out("nixpkgs#llvmPackages_19.llvm")?;
        let resource = lib.join("lib/clang/19");
        Some(Clang {
            cc: clang.join("bin/clang"),
            ar: llvm.join("bin/llvm-ar"),
            cflags: Some(format!("-resource-dir {}", resource.display())),
        })
    }

    /// Compiles what ring's C needs first (a freestanding `stdint.h`) for
    /// wasm32.
    fn probe(&self, work: &Path) -> Result<()> {
        anyhow::ensure!(self.ar.is_file(), "no {}", self.ar.display());
        let mut cmd = Command::new(&self.cc);
        cmd.args(["--target=wasm32-unknown-unknown", "-ffreestanding", "-x", "c", "-c", "-", "-o"]).arg(work.join("probe.o"));
        cmd.args(self.cflags.iter().flat_map(|f| f.split_whitespace()));
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().context("could not start it")?;
        child.stdin.take().expect("piped").write_all(b"#include <stdint.h>\nuint32_t probe;\n")?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            bail!("{}", why.lines().next().unwrap_or("failed"));
        }
        Ok(())
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lockfile_names_one_version() {
        let lock = "[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.127\"\n\n[[package]]\nname = \"wasm-bindgen-shared\"\nversion = \"0.2.127\"\n";
        assert_eq!(locked_version(lock, "wasm-bindgen").as_deref(), Some("0.2.127"));
        assert_eq!(locked_version(lock, "iroh"), None);
        let twice = format!("{lock}\n[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.100\"\n");
        assert_eq!(locked_version(&twice, "wasm-bindgen"), None);
    }

    #[test]
    fn the_entry_and_headers_name_one_build() {
        let e = entry("0123456789abcdef");
        assert!(e.contains("import init from \"./0123456789abcdef/sandcastle_web.js\""));
        assert!(e.contains("new URL(\"./0123456789abcdef/sandcastle_web_bg.wasm.gz\", import.meta.url)"));
        let h = headers("0123456789abcdef");
        assert!(h.starts_with("/__computer/0123456789abcdef/*\n  Cache-Control: public, max-age=31536000, immutable\n"));
        assert!(h.contains("/__computer/0123456789abcdef/sandcastle_web_bg.wasm.gz\n  Content-Type: application/wasm\n  Content-Encoding: gzip\n"));
    }

    #[test]
    fn the_library_is_the_pinned_version() {
        let lock = std::fs::read_to_string(devstack::repo_root().join("Cargo.lock")).expect("Cargo.lock");
        assert_eq!(locked_version(&lock, "wasm-bindgen-cli-support").as_deref(), Some(WASM_BINDGEN));
    }
}
