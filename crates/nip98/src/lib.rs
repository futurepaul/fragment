//! NIP-98 HTTP auth: a kind-27235 Nostr event in `Authorization: Nostr
//! <base64>`, binding the method, the URL, and the body's SHA-256 to a
//! BIP-340 signature. `verify_request` runs in the routers (wasm32); `Keys`
//! signs (the `signer` feature, and `sign` for key generation on hosts).

use base64::Engine;
use k256::schnorr::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

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
    /// A key proof that names someone other than the request's signer.
    NotForSigner,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingHeader => write!(f, "missing `Authorization: Nostr` header"),
            AuthError::Malformed(what) => write!(f, "malformed auth event: {what}"),
            AuthError::WrongKind => write!(f, "auth event is not kind {KIND}"),
            AuthError::BadPubkey => write!(f, "auth event pubkey is not a 64-hex secp256k1 key"),
            AuthError::Stale { skew_s } => write!(f, "auth event created_at is {skew_s} s from now"),
            AuthError::UrlMismatch => write!(f, "auth event `u` tag does not match the request URL"),
            AuthError::MethodMismatch => write!(f, "auth event `method` tag does not match"),
            AuthError::PayloadMismatch => write!(f, "auth event `payload` tag does not match the body"),
            AuthError::IdMismatch => write!(f, "auth event id does not match its content"),
            AuthError::BadSignature => write!(f, "auth event signature does not verify"),
            AuthError::NotForSigner => write!(f, "the key proof's `p` tag does not name this request's signer"),
        }
    }
}

/// A Nostr event as NIP-01 defines it, decoded once from the header: every
/// field is required and typed, and a tag is an array of strings. The
/// signers serialize this same struct.
#[derive(Debug, Serialize, Deserialize)]
struct Event {
    id: String,
    pubkey: String,
    created_at: i64,
    kind: u64,
    tags: Vec<Vec<String>>,
    content: String,
    sig: String,
}

impl Event {
    /// The value of the first tag named `name`. The tags are bounded by the
    /// header they came in, whose size the platform limits.
    fn tag(&self, name: &str) -> Option<&str> {
        for tag in &self.tags {
            if let [key, value, ..] = tag.as_slice() {
                if key == name {
                    return Some(value);
                }
            }
        }
        None
    }
}

/// NIP-01's id: the SHA-256 of `[0, pubkey, created_at, kind, tags, content]`.
fn event_id(pubkey: &str, created_at: i64, kind: u64, tags: &[Vec<String>], content: &str) -> [u8; 32] {
    let preimage = serde_json::to_string(&(0u8, pubkey, created_at, kind, tags, content)).expect("strings and integers serialize");
    Sha256::digest(preimage.as_bytes()).into()
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Whether a signed `u` tag names the URL a request arrived on: the same
/// origin, path, and query (a trailing `?` is no query).
fn same_resource(signed: &Url, arrived: &Url) -> bool {
    let query = |u: &Url| u.query().filter(|q| !q.is_empty()).map(str::to_string);
    signed.origin() == arrived.origin() && signed.path() == arrived.path() && query(signed) == query(arrived)
}

/// The URL a request arrived on, as its client named it and signed it. A
/// proxy that ends TLS in front of celld (Fly's) forwards plain HTTP and
/// says so in `x-forwarded-proto`; the client signed the https URL, and
/// links and cookies name it too.
pub fn arrived_url(mut url: Url, forwarded_proto: Option<&str>) -> Url {
    if url.scheme() == "http" && forwarded_proto == Some("https") {
        let switched = url.set_scheme("https");
        assert!(switched.is_ok(), "http and https are both special schemes, so one becomes the other");
    }
    url
}

fn decode(header: Option<&str>) -> Result<Event, AuthError> {
    let header = header.ok_or(AuthError::MissingHeader)?;
    let b64 = header.strip_prefix("Nostr ").ok_or(AuthError::MissingHeader)?.trim();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| AuthError::Malformed("not base64"))?;
    serde_json::from_slice(&raw).map_err(|e| {
        if e.is_data() {
            AuthError::Malformed("not a NIP-01 event (id, pubkey, created_at, kind, tags, content, sig)")
        } else {
            AuthError::Malformed("not JSON")
        }
    })
}

