//! The fragment API, as the guest speaks it (docs/computers.md, "The
//! fragment API"; docs/api.md): `FRAGMENT_API` over plain HTTP, every
//! request acting as one of the computer's agents (`x-fragment-agent`)
//! except the computer's own routes. The intercept signs; the guest holds
//! no credential.
//!
//! Every route the bridge calls is a method here, so the platform side can
//! match them one for one (docs/bridge.md lists them).

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Method, Request, StatusCode};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::limits;
use crate::net::{self, Base, ClientWs};
use crate::records::Record;
use crate::runtime::Agent;

/// The header naming the agent a request acts as.
pub const AGENT_HEADER: &str = "x-fragment-agent";

/// Why a call failed, typed so callers match without reading strings.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// The platform answered, and refused: its status and `{error, message}`.
    Refused { status: u16, error: String, message: String },
    /// No answer: the connection, or the time it took.
    Transport(String),
    /// An answer that is not what the route returns.
    Decode(String),
    /// An answer past the bound the bridge reads.
    TooLarge,
}

impl ApiError {
    /// The agent is not (or no longer) in that fragment, or it is gone.
    pub fn gone(&self) -> bool {
        matches!(self, ApiError::Refused { status: 403 | 404, .. })
    }

    /// Worth asking again: no answer, too many asks, or the platform's own
    /// failure (every post carries its id, so a retry is the same record).
    pub fn retryable(&self) -> bool {
        match self {
            ApiError::Transport(_) => true,
            ApiError::Refused { status, .. } => *status == 429 || *status >= 500,
            ApiError::Decode(_) | ApiError::TooLarge => false,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Refused { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Refused { status, error, message } => write!(f, "{status} {error}: {message}"),
            ApiError::Transport(m) => write!(f, "no answer: {m}"),
            ApiError::Decode(m) => write!(f, "an answer that does not decode: {m}"),
            ApiError::TooLarge => write!(f, "an answer past the bound"),
        }
    }
}

/// `GET /api/computer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Computer {
    pub computer: String,
    pub owner: String,
    #[serde(default)]
    pub image: String,
    pub agents: Vec<Agent>,
    /// Every environment variable a credential of the deployment may be
    /// in, whether its agents hold it now or not.
    #[serde(default, rename = "credentialEnv")]
    pub credential_env: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FragmentEntry {
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// What it is (`app`, `chat`, `agent`, `brain`, `skills`): its live
    /// `fragment.json`'s `kind`.
    #[serde(default)]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub principal: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub added_at: i64,
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: Value,
    pub channel: String,
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub wake: bool,
}

/// The platform's answer to a post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posted {
    /// The id was posted before, with this body.
    pub replayed: bool,
    /// The record's place in its channel (`None` from a platform that does
    /// not say).
    pub seq: Option<u64>,
}

/// A page of records: `{channel, records, next}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub records: Vec<Record>,
    pub next: u64,
}

/// The fragment API at `FRAGMENT_API`.
#[derive(Clone)]
pub struct Api {
    base: Base,
    client: Client<HttpConnector, Full<Bytes>>,
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A fragment's name in a path: `<label>.<username>` only, so no name
/// ever reaches another route.
fn name(fragment: &str) -> Result<&str, ApiError> {
    let ok = !fragment.is_empty() && fragment.len() <= 128 && fragment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if ok {
        Ok(fragment)
    } else {
        Err(ApiError::Decode(format!("{fragment:?} is not a fragment name")))
    }
}

impl Api {
    pub fn new(base_url: &str) -> Result<Api, String> {
        let base = Base::parse(base_url)?;
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        connector.set_connect_timeout(Some(Duration::from_millis(limits::HTTP_TIMEOUT_MS)));
        let client = Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build(connector);
        Ok(Api { base, client })
    }

    pub fn base(&self) -> &Base {
        &self.base
    }

    /// One call: its status and body (bounded by `max`), or why not.
    async fn call(&self, method: Method, path: &str, agent: Option<&str>, body: Option<(&str, Bytes)>, max: usize) -> Result<Bytes, ApiError> {
        let mut req = Request::builder().method(method).uri(self.base.url(path)).header("host", self.base.authority());
        if let Some(a) = agent {
            req = req.header(AGENT_HEADER, a);
        }
        let body = match body {
            Some((content_type, bytes)) => {
                req = req.header("content-type", content_type).header("content-length", bytes.len());
                bytes
            }
            None => Bytes::new(),
        };
        let req = req.body(Full::new(body)).map_err(|e| ApiError::Transport(e.to_string()))?;
        let call = async {
            let res = self.client.request(req).await.map_err(|e| ApiError::Transport(e.to_string()))?;
            let status = res.status();
            let bytes = Limited::new(res.into_body(), max).collect().await.map_err(|e| {
                if e.downcast_ref::<http_body_util::LengthLimitError>().is_some() {
                    ApiError::TooLarge
                } else {
                    ApiError::Transport(e.to_string())
                }
            })?;
            Ok::<_, ApiError>((status, bytes.to_bytes()))
        };
        let (status, bytes) = match tokio::time::timeout(Duration::from_millis(limits::HTTP_TIMEOUT_MS), call).await {
            Ok(r) => r?,
            Err(_) => return Err(ApiError::Transport(format!("{path}: no answer in {} ms", limits::HTTP_TIMEOUT_MS))),
        };
        if status.is_success() {
            return Ok(bytes);
        }
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Err(ApiError::Refused {
            status: status.as_u16(),
            error: v["error"].as_str().unwrap_or("").to_string(),
            message: v["message"].as_str().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned()),
        })
    }

