//! Web Push (phase 2 slice F): payloads encrypted for one browser
//! subscription (RFC 8291, the `aes128gcm` content coding of RFC 8188) and
//! the VAPID token that identifies the sender to the push service (RFC
//! 8292). The randomness comes from the caller (the cell has no getrandom),
//! so encryption here is deterministic given its inputs. A key is drawn
//! from that randomness once, and checked as it is drawn: encryption takes
//! a key already known to be in range.

use std::collections::BTreeMap;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// A push service accepts records up to 4096 bytes; the payload is at most this.
pub const PAYLOAD_MAX_BYTES: usize = 3800;
const RECORD_SIZE: u32 = 4096;
/// Random draws a P-256 secret may take. A uniform 32-byte draw is out of
/// range (zero, or at least the group order) with probability about
/// 2^-128, so four in a row is a broken random source, not bad luck.
pub const DRAWS_MAX: usize = 4;

/// A P-256 secret from the caller's random draws: an out-of-range draw is
/// drawn again, at most `DRAWS_MAX` times in all.
fn drawn<T>(mut draw: impl FnMut() -> [u8; 32], make: impl Fn([u8; 32]) -> Option<T>) -> T {
    for _ in 0..DRAWS_MAX {
        if let Some(key) = make(draw()) {
            return key;
        }
    }
    panic!("{DRAWS_MAX} random draws in a row were out of range for P-256: the random source is broken");
}

/// The sender's one-time key for a push (RFC 8291's application server
/// key pair), in range by construction.
pub struct Ephemeral(SecretKey);

impl Ephemeral {
    pub fn draw(draw: impl FnMut() -> [u8; 32]) -> Ephemeral {
        Ephemeral(drawn(draw, |bytes| SecretKey::from_slice(&bytes).ok()))
    }
}

/// What a browser's `PushSubscription` gives the sender.
pub struct Subscription<'a> {
    pub endpoint: &'a str,
    /// The browser's public key (`keys.p256dh`, base64url of the uncompressed point).
    pub p256dh: &'a str,
    /// The browser's auth secret (`keys.auth`, base64url, 16 bytes).
    pub auth: &'a str,
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, out).expect("HKDF output lengths here are within 255 blocks");
}

fn uncompressed(key: &PublicKey) -> Vec<u8> {
    key.to_encoded_point(false).as_bytes().to_vec()
}

/// The content key and nonce both sides derive (RFC 8291 section 3.4).
fn derive(ecdh: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8]) -> ([u8; 16], [u8; 12]) {
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    hkdf(auth, ecdh, &info, &mut ikm);
    let mut cek = [0u8; 16];
    hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek);
    let mut nonce = [0u8; 12];
    hkdf(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce);
    (cek, nonce)
}

/// Checks a subscription's keys; answers the browser's public key and auth secret.
pub fn subscription_keys(sub: &Subscription) -> Result<(PublicKey, Vec<u8>), String> {
    let ua = B64URL.decode(sub.p256dh.trim_end_matches('=')).map_err(|_| "keys.p256dh is not base64url")?;
    let ua = PublicKey::from_sec1_bytes(&ua).map_err(|_| "keys.p256dh is not a P-256 point")?;
    let auth = B64URL.decode(sub.auth.trim_end_matches('=')).map_err(|_| "keys.auth is not base64url")?;
    if auth.len() != 16 {
        return Err("keys.auth is not 16 bytes".into());
    }
    Ok((ua, auth))
}

/// The request body for one push: `payload` encrypted for `sub`, with the
/// one-time key `ephemeral` and a record salt `salt` (both random).
pub fn encrypt(sub: &Subscription, payload: &[u8], ephemeral: &Ephemeral, salt: [u8; 16]) -> Result<Vec<u8>, String> {
    if payload.len() > PAYLOAD_MAX_BYTES {
        return Err(format!("a push payload is at most {PAYLOAD_MAX_BYTES} bytes"));
    }
    let (ua, auth) = subscription_keys(sub)?;
    let secret = &ephemeral.0;
    let as_public = uncompressed(&secret.public_key());
    let ecdh = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), ua.as_affine());
    let (cek, nonce) = derive(ecdh.raw_secret_bytes(), &auth, &uncompressed(&ua), &as_public, &salt);
    let mut record = payload.to_vec();
    record.push(0x02); // the last (only) record's padding delimiter
    let sealed = Aes128Gcm::new_from_slice(&cek)
        .expect("a 16-byte key")
        .encrypt(&Nonce::from(nonce), record.as_slice())
        .expect("AES-GCM encryption of an in-memory buffer does not fail");
    let mut body = salt.to_vec();
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(&as_public);
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// A VAPID key (the fragment's, sealed at rest by the cell).
pub struct Vapid(SigningKey);

