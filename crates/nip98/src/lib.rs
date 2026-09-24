//! NIP-98 HTTP auth: a kind-27235 Nostr event in `Authorization: Nostr
//! <base64>`, binding the method, the URL, and the body's SHA-256 to a
//! BIP-340 signature. `verify` runs in the cell (wasm32); `Keys` signs on
//! hosts (the `sign` feature).

use base64::Engine;
use k256::schnorr::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub const KIND: u64 = 27235;

/// Why a request's auth was refused. Each maps to 401 at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    MissingHeader,
    Malformed(&'static str),
    WrongKind,
    BadPubkey,
    Stale { skew_s: i64 },
    UrlMismatch,
    MethodMismatch,
    PayloadMismatch,
    IdMismatch,
    BadSignature,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingHeader => write!(f, "missing `Authorization: Nostr` header"),
            AuthError::Malformed(what) => write!(f, "malformed auth event: {what}"),
            AuthError::WrongKind => write!(f, "auth event is not kind {KIND}"),
            AuthError::BadPubkey => write!(f, "auth event pubkey is not 64 hex characters"),
            AuthError::Stale { skew_s } => write!(f, "auth event created_at is {skew_s} s from now"),
            AuthError::UrlMismatch => write!(f, "auth event `u` tag does not match the request URL"),
            AuthError::MethodMismatch => write!(f, "auth event `method` tag does not match"),
            AuthError::PayloadMismatch => write!(f, "auth event `payload` tag does not match the body"),
            AuthError::IdMismatch => write!(f, "auth event id does not match its content"),
            AuthError::BadSignature => write!(f, "auth event signature does not verify"),
        }
    }
}

/// origin + path + query, the part of a URL the `u` tag must match.
fn canonical_url(raw: &str) -> Option<String> {
    let u = url::Url::parse(raw).ok()?;
    let query = match u.query() {
        Some(q) if !q.is_empty() => format!("?{q}"),
        _ => String::new(),
    };
    Some(format!("{}{}{}", u.origin().ascii_serialization(), u.path(), query))
}

fn event_id(pubkey: &str, created_at: i64, kind: u64, tags: &serde_json::Value, content: &str) -> [u8; 32] {
    let preimage = serde_json::json!([0, pubkey, created_at, kind, tags, content]).to_string();
    Sha256::digest(preimage.as_bytes()).into()
}

fn tag<'a>(tags: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    tags.as_array()?.iter().find_map(|t| {
        let t = t.as_array()?;
        (t.first()?.as_str()? == name).then(|| t.get(1)?.as_str()).flatten()
    })
}

/// Verifies a NIP-98 header for `method url` with `body`, at `now_s`
/// (seconds since the epoch). Returns the signer's public key, hex.
pub fn verify(header: Option<&str>, method: &str, url: &str, body: &[u8], now_s: i64, window_s: i64) -> Result<String, AuthError> {
    let header = header.ok_or(AuthError::MissingHeader)?;
    let b64 = header.strip_prefix("Nostr ").ok_or(AuthError::MissingHeader)?.trim();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| AuthError::Malformed("not base64"))?;
    let ev: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| AuthError::Malformed("not JSON"))?;
    if ev["kind"].as_u64() != Some(KIND) {
        return Err(AuthError::WrongKind);
    }
    let pubkey = ev["pubkey"].as_str().ok_or(AuthError::BadPubkey)?;
    if pubkey.len() != 64 || !pubkey.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err(AuthError::BadPubkey);
    }
    let created_at = ev["created_at"].as_i64().ok_or(AuthError::Malformed("created_at"))?;
    let skew_s = created_at - now_s;
    if skew_s.abs() > window_s {
        return Err(AuthError::Stale { skew_s });
    }
    let tags = &ev["tags"];
    let signed_url = tag(tags, "u").and_then(canonical_url).ok_or(AuthError::UrlMismatch)?;
    if Some(signed_url) != canonical_url(url) {
        return Err(AuthError::UrlMismatch);
    }
    if tag(tags, "method") != Some(method.to_ascii_uppercase().as_str()) {
        return Err(AuthError::MethodMismatch);
    }
    if !body.is_empty() && tag(tags, "payload") != Some(hex::encode(Sha256::digest(body)).as_str()) {
        return Err(AuthError::PayloadMismatch);
    }
    let content = ev["content"].as_str().unwrap_or("");
    let id = event_id(pubkey, created_at, KIND, tags, content);
    if ev["id"].as_str() != Some(hex::encode(id).as_str()) {
        return Err(AuthError::IdMismatch);
    }
    let sig_bytes = ev["sig"].as_str().and_then(|s| hex::decode(s).ok()).ok_or(AuthError::BadSignature)?;
    let sig = Signature::try_from(sig_bytes.as_slice()).map_err(|_| AuthError::BadSignature)?;
    let key_bytes = hex::decode(pubkey).map_err(|_| AuthError::BadPubkey)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| AuthError::BadPubkey)?;
    key.verify_raw(&id, &sig).map_err(|_| AuthError::BadSignature)?;
    Ok(pubkey.to_string())
}

