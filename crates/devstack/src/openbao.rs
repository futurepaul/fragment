//! OpenBao, the secrets service a self-hosted deployment reads its own
//! secrets from (docs/self-host.md, seam 12): the open-source fork of
//! HashiCorp Vault, used as it ships, behind the cell's secrets shim
//! (cell/secrets.mjs, its `openbao` backend). The cell reads Vault's KV v2
//! API, so a company's own Vault or OpenBao stands where this one does.
//!
//! **The binary** is pinned (openbao_release.rs), fetched once into
//! `target/tools/openbao-<version>-<platform>/bao` from the release's
//! tarball, checked against the pinned SHA-256 before anything is read
//! from it; `FRAGMENT_OPENBAO_TARBALL` names an intranet's copy of the same
//! tarball instead, checked the same way.
//!
//! **Its state** is a directory of its own, 0700 (`Layout`):
//!
//! - `seal.key`: 32 random bytes, 0600, made once. OpenBao's static seal
//!   (an auto-unseal) opens its data with it at every start, unattended.
//! - `admin.role-id`, `admin.secret-id` (0600, made once): the AppRole the
//!   stack seeds the secrets and keeps the cell's token with. It can write
//!   the mount, make and renew the cell's tokens, and nothing else.
//! - `cell.token` (0600): the cell's token, "secret zero", the one secret
//!   the cell is given outside OpenBao. Its policy reads the mount and
//!   nothing else. Periodic (`CELL_TOKEN_PERIOD`), renewed at each start
//!   and every `RENEW_EVERY` while the stack runs (`Renewer`); minted
//!   again when it has lapsed.
//! - `data/`: OpenBao's storage, `pebbledb` (one server, the `file`
//!   backend's successor: 2.7 removed `file`).
//! - `openbao.hcl` (rendered at each start) and `openbao.log`.
//!
//! **Initialised once**, by OpenBao itself: the config's `initialize`
//! stanza (declarative self-initialization) runs on the first start
//! alone. It mounts KV v2 at `fragment/`, writes the two policies, the
//! cell's token role and the admin's AppRole (its ids read from their
//! files), and revokes the root token it used. No root token or recovery
//! key exists after it: a lost admin credential means removing the
//! directory, and the next start seeds everything again.
//!
//! **Loopback only.** The cell and OpenBao share the box, so OpenBao
//! listens on 127.0.0.1 with no TLS; nothing off the box reaches it, the
//! LAN's front door included.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::node::{self, Fetched};

pub use crate::openbao_release::{Tarball, OPENBAO_VERSION, TARBALLS};

/// Where the release tarballs come from.
pub const RELEASES_URL: &str = "https://github.com/openbao/openbao/releases/download";
/// Names the pinned tarball on disk, by path, in place of fetching it.
pub const TARBALL_VAR: &str = "FRAGMENT_OPENBAO_TARBALL";
/// A tarball larger than this is refused unread (2.7.1's are 69 to 78 MB).
pub const TARBALL_BYTES_MAX: u64 = 128 << 20;
/// The binary, at most (2.7.1's linux_amd64 `bao` is 182 MB).
pub const BINARY_BYTES_MAX: u64 = 384 << 20;
const BINARY: &str = "bao";

/// The KV v2 mount the deployment's secrets are under, each at
/// `<MOUNT>/<its store name>`, its value in the field `VALUE_FIELD`.
pub const MOUNT: &str = "fragment";
pub const VALUE_FIELD: &str = "value";
/// The cell's policy (read the mount) and the stack's (seed it, keep the
/// cell's token), and the roles that hand out tokens under them.
pub const CELL_POLICY: &str = "fragment-cell";
pub const ADMIN_POLICY: &str = "fragment-admin";
pub const CELL_ROLE: &str = "fragment-cell";
pub const ADMIN_ROLE: &str = "fragment-admin";
/// The cell's token lives this long unless renewed; each renewal starts it
/// again.
pub const CELL_TOKEN_PERIOD: &str = "720h";
/// How often a running stack renews the cell's token (`Renewer`).
pub const RENEW_EVERY: Duration = Duration::from_secs(3600);
/// The Worker variable the shim reads the cell's token from (cell/secrets.mjs).
pub const TOKEN_VAR: &str = "FRAGMENT_SECRETS_TOKEN";
/// The static seal's key: AES-256-GCM's.
pub const SEAL_KEY_BYTES: usize = 32;
/// OpenBao answers, initialised and unsealed, within this of its start.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// A start that stays sealed this long has a key that does not open its data.
const SEALED_GRACE: Duration = Duration::from_secs(5);
/// A graceful stop finishes within this; past it, the process is killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// One request of the stack's to OpenBao, its answer read whole.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest answer read from OpenBao.
const ANSWER_BYTES_MAX: u64 = 1 << 20;
/// The most secrets one seeding writes: a Worker's bindings, with room.
pub const SEED_MAX: usize = 64;
/// A secret's value, at most: the Cloudflare store's own limit.
pub const VALUE_BYTES_MAX: usize = 64 * 1024;

