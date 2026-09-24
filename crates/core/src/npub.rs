//! Principals on the wire. Grants name an identity: `id:` plus 32 hex
//! characters, opaque, made by the registry (docs/finite-integration.md).
//! A key is 64 lowercase hex characters inside the platform and an npub
//! (NIP-19 bech32) in answers; requests may use either. An anonymous
//! visitor is `anon:` plus 32 hex characters.

use bech32::{FromBase32, ToBase32, Variant};

pub const ANON_PREFIX: &str = "anon:";
pub const ID_PREFIX: &str = "id:";

fn is_hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_identity(s: &str) -> bool {
    s.strip_prefix(ID_PREFIX).is_some_and(is_hex32)
}

/// A new identity's id from 16 random bytes (the caller's randomness).
pub fn identity(random: [u8; 16]) -> String {
    format!("{ID_PREFIX}{}", hex::encode(random))
}

/// Someone a request names: an identity, or a key (which the registry
/// resolves to the identity holding it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    Identity(String),
    Key(String),
}

/// `id:…`, an npub, or 64 hex → who it names.
pub fn parse_named(s: &str) -> Option<Named> {
    if is_identity(s) {
        return Some(Named::Identity(s.to_string()));
    }
    parse(s).map(Named::Key)
}

pub fn is_hex_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_anon(s: &str) -> bool {
    s.strip_prefix(ANON_PREFIX).is_some_and(is_hex32)
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

/// A list as configuration names people (identities, npubs, or 64 hex,
/// separated by commas or whitespace) → each identity as it is and each
/// key as 64 lowercase hex; the first entry that is neither is the error.
pub fn parse_list(s: &str) -> Result<Vec<String>, String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|e| !e.is_empty())
        .map(|e| match parse_named(e) {
            Some(Named::Identity(id)) => Ok(id),
            Some(Named::Key(k)) => Ok(k),
            None => Err(format!("{e:?} is not an identity (id:…), an npub, or a 64-hex key")),
        })
        .collect()
}

/// How a principal appears in answers: an npub for a key, the id itself
/// for an identity or an anonymous visitor.
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
        assert!(is_anon("anon:0123456789abcdef0123456789abcdef"));
        assert!(!is_anon("anon:xyz"));
        assert_eq!(display("anon:0123456789abcdef0123456789abcdef"), "anon:0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn identities() {
        let id = identity([0xab; 16]);
        assert_eq!(id, "id:abababababababababababababababab");
        assert!(is_identity(&id));
        assert!(!is_identity("id:ABABABABABABABABABABABABABABABAB"));
        assert!(!is_identity("id:abab"));
        assert!(!is_identity("anon:abababababababababababababababab"));
        assert_eq!(display(&id), id);
        assert_eq!(parse_named(&id), Some(Named::Identity(id.clone())));
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        assert_eq!(parse_named(npub), Some(Named::Key(hex.into())));
        assert_eq!(parse_named("paul"), None);
    }

    #[test]
    fn lists() {
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        assert_eq!(parse_list(&format!(" {npub},\n{} ", hex.to_uppercase())), Ok(vec![hex.to_string(), hex.to_string()]));
        assert_eq!(parse_list(""), Ok(vec![]));
        assert!(parse_list(&format!("{npub}, paul")).unwrap_err().contains("\"paul\""));
        let id = "id:0123456789abcdef0123456789abcdef";
        assert_eq!(parse_list(&format!("{id} {npub}")), Ok(vec![id.to_string(), hex.to_string()]));
    }
}