/// The x-only public key (64 hex) of a 64-hex secret key, or `None` when
/// the secret is not a valid secp256k1 scalar. Needs no randomness, so the
/// cell uses it to check a fragment secret a client generated.
pub fn pubkey_of_secret(secret_hex: &str) -> Option<String> {
    let bytes = hex::decode(secret_hex).ok().filter(|b| b.len() == 32)?;
    let key = k256::schnorr::SigningKey::from_bytes(&bytes).ok()?;
    Some(hex::encode(key.verifying_key().to_bytes()))
}

/// A signing identity.
#[cfg(feature = "signer")]
pub struct Keys {
    key: k256::schnorr::SigningKey,
    pubkey_hex: String,
}

#[cfg(feature = "signer")]
impl Keys {
    /// A new key from the OS's randomness (hosts; a cell makes its secret
    /// from the platform's randomness and uses `from_secret_hex`).
    #[cfg(feature = "sign")]
    pub fn generate() -> Keys {
        Keys::from_key(k256::schnorr::SigningKey::random(&mut rand_core::OsRng))
    }

    pub fn from_secret_hex(secret_hex: &str) -> Option<Keys> {
        let bytes = hex::decode(secret_hex).ok()?;
        k256::schnorr::SigningKey::from_bytes(&bytes).ok().map(Keys::from_key)
    }

    fn from_key(key: k256::schnorr::SigningKey) -> Keys {
        let pubkey_hex = hex::encode(key.verifying_key().to_bytes());
        Keys { key, pubkey_hex }
    }

    pub fn pubkey_hex(&self) -> &str {
        &self.pubkey_hex
    }

    pub fn secret_hex(&self) -> String {
        hex::encode(self.key.to_bytes())
    }

    /// The `Authorization` header value for `method url` with `body`.
    pub fn header(&self, method: &str, url: &str, body: &[u8], created_at: i64) -> String {
        let mut tags = vec![serde_json::json!(["u", url]), serde_json::json!(["method", method.to_ascii_uppercase()])];
        if !body.is_empty() {
            tags.push(serde_json::json!(["payload", hex::encode(Sha256::digest(body))]));
        }
        let tags = serde_json::Value::Array(tags);
        let id = event_id(&self.pubkey_hex, created_at, KIND, &tags, "");
        let sig = self.key.sign_raw(&id, &[0u8; 32]).expect("BIP-340 signing a 32-byte digest");
        let ev = serde_json::json!({
            "id": hex::encode(id), "pubkey": self.pubkey_hex, "created_at": created_at,
            "kind": KIND, "tags": tags, "content": "", "sig": hex::encode(sig.to_bytes()),
        });
        format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(ev.to_string()))
    }
}

#[cfg(all(test, feature = "sign"))]
mod tests {
    #[test]
    fn pubkey_of_secret_matches_keys() {
        let k = super::Keys::generate();
        assert_eq!(super::pubkey_of_secret(&k.secret_hex()).as_deref(), Some(k.pubkey_hex()));
        assert_eq!(super::pubkey_of_secret(&"0".repeat(64)), None);
        assert_eq!(super::pubkey_of_secret("abc"), None);
    }

    use super::*;
    use secp256k1::{Keypair, Message, Secp256k1, SecretKey, XOnlyPublicKey};

    const NOW: i64 = 1_790_000_000;
    const URL: &str = "http://127.0.0.1:8790/api/fragments";

