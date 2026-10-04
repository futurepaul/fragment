//! The browser preview cards are shot with off Cloudflare (docs/self-host.md,
//! seam 7): chrome-headless-shell from Chrome for Testing, pinned
//! (browser_release.rs), fetched once into
//! `target/tools/chrome-headless-shell-<version>-<platform>/` as the pinned
//! Node is (node.rs). Its zip is checked against the pinned SHA-256 before
//! anything is unpacked, and is unpacked beside that directory first: the
//! directory takes its name only once its binary answers with the pinned
//! version, so a directory under that name is always whole.
//!
//! An intranet with no route to Google hands it the zip instead
//! (`FRAGMENT_BROWSER_ZIP`, a path), checked against the same pin.
//!
//! The binary is one of the zip's files, beside its resources; it needs
//! the host's own libraries (NSS, glib, X11's: the zip's `deb.deps` lists
//! them) and fonts. A host that lacks one fails the version check, which
//! says what is missing.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::node::{self, Fetched};

pub use crate::browser_release::{Zip, CHROME_VERSION, ZIPS};

/// Where Chrome for Testing's zips come from.
pub const DIST_URL: &str = "https://storage.googleapis.com/chrome-for-testing-public";
/// Names the pinned zip on disk, by path, in place of fetching it.
pub const ZIP_VAR: &str = "FRAGMENT_BROWSER_ZIP";
/// A zip larger than this is refused unread (154's are 99 to 121 MB).
pub const ZIP_BYTES_MAX: u64 = 256 << 20;
/// What one zip may unpack to (154's linux64: 287 entries, 274 MB).
pub const UNPACK: Limits = Limits { entries: 2_000, bytes: 1 << 30 };
/// The binary's name in the zip's one directory.
const BINARY: &str = "chrome-headless-shell";

/// Bounds on what an unpack writes.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub bytes: u64,
}

/// Why there is no browser to shoot cards with.
#[derive(Debug)]
pub enum BrowserError {
    /// No zip is pinned for this machine.
    UnsupportedPlatform { os: &'static str, arch: &'static str },
    Fetch { from: String, detail: String },
    TooLarge { from: String },
    /// The zip is not the one pinned; nothing of it was unpacked.
    HashMismatch { file: String, expected: String, actual: String },
    Unpack { file: String, detail: String },
    Io { what: String, source: io::Error },
    /// The binary does not answer as the pin: a damaged copy, or a host
    /// without the libraries it needs.
    Corrupt { dir: PathBuf, detail: String },
}

impl fmt::Display for BrowserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrowserError::UnsupportedPlatform { os, arch } => {
                write!(f, "no chrome-headless-shell is pinned for {os}/{arch} (crates/devstack/src/browser_release.rs): this machine shoots no preview cards")
            }
            BrowserError::Fetch { from, detail } => write!(f, "fetching {from}: {detail}"),
            BrowserError::TooLarge { from } => write!(f, "{from} is larger than {ZIP_BYTES_MAX} bytes; refused"),
            BrowserError::HashMismatch { file, expected, actual } => {
                write!(f, "{file} has SHA-256 {actual}, not the pinned {expected}: refused, nothing unpacked")
            }
            BrowserError::Unpack { file, detail } => write!(f, "unpacking {file}: {detail}"),
            BrowserError::Io { what, source } => write!(f, "{what}: {source}"),
            BrowserError::Corrupt { dir, detail } => write!(
                f,
                "{} is not the pinned chrome-headless-shell {CHROME_VERSION} ({detail}): a host without its libraries (its deb.deps) installs them; a damaged copy is removed, and the next run fetches it again",
                dir.display()
            ),
        }
    }
}

/// Each message carries its cause's text, so it names no `source`.
impl std::error::Error for BrowserError {}

impl Zip {
    pub fn file_name(&self) -> String {
        format!("chrome-headless-shell-{}.zip", self.platform)
    }