/// Checks a decoded event against the request, in the order a refusal is
/// named: kind, key, clock, URL, method, body, id, signature.
fn check(event: &Event, method: &str, url: &Url, body: &[u8], now_s: i64, window_s: i64) -> Result<(), AuthError> {
    assert!(window_s >= 0, "an auth window is a duration");
    if event.kind != KIND {
        return Err(AuthError::WrongKind);
    }
    if !is_hex64(&event.pubkey) {
        return Err(AuthError::BadPubkey);
    }
    // abs_diff, not `created_at - now_s`: that subtraction wraps in release
    // builds for a created_at near i64::MIN, and the wrapped skew passed as fresh
    if event.created_at.abs_diff(now_s) > window_s.unsigned_abs() {
        return Err(AuthError::Stale { skew_s: event.created_at.saturating_sub(now_s) });
    }
    let signed_url = event.tag("u").and_then(|u| Url::parse(u).ok()).ok_or(AuthError::UrlMismatch)?;
    if !same_resource(&signed_url, url) {
        return Err(AuthError::UrlMismatch);
    }
    // NIP-98 names the method in upper case
    let method_signed = event.tag("method").is_some_and(|m| m.bytes().all(|b| !b.is_ascii_lowercase()) && m.eq_ignore_ascii_case(method));
    if !method_signed {
        return Err(AuthError::MethodMismatch);
    }
    // A payload tag binds the body the router read, so an empty body is
    // held to it too: a signature over a body, replayed without one, is
    // refused. No payload tag means no body.
    let payload_signed = match event.tag("payload") {
        Some(payload) => payload == hex::encode(Sha256::digest(body)),
        None => body.is_empty(),
    };
    if !payload_signed {
        return Err(AuthError::PayloadMismatch);
    }
    let id = event_id(&event.pubkey, event.created_at, event.kind, &event.tags, &event.content);
    if event.id != hex::encode(id) {
        return Err(AuthError::IdMismatch);
    }
    // Exact sizes before k256 sees them: its `Signature::try_from` splits
    // the slice unchecked and panics on one shorter than 32 bytes.
    let mut sig_bytes = [0u8; 64];
    hex::decode_to_slice(&event.sig, &mut sig_bytes).map_err(|_| AuthError::BadSignature)?;
    let sig = Signature::try_from(sig_bytes.as_slice()).map_err(|_| AuthError::BadSignature)?;
    let mut key_bytes = [0u8; 32];
    hex::decode_to_slice(&event.pubkey, &mut key_bytes).map_err(|_| AuthError::BadPubkey)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| AuthError::BadPubkey)?;
    key.verify_raw(&id, &sig).map_err(|_| AuthError::BadSignature)
}

/// Verifies a NIP-98 header for `method url` with the body the router read
/// (empty when it holds none: then the event must carry no payload tag,
/// or the hash of nothing), at `now_s` (seconds since the epoch). `url` is
/// the URL the request arrived on (`arrived_url`). Returns the signer's
/// public key, hex.
pub fn verify_request(header: Option<&str>, method: &str, url: &Url, body: &[u8], now_s: i64, window_s: i64) -> Result<String, AuthError> {
    let event = decode(header)?;
    check(&event, method, url, body, now_s, window_s)?;
    assert!(is_hex64(&event.pubkey), "a verified event names a 64-hex key");
    Ok(event.pubkey)
}

/// `verify_request` for a URL held as text (an unparseable one names no
/// request, so it is a mismatch).
pub fn verify(header: Option<&str>, method: &str, url: &str, body: &[u8], now_s: i64, window_s: i64) -> Result<String, AuthError> {
    let url = Url::parse(url).map_err(|_| AuthError::UrlMismatch)?;
    verify_request(header, method, &url, body, now_s, window_s)
}

