//! The Node.js every JavaScript tool here runs on, named explicitly: npm
//! (`npm ci`), wrangler (`dev`, `deploy`, and its bundle of the Sandbox
//! SDK), and every process those start. Never the first `node` on PATH: a
//! machine's own (once a v26 alpha, under which miniflare's unpack of
//! Chrome hung and left a half-extracted cache behind) is never picked up.
//!
//! The pin (node_release.rs) is fetched once from nodejs.org/dist into
//! `target/tools/node-v<version>-<platform>/`. Its tarball is checked
//! against the pinned SHA-256 before anything is unpacked, and is unpacked
//! beside that directory first: the directory takes its name only once its
//! node answers with the pinned version, so a directory under that name is
//! always whole. Its source of truth is the pin, a new pin is a new name,
//! and a directory whose node does not answer the pin is refused, named,
//! for removal. There is no other Node: a machine the pin has no tarball
//! for runs nothing.
//!
//! node_modules is the pinned npm's `npm ci` of package-lock.json, run
//! again whenever package.json, package-lock.json or the Node differ from
//! what its stamp (`node_modules/.fragment-npm-ci`) records.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};

pub use crate::node_release::{Tarball, NODE_VERSION, TARBALLS};

/// Where the official tarballs come from.
pub const DIST_URL: &str = "https://nodejs.org/dist";
/// A tarball larger than this is refused unread (24.21's are about 50 MB).
pub const TARBALL_BYTES_MAX: u64 = 128 << 20;
pub const FETCH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// What one tarball may unpack to (24.21's: about 6,000 entries, 190 MB).
pub const UNPACK_ENTRIES_MAX: u32 = 20_000;
pub const UNPACK_BYTES_MAX: u64 = 1 << 30;
/// Variables no JavaScript process here inherits: they change what node
/// runs or loads before a line of the script does.
pub const VARS_CLEARED: [&str; 2] = ["NODE_OPTIONS", "NODE_PATH"];
/// A Node's own npm, from the directory above its `bin/`.
const NPM_CLI: &str = "lib/node_modules/npm/bin/npm-cli.js";
/// What node_modules was installed from (`ensure_modules`).
const STAMP: &str = ".fragment-npm-ci";

/// Why there is no Node to run, or no node_modules for it.
#[derive(Debug)]
pub enum NodeError {
    /// No tarball is pinned for this machine.
    UnsupportedPlatform { os: &'static str, arch: &'static str },
    Fetch { url: String, detail: String },
    TooLarge { url: String },
    /// The tarball is not the one pinned; nothing of it was unpacked.
    HashMismatch { file: String, expected: String, actual: String },
    Unpack { file: String, detail: String },
    Io { what: String, source: io::Error },
    /// `target/tools`' copy of the pin does not answer as the pin.
    Corrupt { dir: PathBuf, detail: String },
    Npm { detail: String },
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeError::UnsupportedPlatform { os, arch } => write!(f, "no Node tarball is pinned for {os}/{arch} (crates/devstack/src/node_release.rs)"),
            NodeError::Fetch { url, detail } => write!(f, "fetching {url}: {detail}"),
            NodeError::TooLarge { url } => write!(f, "{url} is larger than {TARBALL_BYTES_MAX} bytes; refused"),
            NodeError::HashMismatch { file, expected, actual } => {
                write!(f, "{file} has SHA-256 {actual}, not the pinned {expected}: refused, nothing unpacked")
            }
            NodeError::Unpack { file, detail } => write!(f, "unpacking {file}: {detail}"),
            NodeError::Io { what, source } => write!(f, "{what}: {source}"),
            NodeError::Corrupt { dir, detail } => write!(f, "{} is not the pinned Node v{NODE_VERSION} ({detail}): remove it, and the next run fetches it again", dir.display()),
            NodeError::Npm { detail } => write!(f, "npm ci: {detail}"),
        }
    }
}

/// Each message carries its cause's text, so it names no `source`.
impl std::error::Error for NodeError {}

