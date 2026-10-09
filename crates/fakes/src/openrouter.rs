//! OpenRouter, faked (docs/computers.md, "An agent's own model"): the
//! surface the platform and a guest touch, as its documentation shapes it
//! (openrouter.ai/docs/guides/overview/auth/oauth, its API reference).
//!
//! - `GET /auth?callback_url&code_challenge&code_challenge_method&state
//!   &key_label`: OpenRouter's page, which asks the person and sends their
//!   browser back. Here it approves at once (a test lever can make it
//!   decline): a 302 to `callback_url` with `code` and the `state` as it
//!   came, the code kept with its challenge for 10 minutes.
//! - `POST /api/v1/auth/keys {code, code_verifier, code_challenge_method}`
//!   → `{key}`: the code exchanged, once, if the verifier's S256 challenge
//!   is the one the authorization named; else 403, as OpenRouter answers.
//! - `GET /api/v1/key` and `POST /api/v1/chat/completions`: a key's own
//!   view, and a model's answer, each refused (401) without a key the
//!   fake made, and each recording the `Authorization` it got: where a
//!   test reads that the provider saw the person's key, never a
//!   placeholder.
//!
//! The browser and the cell reach it at its own address (the e2e names it
//! as the catalog row's `oauth`); a guest's calls reach it through the
//! swap's upstream (`Upstream::start_with`), as the host `openrouter.ai`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// How long a code lives (OpenRouter's: 10 minutes).
const CODE_TTL: Duration = Duration::from_secs(600);

#[derive(Default)]
struct State {
    /// Each code not yet exchanged: its challenge and when it was made.
    codes: HashMap<String, (String, Instant)>,
    /// Every key made, in order.
    keys: Vec<String>,
    /// Each authorization's query as it came.
    authorizations: Vec<HashMap<String, String>>,
    /// Each exchange's body as it came, and the status answered.
    exchanges: Vec<(Value, u16)>,
    /// Each call with a key (`/api/v1/key`, chat completions): its
    /// `Authorization` and its path.
    calls: Vec<(String, String)>,
    /// The next authorization is declined, as a person who says no.
    decline: bool,
    made: u64,
}

pub struct OpenRouter {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn challenge_of(verifier: &str) -> String {
    use sha2::Digest;
    b64url(&sha2::Sha256::digest(verifier.as_bytes()))
}

/// The answer to `req`, for both its own server and the upstream's host.
fn answer(state: &Mutex<State>, req: &Request) -> Response {
    let mut s = state.lock().expect("openrouter state");
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/auth") => {
            s.authorizations.push(req.query.clone());
            let Some(callback) = req.query.get("callback_url").and_then(|c| url::Url::parse(c).ok()) else {
                return Response::json(400, &json!({ "error": { "code": 400, "message": "callback_url is required" } }));
            };
            let challenge = req.query.get("code_challenge").cloned().unwrap_or_default();
            if req.query.get("code_challenge_method").map(String::as_str) != Some("S256") || challenge.is_empty() {
                return Response::json(400, &json!({ "error": { "code": 400, "message": "Invalid code_challenge_method" } }));
            }
            let mut back = callback;
            if std::mem::take(&mut s.decline) {
                // a person who declines is sent back with no code
                if let Some(state) = req.query.get("state") {
                    back.query_pairs_mut().append_pair("state", state);
                }
                return Response::bytes(302, "text/plain", vec![]).with_header("location", back.as_str());
            }
            s.made += 1;
            let code = format!("orcode-{}-{}", s.made, &challenge_of(&format!("code{}", s.made))[..12]);
            s.codes.insert(code.clone(), (challenge, Instant::now()));
            back.query_pairs_mut().append_pair("code", &code);
            if let Some(state) = req.query.get("state") {
                back.query_pairs_mut().append_pair("state", state);
            }
            Response::bytes(302, "text/plain", vec![]).with_header("location", back.as_str())
        }
        ("POST", "/api/v1/auth/keys") => {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let (code, verifier) = (body["code"].as_str().unwrap_or_default(), body["code_verifier"].as_str().unwrap_or_default());
            let answer = if body["code_challenge_method"] != "S256" {
                (400, json!({ "error": { "code": 400, "message": "Invalid code_challenge_method" } }))
            } else {
                match s.codes.remove(code) {
                    Some((_, at)) if at.elapsed() > CODE_TTL => (403, json!({ "error": { "code": 403, "message": "Authorization code expired" } })),
                    Some((challenge, _)) if challenge == challenge_of(verifier) => {
                        s.made += 1;
                        let key = format!("sk-or-v1-fake{:04}{}", s.made, &hex::encode(challenge_of(code).as_bytes())[..24]);
                        s.keys.push(key.clone());
                        (200, json!({ "key": key, "user_id": "user_fake" }))
                    }
                    _ => (403, json!({ "error": { "code": 403, "message": "Invalid code or code_verifier" } })),
                }
            };
            s.exchanges.push((body, answer.0));
            Response::json(answer.0, &answer.1)
        }
        (method, path) if (method == "GET" && path == "/api/v1/key") || (method == "POST" && path == "/api/v1/chat/completions") => {
            let auth = req.header("authorization").unwrap_or_default().to_string();
            s.calls.push((auth.clone(), path.to_string()));
            let Some(key) = auth.strip_prefix("Bearer ").filter(|k| s.keys.iter().any(|m| m == k)) else {
                return Response::json(401, &json!({ "error": { "code": 401, "message": "No auth credentials found" } }));
            };
            if path == "/api/v1/key" {
                return Response::json(200, &json!({ "data": { "label": format!("{}…", &key[..14]), "usage": 0, "limit": null, "is_free_tier": false } }));
            }
            let asked: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let said = asked["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].clone()).unwrap_or(Value::Null);
            let text = format!("openrouter: {}", said.as_str().unwrap_or("hello"));
            Response::json(
                200,
                &json!({
                    "id": "gen-fake", "object": "chat.completion", "model": asked["model"],
                    "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 },
                }),
            )
        }
        _ => Response::json(404, &json!({ "error": { "code": 404, "message": "Not Found" } })),
    }
}

