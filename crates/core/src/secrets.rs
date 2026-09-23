//! Secrets at rest (docs/secrets.md). A secret is sealed with AES-256-GCM
//! under a key derived per cell: HKDF-SHA256 of the fleet's host secret,
//! salted with the cell's npub. A sealed value names which host secret
//! sealed it (`w1.<kid>.<base64 nonce‖ciphertext>`), so the fleet can
//! rotate: configure the new secret as current and the old one as
//! previous; values sealed under the previous open and are resealed.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

const INFO: &[u8] = b"fragment/wrapped-secrets v1";
const FORMAT: &str = "w1";
/// A host secret shorter than this is a configuration mistake.
pub const HOST_SECRET_MIN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    WeakHostSecret,
    Malformed,
    /// No configured host secret has this key id.
    UnknownKey(String),
    /// The key matched but the value does not decrypt: corrupt, or another cell's.
    Corrupt,
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::WeakHostSecret => write!(f, "the host secret must be at least {HOST_SECRET_MIN_BYTES} bytes"),
            SecretError::Malformed => write!(f, "a sealed secret is not in the w1 format"),
            SecretError::UnknownKey(kid) => write!(f, "no configured host secret has key id {kid}"),
            SecretError::Corrupt => write!(f, "a sealed secret does not open (corrupt, or sealed for another cell)"),
        }
    }
}

/// The short id a sealed value carries to name its host secret.
pub fn key_id(host_secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"fragment host secret\0");
    h.update(host_secret.as_bytes());
    hex::encode(&h.finalize()[..4])
}

fn cipher(host_secret: &str, salt: &str) -> Result<Aes256Gcm, SecretError> {
    if host_secret.len() < HOST_SECRET_MIN_BYTES {
        return Err(SecretError::WeakHostSecret);
    }
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt.as_bytes()), host_secret.as_bytes())
        .expand(INFO, &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 length");
    Ok(Aes256Gcm::new(&key.into()))
}

/// Seals `plaintext` for the cell named by `salt` (its npub). The caller
/// supplies a fresh random nonce.
pub fn seal(host_secret: &str, salt: &str, plaintext: &[u8], nonce: [u8; 12]) -> Result<String, SecretError> {
    let ct = cipher(host_secret, salt)?
        .encrypt(&Nonce::from(nonce), plaintext)
        .expect("AES-GCM encryption of an in-memory buffer does not fail");
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    Ok(format!("{FORMAT}.{}.{}", key_id(host_secret), base64::engine::general_purpose::STANDARD.encode(blob)))
}

pub struct Opened {
    pub plaintext: Vec<u8>,
    /// Sealed under a host secret other than the current one: reseal it.
    pub stale: bool,
}

/// Opens a sealed value. `host_secrets[0]` is the current secret; the rest
/// are previous ones still accepted during a rotation.
pub fn open(host_secrets: &[&str], salt: &str, sealed: &str) -> Result<Opened, SecretError> {
    let mut parts = sealed.splitn(3, '.');
    let (Some(FORMAT), Some(kid), Some(b64)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(SecretError::Malformed);
    };
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| SecretError::Malformed)?;
    if blob.len() < 12 + 16 {
        return Err(SecretError::Malformed);
    }
    let (i, secret) = host_secrets
        .iter()
        .enumerate()
        .find(|(_, s)| key_id(s) == kid)
        .ok_or_else(|| SecretError::UnknownKey(kid.to_string()))?;
    let (nonce, ct) = blob.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().expect("split at 12");
    let plaintext = cipher(secret, salt)?.decrypt(&Nonce::from(nonce), ct).map_err(|_| SecretError::Corrupt)?;
    Ok(Opened { plaintext, stale: i != 0 })
}

/// The secret names a header value refers to as `{{NAME}}` (a job's
/// fetch; the cell substitutes them at its egress point).
pub fn placeholders(v: &str) -> Vec<&str> {
    let mut names = vec![];
    let mut rest = v;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start + 2..].find("}}") else { break };
        names.push(&rest[start + 2..start + 2 + end]);
        rest = &rest[start + 2 + end + 2..];
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_placeholders() {
        assert_eq!(placeholders("Bearer {{API_KEY}}"), vec!["API_KEY"]);
        assert_eq!(placeholders("{{A}}:{{B}}"), vec!["A", "B"]);
        assert!(placeholders("no {{ end").is_empty());
        assert!(placeholders("plain").is_empty());
    }

    const HOST: &str = "0123456789abcdef0123456789abcdef-current";
    const OLD: &str = "0123456789abcdef0123456789abcdef-previous";

    #[test]
    fn seal_and_open() {
        let sealed = seal(HOST, "npub1cell", b"sk-live-123", [7; 12]).unwrap();
        assert!(sealed.starts_with(&format!("w1.{}.", key_id(HOST))));
        assert!(!sealed.contains("sk-live"));
        let opened = open(&[HOST], "npub1cell", &sealed).unwrap();
        assert_eq!(opened.plaintext, b"sk-live-123");
        assert!(!opened.stale);
    }

    #[test]
    fn another_cell_cannot_open() {
        let sealed = seal(HOST, "npub1cell", b"x", [1; 12]).unwrap();
        assert_eq!(open(&[HOST], "npub1other", &sealed).err(), Some(SecretError::Corrupt));
    }

    #[test]
    fn rotation() {
        let sealed = seal(OLD, "npub1cell", b"v", [2; 12]).unwrap();
        assert!(matches!(open(&[HOST], "npub1cell", &sealed), Err(SecretError::UnknownKey(_))));
        let opened = open(&[HOST, OLD], "npub1cell", &sealed).unwrap();
        assert_eq!(opened.plaintext, b"v");
        assert!(opened.stale);
    }

    #[test]
    fn tampering_and_weak_secrets() {
        let sealed = seal(HOST, "npub1cell", b"value", [3; 12]).unwrap();
        let mut bytes = sealed.into_bytes();
        let last = bytes.len() - 3;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(open(&[HOST], "npub1cell", &tampered).is_err());
        assert_eq!(seal("short", "npub1cell", b"v", [0; 12]).err(), Some(SecretError::WeakHostSecret));
        assert_eq!(open(&[HOST], "npub1cell", "plain").err(), Some(SecretError::Malformed));
    }
}