/// Why there is no OpenBao, or it did not do what was asked.
#[derive(Debug)]
pub enum OpenBaoError {
    /// No tarball is pinned for this machine.
    UnsupportedPlatform { os: &'static str, arch: &'static str },
    Fetch { from: String, detail: String },
    TooLarge { from: String },
    /// The tarball is not the one pinned; nothing of it was read.
    HashMismatch { file: String, expected: String, actual: String },
    Unpack { file: String, detail: String },
    /// The binary does not answer as the pin.
    Corrupt { bin: PathBuf, detail: String },
    Io { what: String, source: io::Error },
    /// A secret file others may read, or one that is not what it must be.
    BadFile { path: PathBuf, detail: String },
    /// A state directory whose path cannot go in OpenBao's config.
    BadPath(PathBuf),
    /// A secret's name or value that cannot be stored.
    BadSecret(String),
    /// The server exited, stayed sealed, or never answered.
    Start { detail: String, log: PathBuf, tail: String },
    /// A request answered with what it should not have.
    Api { what: String, status: u16, detail: String },
    /// A request found no server, or no answer in time.
    Down { what: String, detail: String },
}

impl fmt::Display for OpenBaoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenBaoError::UnsupportedPlatform { os, arch } => write!(f, "no OpenBao is pinned for {os}/{arch} (crates/devstack/src/openbao_release.rs)"),
            OpenBaoError::Fetch { from, detail } => write!(f, "fetching {from}: {detail}"),
            OpenBaoError::TooLarge { from } => write!(f, "{from} is larger than {TARBALL_BYTES_MAX} bytes; refused"),
            OpenBaoError::HashMismatch { file, expected, actual } => write!(f, "{file} has SHA-256 {actual}, not the pinned {expected}: refused, nothing read from it"),
            OpenBaoError::Unpack { file, detail } => write!(f, "unpacking {file}: {detail}"),
            OpenBaoError::Corrupt { bin, detail } => {
                write!(f, "{} is not the pinned OpenBao {OPENBAO_VERSION} ({detail}): remove its directory, and the next run fetches it again", bin.display())
            }
            OpenBaoError::Io { what, source } => write!(f, "{what}: {source}"),
            OpenBaoError::BadFile { path, detail } => write!(f, "{}: {detail}", path.display()),
            OpenBaoError::BadPath(p) => write!(f, "{} cannot be OpenBao's state directory: its path holds a quote, a backslash, a control character or a template sequence", p.display()),
            OpenBaoError::BadSecret(detail) => write!(f, "{detail}"),
            OpenBaoError::Start { detail, log, tail } => write!(f, "OpenBao {detail} (its log, {}):\n{tail}", log.display()),
            OpenBaoError::Api { what, status, detail } => write!(f, "OpenBao refused {what} ({status}): {detail}"),
            OpenBaoError::Down { what, detail } => write!(f, "OpenBao did not answer {what}: {detail}"),
        }
    }
}

impl std::error::Error for OpenBaoError {}

fn io_error(what: impl Into<String>) -> impl FnOnce(io::Error) -> OpenBaoError {
    let what = what.into();
    move |source| OpenBaoError::Io { what, source }
}

impl Tarball {
    pub fn file_name(&self) -> String {
        format!("openbao_{OPENBAO_VERSION}_{}.tar.gz", self.platform)
    }

    pub fn url(&self) -> String {
        format!("{RELEASES_URL}/v{OPENBAO_VERSION}/{}", self.file_name())
    }

    /// Its name under `target/tools`: the pin's version in it.
    pub fn dir_name(&self) -> String {
        format!("openbao-{OPENBAO_VERSION}-{}", self.platform)
    }
}

/// The pinned tarball for a machine (`std::env::consts`' OS and ARCH).
pub fn tarball_for(os: &str, arch: &str) -> Option<&'static Tarball> {
    let platform = match (os, arch) {
        ("linux", "x86_64") => "linux_amd64",
        ("linux", "aarch64") => "linux_arm64",
        ("macos", "x86_64") => "darwin_amd64",
        ("macos", "aarch64") => "darwin_arm64",
        _ => return None,
    };
    let tarball = TARBALLS.iter().find(|t| t.platform == platform);
    assert!(tarball.is_some(), "openbao_release.rs pins a tarball for {platform}");
    tarball
}

/// The pinned `bao`, from `tools` (`target/tools`), fetched (or taken from
/// `FRAGMENT_OPENBAO_TARBALL`) there first if it is not.
pub fn locate(tools: &Path) -> Result<PathBuf, OpenBaoError> {
    locate_from(tools, std::env::var_os(TARBALL_VAR).map(PathBuf::from))
}

