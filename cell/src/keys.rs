//! The deployment's keys, held as Worker secrets (docs/secrets.md): the
//! host secret that seals values at rest, the code.storage org key,
//! WorkOS's API key, OpenRouter's management key, and the operator's keys
//! a computer's swap sends (`FRAGMENT_KEY_<NAME>`). Only the platform
//! Worker's env holds them. An app runs in an isolate of its own from the
//! Worker Loader, with an env the platform builds (`js::app_env`), so no
//! author code can name one. A value sealed here names the Durable Object
//! that sealed it (its class and id: `scope`), so it opens only there.

use std::cell::RefCell;

use fragment_core::codestorage::{Claims, OrgKey};
use fragment_core::seal::{self, SealError};
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, State};

use crate::error::{CellError, CellResult};
use crate::js;

/// The secrets' names (`wrangler secret put`; `.dev.vars` in dev).
pub const HOST_SECRET: &str = "FRAGMENT_HOST_SECRET";
/// The host secret before a rotation: values it sealed still open, and are
/// resealed under the current one.
pub const HOST_SECRET_PREVIOUS: &str = "FRAGMENT_HOST_SECRET_PREVIOUS";
pub const CODESTORAGE_PRIVATE_KEY: &str = "CODESTORAGE_PRIVATE_KEY";
pub const WORKOS_API_KEY: &str = "WORKOS_API_KEY";
pub const OPENROUTER_MANAGEMENT_KEY: &str = "OPENROUTER_MANAGEMENT_KEY";

/// The longest code.storage token signed, as for an editor's storage token.
const JWT_TTL_MAX_S: i64 = 900;
const CODESTORAGE_SCOPES: [&str; 4] = ["git:read", "git:write", "repo:write", "org:read"];

