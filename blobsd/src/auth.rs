//! Blossom auth event (kind 24242) verification.
//!
//! Wire form: `Authorization: Nostr <base64(JSON event)>`. The event carries
//! the standard NIP-01 fields plus `t` (verb), `x` (the server URL the client
//! believes it is talking to), and `expiration` (unix seconds).
//!
//! Verification order and status mapping is deliberate:
//! - structural problems (garbled header, bad JSON, id-hash mismatch, bad
//!   signature, missing/expired/unparseable expiration, wrong kind) answer
//!   401 — authentication failed;
//! - semantic problems (`t` verb mismatch, `x` server-URL mismatch, pubkey
//!   not allowlisted) answer 403 — authenticated but not permitted.
//!
//! The id check recomputes the exact NIP-01 canonical serialization
//! `[0, pubkey, created_at, kind, tags, content]` as compact JSON before any
//! signature work. Building it as a `serde_json::Value` keeps byte-for-byte
//! parity with JS `JSON.stringify` (compact separators, insertion order,
//! identical string escaping) — that equivalence is the interop guarantee
//! with @noble-based signers like scripts/nip98.mjs.

use std::sync::OnceLock;

use base64::Engine;
use secp256k1::schnorr::Signature;
use secp256k1::{Message, Secp256k1, VerifyOnly, XOnlyPublicKey};
use sha2::{Digest, Sha256};

use crate::error::ApiError;

/// Bounded decode budget for the auth event JSON. Real events are < 1 KiB;
/// anything near this cap is abuse, rejected before parsing spends memory.
pub const MAX_AUTH_EVENT_BYTES: usize = 64 * 1024;

/// The kind constant for blossom auth events.
const AUTH_KIND: u32 = 24242;

/// Which verb an endpoint demands from the `t` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Upload,
    Delete,
    List,
}

impl Verb {
    fn tag_value(self) -> &'static str {
        match self {
            Verb::Upload => "upload",
            Verb::Delete => "delete",
            Verb::List => "list",
        }
    }
}

/// Who an accepted request is acting as.
#[derive(Debug, Clone)]
pub struct Verified {
    pub pubkey_hex: String,
}

/// Verification-only secp256k1 context. Not a const static because 0.29's
/// constructor allocates internally; a OnceLock keeps the cost to one setup
/// for the process lifetime and lets every handler share it by reference.
fn verify_ctx() -> &'static Secp256k1<VerifyOnly> {
    static CTX: OnceLock<Secp256k1<VerifyOnly>> = OnceLock::new();
    CTX.get_or_init(Secp256k1::verification_only)
}

/// Verify one Authorization header value against configuration, required
/// verb, allowlist and clock. Total function over malformed input: every
/// failure mode maps to one typed [`ApiError`] before any work happens.
pub fn verify_authorization(
    header_value: &str,
    cfg_public_url: &str,
    required: Verb,
    allow_hex_pubkeys: &std::collections::BTreeSet<String>,
    now_unix: i64,
) -> Result<Verified, ApiError> {
    // Ingress check: the configured URL is non-empty — an empty template
    // would silently reject every x tag below and look like an outage.
    assert!(
        !cfg_public_url.is_empty(),
        "configured public url must be non-empty"
    );

    let encoded = header_value
        .strip_prefix("Nostr ")
        .ok_or(ApiError::MissingAuth)?;
    // ~4/3 expansion; bounding the ENCODED length bounds the decode buffer
    // before any allocation happens.
    if encoded.len() > MAX_AUTH_EVENT_BYTES / 3 * 4 {
        return Err(ApiError::MalformedAuth);
    }

    let raw_json = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::MalformedAuth)?;
    if raw_json.len() > MAX_AUTH_EVENT_BYTES {
        return Err(ApiError::MalformedAuth);
    }

    let event: RawEvent = serde_json::from_slice(&raw_json).map_err(|_| ApiError::MalformedAuth)?;
    verify_event(event, cfg_public_url, required, allow_hex_pubkeys, now_unix)
}

/// Exact wire shape of the client event. Scalars keep their original types so
/// canonical reserialization reproduces what the signer hashed.
#[derive(Debug, serde::Deserialize)]
struct RawEvent {
    id: String,
    pubkey: String,
    created_at: i64,
    kind: u32,
    tags: Vec<Vec<String>>,
    content: String,
    sig: String,
}

