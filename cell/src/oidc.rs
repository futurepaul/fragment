//! Sign-in on OpenID Connect (docs/self-host.md, seam 4): the provider's
//! metadata and keys, fetched and kept per isolate, and an id_token
//! verified by them. The rules are `fragment_core::oidc`'s; the router
//! sends the browser to the provider (auth.rs), and the registry exchanges
//! the code with the client's secret from keys.rs (`registry/signin.rs`).
//!
//! The metadata and the JWKS are caches, so their contract:
//! - Source: the provider's `<issuer>/.well-known/openid-configuration`
//!   and the `jwks_uri` it names, over the runtime's TLS (a provider whose
//!   certificate is from a private CA needs the runtime to trust that CA:
//!   docs/self-host.md, seam 4).
//! - Invalidation: each is fetched again once `METADATA_TTL_MS` old; the
//!   JWKS also when an id_token names a key it lacks (a rotation), at most
//!   once a `JWKS_REFETCH_MIN_MS`. A new isolate starts with neither.
//! - Stale reads: a key the provider withdrew is trusted until the hour
//!   ends. Only the provider's own token endpoint hands the cell an
//!   id_token (over TLS, for a code and verifier only this sign-in holds),
//!   so a withdrawn key would also need that endpoint to sign with it.

use std::cell::RefCell;
use std::rc::Rc;

use fragment_core::oidc::{self, Expect, IdTokenError, Jwks, Provider, Verified};
use fragment_proto::ErrorCode;
use worker::{Fetch, Headers, Method, Request, RequestInit};

use crate::config::OidcConfig;
use crate::error::{CellError, CellResult};
use crate::js;

thread_local! {
    /// The provider's metadata, and when it was fetched (ms).
    static PROVIDER: RefCell<Option<(Rc<Provider>, i64)>> = const { RefCell::new(None) };
    /// Its signing keys, and when they were fetched (ms).
    static KEYS: RefCell<Option<(Rc<Jwks>, i64)>> = const { RefCell::new(None) };
}

fn unreachable(what: &str, e: impl std::fmt::Display) -> CellError {
    CellError::new(ErrorCode::UpstreamFailed, format!("the sign-in provider's {what} did not answer: {e}"))
}

/// A GET of the provider's JSON (its metadata, its keys): the body, at
/// most `METADATA_MAX_BYTES`.
async fn get(url: &str, what: &str) -> CellResult<Vec<u8>> {
    let headers = Headers::new();
    headers.set("accept", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let req = Request::new_with_init(url, &init)?;
    let mut resp = Fetch::Request(req).send().await.map_err(|e| unreachable(what, e))?;
    if resp.status_code() != 200 {
        return Err(CellError::new(ErrorCode::UpstreamFailed, format!("the sign-in provider's {what} answered {} ({url})", resp.status_code())));
    }
    let bytes = resp.bytes().await.map_err(|e| unreachable(what, e))?;
    if bytes.len() > oidc::METADATA_MAX_BYTES {
        return Err(CellError::new(ErrorCode::UpstreamFailed, format!("the sign-in provider's {what} is over {} bytes", oidc::METADATA_MAX_BYTES)));
    }
    Ok(bytes)
}

fn misbehaved(e: oidc::OidcError) -> CellError {
    CellError::new(ErrorCode::UpstreamFailed, e.to_string())
}

/// The provider's metadata: the isolate's copy while it is fresh, else
/// fetched (`Provider::parse` checks it).
pub async fn provider(cfg: &OidcConfig) -> CellResult<Rc<Provider>> {
    let now = js::now_ms();
    let kept = PROVIDER.with(|c| c.borrow().as_ref().filter(|(_, at)| !oidc::stale(*at, now)).map(|(p, _)| Rc::clone(p)));
    if let Some(p) = kept {
        return Ok(p);
    }
    let bytes = get(&oidc::discovery_url(&cfg.issuer), "metadata").await?;
    let p = Rc::new(Provider::parse(&cfg.issuer, &bytes).map_err(misbehaved)?);
    PROVIDER.with(|c| *c.borrow_mut() = Some((Rc::clone(&p), now)));
    Ok(p)
}

/// The provider's keys, fetched now.
async fn fetch_keys(provider: &Provider, now: i64) -> CellResult<Rc<Jwks>> {
    let bytes = get(&provider.jwks_uri, "JWKS").await?;
    let keys = Rc::new(Jwks::parse(&bytes).map_err(misbehaved)?);
    KEYS.with(|c| *c.borrow_mut() = Some((Rc::clone(&keys), now)));
    Ok(keys)
}

/// An id_token from the provider's token endpoint, verified for this
/// sign-in (its `nonce`). A key the isolate's JWKS lacks fetches the JWKS
/// again, once, unless the last fetch was within `JWKS_REFETCH_MIN_MS` (the
/// provider rotated);
/// nothing else is tried again. A refusal is 401, logged with why: an
/// id_token that fails is a provider misconfigured (another client's
/// audience, a skewed clock) or one not to trust.
pub async fn verify(cfg: &OidcConfig, provider: &Provider, token: &str, nonce: &str) -> CellResult<Verified> {
    let now = js::now_ms();
    let expect = Expect { issuer: &cfg.issuer, client_id: &cfg.client_id, nonce, now_s: now / 1000, algs: &provider.algs };
    let kept = KEYS.with(|c| c.borrow().as_ref().filter(|(_, at)| !oidc::stale(*at, now)).map(|(k, at)| (Rc::clone(k), *at)));
    let (keys, fetched_at) = match kept {
        Some(k) => k,
        None => (fetch_keys(provider, now).await?, now),
    };
    let verified = match oidc::verify(token, &keys, &expect) {
        Err(IdTokenError::UnknownKey(_)) if oidc::may_refetch(fetched_at, now) => oidc::verify(token, &*fetch_keys(provider, now).await?, &expect),
        verified => verified,
    };
    verified.map_err(|e| {
        worker::console_error!("{}", serde_json::json!({ "event": "signin.refused", "issuer": cfg.issuer, "why": e.to_string() }));
        CellError::new(ErrorCode::Unauthenticated, format!("sign-in refused the provider's id_token: {e}"))
    })
}