fn io_error(what: impl Into<String>) -> impl FnOnce(io::Error) -> NodeError {
    let what = what.into();
    move |source| NodeError::Io { what, source }
}

impl Tarball {
    /// Its file name less `.tar.gz`: the one directory it holds, and its
    /// name under `target/tools`.
    pub fn dir_name(&self) -> String {
        format!("node-v{NODE_VERSION}-{}", self.platform)
    }

    pub fn url(&self) -> String {
        format!("{DIST_URL}/v{NODE_VERSION}/{}.tar.gz", self.dir_name())
    }
}

/// The pinned tarball for a machine (`std::env::consts`' OS and ARCH).
pub fn tarball_for(os: &str, arch: &str) -> Option<&'static Tarball> {
    let platform = match (os, arch) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        _ => return None,
    };
    let tarball = TARBALLS.iter().find(|t| t.platform == platform);
    assert!(tarball.is_some(), "node_release.rs pins a tarball for {platform}");
    tarball
}

/// A Node to run JavaScript on.
#[derive(Debug, Clone)]
pub struct Node {
    pub node: PathBuf,
    /// Its directory: first on the PATH of every JavaScript process.
    pub bin: PathBuf,
    /// Its own npm.
    pub npm_cli: PathBuf,
}

impl Node {
    /// `node <script>` in the environment every JavaScript process here
    /// gets (`command`).
    pub fn script(&self, script: &Path, cache: &Path) -> Result<Command, NodeError> {
        let mut cmd = self.command(cache)?;
        cmd.arg(script);
        Ok(cmd)
    }

    /// `node`, with no arguments yet, in the environment every JavaScript
    /// process here gets: PATH led by this Node's directory, so whatever it
    /// starts through `#!/usr/bin/env node` runs on this Node too, with the
    /// rest of PATH kept (Docker, worker-build, cargo, clang are found as
    /// before); `XDG_CACHE_HOME` at `cache` (miniflare keeps the Chrome for
    /// Testing that Browser Rendering runs on in
    /// `$XDG_CACHE_HOME/.wrangler/chrome`); and none of `VARS_CLEARED`.
    pub fn command(&self, cache: &Path) -> Result<Command, NodeError> {
        assert!(cache.is_absolute(), "a cache directory is named absolutely: its processes run elsewhere");
        let path = self.path(std::env::var_os("PATH").as_deref())?;
        let mut cmd = Command::new(&self.node);
        cmd.env("PATH", path).env("XDG_CACHE_HOME", cache);
        for var in VARS_CLEARED {
            cmd.env_remove(var);
        }
        Ok(cmd)
    }

    /// PATH for a JavaScript process: this Node's directory, then `inherited`.
    pub fn path(&self, inherited: Option<&OsStr>) -> Result<OsString, NodeError> {
        let rest = inherited.map(std::env::split_paths).into_iter().flatten();
        let joined = std::env::join_paths(std::iter::once(self.bin.clone()).chain(rest));
        joined.map_err(|e| NodeError::Io { what: format!("PATH led by {}", self.bin.display()), source: io::Error::other(e) })
    }
}

/// The pin, from `tools` (`target/tools`), fetched there first if it is
/// not. Never a `node` from PATH.
pub fn locate(tools: &Path) -> Result<Node, NodeError> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let tarball = tarball_for(os, arch).ok_or(NodeError::UnsupportedPlatform { os, arch })?;
    let home = tools.join(tarball.dir_name());
    if !home.is_dir() {
        let _lock = lock(tools)?;
        // another run may have fetched it while this one waited
        if !home.is_dir() {
            eprintln!("fetching Node v{NODE_VERSION} for {} from {} into {}", tarball.platform, tarball.url(), tools.display());
            let bytes = download(&tarball.url())?;
            install(&bytes, tarball, tools)?;
        }
    }
    installed(&home)
}