impl Vapid {
    /// From 32 bytes; `None` when they are out of range (a stored key that
    /// does not load).
    pub fn from_bytes(secret: [u8; 32]) -> Option<Vapid> {
        SigningKey::from_slice(&secret).ok().map(Vapid)
    }

    /// A new key from random draws (`DRAWS_MAX` at most).
    pub fn draw(draw: impl FnMut() -> [u8; 32]) -> Vapid {
        drawn(draw, Vapid::from_bytes)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    /// The application server key a browser subscribes with (base64url).
    pub fn public_key(&self) -> String {
        B64URL.encode(self.0.verifying_key().to_encoded_point(false).as_bytes())
    }

    /// `Authorization` for every push to one push service (`audience`,
    /// its origin): `vapid t=<jwt>, k=<key>`.
    fn authorization(&self, audience: &str, subject: &str, now_s: i64) -> String {
        let header = B64URL.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64URL.encode(serde_json::json!({ "aud": audience, "exp": now_s + TOKEN_TTL_S, "sub": subject }).to_string());
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.0.sign(signing_input.as_bytes());
        format!("vapid t={signing_input}.{}, k={}", B64URL.encode(signature.to_bytes()), self.public_key())
    }
}

/// How long a VAPID token is good for (RFC 8292 allows at most 24 hours).
const TOKEN_TTL_S: i64 = 12 * 3600;

/// The VAPID tokens of one push, one per push service: a token names only
/// the service's origin (its audience), never the subscription, so a push
/// to thousands of browsers on three services signs three times. Made for
/// one push and dropped with it, so it is not a cache: its tokens were all
/// signed at `now_s`, and each is good for `TOKEN_TTL_S`, far longer than
/// one push takes to queue.
pub struct Tokens<'a> {
    vapid: &'a Vapid,
    subject: &'a str,
    now_s: i64,
    by_audience: BTreeMap<String, String>,
}