/// Verifies a key proof: a NIP-98 event by a new key for the same request
/// (`method url`), naming the request's signer in a `p` tag. The request is
/// signed by a key the registry knows; the proof in its body shows that
/// whoever sent it also holds the new key, and meant it for this signer
/// (a proof lifted from another request names someone else). Returns the
/// new key, hex.
pub fn verify_proof(proof: &str, method: &str, url: &str, signer_hex: &str, now_s: i64, window_s: i64) -> Result<String, AuthError> {
    let url = Url::parse(url).map_err(|_| AuthError::UrlMismatch)?;
    let event = decode(Some(proof))?;
    check(&event, method, &url, &[], now_s, window_s)?;
    if event.tag("p") != Some(signer_hex) {
        return Err(AuthError::NotForSigner);
    }
    Ok(event.pubkey)
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
#[derive(Clone)]
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
        let payload = (!body.is_empty()).then(|| hex::encode(Sha256::digest(body)));
        self.header_for_payload(method, url, payload.as_deref(), created_at)
    }

    /// `header` for a body the signer holds only the SHA-256 (hex) of, or
    /// for none (`None`: no payload tag).
    pub fn header_for_payload(&self, method: &str, url: &str, payload_sha_hex: Option<&str>, created_at: i64) -> String {
        let mut tags = vec![tag("u", url), tag("method", &method.to_ascii_uppercase())];
        if let Some(p) = payload_sha_hex {
            tags.push(tag("payload", p));
        }
        self.event(tags, created_at)
    }

    /// A key proof (`verify_proof`): this key agrees to join whoever signs
    /// `method url` with `signer_hex`.
    pub fn proof(&self, method: &str, url: &str, signer_hex: &str, created_at: i64) -> String {
        let tags = vec![tag("u", url), tag("method", &method.to_ascii_uppercase()), tag("p", signer_hex)];
        self.event(tags, created_at)
    }

    fn event(&self, tags: Vec<Vec<String>>, created_at: i64) -> String {
        let id = event_id(&self.pubkey_hex, created_at, KIND, &tags, "");
        let sig = self.key.sign_raw(&id, &[0u8; 32]).expect("BIP-340 signing a 32-byte digest");
        let event = Event {
            id: hex::encode(id),
            pubkey: self.pubkey_hex.clone(),
            created_at,
            kind: KIND,
            tags,
            content: String::new(),
            sig: hex::encode(sig.to_bytes()),
        };
        let json = serde_json::to_string(&event).expect("an event serializes");
        format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(json))
    }
}

#[cfg(feature = "signer")]
fn tag(name: &str, value: &str) -> Vec<String> {
    vec![name.to_string(), value.to_string()]
}