    async fn json<T: DeserializeOwned>(&self, method: Method, path: &str, agent: Option<&str>, body: Option<Value>) -> Result<T, ApiError> {
        let body = body.map(|v| ("application/json", Bytes::from(v.to_string())));
        let bytes = self.call(method, path, agent, body, limits::ANSWER_MAX_BYTES).await?;
        serde_json::from_slice(&bytes).map_err(|e| ApiError::Decode(format!("{path}: {e}")))
    }

    // ---- the computer's own routes (no agent) ----

    /// `GET /api/computer`: the agents to run.
    pub async fn computer(&self) -> Result<Computer, ApiError> {
        let c: Computer = self.json(Method::GET, "/api/computer", None, None).await?;
        if c.agents.len() > limits::AGENTS_MAX {
            return Err(ApiError::Decode(format!("{} agents; a computer runs at most {}", c.agents.len(), limits::AGENTS_MAX)));
        }
        if c.agents.iter().any(|a| a.credentials.len() > crate::runtime::CREDENTIALS_MAX) || c.credential_env.len() > crate::runtime::CREDENTIALS_MAX * 4 {
            return Err(ApiError::Decode(format!("more credentials than a deployment offers (at most {})", crate::runtime::CREDENTIALS_MAX)));
        }
        Ok(c)
    }

    /// `GET /api/computer/keepalive`, a WebSocket held while busy.
    pub async fn keepalive(&self) -> Result<ClientWs, String> {
        net::connect_ws(&self.base, "/api/computer/keepalive", &[]).await
    }

    // ---- as an agent ----

    /// `GET /api/fragments`: the fragments the agent holds a role on.
    pub async fn fragments(&self, agent: &str) -> Result<Vec<FragmentEntry>, ApiError> {
        #[derive(Deserialize)]
        struct A {
            fragments: Vec<FragmentEntry>,
        }
        let a: A = self.json(Method::GET, "/api/fragments", Some(agent), None).await?;
        Ok(a.fragments)
    }

    /// `GET /api/f/{name}/channels`.
    pub async fn channels(&self, agent: &str, fragment: &str) -> Result<Vec<ChannelInfo>, ApiError> {
        #[derive(Deserialize)]
        struct A {
            channels: Vec<ChannelInfo>,
        }
        let a: A = self.json(Method::GET, &format!("/api/f/{}/channels", name(fragment)?), Some(agent), None).await?;
        Ok(a.channels)
    }

    /// `GET /api/f/{name}/members`.
    pub async fn members(&self, agent: &str, fragment: &str) -> Result<Vec<Member>, ApiError> {
        #[derive(Deserialize)]
        struct A {
            members: Vec<Member>,
        }
        let a: A = self.json(Method::GET, &format!("/api/f/{}/members", name(fragment)?), Some(agent), None).await?;
        Ok(a.members)
    }

    /// `GET /f/{name}/__people?id=…`: what to call each identity (at most 64).
    pub async fn people(&self, agent: &str, fragment: &str, ids: &[String]) -> Result<BTreeMap<String, String>, ApiError> {
        let ids: Vec<&String> = ids.iter().take(64).collect();
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let query: Vec<String> = ids.iter().map(|i| format!("id={}", encode(i))).collect();
        let v: Value = self.json(Method::GET, &format!("/f/{}/__people?{}", name(fragment)?, query.join("&")), Some(agent), None).await?;
        let mut out = BTreeMap::new();
        if let Some(profiles) = v["profiles"].as_object() {
            for (id, p) in profiles {
                if let Some(u) = p["username"].as_str() {
                    out.insert(id.clone(), u.to_string());
                }
            }
        }
        Ok(out)
    }

