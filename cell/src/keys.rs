//! The deployment's keys, held in its Cloudflare Secrets Store and bound to
//! the platform Worker by name (docs/secrets.md; the bindings' names are
//! `fragment_core::secrets_store`'s): the host secret that seals values at
//! rest, the code.storage org key, WorkOS's client id and API key, and the
//! operator's keys a computer's swap sends; and what is derived from the
//! host secret: the key placeholders' tags are made with (`tag_keys`).
//! This file is the one place the cell reads them (`secret`), through a
//! per-isolate cache that holds a value at most a minute
//! (`secrets_store::CACHE_MS_MAX`), so a value set again in the store is in
//! use everywhere within a minute, with no deploy.
//!
//! Only the platform Worker's env holds the bindings. An app runs in an
//! isolate of its own from the Worker Loader, with an env the platform
//! builds (`js::app_env`), so no author code can name one. A value sealed
//! here names the Durable Object that sealed it (its class and id:
//! `scope`), so it opens only there.

use std::cell::RefCell;

use fragment_core::codestorage::{Claims, OrgKey};
use fragment_core::seal::{self, SealError};
use fragment_core::secrets_store::{self as store, Cache};
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, State};

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;

/// The longest code.storage token signed, as for an editor's storage token.
const JWT_TTL_MAX_S: i64 = 900;
const CODESTORAGE_SCOPES: [&str; 4] = ["git:read", "git:write", "repo:write", "org:read"];

thread_local! {
    /// What each binding read last (`secrets_store::Cache`: a minute at most).
    static CACHE: RefCell<Cache> = const { RefCell::new(Cache::new()) };
    /// The org key, parsed, and the PEM it was parsed from: parsed again
    /// only when the store's value changes.
    static ORG_KEY: RefCell<Option<(String, OrgKey)>> = const { RefCell::new(None) };
}

/// Whether the deployment binds `binding` (a deploy binds what its config
/// names; dev and the e2e, what devstack seeds). Anything else under that
/// name, a Worker secret an earlier deploy left among them, is no binding.
pub fn bound(env: &Env, binding: &str) -> bool {
    env.secret_store(binding).is_ok()
}

/// The secret bound as `binding`, read through the cache; `None` when
/// nothing is bound there. A binding whose secret the store does not hold,
/// or holds empty, is an error: the deploy checks every one exists first.
async fn secret(env: &Env, binding: &str) -> CellResult<Option<String>> {
    let now_ms = js::now_ms();
    if let Some(value) = CACHE.with(|c| c.borrow().fresh(binding, now_ms).map(str::to_string)) {
        return Ok(Some(value));
    }
    let Ok(bound) = env.secret_store(binding) else { return Ok(None) };
    let read = bound.get().await.map_err(|e| CellError::host(format!("the secret bound as {binding} could not be read from the store: {e}")))?;
    let value = read.map(|v| v.trim().to_string()).unwrap_or_default();
    if value.is_empty() {
        return Err(CellError::host(format!("the secret bound as {binding} is missing from the store, or empty")));
    }
    CACHE.with(|c| c.borrow_mut().put(binding, value.clone(), now_ms));
    Ok(Some(value))
}

/// The secret bound as `binding`, which this fleet must have.
async fn required(env: &Env, binding: &str) -> CellResult<String> {
    secret(env, binding).await?.ok_or_else(|| CellError::host(format!("no secret is bound as {binding} (cargo xtask deploy binds it from the config)")))
}

/// The Durable Object `class` whose state is `state`, as a sealed value
/// names it.
pub fn scope(class: &str, state: &State) -> String {
    format!("{class}:{}", state.id())
}

/// The host secrets, the current one first: the previous one after it
/// while a rotation runs. None at all when no host secret is bound (which
/// `seal` refuses).
async fn host_secrets(env: &Env) -> CellResult<Vec<String>> {
    let Some(current) = secret(env, store::HOST_SECRET).await? else { return Ok(vec![]) };
    let mut hosts = vec![current];
    if let Some(previous) = secret(env, store::HOST_SECRET_PREVIOUS).await? {
        hosts.push(previous);
    }
    assert!((1..=2).contains(&hosts.len()), "the current host secret, and at most one before it");
    Ok(hosts)
}