// The tests sign with fixed secrets through the `signer` feature, which
// the crate's dev-dependency on itself turns on, so a plain `cargo test -p
// fragment-nip98` runs them.
#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;
    const URL: &str = "http://127.0.0.1:8790/api/fragments";

    fn keys(n: u8) -> Keys {
        Keys::from_secret_hex(&hex::encode([n; 32])).expect("a valid secret")
    }

    fn url(s: &str) -> Url {
        Url::parse(s).expect("a test URL")
    }

    fn verify_at(header: &str, method: &str, at: &str, body: &[u8]) -> Result<String, AuthError> {
        verify_request(Some(header), method, &url(at), body, NOW, 60)
    }

    fn encode(event: &Event) -> String {
        format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(event).unwrap()))
    }

    fn encode_json(v: &serde_json::Value) -> String {
        format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(v.to_string()))
    }

    fn decoded(header: &str) -> Event {
        decode(Some(header)).expect("a header this test made")
    }

    /// An event by `k`, correctly signed over whatever `edit` made of a
    /// good request header: it fails only on what the edit broke.
    fn signed(k: &Keys, edit: impl FnOnce(&mut Event)) -> String {
        let mut event = decoded(&k.header("POST", URL, b"", NOW));
        edit(&mut event);
        let id = event_id(&event.pubkey, event.created_at, event.kind, &event.tags, &event.content);
        event.id = hex::encode(id);
        event.sig = hex::encode(k.key.sign_raw(&id, &[0u8; 32]).unwrap().to_bytes());
        encode(&event)
    }

    #[test]
    fn pubkey_of_secret_matches_keys() {
        let k = keys(3);
        assert_eq!(pubkey_of_secret(&k.secret_hex()).as_deref(), Some(k.pubkey_hex()));
        assert_eq!(pubkey_of_secret(&"0".repeat(64)), None);
        assert_eq!(pubkey_of_secret("abc"), None);
    }

    /// A pin from another implementation: this secret's x-only key as
    /// @noble/curves computes it (its npub is pinned in fragment-core).
    #[test]
    fn a_known_secret_has_its_known_key() {
        assert_eq!(keys(1).pubkey_hex(), "1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f");
    }

    #[test]
    fn a_signed_request_verifies() {
        let k = keys(1);
        let body = br#"{"name":"a"}"#;
        assert_eq!(verify_at(&k.header("POST", URL, body, NOW), "POST", URL, body).as_deref(), Ok(k.pubkey_hex()));
        assert_eq!(verify_at(&k.header("GET", URL, b"", NOW), "GET", URL, b"").as_deref(), Ok(k.pubkey_hex()));
        // the text form parses the URL and agrees
        assert_eq!(verify(Some(&k.header("POST", URL, body, NOW)), "POST", URL, body, NOW, 60).as_deref(), Ok(k.pubkey_hex()));
        // a trailing `?` is no query; a default port is no port
        assert!(verify_at(&k.header("GET", "http://x/a?", b"", NOW), "GET", "http://x/a", b"").is_ok());
        assert!(verify_at(&k.header("GET", "https://x:443/a", b"", NOW), "GET", "https://x/a", b"").is_ok());
        // the edge of the window is inside it
        assert!(verify_request(Some(&k.header("GET", URL, b"", NOW - 60)), "GET", &url(URL), b"", NOW, 60).is_ok());
    }

    #[test]
    fn a_header_that_is_not_an_event_is_refused() {
        let k = keys(1);
        let good = k.header("POST", URL, b"", NOW);
        assert_eq!(verify_request(None, "POST", &url(URL), b"", NOW, 60), Err(AuthError::MissingHeader));
        assert_eq!(verify_at("Bearer x", "POST", URL, b""), Err(AuthError::MissingHeader));
        assert_eq!(verify_at("Nostr %%%", "POST", URL, b""), Err(AuthError::Malformed("not base64")));
        let not_json = format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode("{not json"));
        assert_eq!(verify_at(&not_json, "POST", URL, b""), Err(AuthError::Malformed("not JSON")));
        // a bad created_at, a missing field, a tag that is not strings
        let mut v = serde_json::to_value(decoded(&good)).unwrap();
        v["created_at"] = serde_json::json!("soon");
        let refused = verify_at(&encode_json(&v), "POST", URL, b"");
        assert!(matches!(refused, Err(AuthError::Malformed(what)) if what.starts_with("not a NIP-01 event")), "{refused:?}");
        v["created_at"] = serde_json::json!(NOW);
        assert!(verify_at(&encode_json(&v), "POST", URL, b"").is_ok(), "the edit above is the only fault");
        let mut missing = v.clone();
        missing.as_object_mut().unwrap().remove("content");
        assert!(matches!(verify_at(&encode_json(&missing), "POST", URL, b""), Err(AuthError::Malformed(_))));
        let mut numeric_tag = v.clone();
        numeric_tag["tags"][0][1] = serde_json::json!(7);
        assert!(matches!(verify_at(&encode_json(&numeric_tag), "POST", URL, b""), Err(AuthError::Malformed(_))));
    }

    #[test]
    fn every_refusal_of_a_well_formed_event() {
        let k = keys(1);
        let body = br#"{"name":"a"}"#;
        let h = k.header("POST", URL, body, NOW);
        // kind, key, clock
        assert_eq!(verify_at(&signed(&k, |e| e.kind = 1), "POST", URL, b""), Err(AuthError::WrongKind));
        assert_eq!(verify_at(&signed(&k, |e| e.pubkey = "ab".into()), "POST", URL, b""), Err(AuthError::BadPubkey));
        assert_eq!(verify_at(&signed(&k, |e| e.pubkey = e.pubkey.to_uppercase()), "POST", URL, b""), Err(AuthError::BadPubkey));
        // 64 hex that is no point on the curve (x past the field's prime)
        assert_eq!(verify_at(&signed(&k, |e| e.pubkey = "f".repeat(64)), "POST", URL, b""), Err(AuthError::BadPubkey));
        assert_eq!(verify_request(Some(&h), "POST", &url(URL), body, NOW + 61, 60), Err(AuthError::Stale { skew_s: -61 }));
        assert_eq!(verify_request(Some(&h), "POST", &url(URL), body, NOW - 61, 60), Err(AuthError::Stale { skew_s: 61 }));
        // the ends of i64 are stale, not a subtraction that wraps to fresh
        for created_at in [i64::MIN, i64::MIN + NOW, i64::MAX] {
            let far = signed(&k, |e| e.created_at = created_at);
            assert!(matches!(verify_at(&far, "POST", URL, b""), Err(AuthError::Stale { .. })), "{created_at}");
        }
        // URL
        assert_eq!(verify_at(&h, "POST", "http://127.0.0.1:8790/api/f/x", body), Err(AuthError::UrlMismatch));
        assert_eq!(verify_at(&h, "POST", "http://127.0.0.1:8790/api/fragments?x=1", body), Err(AuthError::UrlMismatch));
        assert_eq!(verify_at(&h, "POST", "https://127.0.0.1:8790/api/fragments", body), Err(AuthError::UrlMismatch));
        assert_eq!(verify_at(&signed(&k, |e| e.tags.retain(|t| t[0] != "u")), "POST", URL, b""), Err(AuthError::UrlMismatch));
        assert_eq!(verify_at(&signed(&k, |e| e.tags[0][1] = "not a url".into()), "POST", URL, b""), Err(AuthError::UrlMismatch));
        assert_eq!(verify(Some(&h), "POST", "not a url", body, NOW, 60), Err(AuthError::UrlMismatch));
        // method
        assert_eq!(verify_at(&h, "PUT", URL, body), Err(AuthError::MethodMismatch));
        assert_eq!(verify_at(&signed(&k, |e| e.tags[1][1] = "post".into()), "POST", URL, b""), Err(AuthError::MethodMismatch));
        assert_eq!(verify_at(&signed(&k, |e| e.tags.retain(|t| t[0] != "method")), "POST", URL, b""), Err(AuthError::MethodMismatch));
        // body
        assert_eq!(verify_at(&h, "POST", URL, b"{}"), Err(AuthError::PayloadMismatch));
        assert_eq!(verify_at(&k.header("POST", URL, b"", NOW), "POST", URL, body), Err(AuthError::PayloadMismatch), "a body needs a payload tag");
    }

    /// Goal: a signature over a body does not authorize the same request
    /// without it. Method: a header whose payload tag names a body, sent
    /// with none, is refused; one naming the hash of nothing is not.
    #[test]
    fn a_payload_tag_binds_an_empty_body_too() {
        let k = keys(1);
        let h = k.header("POST", URL, br#"{"name":"a"}"#, NOW);
        assert_eq!(verify_at(&h, "POST", URL, b""), Err(AuthError::PayloadMismatch));
        let empty = k.header_for_payload("POST", URL, Some(&hex::encode(Sha256::digest(b""))), NOW);
        assert_eq!(verify_at(&empty, "POST", URL, b"").as_deref(), Ok(k.pubkey_hex()));
        // and a key proof (which carries no body) is held to the same rule
        let signer = "ab".repeat(32);
        let lifted = signed(&k, |e| {
            e.tags.push(tag("payload", &hex::encode(Sha256::digest(b"x"))));
            e.tags.push(tag("p", &signer));
        });
        assert_eq!(verify_proof(&lifted, "POST", URL, &signer, NOW, 60), Err(AuthError::PayloadMismatch));
    }

    #[test]
    fn a_tampered_event_is_refused() {
        let k = keys(1);
        let h = k.header("GET", URL, b"", NOW);
        let mut event = decoded(&h);
        event.created_at = NOW + 1;
        assert_eq!(verify_at(&encode(&event), "GET", URL, b""), Err(AuthError::IdMismatch));
        // another key's name on this signature, the id recomputed to match
        let other = keys(2);
        let mut event = decoded(&h);
        event.pubkey = other.pubkey_hex().to_string();
        event.id = hex::encode(event_id(&event.pubkey, event.created_at, event.kind, &event.tags, &event.content));
        assert_eq!(verify_at(&encode(&event), "GET", URL, b""), Err(AuthError::BadSignature));
        let mut event = decoded(&h);
        event.sig = "zz".into();
        assert_eq!(verify_at(&encode(&event), "GET", URL, b""), Err(AuthError::BadSignature));
        // a signature of any other size is refused, never a panic (k256
        // panicked below 32 bytes); so is one of the right size that does
        // not verify
        for bytes in [0usize, 1, 2, 31, 32, 63, 64, 65, 128] {
            let mut event = decoded(&h);
            event.sig = "ab".repeat(bytes);
            assert_eq!(verify_at(&encode(&event), "GET", URL, b""), Err(AuthError::BadSignature), "{bytes} bytes");
        }
    }

    #[test]
    fn key_proofs() {
        let (signer, new) = (keys(1), keys(2));
        let p = new.proof("POST", URL, signer.pubkey_hex(), NOW);
        assert_eq!(verify_proof(&p, "POST", URL, signer.pubkey_hex(), NOW, 60).as_deref(), Ok(new.pubkey_hex()));
        // meant for someone else, another request, stale
        assert_eq!(verify_proof(&p, "POST", URL, new.pubkey_hex(), NOW, 60), Err(AuthError::NotForSigner));
        assert_eq!(verify_proof(&p, "POST", "http://127.0.0.1:8790/api/x", signer.pubkey_hex(), NOW, 60), Err(AuthError::UrlMismatch));
        assert_eq!(verify_proof(&p, "DELETE", URL, signer.pubkey_hex(), NOW, 60), Err(AuthError::MethodMismatch));
        assert_eq!(verify_proof(&p, "POST", URL, signer.pubkey_hex(), NOW + 61, 60), Err(AuthError::Stale { skew_s: -61 }));
        // an ordinary request header names no one
        let h = new.header("POST", URL, b"", NOW);
        assert_eq!(verify_proof(&h, "POST", URL, signer.pubkey_hex(), NOW, 60), Err(AuthError::NotForSigner));
    }

    #[test]
    fn the_arrived_url_is_the_one_the_client_named() {
        let https = |u: &str, proto: Option<&str>| arrived_url(url(u), proto).to_string();
        assert_eq!(https("http://x/a?b=1", Some("https")), "https://x/a?b=1");
        assert_eq!(https("http://x/a", None), "http://x/a");
        assert_eq!(https("http://x/a", Some("http")), "http://x/a");
        assert_eq!(https("https://x/a", Some("https")), "https://x/a");
        // a signature for the https URL verifies on the request a proxy forwarded
        let k = keys(1);
        let arrived = arrived_url(url("http://fragment.club/api/fragments"), Some("https"));
        let h = k.header("GET", "https://fragment.club/api/fragments", b"", NOW);
        assert!(verify_request(Some(&h), "GET", &arrived, b"", NOW, 60).is_ok());
    }
}
