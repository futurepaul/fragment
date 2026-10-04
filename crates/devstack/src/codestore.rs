//! Where a stack's git lives (docs/self-host.md, seam 5), chosen by
//! configuration: the code.storage fake (`crates/fakes`, the default, which
//! the caller starts), a store already running at a URL, or macrofiche, a
//! self-hosted git store with code.storage's API, started here on a scratch
//! directory with an org key made for it.
//!
//! An external store is named by the variables the cell reads in
//! production (`CODESTORAGE_API_URL`, `CODESTORAGE_ORG`), its org key by
//! its file (`CODESTORAGE_PRIVATE_KEY_FILE`: secrets are files read by
//! path). macrofiche is `MACROFICHE_BIN`.
//!
//! macrofiche's command line is written against its docs/contract.md as it
//! stood on 2026-10-03, before it had a binary: the org and its public key
//! (SPKI PEM, the contract's section 3 and question 7), a data directory,
//! and a loopback port. `macrofiche_command` is the one place that shape
//! lives.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use fragment_core::codestorage::OrgKey;

/// The API base the cell calls (`cell/src/config.rs`).
pub const API_URL_VAR: &str = "CODESTORAGE_API_URL";
/// The org: every token's `iss`.
pub const ORG_VAR: &str = "CODESTORAGE_ORG";
/// The org's private key (PKCS#8 PEM, P-256), by its file.
pub const KEY_FILE_VAR: &str = "CODESTORAGE_PRIVATE_KEY_FILE";
/// The macrofiche binary to start.
pub const MACROFICHE_BIN_VAR: &str = "MACROFICHE_BIN";
/// macrofiche must answer HTTP within this of its start (it makes its data
/// directory and clears stale locks first: its docs/library.md, finding 7).
pub const MACROFICHE_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// A graceful stop must finish within this; past it the group is killed.
pub const MACROFICHE_STOP_TIMEOUT: Duration = Duration::from_secs(20);
/// An org name: what a JWT's `iss` carries.
pub const ORG_BYTES_MAX: usize = 64;

/// Where the stack's git lives.
pub enum CodeStore {
    /// The in-repo fake, which the caller starts (its levers are the e2e's).
    Fake,
    /// A store already running, reached at its URL.
    External(ExternalStore),
    /// macrofiche, started from this binary (`Macrofiche::start`).
    Macrofiche(PathBuf),
}

/// A store already running, as the cell is configured with it.
pub struct ExternalStore {
    pub url: String,
    pub org: String,
    /// The org's private key, PKCS#8 PEM.
    pub key_pem: String,
}

impl ExternalStore {
    /// From `CODESTORAGE_API_URL`, `CODESTORAGE_ORG` and the key's file
    /// `CODESTORAGE_PRIVATE_KEY_FILE`; each is required.
    pub fn from_env() -> Result<ExternalStore> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let url = var(API_URL_VAR).with_context(|| format!("an external code store is named by {API_URL_VAR}, {ORG_VAR} and {KEY_FILE_VAR}: {API_URL_VAR} is unset"))?;
        let org = var(ORG_VAR).with_context(|| format!("{API_URL_VAR} needs {ORG_VAR}, the org its tokens name"))?;
        let file = var(KEY_FILE_VAR).with_context(|| format!("{API_URL_VAR} needs {KEY_FILE_VAR}, the file of the org's private key (PKCS#8 PEM)"))?;
        let key_pem = fs::read_to_string(&file).with_context(|| format!("read {file} ({KEY_FILE_VAR})"))?;
        ExternalStore::new(&url, &org, &key_pem)
    }

    /// Checked as the cell checks them: an http(s) base with no trailing
    /// slash, an org a token can name, and a P-256 key.
    pub fn new(url: &str, org: &str, key_pem: &str) -> Result<ExternalStore> {
        let url = url.trim_end_matches('/');
        if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() <= "https://".len() {
            bail!("{API_URL_VAR} is an http(s) URL, not {url:?}");
        }
        if org.is_empty() || org.len() > ORG_BYTES_MAX || !org.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            bail!("{ORG_VAR} is 1 to {ORG_BYTES_MAX} letters, digits, - and _, not {org:?}");
        }
        OrgKey::from_pem(key_pem).map_err(|e| anyhow::anyhow!("{KEY_FILE_VAR}: {e}"))?;
        Ok(ExternalStore { url: url.to_string(), org: org.to_string(), key_pem: key_pem.to_string() })
    }
}