/// Whether `node --version` (run with none of `VARS_CLEARED`) answers as
/// the pin, and what it answered.
fn answers_as_pin(node: &Path) -> Result<(bool, String), io::Error> {
    let mut cmd = Command::new(node);
    cmd.arg("--version").stdin(Stdio::null());
    for var in VARS_CLEARED {
        cmd.env_remove(var);
    }
    let out = cmd.output()?;
    let answer = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status.success() && answer == format!("v{NODE_VERSION}"), answer))
}

/// The pin's directory as a Node, once it answers as the pin.
fn installed(home: &Path) -> Result<Node, NodeError> {
    let corrupt = |detail: String| NodeError::Corrupt { dir: home.to_path_buf(), detail };
    let node = home.join("bin/node");
    let (pinned, answer) = answers_as_pin(&node).map_err(|e| corrupt(format!("its node does not run: {e}")))?;
    if !pinned {
        return Err(corrupt(format!("its node answers {answer:?}")));
    }
    let npm_cli = home.join(NPM_CLI);
    if !npm_cli.is_file() {
        return Err(corrupt(format!("it has no {NPM_CLI}")));
    }
    Ok(Node { node, bin: home.join("bin"), npm_cli })
}

/// An exclusive hold on `tools` for fetching and installing (`xtask dev`
/// and an e2e may start at once); the OS releases it when the holder exits.
fn lock(tools: &Path) -> Result<fs::File, NodeError> {
    fs::create_dir_all(tools).map_err(io_error(format!("create {}", tools.display())))?;
    let path = tools.join(".lock");
    let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(io_error(format!("open {}", path.display())))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            eprintln!("waiting for another run setting up Node in {}", tools.display());
            file.lock().map_err(io_error(format!("lock {}", path.display())))?;
        }
        Err(fs::TryLockError::Error(e)) => return Err(io_error(format!("lock {}", path.display()))(e)),
    }
    Ok(file)
}

fn download(url: &str) -> Result<Vec<u8>, NodeError> {
    let failed = |detail: String| NodeError::Fetch { url: url.to_string(), detail };
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(FETCH_CONNECT_TIMEOUT)
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| failed(e.to_string()))?;
    let response = client.get(url).send().map_err(|e| failed(e.to_string()))?;
    if !response.status().is_success() {
        return Err(failed(response.status().to_string()));
    }
    let mut bytes = Vec::new();
    response.take(TARBALL_BYTES_MAX + 1).read_to_end(&mut bytes).map_err(|e| failed(e.to_string()))?;
    if bytes.len() as u64 > TARBALL_BYTES_MAX {
        return Err(NodeError::TooLarge { url: url.to_string() });
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `bytes` are the tarball pinned as `sha256`.
pub fn verify(bytes: &[u8], file: &str, sha256: &str) -> Result<(), NodeError> {
    let actual = sha256_hex(bytes);
    if actual == sha256 {
        Ok(())
    } else {
        Err(NodeError::HashMismatch { file: file.to_string(), expected: sha256.to_string(), actual })
    }
}

/// A fetched tarball into `tools/<its directory>`: verified, unpacked
/// beside it (`tools/.partial`, a crashed run's cleared first), its node
/// asked its version, and only then given its name. The caller holds the
/// lock.
fn install(bytes: &[u8], tarball: &Tarball, tools: &Path) -> Result<PathBuf, NodeError> {
    let name = tarball.dir_name();
    let file = format!("{name}.tar.gz");
    verify(bytes, &file, tarball.sha256)?;
    let partial = tools.join(".partial");
    remove_dir_if_present(&partial)?;
    let entries = unpack(bytes, &name, &partial)?;
    let unpacked = partial.join(&name);
    let (pinned, answer) = answers_as_pin(&unpacked.join("bin/node")).map_err(|e| NodeError::Unpack { file: file.clone(), detail: format!("its node does not run: {e}") })?;
    if !pinned {
        return Err(NodeError::Unpack { file, detail: format!("its node answers {answer:?}, not v{NODE_VERSION}") });
    }
    let home = tools.join(&name);
    fs::rename(&unpacked, &home).map_err(io_error(format!("move {} to {}", unpacked.display(), home.display())))?;
    remove_dir_if_present(&partial)?;
    eprintln!("Node v{NODE_VERSION}: {file} verified (sha256 {}), {entries} entries unpacked into {}", tarball.sha256, home.display());
    Ok(home)
}

fn remove_dir_if_present(dir: &Path) -> Result<(), NodeError> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(format!("remove {}", dir.display()))(e)),
    }
}

