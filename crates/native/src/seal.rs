//! Secrets at rest (docs/secrets.md). A value is sealed with AES-256-GCM
//! under a key derived per cell: HKDF-SHA256 of the fleet's host secret,
//! salted with the cell's scope as the host attests it, so a value opens
//! only for the cell that sealed it. A sealed value names which host secret
//! sealed it (`w2.<kid>.<base64 nonce‖ciphertext>`), so the fleet can
//! rotate: configure the new secret as current and the old one as
//! previous; values sealed under the previous one open and are resealed.
//!
//! `w1` values come from before `KEYS` (the cell sealed them itself, salted
//! with a name it chose). They open when the caller names that salt, and
//! are always resealed as `w2`.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

const W1_INFO: &[u8] = b"fragment/wrapped-secrets v1";
const W2_INFO: &[u8] = b"fragment/keys seal v2";
/// A host secret shorter than this is a configuration mistake.
pub const HOST_SECRET_MIN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    WeakHostSecret,
    Malformed,
    /// A `w1` value, and the caller named no salt for it.
    NeedsLegacySalt,
    /// No configured host secret has this key id.
    UnknownKey(String),
    /// The key matched but the value does not decrypt: corrupt, or another cell's.
    Corrupt,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealError::WeakHostSecret => write!(f, "the host secret must be at least {HOST_SECRET_MIN_BYTES} bytes"),
            SealError::Malformed => write!(f, "a sealed value is not in the w1 or w2 format"),
            SealError::NeedsLegacySalt => write!(f, "a w1 value opens only with the salt it was sealed with (legacySalt)"),
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

fn cipher(host_secret: &str, salt: &[u8], info: &[u8]) -> Result<Aes256Gcm, SealError> {
    if host_secret.len() < HOST_SECRET_MIN_BYTES {
        return Err(SealError::WeakHostSecret);
    }
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), host_secret.as_bytes()).expand(info, &mut key).expect("32 bytes is a valid HKDF-SHA256 length");
    Ok(Aes256Gcm::new(&key.into()))
}

fn scope_salt(scope: &str) -> Vec<u8> {
    format!("cell:{scope}").into_bytes()
}

/// Seals `plaintext` for the cell `scope` with a fresh `nonce`.
pub fn seal(host_secret: &str, scope: &str, plaintext: &[u8], nonce: [u8; 12]) -> Result<String, SealError> {
    let ct = cipher(host_secret, &scope_salt(scope), W2_INFO)?
        .encrypt(&Nonce::from(nonce), plaintext)
        .expect("AES-GCM encryption of an in-memory buffer does not fail");
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    Ok(format!("w2.{}.{}", key_id(host_secret), base64::engine::general_purpose::STANDARD.encode(blob)))
}

#[derive(Debug)]
pub struct Opened {
    pub plaintext: Vec<u8>,
    /// Sealed under a previous host secret, or as `w1`: reseal it.
    pub stale: bool,
}

/// Opens a value sealed for the cell `scope`. `host_secrets[0]` is the
/// current secret; the rest are previous ones still accepted during a
/// rotation. `legacy_salt` opens a `w1` value.
pub fn open(host_secrets: &[&str], scope: &str, sealed: &str, legacy_salt: Option<&str>) -> Result<Opened, SealError> {
    let mut parts = sealed.splitn(3, '.');
    let (Some(format), Some(kid), Some(b64)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(SealError::Malformed);
    };
    let (salt, info) = match format {
        "w2" => (scope_salt(scope), W2_INFO),
        "w1" => (legacy_salt.ok_or(SealError::NeedsLegacySalt)?.as_bytes().to_vec(), W1_INFO),
        _ => return Err(SealError::Malformed),
    };
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| SealError::Malformed)?;
    if blob.len() < 12 + 16 {
        return Err(SealError::Malformed);
    }
    let (i, secret) =
        host_secrets.iter().enumerate().find(|(_, s)| key_id(s) == kid).ok_or_else(|| SealError::UnknownKey(kid.to_string()))?;
    let (nonce, ct) = blob.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().expect("split at 12");
    let plaintext = cipher(secret, &salt, info)?.decrypt(&Nonce::from(nonce), ct).map_err(|_| SealError::Corrupt)?;
    Ok(Opened { plaintext, stale: i != 0 || format == "w1" })
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
        let sealed = seal(HOST, CELL, b"sk-live-123", [7; 12]).unwrap();
        assert!(sealed.starts_with(&format!("w2.{}.", key_id(HOST))));
        assert!(!sealed.contains("sk-live"));
        let opened = open(&[HOST], CELL, &sealed, None).unwrap();
        assert_eq!(opened.plaintext, b"sk-live-123");
        assert!(!opened.stale);
    }

    #[test]
    fn another_cell_cannot_open() {
        let sealed = seal(HOST, CELL, b"x", [1; 12]).unwrap();
        assert_eq!(open(&[HOST], OTHER, &sealed, None).err(), Some(SealError::Corrupt));
        // naming a salt does not help with a w2 value: its salt is the caller
        assert_eq!(open(&[HOST], OTHER, &sealed, Some(CELL)).err(), Some(SealError::Corrupt));
    }

    #[test]
    fn rotation() {
        let sealed = seal(OLD, CELL, b"v", [2; 12]).unwrap();
        assert!(matches!(open(&[HOST], CELL, &sealed, None), Err(SealError::UnknownKey(_))));
        let opened = open(&[HOST, OLD], CELL, &sealed, None).unwrap();
        assert_eq!(opened.plaintext, b"v");
        assert!(opened.stale);
    }

    /// A value the cell sealed itself before `KEYS` (core's old `secrets::seal`).
    fn w1(host: &str, salt: &str, plaintext: &[u8], nonce: [u8; 12]) -> String {
        let ct = cipher(host, salt.as_bytes(), W1_INFO).unwrap().encrypt(&Nonce::from(nonce), plaintext).unwrap();
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        format!("w1.{}.{}", key_id(host), base64::engine::general_purpose::STANDARD.encode(blob))
    }

    #[test]
    fn legacy_values_open_with_their_salt_and_are_stale() {
        let old = w1(HOST, "npub1cell", b"legacy", [4; 12]);
        assert_eq!(open(&[HOST], CELL, &old, None).err(), Some(SealError::NeedsLegacySalt));
        assert_eq!(open(&[HOST], CELL, &old, Some("npub1other")).err(), Some(SealError::Corrupt));
        let opened = open(&[HOST], CELL, &old, Some("npub1cell")).unwrap();
        assert_eq!(opened.plaintext, b"legacy");
        assert!(opened.stale, "a w1 value is always resealed");
    }

    #[test]
    fn tampering_and_weak_secrets() {
        let sealed = seal(HOST, CELL, b"value", [3; 12]).unwrap();
        let mut bytes = sealed.into_bytes();
        let last = bytes.len() - 3;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(open(&[HOST], CELL, &tampered, None).is_err());
        assert_eq!(seal("short", CELL, b"v", [0; 12]).err(), Some(SealError::WeakHostSecret));
        assert_eq!(open(&[HOST], CELL, "plain", None).err(), Some(SealError::Malformed));
        assert_eq!(open(&[HOST], CELL, "w9.x.y", None).err(), Some(SealError::Malformed));
    }
}
