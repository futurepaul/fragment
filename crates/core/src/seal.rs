//! Secrets at rest (docs/secrets.md). A value is sealed with AES-256-GCM
//! under a key derived per Durable Object: HKDF-SHA256 of the deployment's
//! host secret (a Worker secret), salted with the object's scope (its class
//! and id), so a value opens only in the object that sealed it. A sealed
//! value names which host secret sealed it (`w2.<kid>.<base64
//! nonce‖ciphertext>`), so a deployment can rotate: configure the new
//! secret as current and the old one as previous; values sealed under the
//! previous one open and are resealed.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

const INFO: &[u8] = b"fragment/keys seal v2";
/// A host secret shorter than this is a configuration mistake.
pub const HOST_SECRET_MIN_BYTES: usize = 32;
/// The largest value sealed (a fragment secret is at most 64 KiB).
pub const PLAINTEXT_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// No host secret is configured.
    NoHostSecret,
    WeakHostSecret,
    TooLarge(usize),
    Malformed,
    /// No configured host secret has this key id.
    UnknownKey(String),
    /// The key matched but the value does not decrypt: corrupt, or another object's.
    Corrupt,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealError::NoHostSecret => write!(f, "no host secret is configured (FRAGMENT_HOST_SECRET)"),
            SealError::WeakHostSecret => write!(f, "the host secret must be at least {HOST_SECRET_MIN_BYTES} bytes"),
            SealError::TooLarge(n) => write!(f, "a sealed value is at most {PLAINTEXT_MAX_BYTES} bytes, not {n}"),
            SealError::Malformed => write!(f, "a sealed value is not in the w2 format"),
            SealError::UnknownKey(kid) => write!(f, "no configured host secret has key id {kid}"),
            SealError::Corrupt => write!(f, "this cell cannot open that value (corrupt, or sealed for another cell)"),
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

fn cipher(host_secret: &str, scope: &str) -> Result<Aes256Gcm, SealError> {
    if host_secret.len() < HOST_SECRET_MIN_BYTES {
        return Err(SealError::WeakHostSecret);
    }
    let mut key = [0u8; 32];
    let salt = format!("cell:{scope}");
    Hkdf::<Sha256>::new(Some(salt.as_bytes()), host_secret.as_bytes()).expand(INFO, &mut key).expect("32 bytes is a valid HKDF-SHA256 length");
    Ok(Aes256Gcm::new(&key.into()))
}

/// Seals `plaintext` for `scope` under `host_secrets[0]` (the current
/// secret) with a fresh `nonce`.
pub fn seal(host_secrets: &[&str], scope: &str, plaintext: &[u8], nonce: [u8; 12]) -> Result<String, SealError> {
    let current = host_secrets.first().ok_or(SealError::NoHostSecret)?;
    if plaintext.len() > PLAINTEXT_MAX_BYTES {
        return Err(SealError::TooLarge(plaintext.len()));
    }
    let ct = cipher(current, scope)?.encrypt(&Nonce::from(nonce), plaintext).expect("AES-GCM encryption of an in-memory buffer does not fail");
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    Ok(format!("w2.{}.{}", key_id(current), base64::engine::general_purpose::STANDARD.encode(blob)))
}

#[derive(Debug)]
pub struct Opened {
    pub plaintext: Vec<u8>,
    /// Sealed under a previous host secret: reseal it.
    pub stale: bool,
}

/// Opens a value sealed for `scope`. `host_secrets[0]` is the current
/// secret; the rest are previous ones still accepted during a rotation.
pub fn open(host_secrets: &[&str], scope: &str, sealed: &str) -> Result<Opened, SealError> {
    if host_secrets.is_empty() {
        return Err(SealError::NoHostSecret);
    }
    let mut parts = sealed.splitn(3, '.');
    let (Some("w2"), Some(kid), Some(b64)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(SealError::Malformed);
    };
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| SealError::Malformed)?;
    if blob.len() < 12 + 16 {
        return Err(SealError::Malformed);
    }
    let (i, secret) = host_secrets.iter().enumerate().find(|(_, s)| key_id(s) == kid).ok_or_else(|| SealError::UnknownKey(kid.to_string()))?;
    let (nonce, ct) = blob.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().expect("split at 12");
    let plaintext = cipher(secret, scope)?.decrypt(&Nonce::from(nonce), ct).map_err(|_| SealError::Corrupt)?;
    Ok(Opened { plaintext, stale: i != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "0123456789abcdef0123456789abcdef-current";
    const OLD: &str = "0123456789abcdef0123456789abcdef-previous";
    const CELL: &str = "Fragment:aaaa";
    const OTHER: &str = "Fragment:bbbb";

    #[test]
    fn seal_and_open() {
        let sealed = seal(&[HOST], CELL, b"sk-live-123", [7; 12]).unwrap();
        assert!(sealed.starts_with(&format!("w2.{}.", key_id(HOST))));
        assert!(!sealed.contains("sk-live"));
        let opened = open(&[HOST], CELL, &sealed).unwrap();
        assert_eq!(opened.plaintext, b"sk-live-123");
        assert!(!opened.stale);
    }

    #[test]
    fn another_cell_cannot_open() {
        let sealed = seal(&[HOST], CELL, b"x", [1; 12]).unwrap();
        assert_eq!(open(&[HOST], OTHER, &sealed).err(), Some(SealError::Corrupt));
    }

    /// The current secret seals; a previous one still opens, and what it
    /// opens is stale, so the caller reseals it.
    #[test]
    fn rotation() {
        let sealed = seal(&[OLD], CELL, b"v", [2; 12]).unwrap();
        assert!(matches!(open(&[HOST], CELL, &sealed), Err(SealError::UnknownKey(_))));
        let opened = open(&[HOST, OLD], CELL, &sealed).unwrap();
        assert_eq!(opened.plaintext, b"v");
        assert!(opened.stale);
        let fresh = seal(&[HOST, OLD], CELL, &opened.plaintext, [3; 12]).unwrap();
        assert!(fresh.starts_with(&format!("w2.{}.", key_id(HOST))));
        assert!(!open(&[HOST], CELL, &fresh).unwrap().stale);
    }

    #[test]
    fn tampering_limits_and_weak_secrets() {
        let sealed = seal(&[HOST], CELL, b"value", [3; 12]).unwrap();
        let mut bytes = sealed.into_bytes();
        let last = bytes.len() - 3;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(open(&[HOST], CELL, &tampered).is_err());
        assert_eq!(seal(&["short"], CELL, b"v", [0; 12]).err(), Some(SealError::WeakHostSecret));
        assert_eq!(seal(&[], CELL, b"v", [0; 12]).err(), Some(SealError::NoHostSecret));
        assert_eq!(open(&[], CELL, "w2.x.y").err(), Some(SealError::NoHostSecret));
        let big = vec![0u8; PLAINTEXT_MAX_BYTES + 1];
        assert_eq!(seal(&[HOST], CELL, &big, [0; 12]).err(), Some(SealError::TooLarge(PLAINTEXT_MAX_BYTES + 1)));
        assert!(seal(&[HOST], CELL, &big[1..], [0; 12]).is_ok(), "at the limit seals");
        assert_eq!(open(&[HOST], CELL, "plain").err(), Some(SealError::Malformed));
        assert_eq!(open(&[HOST], CELL, "w1.x.y").err(), Some(SealError::Malformed), "w1 went at the hard cut");
    }
}