/// `locate`, the tarball from `given` (by path) when it must be installed.
fn locate_from(tools: &Path, given: Option<PathBuf>) -> Result<PathBuf, OpenBaoError> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let tarball = tarball_for(os, arch).ok_or(OpenBaoError::UnsupportedPlatform { os, arch })?;
    let home = tools.join(tarball.dir_name());
    if !home.is_dir() {
        let _lock = node::lock_tools(tools, "OpenBao").map_err(|(what, source)| OpenBaoError::Io { what, source })?;
        // another run may have installed it while this one waited
        if !home.is_dir() {
            let bytes = match given {
                Some(path) => read_capped(&path)?,
                None => {
                    eprintln!("fetching OpenBao {OPENBAO_VERSION} for {} from {} into {}", tarball.platform, tarball.url(), tools.display());
                    node::fetch_capped(&tarball.url(), TARBALL_BYTES_MAX).map_err(|e| match e {
                        Fetched::TooLarge => OpenBaoError::TooLarge { from: tarball.url() },
                        Fetched::Failed(detail) => OpenBaoError::Fetch { from: tarball.url(), detail },
                    })?
                }
            };
            install(&bytes, tarball, tools)?;
        }
    }
    let bin = home.join(BINARY);
    answers_pin(&bin)?;
    Ok(bin)
}

/// A tarball an operator supplied, whole, when it is at most `TARBALL_BYTES_MAX`.
fn read_capped(path: &Path) -> Result<Vec<u8>, OpenBaoError> {
    let from = format!("{TARBALL_VAR}={}", path.display());
    let file = fs::File::open(path).map_err(|e| OpenBaoError::Fetch { from: from.clone(), detail: e.to_string() })?;
    let mut bytes = Vec::new();
    file.take(TARBALL_BYTES_MAX + 1).read_to_end(&mut bytes).map_err(|e| OpenBaoError::Fetch { from: from.clone(), detail: e.to_string() })?;
    if bytes.len() as u64 > TARBALL_BYTES_MAX {
        return Err(OpenBaoError::TooLarge { from });
    }
    Ok(bytes)
}

/// Whether `bao version` answers the pin: `OpenBao v<version> (<commit>), …`.
fn answers_pin(bin: &Path) -> Result<(), OpenBaoError> {
    let out = Command::new(bin).arg("version").env_clear().stdin(Stdio::null()).output().map_err(|e| OpenBaoError::Corrupt { bin: bin.to_path_buf(), detail: format!("it does not run: {e}") })?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match out.status.success() && said.starts_with(&format!("OpenBao v{OPENBAO_VERSION} ")) {
        true => Ok(()),
        false => Err(OpenBaoError::Corrupt { bin: bin.to_path_buf(), detail: format!("`bao version` answers {said:?}") }),
    }
}

/// A tarball into `tools/<its directory>/bao`: verified, its one binary
/// read out (the tarball's other files are its license and notes) beside
/// it (`tools/.partial-openbao`), asked its version, and only then given
/// its directory's name. The caller holds the lock.
fn install(bytes: &[u8], tarball: &Tarball, tools: &Path) -> Result<PathBuf, OpenBaoError> {
    let file = tarball.file_name();
    let actual = node::sha256_hex(bytes);
    if actual != tarball.sha256 {
        return Err(OpenBaoError::HashMismatch { file, expected: tarball.sha256.to_string(), actual });
    }
    let binary = binary_from(bytes, &file)?;
    let partial = tools.join(".partial-openbao");
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial).map_err(io_error(format!("create {}", partial.display())))?;
    let bin = partial.join(BINARY);
    let mut out = fs::OpenOptions::new().write(true).create_new(true).mode(0o755).open(&bin).map_err(io_error(format!("create {}", bin.display())))?;
    out.write_all(&binary).map_err(io_error(format!("write {}", bin.display())))?;
    drop(out);
    answers_pin(&bin).map_err(|e| OpenBaoError::Unpack { file: file.clone(), detail: e.to_string() })?;
    let home = tools.join(tarball.dir_name());
    fs::rename(&partial, &home).map_err(io_error(format!("move {} to {}", partial.display(), home.display())))?;
    eprintln!("OpenBao {OPENBAO_VERSION}: {file} verified (sha256 {}), its bao in {}", tarball.sha256, home.display());
    Ok(home)
}

/// The `bao` a release tarball holds at its top: a regular file, at most
/// `BINARY_BYTES_MAX`.
pub fn binary_from(tarball: &[u8], file: &str) -> Result<Vec<u8>, OpenBaoError> {
    let bad = |detail: String| OpenBaoError::Unpack { file: file.to_string(), detail };
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    // bounded: the tarball's entries, read until `bao`
    for entry in archive.entries().map_err(|e| bad(e.to_string()))? {
        let entry = entry.map_err(|e| bad(e.to_string()))?;
        if entry.path().map_err(|e| bad(e.to_string()))?.to_str() != Some(BINARY) {
            continue;
        }
        if entry.header().entry_type() != tar::EntryType::Regular {
            return Err(bad(format!("its {BINARY} is not a regular file")));
        }
        let mut binary = Vec::new();
        entry.take(BINARY_BYTES_MAX + 1).read_to_end(&mut binary).map_err(|e| bad(e.to_string()))?;
        if binary.len() as u64 > BINARY_BYTES_MAX {
            return Err(bad(format!("its {BINARY} is over {BINARY_BYTES_MAX} bytes")));
        }
        return Ok(binary);
    }
    Err(bad(format!("it holds no {BINARY}")))
}