impl CodeStore {
    /// The dev stack's choice, by what is set: `CODESTORAGE_API_URL` (with
    /// its org and key file) is a store already running, `MACROFICHE_BIN` is
    /// macrofiche started here, neither is the fake, and both is refused.
    pub fn from_env() -> Result<CodeStore> {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        match (set(API_URL_VAR), set(MACROFICHE_BIN_VAR)) {
            (true, true) => bail!("{API_URL_VAR} names a store already running and {MACROFICHE_BIN_VAR} one to start: set one"),
            (true, false) => Ok(CodeStore::External(ExternalStore::from_env()?)),
            (false, true) => Ok(CodeStore::Macrofiche(macrofiche_bin()?)),
            (false, false) => Ok(CodeStore::Fake),
        }
    }
}

/// `MACROFICHE_BIN`, which must be a file.
pub fn macrofiche_bin() -> Result<PathBuf> {
    let bin = std::env::var_os(MACROFICHE_BIN_VAR)
        .map(PathBuf::from)
        .with_context(|| format!("set {MACROFICHE_BIN_VAR} to a macrofiche binary (github.com/futurepaul/macrofiche: cargo build --release)"))?;
    if !bin.is_file() {
        bail!("no macrofiche at {} ({MACROFICHE_BIN_VAR})", bin.display());
    }
    Ok(bin)
}

pub struct MacroficheOptions {
    /// Its state: the repositories, and the org's public key it is given.
    pub dir: PathBuf,
    /// The loopback port it listens on.
    pub port: u16,
    pub org: String,
    /// The org's private key (PKCS#8 PEM); macrofiche is given its public half.
    pub key_pem: String,
    /// Where its log goes (`macrofiche-<port>.log`).
    pub log_dir: PathBuf,
}

/// macrofiche's command line for `opts`, the org's public key at `public`.
/// Provisional (the module's comment): the contract names what macrofiche
/// needs, not its flags.
fn macrofiche_command(bin: &Path, opts: &MacroficheOptions, public: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("serve")
        .arg("--data")
        .arg(&opts.dir)
        .args(["--listen", &format!("127.0.0.1:{}", opts.port)])
        .args(["--org", &opts.org])
        .arg("--org-key")
        .arg(public);
    cmd
}

/// One macrofiche process.
pub struct Macrofiche {
    child: Child,
    pub url: String,
    pub log: PathBuf,
    stopped: bool,
}

