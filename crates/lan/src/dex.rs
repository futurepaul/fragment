//! Dex, the identity provider an intranet runs (docs/self-host.md, seam 4):
//! pinned, fetched, and configured with the zone's client and its people.
//!
//! **The binary.** Dex publishes no release binaries, only images, so the
//! pin is its release image's layer that holds `/usr/local/bin/dex`, by
//! digest (`DEX_LAYER`), fetched from ghcr.io anonymously, then the binary
//! by its own SHA-256 (`DEX_SHA256`). It is a static Go binary (linux
//! amd64); `DEX_BIN` names another.
//!
//! **Its configuration** (`dex.yaml`, mode 0600: it holds the client's
//! secret): the issuer `https://dex.<zone>`, served on loopback behind the
//! front door; one static client, the cell (its secret a file, mode 0600);
//! and the people (`users`), each a static password whose password is a
//! file of its own (mode 0600), made on first use and never printed. A
//! person's id is derived from their name, so their `sub` (Dex's encoding
//! of the id and the connector) is the same across restarts and their
//! fragments stay theirs. Storage is memory: nothing Dex holds outlives a
//! sign-in.

use std::fmt;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const DEX_VERSION: &str = "v2.45.1";
/// ghcr.io/dexidp/dex:v2.45.1, linux/amd64: its image index and manifest
/// (for the record), and the layer holding the binary (the pin).
pub const DEX_REPOSITORY: &str = "dexidp/dex";
pub const DEX_MANIFEST: &str = "sha256:f5f9fb373188b0f701b80edd68b3f4745ced69306c9d382999f11b7774335a98";
pub const DEX_LAYER: &str = "sha256:680f1119fdbcdd7f0ad8985b2534ded871b08b5b77c40f337c38aebd062b59c2";
/// `usr/local/bin/dex` in that layer.
pub const DEX_PATH: &str = "usr/local/bin/dex";
pub const DEX_SHA256: &str = "217aa675738d1cbaf25fe8d7bc04cb00dbc60f91d8465b76b8d8af963d571337";
/// The layer's size is about 15 MB and the binary's 43 MB: bounds well past
/// both, against a registry that sends without end.
const LAYER_BYTES_MAX: u64 = 64 << 20;
const BINARY_BYTES_MAX: u64 = 128 << 20;
const REGISTRY: &str = "https://ghcr.io";

/// The client the cell signs in as.
pub const CLIENT_ID: &str = "fragment";
/// The most people the configuration names.
pub const USERS_MAX: usize = 32;
/// A password a person wrote in their file themselves is at least this long.
pub const PASSWORD_CHARS_MIN: usize = 12;

#[derive(Debug)]
pub enum DexError {
    Io { path: PathBuf, error: std::io::Error },
    Fetch(String),
    /// What came is not what is pinned.
    Digest { what: &'static str, want: String, got: String },
    /// No pinned binary for this machine.
    Platform(String),
    BadUser(String),
    /// A password a person wrote in their file is too short.
    ShortPassword(PathBuf),
    Hash(String),
    KeyMode { path: PathBuf, mode: u32 },
}

impl fmt::Display for DexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DexError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            DexError::Fetch(e) => write!(f, "fetching Dex {DEX_VERSION} from ghcr.io/{DEX_REPOSITORY}: {e}"),
            DexError::Digest { what, want, got } => write!(f, "Dex {DEX_VERSION}: the {what} is {got}, not the pinned {want}"),
            DexError::Platform(p) => write!(f, "Dex is pinned for linux x86_64, not {p}: set DEX_BIN to a Dex {DEX_VERSION} binary"),
            DexError::Hash(e) => write!(f, "bcrypt: {e}"),
            DexError::ShortPassword(p) => write!(f, "the password in {} is shorter than {PASSWORD_CHARS_MIN} characters", p.display()),
            DexError::BadUser(u) => write!(f, "{u:?} is not a user name: lower-case letters, digits and inner hyphens, 1 to 32"),
            DexError::KeyMode { path, mode } => write!(f, "{} is mode {mode:o}: a secret must be readable by its owner alone (chmod 600 it)", path.display()),
        }
    }
}

impl std::error::Error for DexError {}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> DexError + '_ {
    move |error| DexError::Io { path: path.to_path_buf(), error }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The pinned Dex under `tools` (`<tools>/dex-<version>/dex`), fetched and