/// Where everything in a state directory is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    pub fn seal_key(&self) -> PathBuf {
        self.dir.join("seal.key")
    }
    pub fn admin_role_id(&self) -> PathBuf {
        self.dir.join("admin.role-id")
    }
    pub fn admin_secret_id(&self) -> PathBuf {
        self.dir.join("admin.secret-id")
    }
    pub fn cell_token(&self) -> PathBuf {
        self.dir.join("cell.token")
    }
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }
    pub fn config(&self) -> PathBuf {
        self.dir.join("openbao.hcl")
    }
    pub fn log(&self) -> PathBuf {
        self.dir.join("openbao.log")
    }
    /// The audit log, when the stack keeps one (`Options::audit`).
    pub fn audit(&self) -> PathBuf {
        self.dir.join("audit.log")
    }
}

/// Random bytes from the OS.
fn random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).expect("read /dev/urandom");
    buf
}

/// A secret file: made with `make`'s bytes on first use (0600 from its
/// creation), else read as it is. One others may read, or empty, is refused.
fn secret_file(path: &Path, make: impl FnOnce() -> Vec<u8>) -> Result<Vec<u8>, OpenBaoError> {
    if path.exists() {
        let mode = fs::metadata(path).map_err(io_error(format!("stat {}", path.display())))?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(OpenBaoError::BadFile { path: path.to_path_buf(), detail: format!("mode {mode:o}: a secret must be readable by its owner alone (chmod 600 it)") });
        }
        let bytes = fs::read(path).map_err(io_error(format!("read {}", path.display())))?;
        if bytes.is_empty() {
            return Err(OpenBaoError::BadFile { path: path.to_path_buf(), detail: "empty".into() });
        }
        return Ok(bytes);
    }
    let bytes = make();
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(io_error(format!("create {}", path.display())))?;
    f.write_all(&bytes).map_err(io_error(format!("write {}", path.display())))?;
    Ok(bytes)
}

/// Whether a path can go in OpenBao's config as a quoted string.
fn plain_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    p.is_absolute() && !s.contains(['"', '\\']) && !s.chars().any(char::is_control) && !s.contains("${") && !s.contains("%{")
}

/// The state directory, ready for a start: made (0700) with its seal key
/// and the admin's credentials on first use, and each checked after. A
/// directory whose data is there without its seal key or credentials is
/// refused: nothing could open it, or reach it.
pub fn prepare(dir: &Path) -> Result<Layout, OpenBaoError> {
    let dir = std::path::absolute(dir).map_err(io_error(format!("resolve {}", dir.display())))?;
    if !plain_path(&dir) {
        return Err(OpenBaoError::BadPath(dir));
    }
    let layout = Layout { dir: dir.clone() };
    let fresh = !layout.data().exists();
    if !fresh {
        for f in [layout.seal_key(), layout.admin_role_id(), layout.admin_secret_id()] {
            if !f.exists() {
                return Err(OpenBaoError::BadFile {
                    path: f,
                    detail: format!("is gone, and {} holds data only it opens or reaches: remove {} to start again (the stack seeds every secret again)", layout.data().display(), dir.display()),
                });
            }
        }
    }
    fs::create_dir_all(layout.data()).map_err(io_error(format!("create {}", layout.data().display())))?;
    for d in [&dir, &layout.data()] {
        fs::set_permissions(d, fs::Permissions::from_mode(0o700)).map_err(io_error(format!("chmod {}", d.display())))?;
    }
    let key = secret_file(&layout.seal_key(), || random(SEAL_KEY_BYTES))?;
    if key.len() != SEAL_KEY_BYTES {
        return Err(OpenBaoError::BadFile { path: layout.seal_key(), detail: format!("{} bytes: the static seal's key is {SEAL_KEY_BYTES} raw bytes", key.len()) });
    }
    for f in [layout.admin_role_id(), layout.admin_secret_id()] {
        let id = secret_file(&f, || crate::random_hex(32).into_bytes())?;
        if !id.iter().all(u8::is_ascii_alphanumeric) || id.len() > 128 {
            return Err(OpenBaoError::BadFile { path: f, detail: "not an id: 1 to 128 ASCII letters and digits".into() });
        }
    }
    Ok(layout)
}

/// The static seal's key id: named from the key itself, as OpenBao asks
/// (a new key is a new id).
fn seal_key_id(layout: &Layout) -> Result<String, OpenBaoError> {
    let key = fs::read(layout.seal_key()).map_err(io_error(format!("read {}", layout.seal_key().display())))?;
    Ok(format!("fragment-{}", &node::sha256_hex(&key)[..16]))
}

/// A string as HCL quotes it.
fn quoted(s: &str) -> String {
    assert!(!s.contains("${") && !s.contains("%{"), "no template sequence in {s:?}");
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => {
                assert!(!c.is_control(), "no control character in {s:?}");
                out.push(c);
            }
        }
    }
    out.push('"');
    out
}

/// The cell's policy: read each secret under the mount, and nothing else.
pub fn cell_policy() -> String {
    format!("path \"{MOUNT}/data/*\" {{ capabilities = [\"read\"] }}\n")
}

