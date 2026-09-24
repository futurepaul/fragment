// This machine's nostr key and its NIP-98 HTTP auth: crates/nip98 signs
// (the same code the cell verifies with), crates/core names keys as npubs.
use anyhow::{anyhow, bail, Context, Result};
use fragment_core::npub;
use fragment_nip98::Keys;
use serde_json::Value;

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

/// Resolve an identifier to its canonical npub form.
///
/// An npub or a 64-hex key is checked and canonicalised. NIP-05 names
/// (`local@domain`) resolve via the standard well-known path
/// `https://<domain>/.well-known/nostr.json?name=<local>` — the same lookup
/// the other finite CLIs (fbrain, fsite) use — so finite identities like
/// `paul@finite.vip` work anywhere an npub does.
pub fn resolve_npub(input: &str) -> Result<String> {
    let s = input.trim();
    if s.contains('@') {
        let (local, domain) = s
            .split_once('@')
            .ok_or_else(|| anyhow!("'{s}' is not a valid NIP-05 name"))?;
        if local.is_empty() || domain.is_empty() || domain.contains('@') {
            bail!("'{s}' is not a valid NIP-05 name");
        }
        let url = format!("https://{domain}/.well-known/nostr.json?name={local}");
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none()) // NIP-05 forbids redirects
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        let resp = http
            .get(&url)
            .send()
            .with_context(|| format!("NIP-05 lookup failed for '{s}'"))?;
        if !resp.status().is_success() {
            bail!("NIP-05 lookup for '{s}' returned HTTP {}", resp.status());
        }
        let body = resp
            .text()
            .with_context(|| format!("NIP-05 lookup for '{s}' failed"))?;
        let doc: Value = serde_json::from_str(&body)
            .with_context(|| format!("NIP-05 document at {domain} is not valid JSON"))?;
        let published = doc["names"][local]
            .as_str()
            .ok_or_else(|| anyhow!("no npub published for '{s}'"))?;
        let hex = npub::parse(published).ok_or_else(|| anyhow!("'{s}' published an invalid pubkey"))?;
        return Ok(npub::encode(&hex));
    }
    let hex = npub::parse(s).ok_or_else(|| anyhow!("'{s}' is not an npub or a 64-hex key"))?;
    Ok(npub::encode(&hex))
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

    #[test]
    fn identifiers_resolve_to_npubs() {
        let id = fixed(1);
        assert_eq!(resolve_npub(&id.npub()).unwrap(), id.npub());
        assert_eq!(resolve_npub(&format!(" {} ", id.pubkey_hex().to_uppercase())).unwrap(), id.npub());
        assert!(resolve_npub("npub1xyz").is_err());
        assert!(resolve_npub("@x").is_err());
    }
}
