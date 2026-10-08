//! Stripe's API as the cell calls it (docs/billing.md, "Stripe"): form
//! bodies, the pinned `Stripe-Version`, an `Idempotency-Key` on every POST,
//! and only the fields fragment reads parsed (`fragment_core::stripe`). The
//! key is the account's restricted key (`keys::stripe_key`), read when a
//! call needs it. No SDK: the API is a few form posts.

use fragment_core::stripe::{Form, API_VERSION};
use fragment_proto::ErrorCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};

/// A call to Stripe is answered within this, or is `UpstreamFailed`.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// An answer past this is refused (a subscription is a few KB).
const ANSWER_MAX_BYTES: usize = 256 * 1024;

pub struct Stripe {
    api: String,
    key: String,
}

/// Stripe's refusal, as far as it says why.
#[derive(Deserialize)]
struct Refusal {
    error: RefusalError,
}

#[derive(Deserialize)]
struct RefusalError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

/// The deployment's Stripe, when it sells seats.
pub async fn client(env: &Env, cfg: &Config) -> CellResult<Stripe> {
    let api = cfg.stripe()?.api.clone();
    let key = crate::keys::stripe_key(env).await?;
    Ok(Stripe { api, key })
}

impl Stripe {
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> CellResult<T> {
        self.send(Method::Get, path, None, None).await
    }

    /// A POST, once by `idempotency` (Stripe answers a repeat as the first).
    pub async fn post<T: DeserializeOwned>(&self, path: &str, form: &Form, idempotency: &str) -> CellResult<T> {
        assert!(!idempotency.is_empty() && idempotency.len() <= 255, "an idempotency key is 1-255 characters");
        self.send(Method::Post, path, Some(form.encode()), Some(idempotency)).await
    }

    async fn send<T: DeserializeOwned>(&self, method: Method, path: &str, body: Option<String>, idempotency: Option<&str>) -> CellResult<T> {
        assert!(path.starts_with("/v1/"), "a Stripe path is the API's: {path}");
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {}", self.key))?;
        headers.set("stripe-version", API_VERSION)?;
        if let Some(k) = idempotency {
            headers.set("idempotency-key", k)?;
        }
        if body.is_some() {
            headers.set("content-type", "application/x-www-form-urlencoded")?;
        }
        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if let Some(b) = body {
            init.with_body(Some(b.into()));
        }
        let req = Request::new_with_init(&format!("{}{path}", self.api), &init)?;
        let a = crate::cs::fetch_all(req, TIMEOUT, ANSWER_MAX_BYTES).await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("Stripe did not answer {path}: {}", e.message)))?;
        if a.cut {
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("Stripe's answer to {path} is over {ANSWER_MAX_BYTES} bytes")));
        }
        if a.status == 200 {
            return serde_json::from_slice(&a.body).map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("Stripe's answer to {path}: {e}")));
        }
        let why = serde_json::from_slice::<Refusal>(&a.body).ok().map(|r| format!("{}: {}", r.error.kind.unwrap_or_default(), r.error.message.unwrap_or_default()));
        let why = why.unwrap_or_else(|| "no reason given".into());
        // its 4xx is a request it will not take (a bug of ours, or a
        // subscription no longer changeable); 429 and 5xx are its to retry
        let code = match a.status {
            404 => ErrorCode::NotFound,
            429 => ErrorCode::RateLimited,
            400..=499 => ErrorCode::InvalidRequest,
            _ => ErrorCode::UpstreamFailed,
        };
        Err(CellError::new(code, format!("Stripe refused {path} ({}): {why}", a.status)))
    }
}
