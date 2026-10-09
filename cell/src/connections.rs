//! A person's providers (decisions 22 and 37, docs/computers.md): every
//! provider of the deployment's catalog (`FRAGMENT_PROVIDERS`), and the
//! state of the person's credential at each. Their agents use them
//! through their computer's swap (computer.rs).
//!
//! - `GET /api/connections` → `Providers`: each provider, its kind, hosts,
//!   environment variables and (an operator key's) price, and its state: a
//!   connection `connected`, `needs_reauthorization` or `not_connected`,
//!   as WorkOS Pipes says of the account (its connected account: no token
//!   is minted to tell); an operator key `offered`; an own key `set` or
//!   `not_set`. Reading it tells the person's computer the connections'
//!   states, so a guest learns of a new connection at its next read.
//! - `POST /api/connections/{provider}/authorize` → `{url}`: where the
//!   person's browser goes to connect a connection (Pipes' consent, then
//!   the provider's).
//! - `PUT /api/connections/{provider}/key {key}` and `DELETE …/key`: the
//!   person's own key for an `own` provider, kept sealed by their computer
//!   (one computer per person for now: decision 13), which swaps it in.
//! - An own key whose row has a sign-in (`oauth`) is connected from
//!   settings instead, never pasted (Paul, 2026-10-08;
//!   `fragment_core::own_signin`): `POST …/authorize` begins it (their
//!   computer keeps its nonce and verifier) and answers the provider's
//!   page, and `GET /api/connections/{provider}/callback?code=&state=`,
//!   where the provider sends their browser back, exchanges the code for
//!   their key and has their computer seal it. Disconnecting is `DELETE
//!   …/key`; the key itself is revoked at the provider (its `manage` page).

use std::collections::BTreeMap;

use fragment_core::catalog::{Kind, KeyOAuth};
use fragment_core::own_signin;
use fragment_proto::computer::{ProviderModel, ProviderPrice, ProviderSignIn, ProviderState, ProviderView, Providers};
use fragment_proto::{ErrorCode, IdentityKind};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::json_response;
use crate::registry::calls::SubjectOf;

/// An own key, at most (a provider's API key is far shorter).
pub use fragment_core::own_signin::OWN_KEY_MAX_BYTES;

