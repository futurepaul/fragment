//! The fragment fleet as an agent reaches it: the same signed API the CLI
//! uses (NIP-98), with the agent's own key. An agent can do on a fragment
//! exactly what its membership lets it, and nothing through a side door.

use anyhow::{anyhow, Context};
use fragment_proto::ErrorBody;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, SqlStorage};

use crate::keys::{self, Sign};
use crate::store::{kv_get, kv_set};

/// The largest answer the agent reads from a fragment.
const ANSWER_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct Fleet {
    /// The platform's base URL (`FRAGMENT_API`), e.g. https://fragment.club.
    pub base: String,
    pub signer: Signer,
}

/// The agent's key, as `KEYS` holds it: sealed in the agent's own storage,
/// used only through the service.
#[derive(Clone)]
pub struct Signer {
    pub env: Env,
    pub sql: SqlStorage,
}

impl Signer {
    pub async fn sign(&self, what: Sign<'_>) -> anyhow::Result<String> {
        let sealed = kv_get(&self.sql, "secret")?.context("the agent has no key")?;
        // what an agent made before KEYS sealed its key with
        let npub = kv_get(&self.sql, "npub")?.unwrap_or_default();
        let signed = keys::nostr_sign(&self.env, &sealed, &npub, what).await?;
        if let Some(fresh) = signed.resealed {
            kv_set(&self.sql, "secret", fresh)?;
        }
        Ok(signed.header)
    }
}

impl Fleet {
    /// A signed request; answers the status and the JSON body (`Null` when
    /// empty, a string when it is not JSON).
    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> anyhow::Result<(u16, Value)> {
        let url = format!("{}{path}", self.base);
        let bytes = match body {
            Some(b) => serde_json::to_vec(b)?,
            None => Vec::new(),
        };
        let headers = Headers::new();
        let payload = (!bytes.is_empty()).then(|| hex::encode(Sha256::digest(&bytes)));
        let auth = self.signer.sign(Sign::Header { method: method.as_ref(), url: &url, payload }).await?;
        headers.set("authorization", &auth).map_err(|e| anyhow!("{e}"))?;
        let mut init = RequestInit::new();
        init.with_method(method);
        if body.is_some() {
            headers.set("content-type", "application/json").map_err(|e| anyhow!("{e}"))?;
            init.with_body(Some(worker::js_sys::Uint8Array::from(bytes.as_slice()).into()));
        }
        init.with_headers(headers);
        let request = Request::new_with_init(&url, &init).map_err(|e| anyhow!("{e}"))?;
        let mut response = Fetch::Request(request).send().await.map_err(|e| anyhow!("{path}: {e}"))?;
        let status = response.status_code();
        let bytes = response.bytes().await.map_err(|e| anyhow!("{path}: {e}"))?;
        if bytes.len() > ANSWER_MAX_BYTES {
            return Err(anyhow!("{path}: the answer is {} bytes; at most {ANSWER_MAX_BYTES}", bytes.len()));
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Ok((status, value))
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        let (status, value) = self.call(Method::Get, path, None).await?;
        if status != 200 {
            return Err(anyhow!("GET {path}: {status} {}", message(&value)));
        }
        Ok(value)
    }

    /// A signed GET, decoded into its fragment_proto type.
    pub async fn get_as<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let value = self.get(path).await?;
        T::deserialize(&value).with_context(|| format!("GET {path}: not a {}", std::any::type_name::<T>()))
    }
}

/// A refusal's message (its `ErrorBody`), or the whole answer when it is
/// not one.
pub fn message(v: &Value) -> String {
    match ErrorBody::deserialize(v) {
        Ok(refusal) => refusal.message,
        Err(_) => v.to_string(),
    }
}

/// The fleet from the agent's variables.
pub fn base(env: &worker::Env) -> anyhow::Result<String> {
    let base = env.var("FRAGMENT_API").map(|v| v.to_string()).context("FRAGMENT_API is not set on this fleet")?;
    Ok(base.trim().trim_end_matches('/').to_string())
}
