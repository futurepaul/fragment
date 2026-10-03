//! A person's connections (decision 22, docs/computers.md): their accounts
//! at the providers the deployment offers (`FRAGMENT_CONNECTIONS`), which
//! WorkOS Pipes holds and refreshes. The shell lists them and starts a
//! connection; a computer's swap sends their tokens (computer.rs).
//!
//! - `GET /api/connections` → `{connections: [{provider, status}]}`: each
//!   offered provider, `connected`, `expired` (connect it again) or
//!   `none`, as Pipes says when asked for a token.
//! - `POST /api/connections/{provider}/authorize` → `{url}`: where the
//!   person's browser goes to connect it (Pipes' consent, then the
//!   provider's).

use fragment_proto::{ErrorCode, IdentityKind};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::json_response;
use crate::registry::calls::SubjectOf;

/// The WorkOS user the person signed in as, which their connections are.
async fn workos_user(env: &Env, cfg: &Config, who: &str) -> CellResult<String> {
    let workos = cfg.workos()?;
    crate::ask_registry(env, &SubjectOf { identity: who.into(), issuer: workos.issuer() })
        .await?
        .subject
        .ok_or_else(|| CellError::new(ErrorCode::NotConnected, "connections are a signed-in person's: sign in through the platform first"))
}

/// What Pipes says of one account: connected, expired, or none.
fn status_of(status: u16, answer: &Value) -> CellResult<&'static str> {
    if status != 200 {
        let why = answer["message"].as_str().unwrap_or("no reason given");
        return Err(CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS did not say ({status}): {why}")));
    }
    Ok(match (answer["active"].as_bool(), answer["error"].as_str()) {
        (Some(true), _) => "connected",
        (_, Some("needs_reauthorization")) => "expired",
        _ => "none",
    })
}

pub(crate) async fn route(env: &Env, who: &str, kind: IdentityKind, method: Method, rest: &[&str]) -> CellResult<Response> {
    if kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "connections are a person's: their agents use them through their computer"));
    }
    let cfg = Config::from_env(env);
    match (method, rest) {
        (Method::Get, []) => {
            let user = workos_user(env, cfg, who).await?;
            let api = &cfg.workos()?.api;
            let mut out = Vec::with_capacity(cfg.connections.len());
            // at most `swap::CREDENTIALS_MAX` providers, one question each
            for provider in cfg.connections.keys() {
                let (status, answer) = crate::keys::pipes_token(env, api, provider, &user).await?;
                out.push(json!({ "provider": provider, "status": status_of(status, &answer)? }));
            }
            json_response(&json!({ "connections": out }))
        }
        (Method::Post, [provider, "authorize"]) => {
            if !cfg.connections.contains_key(*provider) {
                let offered: Vec<&String> = cfg.connections.keys().collect();
                return Err(CellError::new(ErrorCode::NotFound, format!("no connection {provider:?} here (this deployment offers {offered:?})")));
            }
            let user = workos_user(env, cfg, who).await?;
            let (status, answer) = crate::keys::pipes_authorize(env, &cfg.workos()?.api, provider, &user).await?;
            // https only, but for the fakes of a test fleet
            let url = answer["url"].as_str().filter(|u| status == 200 && (u.starts_with("https://") || (cfg.test_hooks && u.starts_with("http://"))));
            let url = url.ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS gave no consent URL for {provider} ({status})")))?;
            json_response(&json!({ "provider": provider, "url": url }))
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/connections/{}", m.as_ref(), rest.join("/")))),
    }
}