/// Whether `key` is a key the swap may send: a printable token.
pub fn own_key_ok(key: &str) -> bool {
    (1..=OWN_KEY_MAX_BYTES).contains(&key.len()) && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// The WorkOS user the person signed in as, which their connections are.
pub async fn workos_user(env: &Env, cfg: &Config, who: &str) -> CellResult<Option<String>> {
    let issuer = crate::keys::workos(env, cfg).await?.issuer();
    Ok(crate::ask_registry(env, &SubjectOf { identity: who.into(), issuer }).await?.subject)
}

/// Each connection's state for a person (as Pipes says, with no token),
/// whose WorkOS user `user` finds (`workos_user`, or the one their computer
/// keeps), asked only when the catalog has a connection.
pub async fn connection_states(env: &Env, cfg: &Config, user: impl std::future::Future<Output = CellResult<Option<String>>>) -> CellResult<BTreeMap<String, ProviderState>> {
    let connections: Vec<&str> = cfg.providers.providers().iter().filter(|p| p.kind == Kind::Connection).map(|p| p.name.as_str()).collect();
    let mut states = BTreeMap::new();
    if connections.is_empty() {
        return Ok(states);
    }
    let user = user.await?;
    let api = &cfg.workos()?.api;
    // at most `catalog::PROVIDERS_MAX` providers, one question each
    for provider in connections {
        let state = match &user {
            Some(u) => crate::keys::pipes_state(env, api, provider, u).await?,
            None => ProviderState::NotConnected,
        };
        states.insert(provider.to_string(), state);
    }
    Ok(states)
}

pub(crate) async fn route(env: &Env, who: &str, kind: IdentityKind, method: Method, rest: &[&str], body: &[u8]) -> CellResult<Response> {
    if kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "connections are a person's: their agents use them through their computer"));
    }
    let cfg = Config::from_env(env);
    let offered = || cfg.providers.providers().iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    let provider_of = |name: &str| cfg.providers.get(name).ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no provider {name:?} here (this deployment offers {:?})", offered())));
    let computer = fragment_core::computer::default_computer_of(who);
    match (method.clone(), rest) {
        (Method::Get, []) => {
            let states = connection_states(env, cfg, workos_user(env, cfg, who)).await?;
            // the person's computer keeps them, for its guest's next read; one
            // not made yet has nothing to tell, and so no own key either
            let own: Vec<String> = match crate::computer::ask(env, &computer, "computer/own-keys", &json!({ "connections": states })).await {
                Ok(v) => serde_json::from_value(v["providers"].clone()).map_err(|e| CellError::host(format!("the computer's own keys: {e}")))?,
                Err(e) if e.code == ErrorCode::NotFound => vec![],
                Err(e) => return Err(e),
            };
            let providers = cfg
                .providers
                .providers()
                .iter()
                .map(|p| ProviderView {
                    provider: p.name.clone(),
                    kind: p.kind,
                    state: match p.kind {
                        Kind::Connection => states.get(&p.name).copied().unwrap_or(ProviderState::NotConnected),
                        Kind::Operator => ProviderState::Offered,
                        Kind::Own if own.contains(&p.name) => ProviderState::Set,
                        Kind::Own => ProviderState::NotSet,
                    },
                    hosts: p.hosts.clone(),
                    env: p.env.clone(),
                    price: p.price.map(|pr| ProviderPrice { micros: pr.micros, per: pr.per }),
                    sign_in: p.oauth.as_ref().map(|o| ProviderSignIn { manage: o.manage.clone() }),
                    models: p.models.as_ref().map(|m| m.offer.iter().map(|o| ProviderModel { id: o.id.clone(), name: o.name.clone() }).collect()),
                })
                .collect();
            json_response(&Providers { providers })
        }
        (Method::Post, [provider, "authorize"]) => {
            let p = provider_of(provider)?;
            if let (Kind::Own, Some(o)) = (p.kind, &p.oauth) {
                return sign_in(env, cfg, &computer, provider, o).await;
            }
            if p.kind != Kind::Connection {
                return Err(CellError::invalid(format!("{provider} is no connection: it is a key, which is not authorized through WorkOS")));
            }
            let user = workos_user(env, cfg, who).await?.ok_or_else(|| CellError::new(ErrorCode::NotConnected, "connections are a signed-in person's: sign in through the platform first"))?;
            let (status, answer) = crate::keys::pipes_authorize(env, &cfg.workos()?.api, provider, &user).await?;
            // https only, but for the local fakes of a local fleet (a
            // preview's levers do not make its real WorkOS's http good)
            let url = answer["url"].as_str().filter(|u| status == 200 && (u.starts_with("https://") || (cfg.egress_local && u.starts_with("http://"))));
            let url = url.ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS gave no consent URL for {provider} ({status})")))?;
            json_response(&json!({ "provider": provider, "url": url }))
        }
        (Method::Put | Method::Delete, [provider, "key"]) => {
            if provider_of(provider)?.kind != Kind::Own {
                return Err(CellError::invalid(format!("{provider} takes no key of yours: only an own key's provider does")));
            }
            let key = if method == Method::Put {
                let v: Value = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                let key = v["key"].as_str().map(str::trim).unwrap_or_default().to_string();
                if !own_key_ok(&key) {
                    return Err(CellError::invalid(format!("a key is 1 to {OWN_KEY_MAX_BYTES} printable characters, with no space")));
                }
                Some(key)
            } else {
                None
            };
            match crate::computer::ask(env, &computer, "computer/own-key", &json!({ "provider": provider, "key": key })).await {
                Ok(_) => json_response(&json!({ "provider": provider, "state": if key.is_some() { ProviderState::Set } else { ProviderState::NotSet } })),
                Err(e) if e.code == ErrorCode::NotFound => Err(CellError::new(ErrorCode::NotFound, "your own keys are kept by your computer: make it first (POST /api/computers)")),
                Err(e) => Err(e),
            }
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/connections/{}", m.as_ref(), rest.join("/")))),
    }
}

/// Whether the cell may send a person's browser, or a code, to `url`:
/// https, or a local fleet's fake where its egress may reach local
/// addresses (the catalog takes loopback http only for them).
fn reachable(cfg: &Config, url: &str) -> CellResult<()> {
    if url.starts_with("https://") || cfg.egress_local {
        return Ok(());
    }
    Err(CellError::new(ErrorCode::HostFailed, format!("{url} is a local fleet's, and this one's egress may not reach local addresses")))
}

