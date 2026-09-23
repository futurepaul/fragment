//! Web Push (phase 2 slice F): payloads encrypted for one browser
//! subscription (RFC 8291, the `aes128gcm` content coding of RFC 8188) and
//! the VAPID token that identifies the sender to the push service (RFC
//! 8292). The randomness comes from the caller (the cell has no getrandom),
//! so encryption here is deterministic given its inputs.

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

/// The request body for one push: `payload` encrypted for `sub`, with an
/// ephemeral key made from `ephemeral` and a record salt `salt` (both random).
pub fn encrypt(sub: &Subscription, payload: &[u8], ephemeral: [u8; 32], salt: [u8; 16]) -> Result<Vec<u8>, String> {
    if payload.len() > PAYLOAD_MAX_BYTES {
        return Err(format!("a push payload is at most {PAYLOAD_MAX_BYTES} bytes"));
    }
    let (ua, auth) = subscription_keys(sub)?;
    let secret = SecretKey::from_slice(&ephemeral).map_err(|_| "the ephemeral key is out of range; draw again")?;
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

/// A browser's side of `encrypt` (the e2e's push service decrypts with it).
pub fn decrypt(ua_secret: &SecretKey, auth: &[u8], body: &[u8]) -> Result<Vec<u8>, String> {
    if body.len() < 21 {
        return Err("too short for an aes128gcm header".into());
    }
    let salt = &body[..16];
    let id_len = body[20] as usize;
    let as_public = body.get(21..21 + id_len).ok_or("the key id overruns the body")?;
    let sealed = &body[21 + id_len..];
    let sender = PublicKey::from_sec1_bytes(as_public).map_err(|_| "the key id is not a P-256 point")?;
    let ecdh = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), sender.as_affine());
    let (cek, nonce) = derive(ecdh.raw_secret_bytes(), auth, &uncompressed(&ua_secret.public_key()), as_public, salt);
    let nonce: [u8; 12] = nonce;
    let mut record = Aes128Gcm::new_from_slice(&cek)
        .expect("a 16-byte key")
        .decrypt(&Nonce::from(nonce), sealed)
        .map_err(|_| "the record does not decrypt")?;
    match record.iter().rposition(|b| *b != 0) {
        Some(i) if record[i] == 0x02 => {
            record.truncate(i);
            Ok(record)
        }
        _ => Err("no last-record delimiter".into()),
    }
}

/// A VAPID key (the fragment's, sealed at rest by the cell).
pub struct Vapid(SigningKey);

impl Vapid {
    /// From 32 random bytes; `None` in the rare case they are out of range.
    pub fn from_bytes(secret: [u8; 32]) -> Option<Vapid> {
        SigningKey::from_slice(&secret).ok().map(Vapid)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    /// The application server key a browser subscribes with (base64url).
    pub fn public_key(&self) -> String {
        B64URL.encode(self.0.verifying_key().to_encoded_point(false).as_bytes())
    }

    /// `Authorization` for a push to `endpoint`: `vapid t=<jwt>, k=<key>`.
    pub fn authorization(&self, endpoint: &str, subject: &str, now_s: i64) -> Result<String, String> {
        let url = url::Url::parse(endpoint).map_err(|e| format!("endpoint: {e}"))?;
        let audience = url.origin().ascii_serialization();
        let header = B64URL.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64URL.encode(serde_json::json!({ "aud": audience, "exp": now_s + 12 * 3600, "sub": subject }).to_string());
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.0.sign(signing_input.as_bytes());
        Ok(format!("vapid t={signing_input}.{}, k={}", B64URL.encode(signature.to_bytes()), self.public_key()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    fn browser() -> (SecretKey, String, Vec<u8>, String) {
        let secret = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let p256dh = B64URL.encode(uncompressed(&secret.public_key()));
        let auth = vec![9u8; 16];
        let auth_b64 = B64URL.encode(&auth);
        (secret, p256dh, auth, auth_b64)
    }

    #[test]
    fn round_trip() {
        let (secret, p256dh, auth, auth_b64) = browser();
        let sub = Subscription { endpoint: "https://push.example.com/abc", p256dh: &p256dh, auth: &auth_b64 };
        let body = encrypt(&sub, br#"{"title":"hi"}"#, [3u8; 32], [5u8; 16]).unwrap();
        assert_eq!(&body[16..20], &4096u32.to_be_bytes(), "record size");
        assert_eq!(body[20], 65, "the sender's key id is its uncompressed point");
        assert_eq!(decrypt(&secret, &auth, &body).unwrap(), br#"{"title":"hi"}"#);
        let mut tampered = body.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt(&secret, &auth, &tampered).is_err());
        assert!(decrypt(&secret, &[1u8; 16], &body).is_err(), "another auth secret");
        assert!(encrypt(&sub, &[0u8; PAYLOAD_MAX_BYTES + 1], [3u8; 32], [5u8; 16]).is_err());
        let bad = Subscription { endpoint: "x", p256dh: "nope", auth: &auth_b64 };
        assert!(encrypt(&bad, b"x", [3u8; 32], [5u8; 16]).is_err());
    }

    #[test]
    fn vapid_tokens_verify() {
        let vapid = Vapid::from_bytes([11u8; 32]).unwrap();
        let auth = vapid.authorization("https://fcm.googleapis.com/fcm/send/xyz", "mailto:ops@fragment.invalid", 1_790_000_000).unwrap();
        let (t, k) = auth.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(k, vapid.public_key());
        let (signing_input, sig) = t.rsplit_once('.').unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&B64URL.decode(signing_input.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_790_000_000 + 12 * 3600);
        let key = VerifyingKey::from_sec1_bytes(&B64URL.decode(k).unwrap()).unwrap();
        let sig = Signature::from_slice(&B64URL.decode(sig).unwrap()).unwrap();
        assert!(key.verify(signing_input.as_bytes(), &sig).is_ok());
        assert_eq!(Vapid::from_bytes(vapid.to_bytes()).unwrap().public_key(), vapid.public_key());
    }
}