/// Whether a tarball entry's path lies inside its one directory, `top`.
fn inside(path: &Path, top: &str) -> bool {
    let mut components = path.components();
    let first_is_top = matches!(components.next(), Some(Component::Normal(first)) if first == top);
    if first_is_top {
        components.all(|c| matches!(c, Component::Normal(_)))
    } else {
        false
    }
}

/// Unpacks a verified tarball into `into`; every entry lies under `top`.
/// Returns how many entries it held.
fn unpack(bytes: &[u8], top: &str, into: &Path) -> Result<u32, NodeError> {
    let failed = |detail: String| NodeError::Unpack { file: format!("{top}.tar.gz"), detail };
    fs::create_dir_all(into).map_err(io_error(format!("create {}", into.display())))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    // the mode bits as packed, less setuid, setgid and sticky; nothing
    // already there is overwritten (the directory is new)
    archive.set_preserve_permissions(false);
    archive.set_overwrite(false);
    let (mut count, mut size) = (0u32, 0u64);
    // bounded: UNPACK_ENTRIES_MAX entries and UNPACK_BYTES_MAX bytes
    for entry in archive.entries().map_err(|e| failed(e.to_string()))? {
        let mut entry = entry.map_err(|e| failed(e.to_string()))?;
        count += 1;
        if count > UNPACK_ENTRIES_MAX {
            return Err(failed(format!("more than {UNPACK_ENTRIES_MAX} entries")));
        }
        size = size.saturating_add(entry.header().size().map_err(|e| failed(e.to_string()))?);
        if size > UNPACK_BYTES_MAX {
            return Err(failed(format!("more than {UNPACK_BYTES_MAX} bytes")));
        }
        // a pax global header names no file
        if entry.header().entry_type() == tar::EntryType::XGlobalHeader {
            continue;
        }
        let path = entry.path().map_err(|e| failed(e.to_string()))?.into_owned();
        if !inside(&path, top) {
            return Err(failed(format!("an entry outside {top}/: {}", path.display())));
        }
        let placed = entry.unpack_in(into).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        if !placed {
            return Err(failed(format!("tar would not place {}", path.display())));
        }
    }
    if count == 0 {
        return Err(failed("no entries".into()));
    }
    Ok(count)
}

/// What node_modules is installed from: the pin, package.json and
/// package-lock.json.
fn stamp(root: &Path) -> Result<String, NodeError> {
    let mut text = format!("node v{NODE_VERSION}\n");
    for file in ["package.json", "package-lock.json"] {
        let path = root.join(file);
        let bytes = fs::read(&path).map_err(io_error(format!("read {}", path.display())))?;
        text.push_str(&format!("{file} sha256 {}\n", sha256_hex(&bytes)));
    }
    Ok(text)
}