fn verify_event(
    ev: RawEvent,
    cfg_public_url: &str,
    required: Verb,
    allow_hex_pubkeys: &std::collections::BTreeSet<String>,
    now_unix: i64,
) -> Result<Verified, ApiError> {
    // ---- id hash -----------------------------------------------------------
    let canonical = canonical_serialization(&ev);
    let id_digest: [u8; 32] = Sha256::digest(canonical.as_bytes()).into();
    let stated_id = hex_32(&ev.id).ok_or(ApiError::BadAuthId)?;
    if stated_id != id_digest {
        return Err(ApiError::BadAuthId);
    }

    // ---- schnorr signature --------------------------------------------------
    let signature_bytes = hex_64(&ev.sig).ok_or(ApiError::BadAuthSignature)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| ApiError::BadAuthSignature)?;
    let xonly_key = hex_32(&ev.pubkey).ok_or(ApiError::BadAuthSignature)?;
    let public_key =
        XOnlyPublicKey::from_slice(&xonly_key).map_err(|_| ApiError::BadAuthSignature)?;
    let message = Message::from_digest(id_digest);
    verify_ctx()
        .verify_schnorr(&signature, &message, &public_key)
        .map_err(|_| ApiError::BadAuthSignature)?;

    // Egress check for the crypto block: the allowlist match below keys off
    // the SAME bytes the signature just vouched for.
    debug_assert_eq!(hex::encode(xonly_key), ev.pubkey.to_lowercase());

    // ---- structure mandated by the blossom profile --------------------------
    if ev.kind != AUTH_KIND {
        return Err(ApiError::WrongAuthKind);
    }

    let find_tag = |name: &str| -> Option<&str> {
        ev.tags
            .iter()
            .find(|tag| tag.first().map(String::as_str) == Some(name))
            .and_then(|tag| tag.get(1).map(String::as_str))
    };

    let expires_at = find_tag("expiration")
        .ok_or(ApiError::AuthExpired)?
        .parse::<i64>()
        .map_err(|_| ApiError::AuthExpired)?;
    if expires_at <= now_unix {
        return Err(ApiError::AuthExpired);
    }

    // ---- authorization semantics --------------------------------------------
    let verb_matched = find_tag("t").unwrap_or("") == required.tag_value();
    if !verb_matched {
        return Err(ApiError::ForbiddenVerb);
    }
    if find_tag("x").unwrap_or("") != cfg_public_url {
        return Err(ApiError::ForbiddenUrl);
    }
    let pubkey_hex = ev.pubkey.to_lowercase();
    if !allow_hex_pubkeys.contains(pubkey_hex.as_str()) {
        return Err(ApiError::ForbiddenNpub);
    }

    Ok(Verified { pubkey_hex })
}

/// Compact canonical NIP-01 array through serde_json so escaping/order rules
/// stay owned by the serializer instead of hand-maintained string splicing.
fn canonical_serialization(ev: &RawEvent) -> String {
    serde_json::to_string(&serde_json::json!([
        0,
        ev.pubkey,
        ev.created_at,
        ev.kind,
        ev.tags,
        ev.content,
    ]))
    .expect("array of strings/integers always serializes")
}

/// npub (NIP-19 bech32) to lowercase hex pubkey. Strict on hrp and length.
/// bech32 0.11's top-level `decode` checksum-validates and converts to bytes
/// in one step; the returned tuple is (hrp, data). It does not report which
/// checksum variant passed — a bech32m-tagged "npub" would also clear —
/// which only matters to string cosmetics, not to the 32 bound bytes every
/// consumer compares against.
pub fn npub_to_hex(npub: &str) -> Result<String, String> {
    let (hrp, data) = bech32::decode(npub).map_err(|e| format!("not valid bech32: {e}"))?;
    if hrp.as_str() != "npub" {
        return Err(format!("hrp {hrp:?} is not \"npub\""));
    }
    if data.len() != 32 {
        return Err(format!("decoded {} bytes, want 32", data.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&data);
    Ok(hex::encode(out))
}

/// Fixed-width lowercase-hex decode into 64 bytes (schnorr signatures);
/// wrong length or alphabet is one typed miss instead of a panic.
fn hex_64(value: &str) -> Option<[u8; 64]> {
    if value.len() != 128 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 64];
    hex::decode_to_slice(value, &mut out).ok()?;
    Some(out)
}

/// Fixed-width lowercase-hex decode into 32 bytes; wrong length or alphabet
/// is one typed miss instead of a panic.
fn hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(value, &mut out).ok()?;
    Some(out)
}