fn sealing(e: SealError) -> CellError {
    match e {
        SealError::Malformed | SealError::TooLarge(_) => CellError::invalid(e.to_string()),
        SealError::NoHostSecret | SealError::WeakHostSecret | SealError::UnknownKey(_) | SealError::Corrupt => CellError::host(e.to_string()),
    }
}

/// `plaintext`, sealed for `scope`.
pub async fn seal(env: &Env, scope: &str, plaintext: &[u8]) -> CellResult<String> {
    let hosts = host_secrets(env).await?;
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
pub async fn open(env: &Env, scope: &str, sealed: &str) -> CellResult<Opened> {
    let hosts = host_secrets(env).await?;
    let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let opened = seal::open(&hosts, scope, sealed).map_err(sealing)?;
    let resealed = if opened.stale { Some(seal::seal(&hosts, scope, &opened.plaintext, js::random_bytes()).map_err(sealing)?) } else { None };
    Ok(Opened { plaintext: opened.plaintext, resealed })
}

/// A new nostr key: (public key hex, its secret sealed for `scope`).
pub async fn nostr_keypair(env: &Env, scope: &str) -> CellResult<(String, String)> {
    let keys = loop {
        // a 32-byte string outside the curve's order is astronomically rare; draw again
        if let Some(k) = fragment_nip98::Keys::from_secret_hex(&js::random_hex::<32>()) {
            break k;
        }
    };
    Ok((keys.pubkey_hex().to_string(), seal(env, scope, keys.secret_hex().as_bytes()).await?))
}

/// A code.storage JWT for the configured org: (token, expiry in ms).
pub async fn codestorage_token(env: &Env, org: &str, repo: &str, sub: &str, scopes: &[&str], ttl_s: i64) -> CellResult<(String, i64)> {
    assert!((1..=JWT_TTL_MAX_S).contains(&ttl_s), "a code.storage token lives 1..={JWT_TTL_MAX_S} s, not {ttl_s}");
    assert!(!scopes.is_empty() && scopes.iter().all(|s| CODESTORAGE_SCOPES.contains(s)), "scopes are some of {CODESTORAGE_SCOPES:?}");
    let pem = required(env, store::CODESTORAGE_KEY).await?;
    let iat = js::now_ms() / 1000;
    let claims = Claims { iss: org, sub, repo, scopes, iat, exp: iat + ttl_s };
    let token = ORG_KEY.with(|k| {
        let mut k = k.borrow_mut();
        let parsed = matches!(&*k, Some((from, _)) if *from == pem);
        if !parsed {
            let key = OrgKey::from_pem(&pem).map_err(|e| CellError::host(format!("{}: {e}", store::CODESTORAGE_KEY)))?;
            *k = Some((pem.clone(), key));
        }
        Ok::<_, CellError>(k.as_ref().expect("parsed above").1.token(&claims))
    })?;
    Ok((token, (iat + ttl_s) * 1000))
}

/// The WorkOS environment this fleet signs people in with: its client id
/// (bound as `WORKOS_CLIENT`) and its API's base (`WORKOS_API_URL`).
pub struct WorkOs<'a> {
    pub client_id: String,
    pub api: &'a str,
}

impl WorkOs<'_> {
    /// Who vouches for a person's subject: this environment. A person is
    /// keyed by `(issuer, subject)`, so finite.computer's login (another
    /// environment) is another issuer (docs/finite-integration.md).
    pub fn issuer(&self) -> String {
        format!("workos:{}", self.client_id)
    }
}