/// The stack's: write and read the secrets under the mount (never delete
/// them, never their history), and make and renew the cell's tokens.
pub fn admin_policy() -> String {
    format!(
        "path \"{MOUNT}/data/*\" {{ capabilities = [\"create\", \"update\", \"read\"] }}\n\
         path \"auth/token/create/{CELL_ROLE}\" {{ capabilities = [\"update\"] }}\n\
         path \"auth/token/renew\" {{ capabilities = [\"update\"] }}\n"
    )
}

/// OpenBao's config for the stack: on loopback at `port`, its data and
/// seal key in `layout`, an audit log there when `audit`, and the requests
/// that initialise it on its first start.
pub fn render_config(layout: &Layout, port: u16, audit: bool) -> Result<String, OpenBaoError> {
    let key_id = seal_key_id(layout)?;
    let p = |path: PathBuf| quoted(&path.display().to_string());
    let file = |path: PathBuf| format!("{{ eval_type = \"string\", eval_source = \"file\", path = {} }}", p(path));
    let request = |name: &str, path: &str, data: &str| format!("  request \"{name}\" {{\n    operation = \"update\"\n    path = {}\n    data = {data}\n  }}\n", quoted(path));
    let mut c = String::new();
    c.push_str("# Rendered at each start by fragment's dev stack (crates/devstack/src/openbao.rs).\n");
    c.push_str("ui = false\ndisable_clustering = true\nlog_level = \"info\"\n");
    c.push_str(&format!("api_addr = \"http://127.0.0.1:{port}\"\n"));
    c.push_str(&format!("listener \"tcp\" {{\n  address = \"127.0.0.1:{port}\"\n  tls_disable = true\n}}\n"));
    c.push_str(&format!("storage \"pebbledb\" {{\n  path = {}\n}}\n", p(layout.data())));
    c.push_str(&format!("seal \"static\" {{\n  current_key_id = {}\n  current_key = {}\n}}\n", quoted(&key_id), quoted(&format!("file://{}", layout.seal_key().display()))));
    if audit {
        c.push_str(&format!("audit \"file\" \"fragment\" {{\n  description = \"every request, its secrets HMAC'd\"\n  options {{\n    file_path = {}\n  }}\n}}\n", p(layout.audit())));
    }
    // the first start alone: the mount, the policies, the cell's token
    // role, and the stack's AppRole; then OpenBao revokes the root token
    c.push_str("initialize \"fragment\" {\n");
    c.push_str(&request("mount", &format!("sys/mounts/{MOUNT}"), "{ type = \"kv\", options = { version = \"2\" }, description = \"fragment's deployment secrets\" }"));
    c.push_str(&request("cell-policy", &format!("sys/policies/acl/{CELL_POLICY}"), &format!("{{ policy = {} }}", quoted(&cell_policy()))));
    c.push_str(&request("admin-policy", &format!("sys/policies/acl/{ADMIN_POLICY}"), &format!("{{ policy = {} }}", quoted(&admin_policy()))));
    c.push_str(&request(
        "cell-role",
        &format!("auth/token/roles/{CELL_ROLE}"),
        &format!("{{ allowed_policies = [\"{CELL_POLICY}\"], orphan = true, renewable = true, token_period = \"{CELL_TOKEN_PERIOD}\", token_no_default_policy = true, token_type = \"service\" }}"),
    ));
    c.push_str(&request("approle", "sys/auth/approle", "{ type = \"approle\" }"));
    c.push_str(&request(
        "admin-role",
        &format!("auth/approle/role/{ADMIN_ROLE}"),
        &format!("{{ token_policies = [\"{ADMIN_POLICY}\"], token_no_default_policy = true, token_ttl = \"15m\", token_max_ttl = \"1h\", secret_id_ttl = \"0\", secret_id_num_uses = 0 }}"),
    ));
    c.push_str(&request("admin-role-id", &format!("auth/approle/role/{ADMIN_ROLE}/role-id"), &format!("{{ role_id = {} }}", file(layout.admin_role_id()))));
    c.push_str(&request("admin-secret-id", &format!("auth/approle/role/{ADMIN_ROLE}/custom-secret-id"), &format!("{{ secret_id = {} }}", file(layout.admin_secret_id()))));
    c.push_str("}\n");
    Ok(c)
}

/// Where the deployment's secrets are: what the stack seeds and the cell's
/// shim reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// `http://127.0.0.1:<port>`.
    pub addr: String,
    /// The KV v2 mount (`MOUNT`).
    pub mount: String,
    /// Its state: the admin's credentials, the cell's token.
    pub layout: Layout,
}

/// What one seeding did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seeded {
    /// Written as a new version.
    pub written: usize,
    /// Already there, the same.
    pub kept: usize,
}

