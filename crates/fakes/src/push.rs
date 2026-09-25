//! A web push service, and a receiver for `notifyUrls` (phase 2 slice F).
//! Subscriptions are made here as a browser would make them (a P-256 key
//! and an auth secret per endpoint); a push is accepted only with a valid
//! VAPID token for this service's origin, and decrypted as the browser
//! would (RFC 8291), so tests read what the page would have shown.
//! Levers: an endpoint that is gone (410), that fails a few times (503),
//! or that is slow to answer. Each push is recorded with when it landed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use serde_json::Value;
use sha2::Sha256;

use crate::http::{Handler, Request, Response, Server};

struct Sub {
    secret: SecretKey,
    auth: Vec<u8>,
    received: Vec<Value>,
    /// When each push in `received` landed.
    landed: Vec<Instant>,
    gone: bool,
    failures: u32,
    /// How long the endpoint waits before it answers each push.
    delay: Duration,
}

#[derive(Default)]
struct State {
    subs: HashMap<String, Sub>,
    notified: Vec<Value>,
    refused: Vec<String>,
}

pub struct PushService {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

/// What a page sends to `__push-sub`.
pub struct Subscribed {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

fn vapid_ok(header: &str, origin: &str) -> Result<(), String> {
    let (t, k) = header.strip_prefix("vapid t=").and_then(|r| r.split_once(", k=")).ok_or("not a vapid header")?;
    let (signing_input, sig) = t.rsplit_once('.').ok_or("not a JWT")?;
    let claims: Value = serde_json::from_slice(&B64URL.decode(signing_input.split('.').nth(1).ok_or("no claims")?).map_err(|_| "claims")?).map_err(|_| "claims")?;
    if claims["aud"] != origin {
        return Err(format!("aud {} is not {origin}", claims["aud"]));
    }
    let key = VerifyingKey::from_sec1_bytes(&B64URL.decode(k).map_err(|_| "k")?).map_err(|_| "k is not a P-256 key")?;
    let sig = Signature::from_slice(&B64URL.decode(sig).map_err(|_| "sig")?).map_err(|_| "sig")?;
    key.verify(signing_input.as_bytes(), &sig).map_err(|_| "the signature does not verify".to_string())
}

/// A browser's side of RFC 8291: one push's body decrypted with the
/// subscription's key and auth secret. Written apart from the cell's
/// `encrypt` (fragment_core::webpush), so the two halves meet only in the
/// wire format, and each is checked against the RFC's section 5 example:
/// a label both got wrong would still fail against a real browser.
pub fn decrypt(ua_secret: &SecretKey, auth: &[u8], body: &[u8]) -> Result<Vec<u8>, String> {
    // the aes128gcm header (RFC 8188 section 2.1): salt, record size, and
    // the sender's key as the key id
    let (salt, rest) = body.split_at_checked(16).ok_or("too short for an aes128gcm header")?;
    let (&id_len, rest) = rest.get(4..).and_then(<[u8]>::split_first).ok_or("too short for an aes128gcm header")?;
    let (as_public, sealed) = rest.split_at_checked(usize::from(id_len)).ok_or("the key id overruns the body")?;
    let sender = PublicKey::from_sec1_bytes(as_public).map_err(|_| "the key id is not a P-256 point")?;
    let ecdh = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), sender.as_affine());
    // RFC 8291 section 3.4: the auth secret and both public keys make the
    // input key; the salt makes the content key and nonce from it (RFC
    // 8188 section 2.2)
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_secret.public_key().to_encoded_point(false).as_bytes());
    key_info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), ecdh.raw_secret_bytes()).expand(&key_info, &mut ikm).expect("32 bytes is one HKDF block");
    let content = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    content.expand(b"Content-Encoding: aes128gcm\0", &mut cek).expect("16 bytes is one HKDF block");
    content.expand(b"Content-Encoding: nonce\0", &mut nonce).expect("12 bytes is one HKDF block");
    let mut record = Aes128Gcm::new_from_slice(&cek)
        .expect("a 16-byte key")
        .decrypt(&Nonce::from(nonce), sealed)
        .map_err(|_| "the record does not decrypt")?;
    // the last record ends with the delimiter 0x02, then any zero padding
    match record.iter().rposition(|b| *b != 0) {
        Some(i) if record[i] == 0x02 => {
            record.truncate(i);
            Ok(record)
        }
        _ => Err("no last-record delimiter".into()),
    }
}