    fn secp_header(secret: &[u8; 32], method: &str, url: &str, body: &[u8], created_at: i64) -> String {
        // the CLI's signer (cli/src/auth.rs), reproduced
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, secret).unwrap();
        let pubkey = hex::encode(kp.x_only_public_key().0.serialize());
        let mut tags = vec![serde_json::json!(["u", url]), serde_json::json!(["method", method])];
        if !body.is_empty() {
            tags.push(serde_json::json!(["payload", hex::encode(Sha256::digest(body))]));
        }
        let tags = serde_json::Value::Array(tags);
        let id = event_id(&pubkey, created_at, KIND, &tags, "");
        let sig = secp.sign_schnorr_no_aux_rand(&Message::from_digest_slice(&id).unwrap(), &kp);
        let ev = serde_json::json!({"id": hex::encode(id), "pubkey": pubkey, "created_at": created_at,
            "kind": KIND, "tags": tags, "content": "", "sig": hex::encode(sig.as_ref())});
        format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(ev.to_string()))
    }

    #[test]
    fn roundtrip_and_every_refusal() {
        let k = Keys::generate();
        let body = br#"{"name":"a"}"#;
        let h = k.header("POST", URL, body, NOW);
        assert_eq!(verify(Some(&h), "POST", URL, body, NOW, 60).as_deref(), Ok(k.pubkey_hex()));
        assert_eq!(verify(None, "POST", URL, body, NOW, 60), Err(AuthError::MissingHeader));
        assert_eq!(verify(Some("Bearer x"), "POST", URL, body, NOW, 60), Err(AuthError::MissingHeader));
        assert_eq!(verify(Some("Nostr %%%"), "POST", URL, body, NOW, 60), Err(AuthError::Malformed("not base64")));
        assert_eq!(verify(Some(&h), "POST", URL, body, NOW + 61, 60), Err(AuthError::Stale { skew_s: -61 }));
        assert_eq!(verify(Some(&h), "POST", "http://127.0.0.1:8790/api/f/x", body, NOW, 60), Err(AuthError::UrlMismatch));
        assert_eq!(verify(Some(&h), "PUT", URL, body, NOW, 60), Err(AuthError::MethodMismatch));
        assert_eq!(verify(Some(&h), "POST", URL, b"{}", NOW, 60), Err(AuthError::PayloadMismatch));
    }

    #[test]
    fn a_tampered_event_is_refused() {
        let k = Keys::generate();
        let h = k.header("GET", URL, b"", NOW);
        let raw = base64::engine::general_purpose::STANDARD.decode(&h[6..]).unwrap();
        let mut ev: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        ev["created_at"] = serde_json::json!(NOW + 1);
        let forged = format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(ev.to_string()));
        assert_eq!(verify(Some(&forged), "GET", URL, b"", NOW, 60), Err(AuthError::IdMismatch));
        let other = Keys::generate();
        ev["created_at"] = serde_json::json!(NOW);
        ev["pubkey"] = serde_json::json!(other.pubkey_hex());
        let id = event_id(other.pubkey_hex(), NOW, KIND, &ev["tags"], "");
        ev["id"] = serde_json::json!(hex::encode(id));
        let stolen = format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(ev.to_string()));
        assert_eq!(verify(Some(&stolen), "GET", URL, b"", NOW, 60), Err(AuthError::BadSignature));
    }

    #[test]
    fn the_cli_signer_and_this_verifier_agree() {
        let secret = [7u8; 32];
        let h = secp_header(&secret, "POST", URL, b"{}", NOW);
        let pubkey = verify(Some(&h), "POST", URL, b"{}", NOW, 60).expect("a secp256k1 signature verifies with k256");
        let sk = SecretKey::from_slice(&secret).unwrap();
        let expect = XOnlyPublicKey::from_keypair(&Keypair::from_secret_key(&Secp256k1::new(), &sk)).0;
        assert_eq!(pubkey, hex::encode(expect.serialize()));
        // and the other direction
        let k = Keys::from_secret_hex(&hex::encode(secret)).unwrap();
        let h = k.header("GET", URL, b"", NOW);
        let raw = base64::engine::general_purpose::STANDARD.decode(&h[6..]).unwrap();
        let ev: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let sig = secp256k1::schnorr::Signature::from_slice(&hex::decode(ev["sig"].as_str().unwrap()).unwrap()).unwrap();
        let id = hex::decode(ev["id"].as_str().unwrap()).unwrap();
        Secp256k1::verification_only()
            .verify_schnorr(&sig, &Message::from_digest_slice(&id).unwrap(), &expect)
            .expect("a k256 signature verifies with secp256k1");
    }

    #[test]
    fn a_trailing_question_mark_is_no_query() {
        assert_eq!(canonical_url("http://x/a?").as_deref(), Some("http://x/a"));
        assert_eq!(canonical_url("http://x/a?b=1").as_deref(), Some("http://x/a?b=1"));
    }
}
