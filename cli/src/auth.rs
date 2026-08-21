// Nostr keys and NIP-98 HTTP auth (kind 27235), matching runtime/src/auth.js.
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use secp256k1::{Keypair, Message, Secp256k1, XOnlyPublicKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub struct Identity {
    pub secret: [u8; 32],
    pub pubkey_hex: String,
}

impl Identity {
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (sk, _) = secp.generate_keypair(&mut rand::thread_rng());
        Self::from_secret(sk.secret_bytes())
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, &secret).expect("valid secret key");
        let (xonly, _) = XOnlyPublicKey::from_keypair(&kp);
        Self {
            secret,
            pubkey_hex: hex::encode(xonly.serialize()),
        }
    }

    pub fn npub(&self) -> String {
        npub_encode(&self.pubkey_hex).expect("npub encode")
    }

    /// Build the Authorization header value for a NIP-98 request.
    pub fn nip98_header(&self, method: &str, url: &str, body: &[u8]) -> String {
        let mut tags = vec![
            serde_json::json!(["u", url]),
            serde_json::json!(["method", method.to_uppercase()]),
        ];
        if !body.is_empty() {
            let hash = hex::encode(Sha256::digest(body));
            tags.push(serde_json::json!(["payload", hash]));
        }
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // NIP-01 id: sha256 of the canonical serialization of
        // [0, pubkey, created_at, kind, tags, content]
        let preimage = serde_json::to_string(&serde_json::json!([
            0,
            self.pubkey_hex,
            created_at,
            27235,
            tags,
            ""
        ]))
        .unwrap();
        let id = Sha256::digest(preimage.as_bytes());
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, &self.secret).unwrap();
        let msg = Message::from_digest_slice(&id).unwrap();
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp);
        let event = serde_json::json!({
            "id": hex::encode(id),
            "pubkey": self.pubkey_hex,
            "created_at": created_at,
            "kind": 27235,
            "tags": tags,
            "content": "",
            "sig": hex::encode(sig.as_ref()),
        });
        let b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(&event).unwrap());
        format!("Nostr {b64}")
    }
}

pub fn npub_encode(pubkey_hex: &str) -> Result<String> {
    use bech32::ToBase32;
    let bytes = hex::decode(pubkey_hex).context("bad pubkey hex")?;
    bech32::encode("npub", bytes.to_base32(), bech32::Variant::Bech32).map_err(|e| anyhow!(e.to_string()))
}

#[allow(dead_code)] // public helper; used by tests and future grant-validation
pub fn npub_decode(npub: &str) -> Result<String> {
    use bech32::FromBase32;
    let (hrp, data, variant) = bech32::decode(npub).context("bad npub")?;
    if hrp != "npub" {
        return Err(anyhow!("not an npub"));
    }
    if variant != bech32::Variant::Bech32 {
        return Err(anyhow!("npub must be bech32 (not bech32m)"));
    }
    let bytes = Vec::<u8>::from_base32(&data).context("bad npub data")?;
    Ok(hex::encode(bytes))
}

/// Resolve an identifier to its canonical npub form.
///
/// Plain npubs are validated and canonicalised. NIP-05 names (`local@domain`)
/// resolve via the standard well-known path
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
        let hex = if published.starts_with("npub1") {
            npub_decode(published)?
        } else {
            published.to_ascii_lowercase()
        };
        return npub_encode(&hex).with_context(|| format!("'{s}' published an invalid pubkey"));
    }
    let hex = npub_decode(s)?;
    npub_encode(&hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npub_roundtrip() {
        let id = Identity::from_secret([3u8; 32]);
        let npub = id.npub();
        assert!(npub.starts_with("npub1"));
        assert_eq!(npub_decode(&npub).unwrap(), id.pubkey_hex);
    }

    #[test]
    // Cross-implementation pin: value computed via @noble/curves + JS bech32.
    fn npub_pinned() {
        let id = Identity::from_secret([1u8; 32]);
        assert_eq!(
            id.pubkey_hex,
            "1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f"
        );
        assert_eq!(
            id.npub(),
            "npub1rwzv24nmzfjypx2a8m264ws9vht3uxp5vpypnluuzl67n4waq78suk0wul"
        );
    }

    #[test]
    fn nip98_shape() {
        let id = Identity::from_secret([7u8; 32]);
        let h = id.nip98_header("POST", "http://x/api/fragments", br#"{"name":"a"}"#);
        let b64 = h.strip_prefix("Nostr ").unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let ev: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(ev["kind"], 27235);
        assert_eq!(ev["content"], "");
        assert_eq!(ev["tags"][0], serde_json::json!(["u", "http://x/api/fragments"]));
        assert_eq!(ev["tags"][1], serde_json::json!(["method", "POST"]));
        let expect_payload = hex::encode(Sha256::digest(br#"{"name":"a"}"#));
        assert_eq!(ev["tags"][2], serde_json::json!(["payload", expect_payload]));
        // id recompute
        let pre = serde_json::to_string(&serde_json::json!([
            0,
            ev["pubkey"],
            ev["created_at"],
            27235,
            ev["tags"],
            ""
        ]))
        .unwrap();
        assert_eq!(ev["id"].as_str().unwrap(), hex::encode(Sha256::digest(pre.as_bytes())));
        // signature verifies
        let secp = Secp256k1::new();
        let msg = Message::from_digest_slice(&hex::decode(ev["id"].as_str().unwrap()).unwrap()).unwrap();
        let sig = secp256k1::schnorr::Signature::from_slice(&hex::decode(ev["sig"].as_str().unwrap()).unwrap()).unwrap();
        let pk = XOnlyPublicKey::from_slice(&hex::decode(ev["pubkey"].as_str().unwrap()).unwrap()).unwrap();
        secp.verify_schnorr(&sig, &msg, &pk).unwrap();
    }

    #[test]
    fn nip98_no_payload_tag_when_empty() {
        let id = Identity::from_secret([9u8; 32]);
        let h = id.nip98_header("GET", "http://x/api/fragments", b"");
        let b64 = h.strip_prefix("Nostr ").unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let ev: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(ev["tags"].as_array().unwrap().len(), 2);
    }
}