/// Test-support signer standing in for scripts/nip98.mjs-style clients.
/// Ships in the library (integration tests link the lib without cfg(test))
/// but is inert at runtime: nothing in the serving path calls it, it holds
/// no secrets, and it is hidden from docs on purpose.
#[doc(hidden)]
pub mod tests_fixtures {
    use super::*;
    use secp256k1::{Keypair, Secp256k1, SecretKey};

    /// Fixed throwaway key so every lane's tests agree on one identity.
    pub const TEST_SECRET_HEX: &str =
        "67c4092d85ab68a97f92e2129d8b41fdbcb42ef3fbb0f2aae35f78ba923bf34e";
    /// Second key that is deliberately NOT allowlisted anywhere.
    pub const STRANGER_SECRET_HEX: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";

    fn keypair(secret_hex: &str) -> ([u8; 32], Keypair) {
        let ctx = Secp256k1::signing_only();
        let secret_bytes: [u8; 32] = {
            let mut buf = [0u8; 32];
            hex::decode_to_slice(secret_hex, &mut buf).expect("fixture hex");
            buf
        };
        let secret = SecretKey::from_slice(&secret_bytes).expect("fixture secret");
        let pair = Keypair::from_secret_key(&ctx, &secret);
        let (xonly, _) = pair.x_only_public_key();
        (xonly.serialize(), pair)
    }

    pub fn test_pubkey_hex() -> String {
        hex::encode(keypair(TEST_SECRET_HEX).0)
    }

    pub fn stranger_pubkey_hex() -> String {
        hex::encode(keypair(STRANGER_SECRET_HEX).0)
    }

    pub fn stranger_npub() -> String {
        let hrp = bech32::Hrp::parse("npub").expect("npub hrp");
        bech32::encode::<bech32::Bech32>(hrp, &keypair(STRANGER_SECRET_HEX).0)
            .expect("fixture npub")
    }

    pub fn test_npub() -> String {
        // NIP-19: plain-bech32 checksum over the raw 32 key bytes; bech32
        // 0.11's encode handles the 5-bit regrouping internally.
        let hrp = bech32::Hrp::parse("npub").expect("npub hrp");
        bech32::encode::<bech32::Bech32>(hrp, &keypair(TEST_SECRET_HEX).0)
            .expect("fixture npub")
    }

    /// Knobs for building a slightly-wrong event on purpose.
    #[derive(Default)]
    pub struct Override {
        pub t: Option<String>,
        pub x: Option<String>,
        pub expiration_offset_secs: Option<i64>,
        /// Raw tag override; produces an event whose id MATCHES the junk
        /// expiration, isolating the expiration-parse rejection.
        pub expiration_raw: Option<String>,
        pub kind: Option<u32>,
        pub secret: Option<&'static str>,
        /// Corrupt the id AFTER hashing (tests bad_id independent of sig).
        pub corrupt_id: bool,
        /// Corrupt the sig AFTER signing (tests bad_signature).
        pub corrupt_sig: bool,
        pub drop_expiration_tag: bool,
    }

