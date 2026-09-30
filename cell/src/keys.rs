//! The node's `KEYS` service (`crates/native`, docs/hardening.md H1): the
//! fleet's keys stay in the node, and the cell asks for what it needs done
//! with them. The service knows which cell is asking (the host attests it),
//! so a value sealed here opens only for this cell, a code.storage token is
//! signed only for a `Fragment`, WorkOS's exchange only for the `Registry`,
//! OpenRouter's key API only for a `Ledger`, a Sprite only for its `Computer`,
//! and the platform's sandcastle grant only for a `Hermes`.

use base64::Engine;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::Env;

use crate::error::{CellError, CellResult};
use crate::js;

/// The binding (wrangler.jsonc `services`: `native:keys`).
const BINDING: &str = "KEYS";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// (status, answer) of one call.
async fn call_raw(env: &Env, route: &str, body: &Value) -> CellResult<(u16, Value)> {
    let (status, text) = js::service_post(env.as_ref(), BINDING, &format!("https://keys/{route}"), &body.to_string()).await?;
    let answer = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok((status, answer))
}

fn refused(route: &str, status: u16, answer: &Value) -> CellError {
    let why = answer["error"].as_str().map(str::to_string).unwrap_or_else(|| answer.to_string());
    // 502: the host KEYS called for us did not answer
    let code = if status == 502 { ErrorCode::UpstreamFailed } else { ErrorCode::HostFailed };
    CellError::new(code, format!("KEYS {route} ({status}): {why}"))
}

/// One call; a refusal is the service's own words.
async fn call(env: &Env, route: &str, body: &Value) -> CellResult<Value> {
    match call_raw(env, route, body).await? {
        (200, answer) => Ok(answer),
        (status, answer) => Err(refused(route, status, &answer)),
    }
}

fn field(answer: &Value, name: &str, route: &str) -> CellResult<String> {
    answer[name].as_str().map(str::to_string).ok_or_else(|| CellError::host(format!("KEYS {route} answered no {name}")))
}

/// `plaintext`, sealed for this cell.
pub async fn seal(env: &Env, plaintext: &[u8]) -> CellResult<String> {
    field(&call(env, "seal", &json!({ "plaintext": b64().encode(plaintext) })).await?, "sealed", "seal")
}

pub struct Opened {
    pub plaintext: Vec<u8>,
    /// The value sealed again under the current host secret (it was sealed
    /// under a previous one, or by the cell itself before `KEYS`): store it.
    pub resealed: Option<String>,
}

/// Opens a value sealed for this cell. `legacy_salt` is the salt a value
/// the cell sealed itself before `KEYS` was sealed with (its npub, or its
/// org); such a value always comes back resealed.
pub async fn open(env: &Env, sealed: &str, legacy_salt: &str) -> CellResult<Opened> {
    let answer = call(env, "open", &json!({ "sealed": sealed, "legacySalt": legacy_salt })).await?;
    let plaintext = b64().decode(field(&answer, "plaintext", "open")?).map_err(|_| CellError::host("KEYS open answered no base64"))?;
    Ok(Opened { plaintext, resealed: answer["resealed"].as_str().map(str::to_string) })
}

/// A new nostr key made by the service: (public key hex, its secret sealed
/// for this cell). The secret never reaches the cell.
pub async fn nostr_keypair(env: &Env) -> CellResult<(String, String)> {
    let answer = call(env, "nostr/keypair", &json!({})).await?;
    Ok((field(&answer, "pubkey", "nostr/keypair")?, field(&answer, "sealed", "nostr/keypair")?))
}

/// A NIP-98 `Authorization` header for `method url` with `body`, signed by
/// the nostr key sealed for this cell, with a nonce (a sandcastle node's
/// replay cache keys on the event id): (header, the key resealed when it
/// was stale: store it).
pub async fn nostr_sign(env: &Env, sealed: &str, method: &str, url: &str, body: &[u8]) -> CellResult<(String, Option<String>)> {
    let payload = (!body.is_empty()).then(|| {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(body))
    });
    let ask = json!({ "sealed": sealed, "legacySalt": "", "kind": "header", "method": method, "url": url, "payload": payload, "createdAt": js::now_ms() / 1000, "nonce": true });
    let answer = call(env, "nostr/sign", &ask).await?;
    Ok((field(&answer, "header", "nostr/sign")?, answer["resealed"].as_str().map(str::to_string)))
}