impl PushService {
    pub fn start() -> std::io::Result<PushService> {
        let state: Arc<Mutex<State>> = Arc::default();
        let st = Arc::clone(&state);
        let origin = Arc::new(Mutex::new(String::new()));
        let origin_in = Arc::clone(&origin);
        let handler: Handler = Arc::new(move |req: &Request| {
            // a slow endpoint waits without holding the others up
            let delay = req.path.strip_prefix("/push/").and_then(|id| st.lock().expect("push state").subs.get(id).map(|sub| sub.delay));
            std::thread::sleep(delay.unwrap_or_default());
            let mut s = st.lock().expect("push state");
            if req.path == "/notify" {
                s.notified.push(serde_json::from_slice(&req.body).unwrap_or(Value::Null));
                return Response::json(200, &serde_json::json!({ "ok": true }));
            }
            let Some(id) = req.path.strip_prefix("/push/") else { return Response::json(404, &serde_json::json!({})) };
            let origin = origin_in.lock().expect("origin").clone();
            if let Err(why) = vapid_ok(req.header("authorization").unwrap_or(""), &origin) {
                s.refused.push(why);
                return Response::json(403, &serde_json::json!({ "error": "vapid" }));
            }
            if req.header("content-encoding") != Some("aes128gcm") {
                s.refused.push("content-encoding".into());
                return Response::json(400, &serde_json::json!({}));
            }
            let Some(sub) = s.subs.get_mut(id) else { return Response::json(404, &serde_json::json!({})) };
            if sub.gone {
                return Response::json(410, &serde_json::json!({}));
            }
            if sub.failures > 0 {
                sub.failures -= 1;
                return Response::json(503, &serde_json::json!({}));
            }
            match decrypt(&sub.secret, &sub.auth, &req.body) {
                Ok(plain) => {
                    sub.received.push(serde_json::from_slice(&plain).unwrap_or(Value::Null));
                    sub.landed.push(Instant::now());
                    Response::json(201, &serde_json::json!({}))
                }
                Err(why) => {
                    s.refused.push(why);
                    Response::json(400, &serde_json::json!({}))
                }
            }
        });
        let server = Server::start(0, handler)?;
        *origin.lock().expect("origin") = server.url.clone();
        Ok(PushService { url: server.url.clone(), state, _server: server })
    }

    /// A browser's subscription at this service, under `id`.
    pub fn subscribe(&self, id: &str, seed: u8) -> Subscribed {
        let secret = SecretKey::from_slice(&[seed.max(1); 32]).expect("a valid scalar");
        let auth = vec![seed; 16];
        let p256dh = B64URL.encode(secret.public_key().to_encoded_point(false).as_bytes());
        let sub = Sub { secret, auth: auth.clone(), received: vec![], landed: vec![], gone: false, failures: 0, delay: Duration::ZERO };
        self.state.lock().expect("push state").subs.insert(id.to_string(), sub);
        Subscribed { endpoint: format!("{}/push/{id}", self.url), p256dh, auth: B64URL.encode(auth) }
    }

    pub fn received(&self, id: &str) -> Vec<Value> {
        self.state.lock().expect("push state").subs.get(id).map(|s| s.received.clone()).unwrap_or_default()
    }

    /// The endpoint answers 410 from now on (the browser unsubscribed).
    pub fn forget(&self, id: &str) {
        if let Some(s) = self.state.lock().expect("push state").subs.get_mut(id) {
            s.gone = true;
        }
    }

    /// The endpoint answers 503 this many times first.
    pub fn fail(&self, id: &str, times: u32) {
        if let Some(s) = self.state.lock().expect("push state").subs.get_mut(id) {
            s.failures = times;
        }
    }

    /// The endpoint waits this long before it answers each push.
    pub fn slow(&self, id: &str, delay: Duration) {
        if let Some(s) = self.state.lock().expect("push state").subs.get_mut(id) {
            s.delay = delay;
        }
    }

    /// When each push the endpoint took landed, in order.
    pub fn landed(&self, id: &str) -> Vec<Instant> {
        self.state.lock().expect("push state").subs.get(id).map(|s| s.landed.clone()).unwrap_or_default()
    }

    pub fn notified(&self) -> Vec<Value> {
        self.state.lock().expect("push state").notified.clone()
    }

    /// Pushes refused, and why (a bad token, a bad record).
    pub fn refused(&self) -> Vec<String> {
        self.state.lock().expect("push state").refused.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8291 section 5's example, base64url as the RFC prints it (its
    /// line breaks removed): the receiver's key and auth secret, and the
    /// body a push service receives. fragment_core::webpush encrypts the
    /// same plaintext to the same body.
    const RFC_RECEIVER_SECRET: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const RFC_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const RFC_BODY: &str = concat!(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
        "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
        "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
    );

    /// Goal: the fake browser reads a push as RFC 8291 says a browser
    /// does, so the push lane holds the cell to the real format. Method:
    /// section 5's body decrypts to its plaintext with its receiver's key;
    /// a flipped byte, another auth secret, or a cut body is refused.
    #[test]
    fn the_rfc_8291_example_decrypts_to_its_plaintext() {
        let secret = SecretKey::from_slice(&B64URL.decode(RFC_RECEIVER_SECRET).unwrap()).unwrap();
        let (auth, body) = (B64URL.decode(RFC_AUTH).unwrap(), B64URL.decode(RFC_BODY).unwrap());
        assert_eq!(decrypt(&secret, &auth, &body).unwrap(), b"When I grow up, I want to be a watermelon");
        let mut tampered = body.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt(&secret, &auth, &tampered).is_err(), "a flipped byte");
        assert!(decrypt(&secret, &[1u8; 16], &body).is_err(), "another auth secret");
        for cut in [0, 20, 21, 85] {
            assert!(decrypt(&secret, &auth, &body[..cut]).is_err(), "cut at {cut}");
        }
    }
}