/// checked on first use.
pub fn locate(tools: &Path) -> Result<PathBuf, DexError> {
    let bin = tools.join(format!("dex-{DEX_VERSION}")).join("dex");
    if bin.is_file() {
        let bytes = fs::read(&bin).map_err(io(&bin))?;
        let got = sha256_hex(&bytes);
        if got == DEX_SHA256 {
            return Ok(bin);
        }
        // a binary changed in place is fetched again, never run
        fs::remove_file(&bin).map_err(io(&bin))?;
    }
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        return Err(DexError::Platform(format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)));
    }
    let layer = fetch_layer()?;
    let binary = binary_from_layer(&layer)?;
    let dir = bin.parent().expect("the binary has a directory");
    fs::create_dir_all(dir).map_err(io(dir))?;
    let tmp = dir.join("dex.tmp");
    fs::write(&tmp, &binary).map_err(io(&tmp))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(io(&tmp))?;
    fs::rename(&tmp, &bin).map_err(io(&bin))?;
    Ok(bin)
}

/// The pinned layer, by digest, from ghcr.io with an anonymous pull token.
fn fetch_layer() -> Result<Vec<u8>, DexError> {
    let fetch = |e: reqwest::Error| DexError::Fetch(e.to_string());
    let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(300)).build().map_err(fetch)?;
    let token: serde_json::Value =
        http.get(format!("{REGISTRY}/token?scope=repository:{DEX_REPOSITORY}:pull")).send().and_then(|r| r.error_for_status()).and_then(|r| r.json()).map_err(fetch)?;
    let token = token["token"].as_str().ok_or_else(|| DexError::Fetch("the registry gave no token".into()))?;
    let resp = http.get(format!("{REGISTRY}/v2/{DEX_REPOSITORY}/blobs/{DEX_LAYER}")).bearer_auth(token).send().and_then(|r| r.error_for_status()).map_err(fetch)?;
    let mut layer = Vec::new();
    resp.take(LAYER_BYTES_MAX + 1).read_to_end(&mut layer).map_err(|e| DexError::Fetch(e.to_string()))?;
    if layer.len() as u64 > LAYER_BYTES_MAX {
        return Err(DexError::Fetch(format!("the layer is over {LAYER_BYTES_MAX} bytes")));
    }
    let got = format!("sha256:{}", sha256_hex(&layer));
    if got != DEX_LAYER {
        return Err(DexError::Digest { what: "layer", want: DEX_LAYER.into(), got });
    }
    Ok(layer)
}

/// `DEX_PATH` from a gzipped layer, checked against `DEX_SHA256`.
pub fn binary_from_layer(layer: &[u8]) -> Result<Vec<u8>, DexError> {
    let bad = |e: std::io::Error| DexError::Fetch(format!("unpacking the layer: {e}"));
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(layer));
    for entry in archive.entries().map_err(bad)? {
        let entry = entry.map_err(bad)?;
        if entry.path().map_err(bad)?.to_str() != Some(DEX_PATH) {
            continue;
        }
        let mut binary = Vec::new();
        entry.take(BINARY_BYTES_MAX + 1).read_to_end(&mut binary).map_err(bad)?;
        let got = sha256_hex(&binary);
        if got != DEX_SHA256 {
            return Err(DexError::Digest { what: "binary", want: DEX_SHA256.into(), got });
        }
        return Ok(binary);
    }
    Err(DexError::Fetch(format!("the layer holds no {DEX_PATH}")))
}

/// One person Dex signs in.
#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub email: String,
    /// Their password's file (mode 0600).
    pub password_file: PathBuf,
}

/// Dex as the stack runs it.
#[derive(Debug, Clone)]
pub struct DexSetup {
    pub issuer: String,
    /// Where Dex listens: loopback, behind the front door.
    pub listen: std::net::SocketAddr,
    /// The cell's redirect URI (`<platform>/auth/callback`).
    pub redirect_uri: String,
    /// The client's secret's file (mode 0600).
    pub client_secret_file: PathBuf,
    pub users: Vec<User>,
    /// `dex.yaml`.
    pub config_file: PathBuf,
}

/// How a new secret is written.
#[derive(Clone, Copy)]
pub enum Shape {
    /// `n` random bytes, hex: a machine's secret.
    Hex(usize),
    /// A password a person types on a phone: 100 random bits as 20
    /// lower-case letters and digits (RFC 4648's base32 alphabet), in groups
    /// of four (`abcd-efgh-ijkl-mnop-qrst`).
    Typed,
}

/// A password in `Shape::Typed`'s form.
fn typed_password() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut raw = [0u8; 20];
    crate::ca::getrandom(&mut raw);
    let chars: Vec<char> = raw.iter().map(|b| ALPHABET[(b & 31) as usize] as char).collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

