//! The agent's nostr key, sealed at rest under the deployment's host
//! secret (`crates/core/src/seal.rs`) for this agent's own Durable Object:
//! only this agent cell opens it, and only to sign.
//!
//! The host secret is a Secrets Store secret bound to this Worker
//! (`secrets_store::HOST_SECRET`, and `HOST_SECRET_PREVIOUS` while a
//! rotation runs; docs/secrets.md), the same store secrets the platform
//! Worker is bound to. This file is the one place the agent reads them
//! (`secret`), through a per-isolate cache that holds a value at most a
//! minute (`secrets_store::CACHE_MS_MAX`).

use std::cell::RefCell;

use anyhow::{anyhow, Context};
use fragment_core::seal;
use fragment_core::secrets_store::{self as store, Cache};
use worker::{Env, State};

use crate::js;

thread_local! {
    /// What each binding read last (`secrets_store::Cache`: a minute at most).
    static CACHE: RefCell<Cache> = const { RefCell::new(Cache::new()) };
}

/// The secret bound as `binding`, read through the cache; `None` when
/// nothing is bound there. A binding whose secret the store does not hold,
/// or holds empty, is an error: the deploy checks every one exists first.
async fn secret(env: &Env, binding: &str) -> anyhow::Result<Option<String>> {
    let now_ms = i64::try_from(js::now_ms()).expect("the clock reads milliseconds since 1970");
    if let Some(value) = CACHE.with(|c| c.borrow().fresh(binding, now_ms).map(str::to_string)) {
        return Ok(Some(value));
    }
    let Ok(bound) = env.secret_store(binding) else { return Ok(None) };
    let read = bound.get().await.map_err(|e| anyhow!("the secret bound as {binding} could not be read from the store: {e}"))?;
    let value = read.map(|v| v.trim().to_string()).unwrap_or_default();
    if value.is_empty() {
        return Err(anyhow!("the secret bound as {binding} is missing from the store, or empty"));
    }
    CACHE.with(|c| c.borrow_mut().put(binding, value.clone(), now_ms));
    Ok(Some(value))
}

/// The host secrets, the current one first: the previous one after it
/// while a rotation runs. None at all when no host secret is bound (which
/// sealing refuses).
async fn host_secrets(env: &Env) -> anyhow::Result<Vec<String>> {
    let Some(current) = secret(env, store::HOST_SECRET).await? else { return Ok(vec![]) };
    let mut hosts = vec![current];
    if let Some(previous) = secret(env, store::HOST_SECRET_PREVIOUS).await? {
        hosts.push(previous);
    }
    assert!((1..=2).contains(&hosts.len()), "the current host secret, and at most one before it");
    Ok(hosts)
}

/// What a value this agent seals names.
pub fn scope(state: &State) -> String {
    format!("Agent:{}", state.id())
}

/// A new nostr key: (public key hex, its secret sealed for `scope`).
pub async fn nostr_keypair(env: &Env, scope: &str) -> anyhow::Result<(String, String)> {
    let keys = loop {
        // a 32-byte string outside the curve's order is astronomically rare; draw again
        if let Some(k) = fragment_nip98::Keys::from_secret_hex(&hex::encode(js::random_bytes::<32>())) {
            break k;
        }
    };
    let hosts = host_secrets(env).await?;
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

pub async fn nostr_sign(env: &Env, scope: &str, sealed: &str, what: Sign<'_>) -> anyhow::Result<Signed> {
    let hosts = host_secrets(env).await?;
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
