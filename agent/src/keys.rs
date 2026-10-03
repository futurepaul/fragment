//! The agent's nostr key, sealed at rest under the deployment's host
//! secret (a Worker secret, `FRAGMENT_HOST_SECRET`; `crates/core/src/seal.rs`)
//! for this agent's own Durable Object: only this agent cell opens it, and
//! only to sign.

use anyhow::{anyhow, Context};
use fragment_core::seal;
use worker::{Env, State};

use crate::js;

const HOST_SECRET: &str = "FRAGMENT_HOST_SECRET";
const HOST_SECRET_PREVIOUS: &str = "FRAGMENT_HOST_SECRET_PREVIOUS";

fn host_secrets(env: &Env) -> Vec<String> {
    [HOST_SECRET, HOST_SECRET_PREVIOUS]
        .iter()
        .filter_map(|n| env.secret(n).ok().map(|s| s.to_string().trim().to_string()).filter(|s| !s.is_empty()))
        .collect()
}

/// What a value this agent seals names.
pub fn scope(state: &State) -> String {
    format!("Agent:{}", state.id())
}

/// A new nostr key: (public key hex, its secret sealed for `scope`).
pub fn nostr_keypair(env: &Env, scope: &str) -> anyhow::Result<(String, String)> {
    let keys = loop {
        // a 32-byte string outside the curve's order is astronomically rare; draw again
        if let Some(k) = fragment_nip98::Keys::from_secret_hex(&hex::encode(js::random_bytes::<32>())) {
            break k;
        }
    };
    let hosts = host_secrets(env);
    let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let sealed = seal::seal(&hosts, scope, keys.secret_hex().as_bytes(), js::random_bytes()).map_err(|e| anyhow!("sealing the agent's key: {e}"))?;
    Ok((keys.pubkey_hex().to_string(), sealed))
}

/// What the agent's key signs.
pub enum Sign<'a> {
    /// A NIP-98 `Authorization` value; `payload` is the body's SHA-256 hex.
    Header { method: &'a str, url: &'a str, payload: Option<String> },
}

pub struct Signed {
    pub header: String,
    /// The key sealed again under the current host secret: store it.
    pub resealed: Option<String>,
}

pub fn nostr_sign(env: &Env, scope: &str, sealed: &str, what: Sign<'_>) -> anyhow::Result<Signed> {
    let hosts = host_secrets(env);
    let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let opened = seal::open(&hosts, scope, sealed).map_err(|e| anyhow!("the agent's key: {e}"))?;
    let secret = std::str::from_utf8(&opened.plaintext).context("the agent's key is not text")?;
    let keys = fragment_nip98::Keys::from_secret_hex(secret).context("the agent's sealed key is not a nostr key")?;
    let resealed = if opened.stale { Some(seal::seal(&hosts, scope, &opened.plaintext, js::random_bytes()).map_err(|e| anyhow!("resealing the agent's key: {e}"))?) } else { None };
    let created_at = (js::now_ms() / 1000) as i64;
    let header = match what {
        Sign::Header { method, url, payload } => keys.header_for_payload(method, url, payload.as_deref(), created_at),
    };
    Ok(Signed { header, resealed })
}
