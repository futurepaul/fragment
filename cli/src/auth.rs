// This machine's nostr key and its NIP-98 HTTP auth: crates/nip98 signs
// (the same code the cell verifies with), crates/core names keys as npubs.
use anyhow::{anyhow, bail, Result};
use fragment_core::npub;
use fragment_nip98::Keys;

#[derive(Clone)]
pub struct Identity {
    keys: Keys,
}

fn now_s() -> i64 {
    let since_epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock is past 1970");
    i64::try_from(since_epoch.as_secs()).expect("seconds since 1970 fit an i64")
}

impl Identity {
    pub fn generate() -> Identity {
        Identity { keys: Keys::generate() }
    }

    /// The key the config holds (`secret_key`: 64 hex), or `None` when it
    /// is not a secp256k1 secret.
    pub fn from_secret_hex(secret_hex: &str) -> Option<Identity> {
        if secret_hex.len() != 64 {
            return None;
        }
        Keys::from_secret_hex(secret_hex).map(|keys| Identity { keys })
    }

    /// The secret as the config stores it (64 lowercase hex). BIP-340
    /// keeps the secret whose public point has an even y, so this may be
    /// the negation of the hex it was loaded from: the same key.
    pub fn secret_hex(&self) -> String {
        self.keys.secret_hex()
    }

    pub fn pubkey_hex(&self) -> &str {
        self.keys.pubkey_hex()
    }

    pub fn npub(&self) -> String {
        npub::encode(self.keys.pubkey_hex())
    }

    /// The Authorization header value for a NIP-98 request, signed now.
    pub fn nip98_header(&self, method: &str, url: &str, body: &[u8]) -> String {
        self.keys.header(method, url, body, now_s())
    }

    /// A key proof (crates/nip98 `verify_proof`): this key agrees to join
    /// whoever signs `method url` with the key `signer_hex`.
    pub fn proof(&self, method: &str, url: &str, signer_hex: &str) -> String {
        self.keys.proof(method, url, signer_hex, now_s())
    }
}

/// An identity from a fixed secret (tests).
#[cfg(test)]
pub fn fixed(n: u8) -> Identity {
    Identity::from_secret_hex(&hex::encode([n; 32])).expect("a valid secret")
}

/// Whom a member command names (docs/cloudflare-v1.md, decision 48).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    /// Anything shaped like an email, lower case: the person who signs in
    /// as it (or, to `members add`, an invite waiting on it).
    Email(String),
    /// An npub or a 64-hex key, as its npub: the identity, or the one
    /// holding the key.
    Npub(String),
}

impl std::fmt::Display for Member {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Member::Email(e) => f.write_str(e),
            Member::Npub(n) => f.write_str(n),
        }
    }
}

/// An email, or an npub or a 64-hex key, checked and made canonical.
pub fn member_of(input: &str) -> Result<Member> {
    let s = input.trim();
    if s.contains('@') {
        let email = s.to_ascii_lowercase();
        if !fragment_core::mail::valid_address(&email) {
            bail!("'{s}' is not an email");
        }
        return Ok(Member::Email(email));
    }
    let hex = npub::parse(s).ok_or_else(|| anyhow!("'{s}' is not an email, an npub, or a 64-hex key"))?;
    Ok(Member::Npub(npub::encode(&hex)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key saved as hex before the CLI signed through crates/nip98 still
    /// loads as the same key (a known answer, from @noble/curves), and
    /// signs what the cell verifies.
    #[test]
    fn a_stored_hex_key_keeps_working() {
        let stored = hex::encode([1u8; 32]);
        let id = Identity::from_secret_hex(&stored).expect("the stored key loads");
        assert_eq!(id.pubkey_hex(), "1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f");
        // what this CLI saves loads back as the same key
        let saved = Identity::from_secret_hex(&id.secret_hex()).expect("the saved key loads");
        assert_eq!(saved.pubkey_hex(), id.pubkey_hex());
        let fresh = Identity::generate();
        assert_eq!(Identity::from_secret_hex(&fresh.secret_hex()).map(|k| k.npub()), Some(fresh.npub()));
        let url = "http://x/api/fragments";
        let h = id.nip98_header("POST", url, b"{}");
        assert_eq!(fragment_nip98::verify(Some(&h), "POST", url, b"{}", now_s(), 60).as_deref(), Ok(id.pubkey_hex()));
        for bad in ["", "abc", &"0".repeat(64), &"zz".repeat(32), &"01".repeat(33)] {
            assert!(Identity::from_secret_hex(bad).is_none(), "{bad:?}");
        }
    }

    /// Goal: decision 48's "anything shaped like an email is an email",
    /// and keys as their npubs. Method: each form, valid and not.
    #[test]
    fn a_member_is_an_email_or_an_npub() {
        let id = fixed(1);
        assert_eq!(member_of(&id.npub()).unwrap(), Member::Npub(id.npub()));
        assert_eq!(member_of(&format!(" {} ", id.pubkey_hex().to_uppercase())).unwrap(), Member::Npub(id.npub()));
        assert_eq!(member_of(" Bea@Example.com ").unwrap(), Member::Email("bea@example.com".into()));
        assert_eq!(member_of("paul@finite.vip").unwrap().to_string(), "paul@finite.vip", "an email, never a NIP-05 name");
        for bad in ["npub1xyz", "@x", "bea@", "bea@example", "a b@example.com", ""] {
            assert!(member_of(bad).is_err(), "{bad:?}");
        }
    }
}