impl Macrofiche {
    /// Starts macrofiche on `opts.dir` and waits until it answers HTTP: an
    /// unsigned `GET /api/repos`, which a store refuses (401) once it serves.
    pub fn start(bin: &Path, opts: &MacroficheOptions) -> Result<Macrofiche> {
        fs::create_dir_all(&opts.dir).with_context(|| format!("create {}", opts.dir.display()))?;
        fs::create_dir_all(&opts.log_dir)?;
        let key = OrgKey::from_pem(&opts.key_pem).map_err(|e| anyhow::anyhow!("{e}"))?;
        let public = opts.dir.join(format!("{}.pub.pem", opts.org));
        fs::write(&public, key.public_pem())?;
        let log = opts.log_dir.join(format!("macrofiche-{}.log", opts.port));
        let out = fs::OpenOptions::new().create(true).append(true).open(&log).with_context(|| format!("open {}", log.display()))?;
        // in the caller's process group, so the Ctrl-C that stops `xtask
        // dev` stops it too; its git children are its own to end
        let mut cmd = macrofiche_command(bin, opts, &public);
        cmd.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out);
        let child = cmd.spawn().with_context(|| format!("start {}", bin.display()))?;
        let url = format!("http://127.0.0.1:{}", opts.port);
        let mut m = Macrofiche { child, url, log, stopped: false };
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build()?;
        let t0 = Instant::now();
        // Bounded by MACROFICHE_READY_TIMEOUT.
        loop {
            if http.get(format!("{}/api/repos", m.url)).send().is_ok() {
                return Ok(m);
            }
            if let Some(status) = m.child.try_wait()? {
                m.stopped = true;
                bail!("macrofiche exited ({status}) before it answered:\n{}", fs::read_to_string(&m.log).unwrap_or_default());
            }
            if t0.elapsed() > MACROFICHE_READY_TIMEOUT {
                bail!("macrofiche on :{} did not answer within {MACROFICHE_READY_TIMEOUT:?}:\n{}", opts.port, fs::read_to_string(&m.log).unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// SIGTERM, and its exit awaited: a store that answered a write has
    /// made it durable (its contract, section 9), so a stop loses nothing.
    /// Killed past `MACROFICHE_STOP_TIMEOUT`.
    pub fn stop(mut self) -> Result<()> {
        self.stopped = true;
        let _ = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).stderr(Stdio::null()).status();
        let t0 = Instant::now();
        // Bounded by MACROFICHE_STOP_TIMEOUT.
        while self.child.try_wait()?.is_none() {
            if t0.elapsed() > MACROFICHE_STOP_TIMEOUT {
                let _ = self.child.kill();
                let _ = self.child.wait();
                bail!("macrofiche did not stop within {MACROFICHE_STOP_TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

impl Drop for Macrofiche {
    /// An early error must not leave it holding its port and its data.
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed P-256 key as PKCS#8 PEM: the shape the cell reads.
    fn key() -> String {
        use p256::pkcs8::EncodePrivateKey;
        p256::SecretKey::from_slice(&[5u8; 32]).unwrap().to_pkcs8_pem(Default::default()).unwrap().to_string()
    }

    /// Goal: an external store is taken only whole and well formed, as the
    /// cell would read it. Method: each part missing or malformed in turn.
    #[test]
    fn an_external_store_is_checked_as_the_cell_reads_it() {
        let ok = ExternalStore::new("http://127.0.0.1:9000/", "fragment-dev", &key()).expect("a well-formed store");
        assert_eq!((ok.url.as_str(), ok.org.as_str()), ("http://127.0.0.1:9000", "fragment-dev"), "a trailing slash goes, as the cell trims it");
        for (url, org, pem) in [
            ("127.0.0.1:9000", "fragment-dev", key()),
            ("https://", "fragment-dev", key()),
            ("http://127.0.0.1:9000", "", key()),
            ("http://127.0.0.1:9000", "an org", key()),
            ("http://127.0.0.1:9000", "fragment-dev", "not a key".to_string()),
        ] {
            assert!(ExternalStore::new(url, org, &pem).is_err(), "{url} {org:?}");
        }
    }

    /// Goal: macrofiche is told its org, its data, its port and the org's
    /// public half, never the private key. Method: the command it is
    /// started with, read back.
    #[test]
    fn macrofiche_is_given_the_public_half_alone() {
        let opts = MacroficheOptions { dir: PathBuf::from("/tmp/mf"), port: 9101, org: "fragment-e2e".into(), key_pem: key(), log_dir: PathBuf::from("/tmp") };
        let cmd = macrofiche_command(Path::new("/bin/macrofiche"), &opts, Path::new("/tmp/mf/fragment-e2e.pub.pem"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["serve", "--data", "/tmp/mf", "--listen", "127.0.0.1:9101", "--org", "fragment-e2e", "--org-key", "/tmp/mf/fragment-e2e.pub.pem"]);
        assert!(!args.iter().any(|a| a.contains("PRIVATE")), "{args:?}");
    }
}