fn secret(env: &Env, name: &str) -> Option<String> {
    env.secret(name).ok().map(|s| s.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

/// The Durable Object `class` whose state is `state`, as a sealed value
/// names it.
pub fn scope(class: &str, state: &State) -> String {
    format!("{class}:{}", state.id())
}

fn host_secrets(env: &Env) -> Vec<String> {
    [HOST_SECRET, HOST_SECRET_PREVIOUS].iter().filter_map(|n| secret(env, n)).collect()
}

fn sealing(e: SealError) -> CellError {
    match e {
        SealError::Malformed | SealError::TooLarge(_) => CellError::invalid(e.to_string()),
        SealError::NoHostSecret | SealError::WeakHostSecret | SealError::UnknownKey(_) | SealError::Corrupt => CellError::host(e.to_string()),
    }
}

/// `plaintext`, sealed for `scope`.
pub fn seal(env: &Env, scope: &str, plaintext: &[u8]) -> CellResult<String> {
    let hosts = host_secrets(env);
    let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
    seal::seal(&hosts, scope, plaintext, js::random_bytes()).map_err(sealing)
}

pub struct Opened {
    pub plaintext: Vec<u8>,
    /// The value sealed again under the current host secret (it was sealed
    /// under a previous one): store it.
    pub resealed: Option<String>,
}

/// Opens a value sealed for `scope`.
pub fn open(env: &Env, scope: &str, sealed: &str) -> CellResult<Opened> {
    let hosts = host_secrets(env);
    let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let opened = seal::open(&hosts, scope, sealed).map_err(sealing)?;
    let resealed = if opened.stale { Some(seal::seal(&hosts, scope, &opened.plaintext, js::random_bytes()).map_err(sealing)?) } else { None };
    Ok(Opened { plaintext: opened.plaintext, resealed })
}

/// A new nostr key: (public key hex, its secret sealed for `scope`).
pub fn nostr_keypair(env: &Env, scope: &str) -> CellResult<(String, String)> {
    let keys = loop {
        // a 32-byte string outside the curve's order is astronomically rare; draw again
        if let Some(k) = fragment_nip98::Keys::from_secret_hex(&js::random_hex::<32>()) {
            break k;
        }
    };
    Ok((keys.pubkey_hex().to_string(), seal(env, scope, keys.secret_hex().as_bytes())?))
}

thread_local! {
    /// The org key, parsed once per isolate (its PEM is a secret that does
    /// not change while the isolate lives).
    static ORG_KEY: RefCell<Option<OrgKey>> = const { RefCell::new(None) };
}

/// A code.storage JWT for the configured org: (token, expiry in ms).
pub fn codestorage_token(env: &Env, org: &str, repo: &str, sub: &str, scopes: &[&str], ttl_s: i64) -> CellResult<(String, i64)> {
    assert!((1..=JWT_TTL_MAX_S).contains(&ttl_s), "a code.storage token lives 1..={JWT_TTL_MAX_S} s, not {ttl_s}");
    assert!(!scopes.is_empty() && scopes.iter().all(|s| CODESTORAGE_SCOPES.contains(s)), "scopes are some of {CODESTORAGE_SCOPES:?}");
    let iat = js::now_ms() / 1000;
    let claims = Claims { iss: org, sub, repo, scopes, iat, exp: iat + ttl_s };
    let token = ORG_KEY.with(|k| {
        let mut k = k.borrow_mut();
        if k.is_none() {
            let pem = secret(env, CODESTORAGE_PRIVATE_KEY).ok_or_else(|| CellError::host(format!("{CODESTORAGE_PRIVATE_KEY} is not set")))?;
            *k = Some(OrgKey::from_pem(&pem).map_err(|e| CellError::host(format!("{CODESTORAGE_PRIVATE_KEY}: {e}")))?);
        }
        Ok::<_, CellError>(k.as_ref().expect("set above").token(&claims))
    })?;
    Ok((token, (iat + ttl_s) * 1000))
}

/// POSTs `body` as JSON with a bearer key: (status, JSON answer or null).
/// Not reaching the host is `UpstreamFailed`.
async fn post_json(url: &str, method: Method, bearer: Option<&str>, body: Option<&Value>, host: &str) -> CellResult<(u16, Value)> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    if let Some(key) = bearer {
        headers.set("authorization", &format!("Bearer {key}"))?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(b) = body {
        init.with_body(Some(b.to_string().into()));
    }
    let req = Request::new_with_init(url, &init)?;
    let mut resp = Fetch::Request(req).send().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("{host} did not answer: {e}")))?;
    let status = resp.status_code();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// WorkOS's code exchange with the API key added: (status, WorkOS's
/// answer, its refresh token dropped: the platform keeps its own session).
pub async fn workos_authenticate(env: &Env, api: &str, client_id: &str, code: &str) -> CellResult<(u16, Value)> {
    let key = secret(env, WORKOS_API_KEY).ok_or_else(|| CellError::host(format!("{WORKOS_API_KEY} is not set")))?;
    let payload = json!({ "client_id": client_id, "client_secret": key, "grant_type": "authorization_code", "code": code });
    let (status, mut answer) = post_json(&format!("{api}/user_management/authenticate"), Method::Post, None, Some(&payload), "WorkOS").await?;
    if let Some(o) = answer.as_object_mut() {
        o.remove("refresh_token");
    }
    Ok((status, answer))
}

/// A WorkOS Pipes access token for `user`'s account at `provider`
/// (`POST /data-integrations/{provider}/token`): (status, WorkOS's answer,
/// `{active, access_token: {access_token, expires_at, …}}` or `{active:
/// false, error}`).
pub async fn pipes_token(env: &Env, api: &str, provider: &str, user: &str) -> CellResult<(u16, Value)> {
    assert!(fragment_core::swap::valid_name(provider), "a provider is checked before WorkOS is asked");
    let key = secret(env, WORKOS_API_KEY).ok_or_else(|| CellError::host(format!("{WORKOS_API_KEY} is not set")))?;
    post_json(&format!("{api}/data-integrations/{provider}/token"), Method::Post, Some(&key), Some(&json!({ "user_id": user })), "WorkOS").await
}

/// The operator's key `name` (`swap::key_secret_name`), when the
/// deployment holds it.
pub fn operator_key(env: &Env, name: &str) -> Option<String> {
    secret(env, &fragment_core::swap::key_secret_name(name))
}

/// OpenRouter's key API with the management key: (status, OpenRouter's
/// answer), or `None` when no management key is set (the deployment pays
/// for no AI).
pub async fn openrouter_keys(env: &Env, api: &str, method: Method, hash: Option<&str>, body: Option<&Value>) -> CellResult<Option<(u16, Value)>> {
    let Some(key) = secret(env, OPENROUTER_MANAGEMENT_KEY) else { return Ok(None) };
    let path = match hash {
        None => "keys".to_string(),
        Some(h) => {
            assert!(!h.is_empty() && h.len() <= 128 && h.bytes().all(|b| b.is_ascii_alphanumeric()), "a key's hash is letters and digits");
            format!("keys/{h}")
        }
    };
    post_json(&format!("{api}/api/v1/{path}"), method, Some(&key), body, "OpenRouter").await.map(Some)
}