impl Target {
    fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            // a token never follows a redirect
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("an HTTP client")
    }

    /// `method path` with `token` and `body`: its status and its answer's
    /// JSON (null when it has none).
    fn call(&self, method: reqwest::Method, path: &str, token: Option<&str>, body: Option<&Value>, what: &str) -> Result<(u16, Value), OpenBaoError> {
        let mut req = Self::client().request(method, format!("{}/v1/{path}", self.addr));
        if let Some(t) = token {
            req = req.header("x-vault-token", t);
        }
        if let Some(b) = body {
            req = req.header("content-type", "application/json").body(b.to_string());
        }
        let down = |e: &dyn fmt::Display| OpenBaoError::Down { what: what.to_string(), detail: e.to_string() };
        let resp = req.send().map_err(|e| down(&e))?;
        let status = resp.status().as_u16();
        let mut bytes = Vec::new();
        resp.take(ANSWER_BYTES_MAX + 1).read_to_end(&mut bytes).map_err(|e| down(&e))?;
        if bytes.len() as u64 > ANSWER_BYTES_MAX {
            return Err(OpenBaoError::Api { what: what.to_string(), status, detail: format!("an answer over {ANSWER_BYTES_MAX} bytes") });
        }
        Ok((status, serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
    }

    /// The first of an answer's `errors`, for a message.
    fn said(body: &Value) -> String {
        let first = body["errors"].as_array().and_then(|e| e.first()).and_then(Value::as_str).unwrap_or("");
        let first = first.split_whitespace().collect::<Vec<_>>().join(" ");
        first.chars().take(200).collect()
    }

    /// A short-lived token of the stack's own (the AppRole `ADMIN_ROLE`).
    pub fn admin_token(&self) -> Result<String, OpenBaoError> {
        let read = |path: PathBuf| fs::read_to_string(&path).map(|s| s.trim().to_string()).map_err(io_error(format!("read {}", path.display())));
        let body = json!({ "role_id": read(self.layout.admin_role_id())?, "secret_id": read(self.layout.admin_secret_id())? });
        let what = "the stack's AppRole login";
        let (status, answer) = self.call(reqwest::Method::POST, "auth/approle/login", None, Some(&body), what)?;
        match (status, answer["auth"]["client_token"].as_str()) {
            (200, Some(token)) => Ok(token.to_string()),
            _ => Err(OpenBaoError::Api { what: what.into(), status, detail: Self::said(&answer) }),
        }
    }

    /// Each secret (its store name, its value) under the mount, a new
    /// version only where the value differs: a restart writes nothing new.
    pub fn seed(&self, values: &[(&str, &str)]) -> Result<Seeded, OpenBaoError> {
        if values.len() > SEED_MAX {
            return Err(OpenBaoError::BadSecret(format!("{} secrets; one seeding writes at most {SEED_MAX}", values.len())));
        }
        for (name, value) in values {
            if !crate::store::valid_name(name) {
                return Err(OpenBaoError::BadSecret(format!("{name:?} is not a secret's name (letters, digits, '-' and '_', at most {})", crate::store::NAME_MAX_BYTES)));
            }
            if value.is_empty() || value.len() > VALUE_BYTES_MAX {
                return Err(OpenBaoError::BadSecret(format!("the value of {name} is {} bytes: a secret's is 1 to {VALUE_BYTES_MAX}", value.len())));
            }
        }
        let token = self.admin_token()?;
        let mut seeded = Seeded { written: 0, kept: 0 };
        for (name, value) in values {
            let path = format!("{}/data/{name}", self.mount);
            let (status, held) = self.call(reqwest::Method::GET, &path, Some(&token), None, &format!("a read of {name}"))?;
            match status {
                200 if held["data"]["data"][VALUE_FIELD].as_str() == Some(value) => {
                    seeded.kept += 1;
                    continue;
                }
                200 | 404 => {}
                _ => return Err(OpenBaoError::Api { what: format!("a read of {name}"), status, detail: Self::said(&held) }),
            }
            let what = format!("a write of {name}");
            let (status, answer) = self.call(reqwest::Method::POST, &path, Some(&token), Some(&json!({ "data": { VALUE_FIELD: value } })), &what)?;
            if status != 200 {
                return Err(OpenBaoError::Api { what, status, detail: Self::said(&answer) });
            }
            seeded.written += 1;
        }
        assert_eq!(seeded.written + seeded.kept, values.len(), "every secret written or kept");
        Ok(seeded)
    }

    /// The cell's token: the one in `cell.token`, renewed; or, when there is
    /// none or it has lapsed, a new one from the cell's role, written there.
    pub fn cell_token(&self) -> Result<String, OpenBaoError> {
        let admin = self.admin_token()?;
        let path = self.layout.cell_token();
        if path.exists() {
            let held = String::from_utf8(secret_file(&path, Vec::new)?).map_err(|_| OpenBaoError::BadFile { path: path.clone(), detail: "not text".into() })?;
            let held = held.trim().to_string();
            match self.renew(&admin, &held)? {
                true => return Ok(held),
                // lapsed or revoked: a new one below
                false => fs::remove_file(&path).map_err(io_error(format!("remove {}", path.display())))?,
            }
        }
        let what = "a token from the cell's role";
        let body = json!({ "display_name": CELL_ROLE, "meta": { "for": "the cell's secrets shim (cell/secrets.mjs)" } });
        let (status, answer) = self.call(reqwest::Method::POST, &format!("auth/token/create/{CELL_ROLE}"), Some(&admin), Some(&body), what)?;
        let token = match (status, answer["auth"]["client_token"].as_str()) {
            (200, Some(t)) => t.to_string(),
            _ => return Err(OpenBaoError::Api { what: what.into(), status, detail: Self::said(&answer) }),
        };
        let tmp = path.with_extension("token.new");
        let _ = fs::remove_file(&tmp);
        secret_file(&tmp, || token.clone().into_bytes())?;
        fs::rename(&tmp, &path).map_err(io_error(format!("move {} to {}", tmp.display(), path.display())))?;
        Ok(token)
    }

    /// Renews the cell's token in `cell.token` for another period: `false`
    /// when there is none, or it has lapsed (the stack must start again,
    /// which mints another).
    pub fn renew_cell_token(&self) -> Result<bool, OpenBaoError> {
        let path = self.layout.cell_token();
        if !path.exists() {
            return Ok(false);
        }
        let held = String::from_utf8(secret_file(&path, Vec::new)?).map_err(|_| OpenBaoError::BadFile { path: path.clone(), detail: "not text".into() })?;
        self.renew(&self.admin_token()?, held.trim())
    }

    /// Renews `token` with the stack's `admin` token: `false` when OpenBao
    /// knows no such token.
    fn renew(&self, admin: &str, token: &str) -> Result<bool, OpenBaoError> {
        let what = "a renewal of the cell's token";
        let (status, answer) = self.call(reqwest::Method::POST, "auth/token/renew", Some(admin), Some(&json!({ "token": token })), what)?;
        match status {
            200 => Ok(true),
            // "token not found", or one no longer valid
            400 | 403 => Ok(false),
            _ => Err(OpenBaoError::Api { what: what.into(), status, detail: Self::said(&answer) }),
        }
    }

    /// `FRAGMENT_SECRETS` for the shim's `openbao` backend, for `bindings`
    /// (each a binding and its secret's store name).
    pub fn shim_config(&self, bindings: &[(String, &str)]) -> Value {
        let secrets: Vec<Value> = bindings.iter().map(|(binding, name)| json!({ "binding": binding, "secret_name": name })).collect();
        json!({ "backend": "openbao", "addr": self.addr, "mount": self.mount, "secrets": secrets })
    }
}

/// How the stack runs its OpenBao.
#[derive(Clone, Debug)]
pub struct Options {
    /// The state directory (`prepare`).
    pub dir: PathBuf,
    /// Its port on 127.0.0.1.
    pub port: u16,
    /// Every request in `audit.log` (its secrets HMAC'd, as OpenBao's audit
    /// devices keep them). It grows without end: the e2e's, never dev's.
    pub audit: bool,
}

/// OpenBao, running: this process's child, stopped when this is dropped.
pub struct OpenBao {
    bin: PathBuf,
    opts: Options,
    layout: Layout,
    child: Option<Child>,
    starts: u32,
}

impl OpenBao {
    /// Starts it on its state (made on first use), and waits until it is
    /// unsealed and initialised: until the stack's AppRole logs in.
    pub fn start(bin: &Path, opts: Options) -> Result<(OpenBao, Duration), OpenBaoError> {
        let layout = prepare(&opts.dir)?;
        let mut bao = OpenBao { bin: bin.to_path_buf(), opts, layout, child: None, starts: 0 };
        let took = bao.up()?;
        Ok((bao, took))
    }

    /// Where it is, for the fleet's secrets.
    pub fn target(&self) -> Target {
        Target { addr: format!("http://127.0.0.1:{}", self.opts.port), mount: MOUNT.into(), layout: self.layout.clone() }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn running(&self) -> bool {
        self.child.is_some()
    }

    /// Starts it again on the same state, after `down`: unsealed by its
    /// static seal, never initialised again.
    pub fn up(&mut self) -> Result<Duration, OpenBaoError> {
        assert!(self.child.is_none(), "one OpenBao at a time on a state");
        let layout = prepare(&self.layout.dir)?;
        let config = render_config(&layout, self.opts.port, self.opts.audit)?;
        let tmp = layout.config().with_extension("hcl.new");
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(io_error(format!("create {}", tmp.display())))?;
        f.write_all(config.as_bytes()).map_err(io_error(format!("write {}", tmp.display())))?;
        fs::rename(&tmp, layout.config()).map_err(io_error(format!("move {} to {}", tmp.display(), layout.config().display())))?;
        self.starts += 1;
        let log_path = layout.log();
        let mut log = fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&log_path).map_err(io_error(format!("open {}", log_path.display())))?;
        writeln!(log, "== fragment: start {} of OpenBao {OPENBAO_VERSION} on 127.0.0.1:{}", self.starts, self.opts.port).map_err(io_error(format!("write {}", log_path.display())))?;
        let err = log.try_clone().map_err(io_error("clone the log"))?;
        let t0 = Instant::now();
        // nothing from this environment: every setting is the config's
        let child = Command::new(&self.bin)
            .arg("server")
            .arg(format!("-config={}", layout.config().display()))
            .env_clear()
            .env("HOME", &layout.dir)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(io_error(format!("start {}", self.bin.display())))?;
        self.child = Some(child);
        if let Err(e) = self.wait_ready(t0) {
            let _ = self.down();
            return Err(e);
        }
        Ok(t0.elapsed())
    }

    fn wait_ready(&mut self, t0: Instant) -> Result<(), OpenBaoError> {
        let target = self.target();
        let failed = |detail: String, log: &Path| OpenBaoError::Start { detail, log: log.to_path_buf(), tail: tail(log) };
        let mut sealed_since: Option<Instant> = None;
        // bounded by READY_TIMEOUT
        loop {
            if let Some(status) = self.child.as_mut().expect("started").try_wait().map_err(io_error("wait for OpenBao"))? {
                self.child = None;
                return Err(failed(format!("exited ({status}) before it was ready"), &self.layout.log()));
            }
            if target.admin_token().is_ok() {
                return Ok(());
            }
            let health = target.call(reqwest::Method::GET, "sys/health", None, None, "its health").ok();
            match health {
                Some((_, h)) if h["initialized"] == true && h["sealed"] == true => {
                    let since = *sealed_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > SEALED_GRACE {
                        return Err(failed(format!("stays sealed: its seal key, {}, does not open {}", self.layout.seal_key().display(), self.layout.data().display()), &self.layout.log()));
                    }
                }
                _ => sealed_since = None,
            }
            if t0.elapsed() > READY_TIMEOUT {
                return Err(failed(format!("was not ready (initialised, unsealed, the stack's AppRole logging in) after {READY_TIMEOUT:?}"), &self.layout.log()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Stops it: SIGTERM, as an operator's stop; SIGKILL past `STOP_TIMEOUT`.
    pub fn down(&mut self) -> Result<(), OpenBaoError> {
        let Some(mut child) = self.child.take() else { return Ok(()) };
        if child.try_wait().map_err(io_error("wait for OpenBao"))?.is_some() {
            return Ok(());
        }
        // its own PID: this process started it
        let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).stderr(Stdio::null()).status();
        let t0 = Instant::now();
        // bounded by STOP_TIMEOUT
        while child.try_wait().map_err(io_error("wait for OpenBao"))?.is_none() {
            if t0.elapsed() > STOP_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OpenBaoError::Start { detail: format!("did not stop within {STOP_TIMEOUT:?}, and was killed"), log: self.layout.log(), tail: tail(&self.layout.log()) });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

impl Drop for OpenBao {
    fn drop(&mut self) {
        let _ = self.down();
    }
}

/// The last lines of a log, for an error.
fn tail(log: &Path) -> String {
    let text = fs::read(log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

/// How many times OpenBao has initialised its state, from its log: once,
/// on its first start, whatever starts followed.
pub fn initialisations(layout: &Layout) -> Result<usize, OpenBaoError> {
    let log = fs::read_to_string(layout.log()).map_err(io_error(format!("read {}", layout.log().display())))?;
    Ok(log.matches(&format!("successful mount: namespace=\"\" path={MOUNT}/")).count())
}

/// What an audit log holds (`Options::audit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Audited {
    /// Every request OpenBao took.
    pub requests: usize,
    /// The reads of a secret under the mount by a token with the cell's
    /// policy: the shim's.
    pub cell_reads: usize,
    pub bytes: u64,
}

/// The audit log's counts: one JSON object per line, a request's and then
/// its response's.
pub fn audited(layout: &Layout) -> Result<Audited, OpenBaoError> {
    let path = layout.audit();
    let text = fs::read_to_string(&path).map_err(io_error(format!("read {}", path.display())))?;
    let mut a = Audited { requests: 0, cell_reads: 0, bytes: text.len() as u64 };
    for line in text.lines() {
        let entry: Value = serde_json::from_str(line).map_err(|e| OpenBaoError::BadFile { path: path.clone(), detail: format!("a line that is not JSON: {e}") })?;
        if entry["type"] != "request" {
            continue;
        }
        a.requests += 1;
        let cell = entry["auth"]["policies"].as_array().is_some_and(|p| p.iter().any(|p| p == CELL_POLICY));
        let read = entry["request"]["operation"] == "read" && entry["request"]["path"].as_str().is_some_and(|p| p.starts_with(&format!("{MOUNT}/data/")));
        if cell && read {
            a.cell_reads += 1;
        }
    }
    Ok(a)
}

/// Renews the cell's token every `every` while the stack runs, so a stack
/// up for longer than its period keeps reading; stopped when dropped.
pub struct Renewer {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Renewer {
    pub fn start(target: Target, every: Duration) -> Renewer {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            // bounded: until the stack drops its renewer
            while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                match target.renew_cell_token() {
                    Ok(true) => {}
                    Ok(false) => eprintln!("OpenBao: the cell's token has lapsed or is gone; start the stack again for a new one"),
                    Err(e) => eprintln!("OpenBao: renewing the cell's token failed, tried again in {every:?}: {e}"),
                }
            }
        });
        Renewer { stop: Some(stop), thread: Some(thread) }
    }
}

impl Drop for Renewer {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests;