    pub fn build_auth_header(
        public_url: &str,
        verb: &str,
        o: &Override,
    ) -> String {
        let secret_hex = o.secret.unwrap_or(TEST_SECRET_HEX);
        let (_, pair) = keypair(secret_hex);
        let pubkey_hex = hex::encode(pair.x_only_public_key().0.serialize());
        let created_at = chrono_now_fixture();
        let expiration = created_at + o.expiration_offset_secs.unwrap_or(3600);

        let mut tags: Vec<Vec<String>> = vec![vec!["t".into(), o.t.clone().unwrap_or_else(|| verb.into())]];
        if !o.drop_expiration_tag {
            let expiration_value = o.expiration_raw.clone().unwrap_or_else(|| expiration.to_string());
            tags.push(vec!["expiration".into(), expiration_value]);
        }
        tags.push(vec!["x".into(), o.x.clone().unwrap_or_else(|| public_url.to_string())]);

        let event_json_for_id = serde_json::json!([
            0, pubkey_hex, created_at, o.kind.unwrap_or(24242), tags, ""
        ]);
        let mut id_bytes: [u8; 32] =
            Sha256::digest(serde_json::to_string(&event_json_for_id).unwrap().as_bytes()).into();
        if o.corrupt_id {
            id_bytes[0] ^= 0x01;
        }
        // Aux-randomness-free signing: fixtures only need a VALID
        // signature, and determinism makes test failures reproducible.
        let ctx = Secp256k1::signing_only();
        let signature = ctx.sign_schnorr_no_aux_rand(&Message::from_digest(id_bytes), &pair);
        let mut sig_hex = hex::encode(signature.serialize());
        if o.corrupt_sig {
            sig_hex.replace_range(..2, "ff");
        }

        let event = serde_json::json!({
            "id": hex::encode(id_bytes),
            "pubkey": pubkey_hex,
            "created_at": created_at,
            "kind": o.kind.unwrap_or(24242),
            "tags": tags,
            "content": "",
            "sig": sig_hex,
        });
        format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD.encode(event.to_string())
        )
    }

    fn chrono_now_fixture() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::tests_fixtures::{self as fx, Override};
    use super::*;

    const SERVER_URL: &str = "https://blobsd.example.com";
    const ALTERNATE_URL: &str = "https://other.example.net";

    fn allowlist() -> std::collections::BTreeSet<String> {
        std::collections::BTreeSet::from([fx::test_pubkey_hex()])
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs() as i64
    }

    #[test]
    fn valid_upload_event_accepts_and_reports_key() {
        let header = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
        let verified = verify_authorization(&header, SERVER_URL, Verb::Upload, &allowlist(), now())
            .expect("valid event accepted");
        assert_eq!(verified.pubkey_hex, fx::test_pubkey_hex());
    }

    #[test]
    fn accept_matrix_rejects_each_mutation_with_typed_error() {
        // wrong server url
        let err = verify_authorization(
            &fx::build_auth_header(ALTERNATE_URL, "upload", &Override::default()),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::ForbiddenUrl);

        // bad kind
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { kind: Some(27235), ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::WrongAuthKind);

        // expired expiration tag
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { expiration_offset_secs: Some(-10), ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::AuthExpired);

        // missing expiration tag
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { drop_expiration_tag: true, ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::AuthExpired);

        // unparseable expiration value (id consistent, tag junk)
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { expiration_raw: Some("not-a-number".to_string()), ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::AuthExpired);

        // corrupted id hash
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { corrupt_id: true, ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::BadAuthId);

        // corrupted signature
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { corrupt_sig: true, ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::BadAuthSignature);

        // non-allowlisted signer (valid crypto, wrong identity)
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { secret: Some(fx::STRANGER_SECRET_HEX), ..Default::default() },
            ),
            SERVER_URL,
            Verb::Upload,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::ForbiddenNpub);

        // t tag not matching required verb
        let err = verify_authorization(
            &fx::build_auth_header(
                SERVER_URL,
                "delete",
                &Override { t: Some("upload".to_string()), ..Default::default() },
            ),
            SERVER_URL,
            Verb::Delete,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::ForbiddenVerb);

        // upload-tagged event offered at the delete endpoint
        let err = verify_authorization(
            &fx::build_auth_header(SERVER_URL, "upload", &Override::default()),
            SERVER_URL,
            Verb::Delete,
            &allowlist(),
            now(),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::ForbiddenVerb);
    }

    #[test]
    fn non_auth_headers_fail_structurally() {
        let allow = allowlist();
        // No nostr prefix at all.
        assert_eq!(
            verify_authorization("Bearer something", SERVER_URL, Verb::Upload, &allow, now())
                .unwrap_err(),
            ApiError::MissingAuth
        );
        // Prefix right, payload garbage.
        assert_eq!(
            verify_authorization("Nostr !!!not-base64!!!", SERVER_URL, Verb::Upload, &allow, now())
                .unwrap_err(),
            ApiError::MalformedAuth
        );
        // Base64 of non-JSON.
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"<html>");
        assert_eq!(
            verify_authorization(&format!("Nostr {b64}"), SERVER_URL, Verb::Upload, &allow, now())
                .unwrap_err(),
            ApiError::MalformedAuth
        );
    }


    #[test]
    fn npub_roundtrip_matches_own_encoder_target() {
        let decoded = npub_to_hex(&fx::test_npub()).expect("roundtrip");
        assert_eq!(decoded, fx::test_pubkey_hex());
        // Wrong hrp is refused by name.
        let err = npub_to_hex("nprofile1qqqqnotarealthing").expect_err("bad entity refused");
        assert!(err.contains("hrp") || err.contains("bech32"), "{err}");
        // Garbage is refused without panicking.
        assert!(npub_to_hex("").is_err());
        assert!(npub_to_hex("npub1").is_err());
    }
}
// scratch: reproduce fixture sign/verify roundtrip
