//! Native services: Rust the celld node runs on behalf of our cells
//! (docs/hardening.md, H1).
//!
//! A cell reaches one through an ordinary service binding whose target is
//! `native:<name>` (`"services": [{"binding": "KEYS", "service":
//! "native:keys"}]`). The fork's seam hands the call here with the calling
//! cell's scope (`<Class>:<64 hex>`), which the host derives from the active
//! event: JavaScript cannot claim another cell's. Every service decides its
//! authority from that scope.
//!
//! This crate does not depend on celld; celld depends on it. Everything a
//! service needs from the host arrives in [`Request`].

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

pub mod keys;
pub mod seal;

/// The service-binding target prefix the seam routes here.
pub const PREFIX: &str = "native:";

pub struct Request {
    /// The calling cell's scope (`<Class>:<64 hex>`), or `None` when the
    /// caller is not a cell (a stateless Worker, a Workflow step).
    pub caller: Option<String>,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The path after the host, without its query (`seal`, `nostr/sign`).
    pub fn path(&self) -> &str {
        let rest = self.url.split_once("://").map_or(self.url.as_str(), |(_, rest)| rest);
        let path = rest.split_once('/').map_or("", |(_, p)| p);
        path.split(['?', '#']).next().unwrap_or("")
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, v: &serde_json::Value) -> Response {
        Response { status, headers: vec![("content-type".into(), "application/json".into())], body: v.to_string().into_bytes() }
    }

    pub fn error(status: u16, message: impl Into<String>) -> Response {
        Response::json(status, &serde_json::json!({ "error": message.into() }))
    }
}

pub type Reply = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait Service: Send + Sync {
    fn fetch(&self, req: Request) -> Reply;
}

/// Whether a service-binding target is ours rather than a co-hosted script.
pub fn is_native(target: &str) -> bool {
    target.starts_with(PREFIX)
}

fn registry() -> &'static HashMap<&'static str, Box<dyn Service>> {
    static SERVICES: OnceLock<HashMap<&'static str, Box<dyn Service>>> = OnceLock::new();
    SERVICES.get_or_init(|| {
        let mut m: HashMap<&'static str, Box<dyn Service>> = HashMap::new();
        // Keys come from the node's environment, never from the deployment
        // manifest or a Worker's `vars`: never in a JS heap or the bucket.
        m.insert("keys", Box::new(keys::Keys::from_env()));
        m
    })
}

/// The service a `native:<name>` target names.
pub fn lookup(target: &str) -> Option<&'static dyn Service> {
    registry().get(target.strip_prefix(PREFIX)?).map(|s| s.as_ref())
}
