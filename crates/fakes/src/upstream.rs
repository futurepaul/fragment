//! The internet a computer's swap sends to (`FRAGMENT_SWAP_UPSTREAM`): the
//! provider APIs a connection or an operator key is for. Every request is
//! answered with what arrived (its host, from `x-fragment-upstream-host`,
//! its method, path and auth headers), so a test reads what the provider
//! would have seen, and each is recorded.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// The headers a provider takes a credential in.
const AUTH_HEADERS: [&str; 3] = ["authorization", "x-api-key", "x-goog-api-key"];

pub struct Upstream {
    pub url: String,
    seen: Arc<Mutex<Vec<Value>>>,
    _server: Server,
}

fn seen_of(req: &Request) -> Value {
    let mut auth = serde_json::Map::new();
    for h in AUTH_HEADERS {
        if let Some(v) = req.header(h) {
            auth.insert(h.into(), json!(v));
        }
    }
    json!({
        "host": req.header("x-fragment-upstream-host"),
        "method": req.method,
        "path": req.path,
        "auth": auth,
        "agent": req.header("x-fragment-agent"),
        "body": String::from_utf8_lossy(&req.body),
    })
}

impl Upstream {
    pub fn start() -> std::io::Result<Upstream> {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = Arc::clone(&seen);
        let handler: Handler = Arc::new(move |req: &Request| {
            let v = seen_of(req);
            log.lock().expect("upstream log").push(v.clone());
            // a redirect, for the test that the swap never follows one
            if req.path == "/redirect" {
                return Response::bytes(302, "text/plain", b"elsewhere".to_vec()).with_header("location", "https://elsewhere.test/");
            }
            Response::json(200, &v)
        });
        let server = Server::start(0, handler)?;
        Ok(Upstream { url: server.url.clone(), seen, _server: server })
    }

    /// Every request that arrived, oldest first.
    pub fn seen(&self) -> Vec<Value> {
        self.seen.lock().expect("upstream log").clone()
    }
}
