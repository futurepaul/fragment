//! The node's `KEYS` service (`crates/native`, docs/hardening.md H1). The
//! agent's nostr key is made there and signs there: the agent holds it
//! only sealed, and only this agent cell can have it used. A computer's
//! token is sealed for this cell alone too.

use anyhow::{anyhow, Context};
use base64::Engine;
use serde_json::{json, Value};
use worker::Env;

use crate::js;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

async fn call(env: &Env, route: &str, body: &Value) -> anyhow::Result<Value> {
    let (status, text) = js::service_post(env.as_ref(), "KEYS", &format!("https://keys/{route}"), &body.to_string()).await?;
    let answer: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    if status != 200 {
        return Err(anyhow!("KEYS {route} ({status}): {}", answer["error"].as_str().map(str::to_string).unwrap_or_else(|| answer.to_string())));
    }
    Ok(answer)
}

fn field(answer: &Value, name: &str) -> anyhow::Result<String> {
    answer[name].as_str().map(str::to_string).with_context(|| format!("KEYS answered no {name}"))
}

pub async fn seal(env: &Env, plaintext: &[u8]) -> anyhow::Result<String> {
    field(&call(env, "seal", &json!({ "plaintext": b64().encode(plaintext) })).await?, "sealed")
}

pub struct Opened {
    pub plaintext: Vec<u8>,
    /// Sealed again under the current host secret: store it.
    pub resealed: Option<String>,
}

/// Opens a value sealed for this cell (`legacy_salt`: what the agent
/// sealed it with itself, before `KEYS`).
pub async fn open(env: &Env, sealed: &str, legacy_salt: &str) -> anyhow::Result<Opened> {
    let answer = call(env, "open", &json!({ "sealed": sealed, "legacySalt": legacy_salt })).await?;
    let plaintext = b64().decode(field(&answer, "plaintext")?).context("KEYS open answered no base64")?;
    Ok(Opened { plaintext, resealed: answer["resealed"].as_str().map(str::to_string) })
}

/// A new nostr key: (public key hex, its secret sealed for this cell).
pub async fn nostr_keypair(env: &Env) -> anyhow::Result<(String, String)> {
    let answer = call(env, "nostr/keypair", &json!({})).await?;
    Ok((field(&answer, "pubkey")?, field(&answer, "sealed")?))
}

/// What the agent's key signs.
pub enum Sign<'a> {
    /// A NIP-98 `Authorization` value; `payload` is the body's SHA-256 hex.
    Header { method: &'a str, url: &'a str, payload: Option<String> },
}

pub struct Signed {
    pub header: String,
    pub resealed: Option<String>,
}

pub async fn nostr_sign(env: &Env, sealed: &str, legacy_salt: &str, what: Sign<'_>) -> anyhow::Result<Signed> {
    let created_at = js::now_ms() / 1000;
    let mut body = json!({ "sealed": sealed, "legacySalt": legacy_salt, "createdAt": created_at });
    match what {
        Sign::Header { method, url, payload } => {
            body["kind"] = json!("header");
            body["method"] = json!(method);
            body["url"] = json!(url);
            body["payload"] = json!(payload);
        }
    }
    let answer = call(env, "nostr/sign", &body).await?;
    Ok(Signed { header: field(&answer, "header")?, resealed: answer["resealed"].as_str().map(str::to_string) })
}