/// `npm ci` at `root` with `node`'s own npm, when node_modules is missing
/// or its stamp records another Node, package.json or lockfile. npm's cache
/// is `cache/npm`. `npm ci` never writes the lockfile; that it did not is
/// checked.
pub fn ensure_modules(node: &Node, root: &Path, tools: &Path, cache: &Path) -> Result<(), NodeError> {
    let wanted = stamp(root)?;
    let stamp_path = root.join("node_modules").join(STAMP);
    let current = || fs::read_to_string(&stamp_path).ok();
    if current().as_deref() == Some(wanted.as_str()) {
        return Ok(());
    }
    let _lock = lock(tools)?;
    // another run may have installed it while this one waited
    if current().as_deref() == Some(wanted.as_str()) {
        return Ok(());
    }
    eprintln!("npm ci with Node v{NODE_VERSION} ({}): node_modules is missing or was installed from another lockfile or Node", node.node.display());
    let lockfile = root.join("package-lock.json");
    let before = fs::read(&lockfile).map_err(io_error(format!("read {}", lockfile.display())))?;
    let status = node
        .script(&node.npm_cli, cache)?
        .args(["ci", "--no-audit", "--no-fund"])
        .env("npm_config_cache", cache.join("npm"))
        .env("npm_config_update_notifier", "false")
        .current_dir(root)
        .stdin(Stdio::null())
        .status()
        .map_err(io_error(format!("run {} {} ci", node.node.display(), node.npm_cli.display())))?;
    if !status.success() {
        return Err(NodeError::Npm { detail: format!("failed: {status}") });
    }
    let after = fs::read(&lockfile).map_err(io_error(format!("read {}", lockfile.display())))?;
    if before != after {
        return Err(NodeError::Npm { detail: "it rewrote package-lock.json (git diff shows how); nothing should".into() });
    }
    // written last: an npm ci that fails or is cut short leaves no stamp
    fs::write(&stamp_path, &wanted).map_err(io_error(format!("write {}", stamp_path.display())))?;
    assert_eq!(current().as_deref(), Some(wanted.as_str()), "node_modules' stamp reads back as written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devstack-node-{name}-{}", crate::random_hex(6)));
        fs::create_dir_all(&dir).expect("make the test's directory");
        dir
    }

    /// A gzipped tarball of `files` (path, mode, contents).
    fn tarball(files: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, mode, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// Each machine the repo runs on gets its own tarball and the hash
    /// pinned for it; any other machine gets none. Method: the table, read
    /// through `tarball_for`, and the hashes' shape.
    #[test]
    fn each_platform_gets_its_tarball_and_hash() {
        let darwin = tarball_for("macos", "aarch64").expect("Apple silicon");
        assert_eq!(darwin.platform, "darwin-arm64");
        assert_eq!(darwin.sha256, "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057");
        assert_eq!(darwin.url(), format!("https://nodejs.org/dist/v{NODE_VERSION}/node-v{NODE_VERSION}-darwin-arm64.tar.gz"));
        assert_eq!(tarball_for("macos", "x86_64").unwrap().platform, "darwin-x64");
        assert_eq!(tarball_for("linux", "aarch64").unwrap().platform, "linux-arm64");
        let linux = tarball_for("linux", "x86_64").expect("CI's runners");
        assert_eq!((linux.platform, linux.sha256), ("linux-x64", "6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff"));
        assert_eq!(linux.dir_name(), format!("node-v{NODE_VERSION}-linux-x64"));
        for (os, arch) in [("windows", "x86_64"), ("freebsd", "x86_64"), ("linux", "riscv64"), ("macos", "powerpc")] {
            assert!(tarball_for(os, arch).is_none(), "{os}/{arch}");
        }
        for (i, t) in TARBALLS.iter().enumerate() {
            assert!(t.sha256.len() == 64 && t.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}'s hash is SHA-256 hex", t.platform);
            assert!(TARBALLS[..i].iter().all(|o| o.platform != t.platform && o.sha256 != t.sha256), "{} is pinned once", t.platform);
        }
    }

    /// A tarball that is not the pinned one is refused before anything of
    /// it is unpacked; the one pinned is unpacked and takes its name only
    /// once its node answers as the pin. Method: a small tarball shaped as
    /// Node's, pinned under a test platform, installed with its own hash
    /// and with another.
    #[test]
    fn a_tarball_whose_hash_differs_is_refused_unpacked() {
        let tools = scratch("hash");
        let top = format!("node-v{NODE_VERSION}-test-x64");
        let node = format!("#!/bin/sh\necho v{NODE_VERSION}\n");
        let files: [(String, u32, &[u8]); 2] = [(format!("{top}/bin/node"), 0o755, node.as_bytes()), (format!("{top}/{NPM_CLI}"), 0o644, b"// npm")];
        let bytes = tarball(&files.iter().map(|(p, m, c)| (p.as_str(), *m, *c)).collect::<Vec<_>>());
        let other = sha256_hex(b"another tarball");
        let wrong = Tarball { platform: "test-x64", sha256: Box::leak(other.clone().into_boxed_str()) };
        match install(&bytes, &wrong, &tools) {
            Err(NodeError::HashMismatch { expected, actual, .. }) => assert_eq!((expected, actual), (other, sha256_hex(&bytes))),
            got => panic!("a mismatch is refused, got {got:?}"),
        }
        assert!(!tools.join(&top).exists() && !tools.join(".partial").exists(), "nothing was unpacked");

        let right = Tarball { platform: "test-x64", sha256: Box::leak(sha256_hex(&bytes).into_boxed_str()) };
        let home = install(&bytes, &right, &tools).expect("the pinned tarball installs");
        assert_eq!(home, tools.join(&top));
        assert!(!tools.join(".partial").exists(), "the staging directory is gone");
        let node = installed(&home).expect("it answers as the pin");
        assert_eq!(node.bin, home.join("bin"));
        // a node that answers otherwise (a v26 alpha, say) is not the pin
        fs::write(home.join("bin/node"), "#!/bin/sh\necho v26.8.0-alpha.0.0.0\n").unwrap();
        assert!(matches!(installed(&home), Err(NodeError::Corrupt { .. })));
        fs::remove_dir_all(&tools).unwrap();
    }

    /// An entry outside the tarball's one directory is refused.
    #[test]
    fn a_tarball_entry_outside_its_directory_is_refused() {
        let into = scratch("outside");
        let top = format!("node-v{NODE_VERSION}-test-x64");
        let bytes = tarball(&[(&format!("{top}/bin/node"), 0o755, b"x"), ("elsewhere/x", 0o644, b"x")]);
        assert!(matches!(unpack(&bytes, &top, &into), Err(NodeError::Unpack { .. })));
        assert!(!into.join("elsewhere").exists());
        assert!(inside(Path::new(&format!("{top}/lib/x.js")), &top));
        assert!(!inside(Path::new(&format!("{top}/../x")), &top) && !inside(Path::new(&format!("/{top}/x")), &top));
        fs::remove_dir_all(&into).unwrap();
    }

    /// A JavaScript process finds this Node first on its PATH, keeps the
    /// rest of it, keeps its caches under the repo, and inherits none of
    /// the variables that change what node runs.
    #[test]
    fn a_script_runs_on_this_node_first_on_path() {
        let node = Node { node: "/r/target/tools/n/bin/node".into(), bin: "/r/target/tools/n/bin".into(), npm_cli: "/r/target/tools/n/npm-cli.js".into() };
        let path = node.path(Some(OsStr::new("/home/me/.local/bin:/usr/bin"))).unwrap();
        assert_eq!(path, OsString::from("/r/target/tools/n/bin:/home/me/.local/bin:/usr/bin"));
        assert_eq!(node.path(None).unwrap(), OsString::from("/r/target/tools/n/bin"));
        let cmd = node.script(Path::new("/r/x.js"), Path::new("/r/target/cache")).unwrap();
        assert_eq!(cmd.get_program(), "/r/target/tools/n/bin/node");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["/r/x.js"]);
        let envs: Vec<(&OsStr, Option<&OsStr>)> = cmd.get_envs().collect();
        let path = envs.iter().find(|(k, _)| *k == "PATH").and_then(|(_, v)| *v).expect("PATH is set");
        assert_eq!(std::env::split_paths(path).next(), Some(PathBuf::from("/r/target/tools/n/bin")));
        assert!(envs.contains(&(OsStr::new("XDG_CACHE_HOME"), Some(OsStr::new("/r/target/cache")))));
        for var in VARS_CLEARED {
            assert!(envs.contains(&(OsStr::new(var), None)), "{var} is cleared");
        }
    }
}
