//! Principals on the wire. Grants name an identity: the npub (NIP-19
//! bech32) of the key it was made with, its name for good, even once that
//! key is retired (docs/cloudflare-v1.md, decision 45). A person's is a
//! key the registry makes and keeps at their first sign-in; an agent's,
//! its fragment's. A key is 64 lowercase hex characters inside the
//! platform and an npub in answers; requests may use either, and an npub
//! a request names is an identity or the key of one (the registry tells
//! which). An anonymous visitor is `anon:` plus 32 hex characters.

use bech32::{FromBase32, ToBase32, Variant};

pub const ANON_PREFIX: &str = "anon:";

/// Whether `s` is an identity: an npub as `encode` writes one.
pub fn is_identity(s: &str) -> bool {
    s.starts_with("npub1") && parse(s).is_some_and(|hex| encode(&hex) == s)
}

/// The identity a new key makes: its npub.
pub fn identity_of(hex_key: &str) -> String {
    encode(hex_key)
}

pub fn is_hex_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The npub of a 64-hex key.
pub fn encode(hex_key: &str) -> String {
    assert!(is_hex_key(hex_key), "npub::encode takes a 64-hex key");
    let bytes = hex::decode(hex_key).expect("checked hex");
    bech32::encode("npub", bytes.to_base32(), Variant::Bech32).expect("npub is a valid hrp")
}

/// A principal as a request names it (npub or 64 hex) → 64 lowercase hex.
pub fn parse(s: &str) -> Option<String> {
    let lower = s.to_ascii_lowercase();
    if is_hex_key(&lower) {
        return Some(lower);
    }
    let (hrp, data, variant) = bech32::decode(s).ok()?;
    if hrp != "npub" || variant != Variant::Bech32 {
        return None;
    }
    let bytes = Vec::<u8>::from_base32(&data).ok()?;
    (bytes.len() == 32).then(|| hex::encode(bytes))
}

/// A list as configuration names people (npubs or 64 hex, separated by
/// commas or whitespace) → each as 64 lowercase hex: a key, or the key an
/// identity was made with. The first entry that is neither is the error.
pub fn parse_list(s: &str) -> Result<Vec<String>, String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|e| !e.is_empty())
        .map(|e| parse(e).ok_or_else(|| format!("{e:?} is not an npub or a 64-hex key")))
        .collect()
}

/// How a principal appears in answers: an npub for a key, and an
/// identity or an anonymous visitor as it is.
pub fn display(principal: &str) -> String {
    if is_hex_key(principal) {
        encode(principal)
    } else {
        principal.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        // NIP-19's example key
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        assert_eq!(encode(hex), npub);
        assert_eq!(parse(npub).as_deref(), Some(hex));
        assert_eq!(parse(&hex.to_uppercase()).as_deref(), Some(hex));
        assert_eq!(parse("npub1xyz"), None);
        assert_eq!(parse("nsec180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwstu7nhp"), None);
        assert_eq!(display("anon:0123456789abcdef0123456789abcdef"), "anon:0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn identities() {
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        assert_eq!(identity_of(hex), npub);
        assert!(is_identity(npub));
        assert_eq!(display(npub), npub);
        // only the form `encode` writes: not hex, not upper case, not another hrp
        assert!(!is_identity(hex));
        assert!(!is_identity(&npub.to_uppercase()));
        assert!(!is_identity("npub1xyz"));
        assert!(!is_identity("nsec180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwstu7nhp"));
        assert!(!is_identity("anon:abababababababababababababababab"));
        assert!(!is_identity("id:abababababababababababababababab"));
    }

    #[test]
    fn lists() {
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        assert_eq!(parse_list(&format!(" {npub},\n{} ", hex.to_uppercase())), Ok(vec![hex.to_string(), hex.to_string()]));
        assert_eq!(parse_list(""), Ok(vec![]));
        assert!(parse_list(&format!("{npub}, paul")).unwrap_err().contains("\"paul\""));
        assert!(parse_list("id:0123456789abcdef0123456789abcdef").is_err());
    }
}