    /// The one directory the zip holds.
    pub fn top(&self) -> String {
        format!("chrome-headless-shell-{}", self.platform)
    }

    pub fn url(&self) -> String {
        format!("{DIST_URL}/{CHROME_VERSION}/{}/{}", self.platform, self.file_name())
    }

    /// Its name under `target/tools`: the pin's version in it, so a new
    /// pin is a new name.
    pub fn dir_name(&self) -> String {
        format!("chrome-headless-shell-{CHROME_VERSION}-{}", self.platform)
    }
}

/// The pinned zip for a machine (`std::env::consts`' OS and ARCH).
pub fn zip_for(os: &str, arch: &str) -> Option<&'static Zip> {
    let platform = match (os, arch) {
        ("linux", "x86_64") => "linux64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "mac-arm64",
        ("macos", "x86_64") => "mac-x64",
        _ => return None,
    };
    let zip = ZIPS.iter().find(|z| z.platform == platform);
    assert!(zip.is_some(), "browser_release.rs pins a zip for {platform}");
    zip
}

/// The pinned browser, unpacked.
#[derive(Debug, Clone)]
pub struct Browser {
    pub bin: PathBuf,
    /// What it answers to `--version`.
    pub version: String,
}

/// The pinned browser, from `tools` (`target/tools`), fetched (or taken
/// from `FRAGMENT_BROWSER_ZIP`) there first if it is not.
pub fn locate(tools: &Path) -> Result<Browser, BrowserError> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let zip = zip_for(os, arch).ok_or(BrowserError::UnsupportedPlatform { os, arch })?;
    let home = tools.join(zip.dir_name());
    if !home.is_dir() {
        let _lock = node::lock_tools(tools, "chrome-headless-shell").map_err(|(what, source)| BrowserError::Io { what, source })?;
        // another run may have installed it while this one waited
        if !home.is_dir() {
            let bytes = match std::env::var_os(ZIP_VAR) {
                Some(path) => read_capped(Path::new(&path))?,
                None => {
                    eprintln!("fetching chrome-headless-shell {CHROME_VERSION} for {} from {} into {}", zip.platform, zip.url(), tools.display());
                    node::fetch_capped(&zip.url(), ZIP_BYTES_MAX).map_err(|e| match e {
                        Fetched::TooLarge => BrowserError::TooLarge { from: zip.url() },
                        Fetched::Failed(detail) => BrowserError::Fetch { from: zip.url(), detail },
                    })?
                }
            };
            install(&bytes, zip, tools, UNPACK)?;
        }
    }
    installed(&home)
}

/// A zip an operator supplied, whole, when it is at most `ZIP_BYTES_MAX`.
fn read_capped(path: &Path) -> Result<Vec<u8>, BrowserError> {
    let from = format!("{ZIP_VAR}={}", path.display());
    let file = fs::File::open(path).map_err(|e| BrowserError::Fetch { from: from.clone(), detail: e.to_string() })?;
    let mut bytes = Vec::new();
    file.take(ZIP_BYTES_MAX + 1).read_to_end(&mut bytes).map_err(|e| BrowserError::Fetch { from: from.clone(), detail: e.to_string() })?;
    if bytes.len() as u64 > ZIP_BYTES_MAX {
        return Err(BrowserError::TooLarge { from });
    }
    Ok(bytes)
}