impl OpenRouter {
    pub fn start() -> std::io::Result<OpenRouter> {
        let state: Arc<Mutex<State>> = Arc::default();
        let served = Arc::clone(&state);
        let server = Server::start(0, Arc::new(move |req: &Request| answer(&served, req)))?;
        Ok(OpenRouter { url: server.url.clone(), state, _server: server })
    }

    /// Its answers, for the swap's upstream to give a guest's calls to its host.
    pub fn handler(&self) -> Handler {
        let state = Arc::clone(&self.state);
        Arc::new(move |req: &Request| answer(&state, req))
    }

    /// Its catalog row's sign-in, at this fake (`oauth`), its manage page OpenRouter's own.
    pub fn oauth(&self) -> Value {
        json!({ "authorize": format!("{}/auth", self.url), "exchange": format!("{}/api/v1/auth/keys", self.url), "manage": "https://openrouter.ai/settings/keys" })
    }

    /// The next authorization is declined (the person says no).
    pub fn decline_next(&self) {
        self.state.lock().expect("openrouter state").decline = true;
    }

    /// Every key it made, oldest first.
    pub fn keys(&self) -> Vec<String> {
        self.state.lock().expect("openrouter state").keys.clone()
    }

    /// Every authorization's query.
    pub fn authorizations(&self) -> Vec<HashMap<String, String>> {
        self.state.lock().expect("openrouter state").authorizations.clone()
    }

    /// Every exchange: its body and the status answered.
    pub fn exchanges(&self) -> Vec<(Value, u16)> {
        self.state.lock().expect("openrouter state").exchanges.clone()
    }

    /// Every call that named a key: its `Authorization` and path.
    pub fn calls(&self) -> Vec<(String, String)> {
        self.state.lock().expect("openrouter state").calls.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, path: &str, query: &[(&str, &str)], body: Value, auth: Option<&str>) -> Request {
        let mut target = url::Url::parse("http://f.test").unwrap();
        target.set_path(path);
        for (k, v) in query {
            target.query_pairs_mut().append_pair(k, v);
        }
        let target = format!("{}{}", target.path(), target.query().map(|q| format!("?{q}")).unwrap_or_default());
        let headers: Vec<(&str, &str)> = auth.map(|a| vec![("authorization", a)]).unwrap_or_default();
        let body = if body.is_null() { vec![] } else { body.to_string().into_bytes() };
        Request::new(method, &target, &headers, &body)
    }

    /// Valid: an authorization sends the browser back with a code and its
    /// state; the code and its verifier make a key, once; the key is taken.
    /// Invalid: another verifier, a code spent, no key, a declined person.
    #[test]
    fn a_code_and_its_verifier_make_a_key_once() {
        let state = Mutex::new(State::default());
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let q = [("callback_url", "http://127.0.0.1:9/api/connections/openrouter/callback"), ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"), ("code_challenge_method", "S256"), ("state", "st")];
        let r = answer(&state, &req("GET", "/auth", &q, Value::Null, None));
        let location = r.headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.clone()).unwrap();
        let back = url::Url::parse(&location).unwrap();
        let pairs: HashMap<String, String> = back.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!((r.status, back.path(), pairs["state"].as_str()), (302, "/api/connections/openrouter/callback", "st"));
        let code = pairs["code"].clone();
        let exchange = |v: &str| answer(&state, &req("POST", "/api/v1/auth/keys", &[], json!({ "code": code, "code_verifier": v, "code_challenge_method": "S256" }), None));
        let wrong = exchange("another-verifier-another-verifier-another-v");
        assert_eq!(wrong.status, 403, "another verifier: and the code is spent");
        let r = answer(&state, &req("GET", "/auth", &q, Value::Null, None));
        let code = url::Url::parse(&r.headers.iter().find(|(k, _)| k == "location").unwrap().1).unwrap().query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
        let exchange = |v: &str| answer(&state, &req("POST", "/api/v1/auth/keys", &[], json!({ "code": code, "code_verifier": v, "code_challenge_method": "S256" }), None));
        let made = exchange(verifier);
        let key: Value = serde_json::from_slice(&made.body).unwrap();
        let key = key["key"].as_str().unwrap().to_string();
        assert!(made.status == 200 && key.starts_with("sk-or-v1-"));
        assert_eq!(exchange(verifier).status, 403, "a code is exchanged once");
        assert_eq!(answer(&state, &req("GET", "/api/v1/key", &[], Value::Null, Some(&format!("Bearer {key}")))).status, 200);
        assert_eq!(answer(&state, &req("GET", "/api/v1/key", &[], Value::Null, Some("Bearer fck_openrouter_00"))).status, 401, "a placeholder is no key");
        let said = answer(&state, &req("POST", "/api/v1/chat/completions", &[], json!({ "model": "z-ai/glm-5.3", "messages": [{ "role": "user", "content": "hi" }] }), Some(&format!("Bearer {key}"))));
        assert!(String::from_utf8_lossy(&said.body).contains("openrouter: hi"));
        assert_eq!(state.lock().unwrap().calls.len(), 3);
        state.lock().unwrap().decline = true;
        let r = answer(&state, &req("GET", "/auth", &q, Value::Null, None));
        assert!(!r.headers.iter().find(|(k, _)| k == "location").unwrap().1.contains("code="), "declined: no code");
    }
}