    /// `GET /api/f/{name}/subscriptions`: the agent's own.
    pub async fn subscriptions(&self, agent: &str, fragment: &str) -> Result<Vec<Subscription>, ApiError> {
        #[derive(Deserialize)]
        struct A {
            subscriptions: Vec<Subscription>,
        }
        let a: A = self.json(Method::GET, &format!("/api/f/{}/subscriptions", name(fragment)?), Some(agent), None).await?;
        Ok(a.subscriptions)
    }

    /// `POST /api/f/{name}/subscriptions {channel, wake: true}`: a record on
    /// the channel wakes this computer.
    pub async fn subscribe_wake(&self, agent: &str, fragment: &str, channel: &str) -> Result<Subscription, ApiError> {
        self.json(Method::POST, &format!("/api/f/{}/subscriptions", name(fragment)?), Some(agent), Some(json!({ "channel": channel, "wake": true }))).await
    }

    /// `GET /api/f/{name}/channels/{channel}?after=&limit=`.
    pub async fn records(&self, agent: &str, fragment: &str, channel: &str, after: u64, limit: u32) -> Result<Page, ApiError> {
        self.json(Method::GET, &format!("/api/f/{}/channels/{}?after={after}&limit={limit}", name(fragment)?, encode(channel)), Some(agent), None).await
    }

    /// `POST /api/f/{name}/channels/{channel} {id, body}` → `{record,
    /// replayed}`: `replayed` when the id was posted before (with this
    /// body), and the record's seq (the first post's, for a replay).
    pub async fn post(&self, agent: &str, fragment: &str, channel: &str, id: &str, body: &Value) -> Result<Posted, ApiError> {
        let v: Value = self.json(Method::POST, &format!("/api/f/{}/channels/{}", name(fragment)?, encode(channel)), Some(agent), Some(json!({ "id": id, "body": body }))).await?;
        Ok(Posted { replayed: v["replayed"].as_bool().unwrap_or(false), seq: v["record"]["seq"].as_u64().filter(|s| *s > 0) })
    }

    /// `PUT /api/f/{name}/channels/{channel}/draft {turn, text}`.
    pub async fn draft(&self, agent: &str, fragment: &str, channel: &str, turn: &str, text: Option<&str>) -> Result<(), ApiError> {
        let _: Value = self.json(Method::PUT, &format!("/api/f/{}/channels/{}/draft", name(fragment)?, encode(channel)), Some(agent), Some(json!({ "turn": turn, "text": text }))).await?;
        Ok(())
    }

    /// `PUT /api/f/{name}/blobs/{sha256}`, typed `content-type`.
    pub async fn put_blob(&self, agent: &str, fragment: &str, sha256: &str, content_type: &str, bytes: Bytes) -> Result<(), ApiError> {
        assert!(sha256.len() == 64, "a blob is named by its sha256");
        self.call(Method::PUT, &format!("/api/f/{}/blobs/{sha256}", name(fragment)?), Some(agent), Some((content_type, bytes)), limits::ANSWER_MAX_BYTES).await?;
        Ok(())
    }

    /// `GET /api/f/{name}/blobs/{sha256}`, at most `ATTACHMENT_MAX_BYTES`.
    pub async fn get_blob(&self, agent: &str, fragment: &str, sha256: &str) -> Result<Bytes, ApiError> {
        let max = usize::try_from(limits::ATTACHMENT_MAX_BYTES).expect("25 MiB fits");
        self.call(Method::GET, &format!("/api/f/{}/blobs/{}", name(fragment)?, encode(sha256)), Some(agent), None, max).await
    }

    /// `GET /f/{name}/__live`, as the agent.
    pub async fn live(&self, agent: &str, fragment: &str) -> Result<ClientWs, String> {
        let fragment = name(fragment).map_err(|e| e.to_string())?;
        net::connect_ws(&self.base, &format!("/f/{fragment}/__live"), &[(AGENT_HEADER, agent.to_string())]).await
    }
}

/// Whether a status is the platform's "too many": a draft past the pace.
pub fn rate_limited(e: &ApiError) -> bool {
    e.status() == Some(StatusCode::TOO_MANY_REQUESTS.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_queries_stay_in_their_route() {
        assert!(name("talk.paul").is_ok());
        for bad in ["", "../x", "a/b", "a?b", "a b"] {
            assert!(name(bad).is_err(), "{bad}");
        }
        assert_eq!(encode("id:ab cd/"), "id%3Aab%20cd%2F");
        assert!(ApiError::Refused { status: 503, error: String::new(), message: String::new() }.retryable());
        assert!(ApiError::Refused { status: 429, error: String::new(), message: String::new() }.retryable());
        assert!(!ApiError::Refused { status: 409, error: String::new(), message: String::new() }.retryable());
        assert!(ApiError::Refused { status: 403, error: String::new(), message: String::new() }.gone());
        assert!(ApiError::Transport("x".into()).retryable());
    }
}