/// An admission (docs/runtime-seam.md) signed with a sealed key: `peer` may
/// reach `computer` on `node` until `expires_at`. (The event's JSON, the key
/// resealed if `KEYS` did.)
pub async fn nostr_admission(env: &Env, sealed: &str, peer: &str, computer: &str, node: &str, created_at: i64, expires_at: i64) -> CellResult<(String, Option<String>)> {
    let ask = json!({ "sealed": sealed, "legacySalt": "", "kind": "admission", "peer": peer, "computer": computer, "node": node, "createdAt": created_at, "expiresAt": expires_at });
    let answer = call(env, "nostr/sign", &ask).await?;
    Ok((field(&answer, "admission", "nostr/sign")?, answer["resealed"].as_str().map(str::to_string)))
}

/// The platform's grant of a computer on the fleet's sandcastle node to
/// this cell's key (a `Hermes` only): (the node's status, its answer).
pub async fn sandcastle_grant(env: &Env, pubkey: &str, grant: &Value) -> CellResult<(u16, Value)> {
    let answer = call(env, "sandcastle/grant", &json!({ "pubkey": pubkey, "grant": grant })).await?;
    Ok((answer["status"].as_u64().unwrap_or(0) as u16, answer["body"].clone()))
}

/// A code.storage JWT (a `Fragment` only): (token, expiry in ms).
pub async fn codestorage_token(env: &Env, repo: &str, sub: &str, scopes: &[&str], ttl_s: i64) -> CellResult<(String, i64)> {
    let answer = call(env, "codestorage/token", &json!({ "repo": repo, "sub": sub, "scopes": scopes, "ttlS": ttl_s })).await?;
    let expires = answer["expiresAt"].as_i64().ok_or_else(|| CellError::host("KEYS codestorage/token answered no expiresAt"))?;
    Ok((field(&answer, "token", "codestorage/token")?, expires))
}

/// WorkOS's code exchange (the `Registry` only): (status, WorkOS's answer).
pub async fn workos_authenticate(env: &Env, client_id: &str, code: &str) -> CellResult<(u16, Value)> {
    let answer = call(env, "workos/authenticate", &json!({ "clientId": client_id, "code": code })).await?;
    Ok((answer["status"].as_u64().unwrap_or(0) as u16, answer["body"].clone()))
}

/// OpenRouter's key API (a `Ledger` only): (status, OpenRouter's answer),
/// or `None` when the node holds no management key (the fleet pays for no AI).
pub async fn openrouter_keys(env: &Env, method: &str, hash: Option<&str>, body: Option<&Value>) -> CellResult<Option<(u16, Value)>> {
    let route = "openrouter/keys";
    match call_raw(env, route, &json!({ "method": method, "hash": hash, "body": body })).await? {
        (200, answer) => Ok(Some((answer["status"].as_u64().unwrap_or(0) as u16, answer["body"].clone()))),
        (503, _) => Ok(None),
        (status, answer) => Err(refused(route, status, &answer)),
    }
}

/// The Sprites API on this `Computer` cell's own Sprite: `op` is `create`,
/// `get`, `delete`, or `exec` (`cmd` its argv, `stdin` its input), and the
/// answer is (Sprites' status, its text).
pub async fn sprites(env: &Env, op: &str, cmd: &[&str], stdin: &str) -> CellResult<(u16, String)> {
    let answer = call(env, "sprites", &json!({ "op": op, "cmd": cmd, "stdin": stdin })).await?;
    Ok((answer["status"].as_u64().unwrap_or(0) as u16, answer["body"].as_str().unwrap_or_default().to_string()))
}