/// The fleet's WorkOS environment, when sign-in is configured (`cfg.workos`).
pub async fn workos<'a>(env: &Env, cfg: &'a Config) -> CellResult<WorkOs<'a>> {
    let api = &cfg.workos()?.api;
    let client_id = required(env, store::WORKOS_CLIENT).await?;
    Ok(WorkOs { client_id, api })
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
    let key = required(env, store::WORKOS_KEY).await?;
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
/// false, error}`). Asked only to swap a token in: it may refresh one.
pub async fn pipes_token(env: &Env, api: &str, provider: &str, user: &str) -> CellResult<(u16, Value)> {
    assert!(fragment_core::catalog::valid_name(provider), "a provider is checked before WorkOS is asked");
    let key = required(env, store::WORKOS_KEY).await?;
    post_json(&format!("{api}/data-integrations/{provider}/token"), Method::Post, Some(&key), Some(&json!({ "user_id": user })), "WorkOS").await
}

/// A connection's state, without a token (WorkOS Pipes' connected account,
/// `GET /user_management/users/{user}/connected_accounts/{provider}`):
/// what Pipes says of `user`'s account at `provider`, never minting or
/// refreshing a token. 404 is no account.
pub async fn pipes_state(env: &Env, api: &str, provider: &str, user: &str) -> CellResult<fragment_proto::computer::ProviderState> {
    use fragment_proto::computer::ProviderState;
    assert!(fragment_core::catalog::valid_name(provider), "a provider is checked before WorkOS is asked");
    // it goes in a path: a user id is WorkOS' token (`user_01…`), nothing else
    if user.is_empty() || !user.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(CellError::host(format!("a WorkOS user id that is no token: {user:?}")));
    }
    let key = required(env, store::WORKOS_KEY).await?;
    let (status, answer) = post_json(&format!("{api}/user_management/users/{user}/connected_accounts/{provider}"), Method::Get, Some(&key), None, "WorkOS").await?;
    match (status, answer["state"].as_str()) {
        (200, Some("connected")) => Ok(ProviderState::Connected),
        (200, Some("needs_reauthorization")) => Ok(ProviderState::NeedsReauthorization),
        (200, _) | (404, _) => Ok(ProviderState::NotConnected),
        (s, _) => Err(CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS did not say whether {provider} is connected ({s}): {}", answer["message"].as_str().unwrap_or("no reason given")))),
    }
}

/// A WorkOS Pipes consent URL for `user` to connect `provider`
/// (`POST /data-integrations/{provider}/authorize`, spike S5): (status,
/// WorkOS's answer, `{url, state}`).
pub async fn pipes_authorize(env: &Env, api: &str, provider: &str, user: &str) -> CellResult<(u16, Value)> {
    assert!(fragment_core::catalog::valid_name(provider), "a provider is checked before WorkOS is asked");
    let key = required(env, store::WORKOS_KEY).await?;
    post_json(&format!("{api}/data-integrations/{provider}/authorize"), Method::Post, Some(&key), Some(&json!({ "user_id": user })), "WorkOS").await
}

/// Whether the deployment binds the operator's key `name`
/// (`secrets_store::operator_key`), without reading it.
pub fn holds_operator_key(env: &Env, name: &str) -> bool {
    bound(env, &store::operator_key(name))
}

/// The operator's key `name` (`secrets_store::operator_key`), which the
/// deployment must bind.
pub async fn operator_key(env: &Env, name: &str) -> CellResult<String> {
    required(env, &store::operator_key(name)).await
}

/// The keys placeholders' tags are made with (`swap::TagKey`), derived
/// from the host secret: the current one's first, then the previous one's
/// while a rotation is under way (a guest reads its placeholders again
/// within seconds). Only the platform holds them.
pub async fn tag_keys(env: &Env) -> CellResult<Vec<fragment_core::swap::TagKey>> {
    let hosts = host_secrets(env).await?;
    if hosts.is_empty() {
        return Err(sealing(SealError::NoHostSecret));
    }
    if hosts.iter().any(|h| h.len() < fragment_core::seal::HOST_SECRET_MIN_BYTES) {
        return Err(sealing(SealError::WeakHostSecret));
    }
    Ok(hosts.iter().map(|h| fragment_core::swap::TagKey::derive(h)).collect())
}

/// The self-hosted model upstream's key (bound as `MODEL_KEY`), when it
/// takes one (docs/self-host.md, seam 3).
pub async fn model_key(env: &Env) -> CellResult<Option<String>> {
    secret(env, store::MODEL_KEY).await
}