/// `<bin> --version`, trimmed: its answer, or what it said instead.
fn version_of(bin: &Path) -> Result<String, String> {
    let out = Command::new(bin).arg("--version").env_clear().stdin(Stdio::null()).output().map_err(|e| format!("it does not run: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("--version failed ({}): {}", out.status, err.trim().lines().last().unwrap_or("")));
    }
    Ok(said)
}

/// Whether `--version`'s answer is the pin's: `Google Chrome for Testing <version>`.
pub fn answers_pin(answer: &str) -> bool {
    answer.strip_prefix("Google Chrome for Testing ").is_some_and(|v| v == CHROME_VERSION)
}

/// The pin's directory as a browser, once its binary answers as the pin.
fn installed(home: &Path) -> Result<Browser, BrowserError> {
    let bin = home.join(BINARY);
    let version = version_of(&bin).map_err(|detail| BrowserError::Corrupt { dir: home.to_path_buf(), detail })?;
    if !answers_pin(&version) {
        return Err(BrowserError::Corrupt { dir: home.to_path_buf(), detail: format!("its binary answers {version:?}") });
    }
    Ok(Browser { bin, version })
}

/// A zip into `tools/<its directory>`: verified, unpacked beside it
/// (`tools/.partial-browser`, a crashed run's cleared first), its binary
/// asked its version, and only then given its name. The caller holds the
/// lock.
fn install(bytes: &[u8], zip: &Zip, tools: &Path, limits: Limits) -> Result<PathBuf, BrowserError> {
    let file = zip.file_name();
    let actual = node::sha256_hex(bytes);
    if actual != zip.sha256 {
        return Err(BrowserError::HashMismatch { file, expected: zip.sha256.to_string(), actual });
    }
    let partial = tools.join(".partial-browser");
    remove_dir_if_present(&partial)?;
    let entries = unpack(bytes, &zip.top(), &partial, limits)?;
    let unpacked = partial.join(zip.top());
    let version = version_of(&unpacked.join(BINARY)).map_err(|detail| BrowserError::Unpack { file: file.clone(), detail })?;
    if !answers_pin(&version) {
        return Err(BrowserError::Unpack { file, detail: format!("its binary answers {version:?}, not {CHROME_VERSION}") });
    }
    let home = tools.join(zip.dir_name());
    fs::rename(&unpacked, &home).map_err(|source| BrowserError::Io { what: format!("move {} to {}", unpacked.display(), home.display()), source })?;
    remove_dir_if_present(&partial)?;
    eprintln!("chrome-headless-shell {CHROME_VERSION}: {file} verified (sha256 {}), {entries} entries unpacked into {}", zip.sha256, home.display());
    Ok(home)
}

fn remove_dir_if_present(dir: &Path) -> Result<(), BrowserError> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BrowserError::Io { what: format!("remove {}", dir.display()), source }),
    }
}