/// The secret in `path`: made on first use (mode 0600 from its creation),
/// else read as it is, so a person may write their own password there
/// first. One others may read is refused.
pub fn secret_file(path: &Path, shape: Shape) -> Result<String, DexError> {
    if path.exists() {
        let mode = fs::metadata(path).map_err(io(path))?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(DexError::KeyMode { path: path.to_path_buf(), mode });
        }
        return Ok(fs::read_to_string(path).map_err(io(path))?.trim().to_string());
    }
    let value = match shape {
        Shape::Hex(bytes) => {
            let mut raw = vec![0u8; bytes];
            crate::ca::getrandom(&mut raw);
            hex::encode(raw)
        }
        Shape::Typed => typed_password(),
    };
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(io(path))?;
    f.write_all(value.as_bytes()).map_err(io(path))?;
    Ok(value)
}

/// A stable UUID-shaped id for a person, from their zone and name.
fn user_id(zone: &str, name: &str) -> String {
    let h = Sha256::digest(format!("fragment-lan user {zone} {name}").as_bytes());
    let hex = hex::encode(&h[..16]);
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Dex's configuration for the stack, in `dir` (0700): `dex.yaml` (0600),
/// the client's secret (`client-secret`) and each person's password
/// (`passwords/<name>`), made on first use. `names` are the people.
pub fn configure(dir: &Path, zone: &str, issuer: &str, listen: std::net::SocketAddr, redirect_uri: &str, names: &[String]) -> Result<DexSetup, DexError> {
    assert!(!names.is_empty() && names.len() <= USERS_MAX, "1 to {USERS_MAX} people");
    for n in names {
        if !(1..=32).contains(&n.len()) || !crate::valid_label(n) {
            return Err(DexError::BadUser(n.clone()));
        }
    }
    let passwords = dir.join("passwords");
    fs::create_dir_all(&passwords).map_err(io(&passwords))?;
    for d in [dir, passwords.as_path()] {
        fs::set_permissions(d, fs::Permissions::from_mode(0o700)).map_err(io(d))?;
    }
    let client_secret_file = dir.join("client-secret");
    let client_secret = secret_file(&client_secret_file, Shape::Hex(32))?;
    let mut users = vec![];
    let mut static_passwords = vec![];
    for name in names {
        let password_file = passwords.join(name);
        let password = secret_file(&password_file, Shape::Typed)?;
        if password.chars().count() < PASSWORD_CHARS_MIN {
            return Err(DexError::ShortPassword(password_file));
        }
        let hash = bcrypt::hash(&password, 10).map_err(|e| DexError::Hash(e.to_string()))?;
        let email = format!("{name}@{zone}");
        static_passwords.push(serde_json::json!({
            "email": email,
            "hash": hash,
            "username": name,
            "preferredUsername": name,
            "userID": user_id(zone, name),
        }));
        users.push(User { name: name.clone(), email, password_file });
    }
    // JSON is YAML: Dex reads it as its config
    let config = serde_json::json!({
        "issuer": issuer,
        "storage": { "type": "memory" },
        "web": { "http": listen.to_string() },
        "logger": { "level": "info", "format": "text" },
        "oauth2": { "skipApprovalScreen": true, "responseTypes": ["code"] },
        "frontend": { "issuer": format!("fragment on {zone}") },
        "staticClients": [{
            "id": CLIENT_ID,
            "name": format!("fragment on {zone}"),
            "secret": client_secret,
            "redirectURIs": [redirect_uri],
        }],
        "enablePasswordDB": true,
        "staticPasswords": static_passwords,
    });
    let config_file = dir.join("dex.yaml");
    let tmp = dir.join("dex.yaml.tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(io(&tmp))?;
    f.write_all(serde_json::to_string_pretty(&config).expect("JSON").as_bytes()).map_err(io(&tmp))?;
    fs::rename(&tmp, &config_file).map_err(io(&config_file))?;
    Ok(DexSetup { issuer: issuer.into(), listen, redirect_uri: redirect_uri.into(), client_secret_file, users, config_file })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let mut b = [0u8; 6];
        crate::ca::getrandom(&mut b);
        std::env::temp_dir().join(format!("fragment-lan-{name}-{}", hex::encode(b)))
    }

    // Goal: the configuration names the issuer, the cell's client and each
    // person with a bcrypt hash of a password kept in a file of its own;
    // a second run keeps every secret, so people and their ids are the same.
    #[test]
    fn the_configuration_keeps_its_secrets_and_ids() {
        let dir = tmp("dex");
        let listen = "127.0.0.1:5556".parse().unwrap();
        let names = vec!["paul".to_string(), "guest".to_string()];
        let setup = configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://fragment.home.arpa/auth/callback", &names).unwrap();
        let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(&setup.config_file).unwrap()).unwrap();
        assert_eq!(config["issuer"], "https://dex.fragment.home.arpa");
        assert_eq!(config["web"]["http"], "127.0.0.1:5556");
        assert_eq!(config["staticClients"][0]["redirectURIs"][0], "https://fragment.home.arpa/auth/callback");
        let secret = fs::read_to_string(&setup.client_secret_file).unwrap();
        assert_eq!(config["staticClients"][0]["secret"], secret.as_str());
        let paul = &config["staticPasswords"][0];
        assert_eq!(paul["email"], "paul@fragment.home.arpa");
        let password = fs::read_to_string(&setup.users[0].password_file).unwrap();
        assert_eq!(password.len(), 24);
        assert!(password.split('-').all(|g| g.len() == 4 && g.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))), "{}", password.len());
        assert!(bcrypt::verify(&password, paul["hash"].as_str().unwrap()).unwrap());
        assert!(!fs::read_to_string(&setup.config_file).unwrap().contains(&password), "the config holds a hash, never the password");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for p in [&setup.config_file, &setup.client_secret_file, &setup.users[0].password_file] {
            assert_eq!(mode(p), 0o600, "{}", p.display());
        }
        let again = configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://fragment.home.arpa/auth/callback", &names).unwrap();
        let config2: serde_json::Value = serde_json::from_str(&fs::read_to_string(&again.config_file).unwrap()).unwrap();
        assert_eq!(config2["staticClients"][0]["secret"], secret.as_str());
        assert_eq!(config2["staticPasswords"][0]["userID"], paul["userID"]);
        assert_eq!(fs::read_to_string(&again.users[0].password_file).unwrap(), password);
        assert_ne!(config["staticPasswords"][1]["userID"], paul["userID"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: a password a person wrote in their file first is theirs (Dex
    // takes its hash); one too short is refused.
    #[test]
    fn a_person_may_choose_their_password() {
        let dir = tmp("dex-own");
        let listen = "127.0.0.1:5556".parse().unwrap();
        fs::create_dir_all(dir.join("passwords")).unwrap();
        let own = dir.join("passwords/paul");
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&own).unwrap();
        f.write_all(b"correct horse battery staple\n").unwrap();
        let setup = configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://x/auth/callback", &["paul".to_string()]).unwrap();
        let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(&setup.config_file).unwrap()).unwrap();
        assert!(bcrypt::verify("correct horse battery staple", config["staticPasswords"][0]["hash"].as_str().unwrap()).unwrap());
        fs::write(&own, "short").unwrap();
        let r = configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://x/auth/callback", &["paul".to_string()]);
        assert!(matches!(r, Err(DexError::ShortPassword(_))));
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: a name that is not one, and a secret others may read, are refused.
    #[test]
    fn bad_names_and_readable_secrets_are_refused() {
        let dir = tmp("dex-bad");
        let listen = "127.0.0.1:5556".parse().unwrap();
        for bad in ["Paul", "a b", "-x", ""] {
            let r = configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://x/auth/callback", &[bad.to_string()]);
            assert!(matches!(r, Err(DexError::BadUser(_))), "{bad}");
        }
        configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://x/auth/callback", &["paul".to_string()]).unwrap();
        fs::set_permissions(dir.join("client-secret"), fs::Permissions::from_mode(0o640)).unwrap();
        let r = configure(&dir, "fragment.home.arpa", "https://dex.fragment.home.arpa", listen, "https://x/auth/callback", &["paul".to_string()]);
        assert!(matches!(r, Err(DexError::KeyMode { mode: 0o640, .. })));
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: only the pinned binary comes out of a layer; another file at
    // its path is refused by its digest.
    #[test]
    fn a_layer_yields_only_the_pinned_binary() {
        let pack = |content: &[u8]| {
            let mut tar = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, DEX_PATH, content).unwrap();
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(&tar.into_inner().unwrap()).unwrap();
            gz.finish().unwrap()
        };
        let refused = binary_from_layer(&pack(b"#!/bin/sh\necho not dex\n")).unwrap_err();
        assert!(matches!(refused, DexError::Digest { what: "binary", .. }), "{refused}");
        assert!(binary_from_layer(b"not a gzip").is_err());
    }
}