/// Where the provider sends the person's browser back.
fn callback_url(cfg: &Config, provider: &str) -> String {
    format!("{}/api/connections/{provider}/callback", cfg.platform())
}

/// A sign-in to `provider`'s own key, begun: the person's computer keeps
/// its nonce and verifier; answers the provider's page for their browser.
async fn sign_in(env: &Env, cfg: &Config, computer: &str, provider: &str, o: &KeyOAuth) -> CellResult<Response> {
    reachable(cfg, &o.authorize)?;
    let begun = match crate::computer::ask(env, computer, "computer/sign-in", &json!({ "provider": provider })).await {
        Ok(v) => v,
        Err(e) if e.code == ErrorCode::NotFound => return Err(CellError::new(ErrorCode::NotFound, "your own keys are kept by your computer: make it first (POST /api/computers)")),
        Err(e) => return Err(e),
    };
    let (nonce, challenge) = (begun["nonce"].as_str().unwrap_or_default(), begun["challenge"].as_str().unwrap_or_default());
    if !own_signin::valid_token(nonce) || !own_signin::valid_token(challenge) {
        return Err(CellError::host("the computer began a sign-in with no nonce"));
    }
    let url = own_signin::authorize_url(o, &callback_url(cfg, provider), challenge, &own_signin::state(computer, nonce));
    json_response(&json!({ "provider": provider, "url": url }))
}

/// `GET /api/connections/{provider}/callback?code=&state=` on the
/// platform's host: the person's browser, back from the provider's page.
/// The state names their computer and the sign-in it began, once, so no
/// session is needed: its key goes to that computer alone. The code is
/// exchanged for their key, which their computer seals; a page says so.
/// A refusal at the provider (the person declined: no code) or here is a
/// page that says what to do; nothing is set.
pub(crate) async fn callback(env: &Env, url: &Url, provider: &str) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    if !cfg.is_platform_host(url.host_str().unwrap_or_default()) {
        return Err(CellError::new(ErrorCode::NotFound, "a sign-in comes back to the platform's own host"));
    }
    let failed = |why: &str| crate::auth::page(400, "Not connected", &format!("<p>{}</p><p><a href=\"/settings\">Back to settings</a></p>", crate::auth::esc(why)));
    let Some(p) = cfg.providers.get(provider).filter(|p| p.kind == Kind::Own) else { return failed("This platform offers no such key.") };
    let Some(o) = &p.oauth else { return failed("That key is given in settings, not signed in to.") };
    let query = |k: &str| crate::auth::query(url, k);
    let Some((computer, nonce)) = query("state").as_deref().and_then(own_signin::parse_state) else { return failed("This is not a sign-in this platform began: connect again from settings.") };
    let Some(code) = query("code").filter(|c| own_signin::valid_code(c)) else {
        // nothing exchanged, and the sign-in under way is let go
        let _ = crate::computer::ask(env, &computer, "computer/signed-in", &json!({ "provider": provider, "nonce": nonce })).await;
        return failed("The provider sent no key back (declined, or it failed): connect again from settings if you meant to.");
    };
    let verifier = match crate::computer::ask(env, &computer, "computer/signed-in", &json!({ "provider": provider, "nonce": nonce })).await {
        Ok(v) => v["verifier"].as_str().unwrap_or_default().to_string(),
        Err(e) if matches!(e.code, ErrorCode::NotFound | ErrorCode::InvalidRequest) => return failed(&e.message),
        Err(e) => return Err(e),
    };
    reachable(cfg, &o.exchange)?;
    let host = url::Url::parse(&o.exchange).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let (status, answer) = crate::keys::own_key_exchange(&o.exchange, &own_signin::exchange_body(&code, &verifier), &host).await?;
    let key = match own_signin::key_of(status, &answer) {
        Ok(k) => k,
        Err(e) => return failed(&e.message()),
    };
    crate::computer::ask(env, &computer, "computer/own-key", &json!({ "provider": provider, "key": key })).await?;
    crate::auth::page(
        200,
        "Connected",
        &format!(
            "<p>{} is connected: your agents can use its models now. You can close this window.</p><p><a href=\"/settings\">Back to settings</a></p>",
            crate::auth::esc(provider)
        ),
    )
}