/// Unpacks a verified zip into `into`: every entry a file or directory
/// under `top`, none a link, within `limits`. Returns how many it held.
fn unpack(bytes: &[u8], top: &str, into: &Path, limits: Limits) -> Result<usize, BrowserError> {
    let failed = |detail: String| BrowserError::Unpack { file: format!("{top}.zip"), detail };
    let io = |what: String| move |source: io::Error| BrowserError::Io { what, source };
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| failed(e.to_string()))?;
    if archive.is_empty() {
        return Err(failed("no entries".into()));
    }
    if archive.len() > limits.entries {
        return Err(failed(format!("more than {} entries", limits.entries)));
    }
    fs::create_dir_all(into).map_err(io(format!("create {}", into.display())))?;
    let mut size = 0u64;
    // bounded: limits.entries entries (above) and limits.bytes bytes
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| failed(e.to_string()))?;
        let name = entry.name().to_string();
        let Some(path) = entry.enclosed_name() else { return Err(failed(format!("an entry outside the archive: {name}"))) };
        if !node::inside(&path, top) {
            return Err(failed(format!("an entry outside {top}/: {name}")));
        }
        if entry.is_symlink() {
            return Err(failed(format!("a link: {name}")));
        }
        let target = into.join(&path);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(io(format!("create {}", target.display())))?;
            continue;
        }
        let declared = entry.size();
        size = size.saturating_add(declared);
        if size > limits.bytes {
            return Err(failed(format!("more than {} bytes", limits.bytes)));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io(format!("create {}", parent.display())))?;
        }
        // nothing already there is overwritten (the directory is new)
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(io(format!("create {}", target.display())))?;
        // what is written is bounded by what the entry declares, whatever its data says
        let written = io::copy(&mut (&mut entry).take(declared + 1), &mut out).map_err(|e| failed(format!("{name}: {e}")))?;
        if written != declared {
            return Err(failed(format!("{name} holds {written} bytes, not the {declared} it declares")));
        }
        // the mode bits as packed, less setuid, setgid, sticky and others' write
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o755)).map_err(io(format!("chmod {}", target.display())))?;
        }
    }
    Ok(archive.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devstack-browser-{name}-{}", crate::random_hex(6)));
        fs::create_dir_all(&dir).expect("make the test's directory");
        dir
    }

    /// A zip of `files` (path, mode, contents), and links (path, target).
    fn zip_of(files: &[(&str, u32, &[u8])], links: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (path, mode, contents) in files {
            w.start_file(*path, SimpleFileOptions::default().unix_permissions(*mode)).unwrap();
            w.write_all(contents).unwrap();
        }
        for (path, target) in links {
            w.add_symlink(*path, *target, SimpleFileOptions::default()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// A zip shaped as the pin's for `platform`, its binary a script that
    /// answers `--version` with `answer`.
    fn fake_zip(platform: &str, answer: &str) -> Vec<u8> {
        let top = format!("chrome-headless-shell-{platform}");
        let script = format!("#!/bin/sh\necho '{answer}'\n");
        zip_of(&[(&format!("{top}/{BINARY}"), 0o4755, script.as_bytes()), (&format!("{top}/resources.pak"), 0o644, b"pak")], &[])
    }

    /// Each machine the stack runs on gets its own zip and the hash pinned
    /// for it; any other machine gets none.
    #[test]
    fn each_platform_gets_its_zip_and_hash() {
        let linux = zip_for("linux", "x86_64").expect("this box and CI");
        assert_eq!((linux.platform, linux.sha256), ("linux64", "636aa5c79f2693632e9921b8bbb050038ba11672e02346c06c20f991aed096f9"));
        assert_eq!(linux.url(), format!("https://storage.googleapis.com/chrome-for-testing-public/{CHROME_VERSION}/linux64/chrome-headless-shell-linux64.zip"));
        assert_eq!(linux.dir_name(), format!("chrome-headless-shell-{CHROME_VERSION}-linux64"));
        assert_eq!(zip_for("linux", "aarch64").unwrap().platform, "linux-arm64");
        assert_eq!(zip_for("macos", "aarch64").unwrap().platform, "mac-arm64");
        assert_eq!(zip_for("macos", "x86_64").unwrap().platform, "mac-x64");
        for (os, arch) in [("windows", "x86_64"), ("freebsd", "x86_64"), ("linux", "riscv64")] {
            assert!(zip_for(os, arch).is_none(), "{os}/{arch}");
        }
        for (i, z) in ZIPS.iter().enumerate() {
            assert!(z.sha256.len() == 64 && z.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}'s hash is SHA-256 hex", z.platform);
            assert!(ZIPS[..i].iter().all(|o| o.platform != z.platform && o.sha256 != z.sha256), "{} is pinned once", z.platform);
        }
        assert!(answers_pin(&format!("Google Chrome for Testing {CHROME_VERSION}")));
        for other in ["Google Chrome for Testing 126.0.6478.126", "Chromium 154.0.8037.92", "", "Google Chrome for Testing"] {
            assert!(!answers_pin(other), "{other}");
        }
    }

    /// A zip that is not the pinned one is refused before anything of it
    /// is unpacked; the one pinned is unpacked, its modes less setuid, and
    /// takes its name only once its binary answers as the pin; one whose
    /// binary answers otherwise is refused and leaves no directory.
    #[test]
    fn a_zip_whose_hash_differs_is_refused_unpacked() {
        let _exec = crate::TEST_EXEC.lock().unwrap_or_else(|e| e.into_inner());
        let tools = scratch("hash");
        let bytes = fake_zip("test64", &format!("Google Chrome for Testing {CHROME_VERSION}"));
        let other = node::sha256_hex(b"another zip");
        let wrong = Zip { platform: "test64", sha256: Box::leak(other.clone().into_boxed_str()) };
        match install(&bytes, &wrong, &tools, UNPACK) {
            Err(BrowserError::HashMismatch { expected, actual, .. }) => assert_eq!((expected, actual), (other, node::sha256_hex(&bytes))),
            got => panic!("a mismatch is refused, got {got:?}"),
        }
        assert!(!tools.join(wrong.dir_name()).exists() && !tools.join(".partial-browser").exists(), "nothing was unpacked");

        let right = Zip { platform: "test64", sha256: Box::leak(node::sha256_hex(&bytes).into_boxed_str()) };
        let home = install(&bytes, &right, &tools, UNPACK).expect("the pinned zip installs");
        assert_eq!(home, tools.join(right.dir_name()));
        assert!(!tools.join(".partial-browser").exists(), "the staging directory is gone");
        let browser = installed(&home).expect("it answers as the pin");
        assert_eq!(browser.bin, home.join(BINARY));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&browser.bin).unwrap().permissions().mode() & 0o7777, 0o755, "setuid is never unpacked");

        let old = fake_zip("old64", "Google Chrome for Testing 126.0.6478.126");
        let pinned_old = Zip { platform: "old64", sha256: Box::leak(node::sha256_hex(&old).into_boxed_str()) };
        assert!(matches!(install(&old, &pinned_old, &tools, UNPACK), Err(BrowserError::Unpack { .. })));
        assert!(!tools.join(pinned_old.dir_name()).exists(), "a binary that is not the pin is never named");
        fs::remove_dir_all(&tools).unwrap();
    }

    /// An entry outside the zip's one directory, a link, and more entries
    /// or bytes than the limits allow are each refused.
    #[test]
    fn a_zip_that_reaches_outside_or_past_its_limits_is_refused() {
        let top = "chrome-headless-shell-test64";
        let into = scratch("outside");
        let outside = zip_of(&[(&format!("{top}/{BINARY}"), 0o755, b"x"), ("elsewhere/x", 0o644, b"x")], &[]);
        assert!(matches!(unpack(&outside, top, &into.join("a"), UNPACK), Err(BrowserError::Unpack { .. })));
        assert!(!into.join("a/elsewhere").exists());
        let climbing = zip_of(&[(&format!("{top}/../../x"), 0o644, b"x")], &[]);
        assert!(matches!(unpack(&climbing, top, &into.join("b"), UNPACK), Err(BrowserError::Unpack { .. })));
        let linked = zip_of(&[(&format!("{top}/{BINARY}"), 0o755, b"x")], &[(&format!("{top}/lib"), "/etc")]);
        assert!(matches!(unpack(&linked, top, &into.join("c"), UNPACK), Err(BrowserError::Unpack { .. })));
        assert!(!into.join(format!("c/{top}/lib")).exists(), "no link is made");
        let three = zip_of(&[(&format!("{top}/a"), 0o644, b"aaaa"), (&format!("{top}/b"), 0o644, b"bbbb"), (&format!("{top}/c"), 0o644, b"cccc")], &[]);
        assert!(matches!(unpack(&three, top, &into.join("d"), Limits { entries: 2, bytes: 1 << 20 }), Err(BrowserError::Unpack { .. })));
        assert!(matches!(unpack(&three, top, &into.join("e"), Limits { entries: 10, bytes: 10 }), Err(BrowserError::Unpack { .. })));
        assert_eq!(unpack(&three, top, &into.join("f"), Limits { entries: 3, bytes: 12 }).unwrap(), 3, "exactly at the limits unpacks");
        fs::remove_dir_all(&into).unwrap();
    }
}