impl<'a> Tokens<'a> {
    pub fn new(vapid: &'a Vapid, subject: &'a str, now_s: i64) -> Tokens<'a> {
        Tokens { vapid, subject, now_s, by_audience: BTreeMap::new() }
    }

    /// `Authorization` for a push to `endpoint`: signed the first time its
    /// push service comes up, the same token after that.
    pub fn authorization(&mut self, endpoint: &str) -> Result<&str, String> {
        let url = url::Url::parse(endpoint).map_err(|e| format!("endpoint: {e}"))?;
        let (vapid, subject, now_s) = (self.vapid, self.subject, self.now_s);
        let token = self.by_audience.entry(url.origin().ascii_serialization()).or_insert_with_key(|audience| vapid.authorization(audience, subject, now_s));
        Ok(token.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    /// RFC 8291 section 5's example, base64url as the RFC prints it (its
    /// line breaks removed): the sender's key and salt, the receiver's
    /// public key and auth secret, the plaintext, and the body a push
    /// service receives. crates/fakes decrypts the same body.
    const RFC_SENDER_SECRET: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const RFC_SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const RFC_RECEIVER_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const RFC_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const RFC_PLAINTEXT: &[u8] = b"When I grow up, I want to be a watermelon";
    const RFC_BODY: &str = concat!(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
        "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
        "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
    );

    fn rfc_subscription() -> Subscription<'static> {
        Subscription { endpoint: "https://push.example.net/push/JzLQ3raZJfFBR0aqvOMsLrt54w4rJUsV", p256dh: RFC_RECEIVER_PUBLIC, auth: RFC_AUTH }
    }

    fn rfc_ephemeral() -> Ephemeral {
        let secret: [u8; 32] = B64URL.decode(RFC_SENDER_SECRET).unwrap().try_into().unwrap();
        Ephemeral::draw(|| secret)
    }

    /// Goal: a push is encrypted as RFC 8291 says, byte for byte, so a real
    /// browser can read it (a round trip through our own decrypt passed
    /// with a wrong HKDF label on both sides). Method: section 5's example,
    /// its body written out.
    #[test]
    fn the_rfc_8291_example_encrypts_to_its_body() {
        let salt: [u8; 16] = B64URL.decode(RFC_SALT).unwrap().try_into().unwrap();
        let body = encrypt(&rfc_subscription(), RFC_PLAINTEXT, &rfc_ephemeral(), salt).unwrap();
        assert_eq!(B64URL.encode(&body), RFC_BODY);
    }

    #[test]
    fn what_encrypt_refuses() {
        let ephemeral = rfc_ephemeral();
        assert!(encrypt(&rfc_subscription(), &[0u8; PAYLOAD_MAX_BYTES], &ephemeral, [5u8; 16]).is_ok());
        assert!(encrypt(&rfc_subscription(), &[0u8; PAYLOAD_MAX_BYTES + 1], &ephemeral, [5u8; 16]).is_err());
        for (p256dh, auth) in [("nope", RFC_AUTH), (RFC_SENDER_SECRET, RFC_AUTH), (RFC_RECEIVER_PUBLIC, "nope!"), (RFC_RECEIVER_PUBLIC, RFC_SALT.get(..20).unwrap())] {
            let sub = Subscription { endpoint: "https://push.example.net/x", p256dh, auth };
            assert!(encrypt(&sub, b"x", &ephemeral, [5u8; 16]).is_err(), "{p256dh} {auth}");
        }
    }

    /// Zero and all-ones are both out of range for P-256 (all-ones is past
    /// the group order): a draw of either is drawn again, and the first
    /// draw in range is the key.
    #[test]
    fn a_key_draw_skips_out_of_range_bytes() {
        let draws = [[0u8; 32], [0xffu8; 32], [3u8; 32], [4u8; 32]];
        let mut next = draws.iter().copied();
        let mut taken = 0;
        let ephemeral = Ephemeral::draw(|| {
            taken += 1;
            next.next().unwrap()
        });
        assert_eq!(taken, 3, "two draws out of range, the third taken");
        assert_eq!(uncompressed(&ephemeral.0.public_key()), uncompressed(&SecretKey::from_slice(&[3u8; 32]).unwrap().public_key()));
        let mut next = draws.iter().copied();
        let vapid = Vapid::draw(|| next.next().unwrap());
        assert_eq!(vapid.to_bytes(), [3u8; 32]);
    }

    #[test]
    #[should_panic(expected = "the random source is broken")]
    fn a_key_draw_stops_after_its_bound() {
        let mut taken = 0;
        let _ = Ephemeral::draw(|| {
            taken += 1;
            assert!(taken <= DRAWS_MAX, "drew past the bound");
            [0u8; 32]
        });
    }

    /// A token's claims, once its signature verifies under `vapid`'s key.
    fn verified_claims(vapid: &Vapid, auth: &str) -> serde_json::Value {
        let (t, k) = auth.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(k, vapid.public_key());
        let (signing_input, sig) = t.rsplit_once('.').unwrap();
        let key = VerifyingKey::from_sec1_bytes(&B64URL.decode(k).unwrap()).unwrap();
        let sig = Signature::from_slice(&B64URL.decode(sig).unwrap()).unwrap();
        assert!(key.verify(signing_input.as_bytes(), &sig).is_ok());
        serde_json::from_slice(&B64URL.decode(signing_input.split('.').nth(1).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn vapid_tokens_verify() {
        let vapid = Vapid::from_bytes([11u8; 32]).unwrap();
        let mut tokens = Tokens::new(&vapid, "mailto:ops@fragment.invalid", 1_790_000_000);
        let claims = verified_claims(&vapid, tokens.authorization("https://fcm.googleapis.com/fcm/send/xyz").unwrap());
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_790_000_000 + 12 * 3600);
        assert_eq!(claims["sub"], "mailto:ops@fragment.invalid");
        assert!(tokens.authorization("not a url").is_err());
        assert_eq!(Vapid::from_bytes(vapid.to_bytes()).unwrap().public_key(), vapid.public_key());
    }

    /// Five subscriptions on three push services (the same host on another
    /// port is another origin): three signatures, and each subscription's
    /// token names its own service.
    #[test]
    fn one_push_signs_once_per_push_service() {
        let vapid = Vapid::from_bytes([11u8; 32]).unwrap();
        let mut tokens = Tokens::new(&vapid, "mailto:ops@fragment.invalid", 1_790_000_000);
        let endpoints = [
            ("https://fcm.googleapis.com/fcm/send/a", "https://fcm.googleapis.com"),
            ("https://updates.push.services.mozilla.com/wpush/v2/b", "https://updates.push.services.mozilla.com"),
            ("https://fcm.googleapis.com/fcm/send/c", "https://fcm.googleapis.com"),
            ("https://fcm.googleapis.com:8443/fcm/send/d", "https://fcm.googleapis.com:8443"),
            ("https://updates.push.services.mozilla.com/wpush/v2/e", "https://updates.push.services.mozilla.com"),
        ];
        for (endpoint, audience) in endpoints {
            let auth = tokens.authorization(endpoint).unwrap().to_string();
            assert_eq!(verified_claims(&vapid, &auth)["aud"], audience, "{endpoint}");
        }
        assert_eq!(tokens.by_audience.len(), 3, "one signature per push service, not per subscription");
    }
}
