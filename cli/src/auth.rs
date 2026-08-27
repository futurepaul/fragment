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
        self.nostr_header(27235, tags, "", created_at)
    }

    /// Shared signing core: kind + tags + content -> NIP-01 id over the
    /// canonical serialization of [0, pubkey, created_at, kind, tags, content],
    /// schnorr-signed, wrapped as the `Authorization: Nostr <b64>` value.
    fn nostr_header(&self, kind: u64, tags: Vec<Value>, content: &str, created_at: u64) -> String {
        let preimage =
            serde_json::to_string(&serde_json::json!([0, self.pubkey_hex, created_at, kind, tags, content]))
                .expect("event preimage serializes");
        let id = Sha256::digest(preimage.as_bytes());
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, &self.secret).expect("valid secret key");
        let msg = Message::from_digest_slice(&id).expect("32-byte digest");
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp);
        let event = serde_json::json!({
            "id": hex::encode(id),
            "pubkey": self.pubkey_hex,
            "created_at": created_at,
            "kind": kind,
            "tags": tags,
            "content": content,
            "sig": hex::encode(sig.as_ref()),
        });
        let b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(&event).unwrap());
        format!("Nostr {b64}")
    }

    /// Blossom auth header (kind 24242) for the blob tier per
    /// docs/blob-tier.md: `t` = action verb, `x` = the server's public URL
    /// (this deployment pins the instance in x, not the content hash),
    /// optional `payload` = sha256 of the body being authorized, required
    /// `expiration` unix seconds. Used for blobsd upload/delete/list.
    pub fn blossom_header(&self, action: &str, server_url: &str, payload_hash: &str, expires_secs: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.blossom_header_at(action, server_url, payload_hash, expires_secs, now)
    }

    /// Deterministic variant of [`blossom_header`] for cross-implementation
    /// vector tests (node scripts/nip98.mjs buildEvent must agree byte-for-byte).
    pub(crate) fn blossom_header_at(
        &self,
        action: &str,
        server_url: &str,
        payload_hash: &str,
        expires_secs: u64,
        created_at: u64,
    ) -> String {
        let mut tags = vec![
            serde_json::json!(["t", action]),
            serde_json::json!(["x", server_url]),
        ];
        if !payload_hash.is_empty() {
            tags.push(serde_json::json!(["payload", payload_hash]));
        }
        tags.push(serde_json::json!(["expiration", (created_at + expires_secs).to_string()]));
        self.nostr_header(24242, tags, "", created_at)
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

    #[test]
    // Cross-implementation pin: `buildEvent` in scripts/nip98.mjs generated
    // this vector with created_at pinned to 1787800000 and the same inputs.
    // The Rust signer must derive the identical event id, and node's
    // signature must verify under this identity's pubkey (BIP340 is
    // deterministic in the key+message, not the signer).
    fn blossom_vector_pinned_vs_node() {
        const NODE_B64: &str = "eyJwdWJrZXkiOiI5ODljMGI3NmNiNTYzOTcxZmRjOWJlZjMxZWMwNmMzNTYwZjMyNDlkNmVlOWU1ZDgzYzU3NjI1NTk2ZTA1ZjZmIiwiY3JlYXRlZF9hdCI6MTc4NzgwMDAwMCwia2luZCI6MjQyNDIsInRhZ3MiOltbInQiLCJ1cGxvYWQiXSxbIngiLCJodHRwOi8vMTI3LjAuMC4xOjk5NDEiXSxbInBheWxvYWQiLCI1NGUyZTAxZjYyZTlmZjMyMGVkZDQ1N2YwNjQ2MzNjZDFmMTZkZmFlNmI0ZGMzYmY3M2U0Y2RjNzZlZmUyZDc5Il0sWyJleHBpcmF0aW9uIiwiMTc4NzgwMDYwMCJdXSwiY29udGVudCI6IiIsImlkIjoiNjE3ODcyYjExYjc5MjNjMzhjZTcyMzA0ODRjN2NiMTMwYjI3OGY0NGFkNjIyMGJjYTY4MjUzYjYwMzkzNTFiMSIsInNpZyI6IjU0OTlkMmYwZWUyYmI4ZGI1MGU1MzBjNDk2YWFmM2ExMjY3YWJkZjVhMTU2MjY3NDdhZjU4NzRkZTA1MGE3ZjA2ODhjODhjYWU2MzM3ZWU3ZTYwYTllZGIwMDhhYzc0YmRiNmVmZjMxNDE4YzJiMWEwNGQzNmZmYzYwZmNhOTRhIn0=";
        let id = Identity::from_secret([7u8; 32]);
        let mine = id.blossom_header_at(
            "upload",
            "http://127.0.0.1:9941",
            "54e2e01f62e9ff320edd457f064633cd1f16dfae6b4dc3bf73e4cdc76efe2d79",
            600,
            1_787_800_000,
        );
        let std64 = base64::engine::general_purpose::STANDARD;
        let node_ev: Value =
            serde_json::from_slice(&std64.decode(NODE_B64).unwrap()).unwrap();
        let my_ev: Value =
            serde_json::from_slice(&std64.decode(mine.strip_prefix("Nostr ").unwrap()).unwrap())
                .unwrap();
        assert_eq!(node_ev["kind"], 24242);
        assert_eq!(node_ev["tags"], my_ev["tags"], "tag shape/order must match node");
        assert_eq!(my_ev["id"], node_ev["id"], "same inputs must yield the same NIP-01 id");
        // the node-produced signature verifies under this identity's pubkey
        let secp = Secp256k1::new();
        let msg = Message::from_digest_slice(&hex::decode(node_ev["id"].as_str().unwrap()).unwrap()).unwrap();
        let sig = secp256k1::schnorr::Signature::from_slice(&hex::decode(node_ev["sig"].as_str().unwrap()).unwrap()).unwrap();
        let pk = XOnlyPublicKey::from_slice(&hex::decode(node_ev["pubkey"].as_str().unwrap()).unwrap()).unwrap();
        secp.verify_schnorr(&sig, &msg, &pk).expect("node signature verifies");
    }
}
