//! WorkOS AuthKit, for the surface the platform calls (phase 4 slice B):
//! the authorize redirect, the code exchange, and the logout redirect
//! (https://workos.com/docs/reference/authkit). Users are made on demand:
//! an authorize with `login_hint` signs in as that email (a new user the
//! first time); without one, a browser gets a one-field form (dev), and a
//! test sets who comes next. Levers: change a user's email, fail the next
//! authorize, the codes and sessions it handed out.
//!
//! And Pipes' access tokens (https://workos.com/docs/reference/pipes/access-token):
//! a user's account at a provider is connected by the test (`connect`), or
//! by the user's browser through the consent URL the authorize call gives
//! (`POST /data-integrations/{provider}/authorize`, then `GET
//! /pipes/consent`), and may come to need authorizing again; each token
//! asked for is new, and lasts an hour. Its state is read with no token
//! (`GET /user_management/users/{user}/connected_accounts/{provider}`,
//! https://workos.com/docs/reference/pipes/connected-account): `connected`
//! or `needs_reauthorization`, 404 when there is none.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

#[derive(Clone, Debug)]
pub struct User {
    pub id: String,
    pub email: String,
    /// Whether WorkOS verified the email (made verified).
    pub verified: bool,
}

#[derive(Default)]
struct State {
    users: Vec<User>,
    /// code → (user id, session id), each used once.
    codes: HashMap<String, (String, String)>,
    ended: Vec<String>,
    next_error: Option<String>,
    counter: u64,
    /// (user id, provider) → whether the account is still authorized.
    pipes: HashMap<(String, String), bool>,
    /// Tokens handed out, by provider.
    tokens: HashMap<String, Vec<String>>,
    /// Consents given out and not yet followed: state → (user id, provider).
    consents: HashMap<String, (String, String)>,
    /// Connected accounts' states read (each with no token).
    states_read: u64,
}

impl State {
    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}_{:026}", self.counter)
    }

    fn user_for(&mut self, email: &str) -> User {
        if let Some(u) = self.users.iter().find(|u| u.email.eq_ignore_ascii_case(email)) {
            return u.clone();
        }
        let user = User { id: self.next("user"), email: email.to_string(), verified: true };
        self.users.push(user.clone());
        user
    }
}

pub struct WorkOs {
    pub url: String,
    pub client_id: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn problem(status: u16, error: &str, description: &str) -> Response {
    Response::json(status, &json!({ "error": error, "error_description": description }))
}

fn redirect(to: &str) -> Response {
    Response::bytes(302, "text/plain", vec![]).with_header("location", to)
}

fn with_query(base: &str, pairs: &[(&str, &str)]) -> String {
    let q: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect();
    format!("{base}{}{}", if base.contains('?') { "&" } else { "?" }, q.join("&"))
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// An access token shaped like AuthKit's (a JWT); the platform reads its
/// `sid` from the exchange's answer and never trusts it from a browser, so
/// the fake does not sign it.
fn access_token(user: &str, sid: &str, client_id: &str) -> String {
    let b64 = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    format!(
        "{}.{}.fake",
        b64(json!({ "alg": "none", "typ": "JWT" })),
        b64(json!({ "iss": "https://api.workos.com", "sub": user, "sid": sid, "client_id": client_id }))
    )
}

impl WorkOs {
    /// Serves on a free port for one environment: its client id and API key.
    pub fn start(client_id: &str, api_key: &str) -> std::io::Result<WorkOs> {
        Self::start_on(0, client_id, api_key)
    }

    pub fn start_on(port: u16, client_id: &str, api_key: &str) -> std::io::Result<WorkOs> {
        let state: Arc<Mutex<State>> = Arc::default();
        let (st, client, key) = (Arc::clone(&state), client_id.to_string(), api_key.to_string());
        // where the fake answers, for the consent URLs it gives (known once it listens)
        let base: Arc<Mutex<String>> = Arc::default();
        let at = Arc::clone(&base);
        let handler: Handler = Arc::new(move |req: &Request| {
            let q = |k: &str| req.query.get(k).cloned().unwrap_or_default();
            let mut s = st.lock().expect("workos state");
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/user_management/authorize") | ("GET", "/user_management/authorize/as") => {
                    if q("client_id") != client {
                        return problem(400, "invalid_client", "unknown client_id");
                    }
                    if q("response_type") != "code" || q("redirect_uri").is_empty() {
                        return problem(400, "invalid_request", "response_type=code and a redirect_uri");
                    }
                    let back = q("redirect_uri");
                    if let Some(error) = s.next_error.take() {
                        return redirect(&with_query(&back, &[("error", &error), ("error_description", "a failure the test asked for"), ("state", &q("state"))]));
                    }
                    let email = q("login_hint");
                    if email.is_empty() {
                        // a person at a browser: who are you?
                        let fields: String = ["client_id", "redirect_uri", "response_type", "provider", "state"]
                            .iter()
                            .map(|k| format!(r#"<input type="hidden" name="{k}" value="{}">"#, q(k).replace('"', "&quot;")))
                            .collect();
                        let page = format!(
                            r#"<!doctype html><title>WorkOS (fake)</title><form action="/user_management/authorize/as" method="get">
<p>The WorkOS fake: sign in as</p>{fields}<input name="login_hint" value="dev@fragment.localhost" size="32"> <button>Sign in</button></form>"#
                        );
                        return Response::bytes(200, "text/html; charset=utf-8", page.into_bytes());
                    }
                    let user = s.user_for(&email);
                    let (code, sid) = (s.next("code"), s.next("session"));
                    s.codes.insert(code.clone(), (user.id, sid));
                    redirect(&with_query(&back, &[("code", &code), ("state", &q("state"))]))
                }
                ("POST", "/user_management/authenticate") => {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                    if body["client_id"] != client.as_str() || body["client_secret"] != key.as_str() {
                        return problem(401, "invalid_client", "the client id or secret is wrong");
                    }
                    if body["grant_type"] != "authorization_code" {
                        return problem(400, "unsupported_grant_type", "authorization_code only");
                    }
                    let code = body["code"].as_str().unwrap_or("");
                    let Some((uid, sid)) = s.codes.remove(code) else {
                        return problem(400, "invalid_grant", "the code is invalid or was used");
                    };
                    let user = s.users.iter().find(|u| u.id == uid).cloned().expect("a code names a user");
                    Response::json(
                        200,
                        &json!({
                            "user": {
                                "object": "user", "id": user.id, "email": user.email, "email_verified": user.verified,
                                "first_name": null, "last_name": null, "profile_picture_url": null,
                                "created_at": "2026-09-24T00:00:00.000Z", "updated_at": "2026-09-24T00:00:00.000Z",
                            },
                            "organization_id": null,
                            "access_token": access_token(&user.id, &sid, &client),
                            "refresh_token": format!("refresh_{sid}"),
                            "authentication_method": "Password",
                        }),
                    )
                }
                ("GET", "/user_management/sessions/logout") => {
                    let sid = q("session_id");
                    if sid.is_empty() {
                        return problem(400, "invalid_request", "session_id");
                    }
                    s.ended.push(sid);
                    match q("return_to") {
                        r if r.is_empty() => Response::bytes(200, "text/plain", b"signed out".to_vec()),
                        r => redirect(&r),
                    }
                }
                ("POST", p) if p.starts_with("/data-integrations/") && p.ends_with("/authorize") => {
                    if req.header("authorization") != Some(format!("Bearer {key}").as_str()) {
                        return problem(401, "unauthorized", "the API key is wrong");
                    }
                    let provider = p["/data-integrations/".len()..p.len() - "/authorize".len()].to_string();
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                    let Some(user) = body["user_id"].as_str().filter(|u| s.users.iter().any(|x| x.id == *u)).map(str::to_string) else {
                        return problem(404, "not_found", "no such user");
                    };
                    let state = s.next("pipes_state");
                    s.consents.insert(state.clone(), (user, provider));
                    let url = format!("{}/pipes/consent?state={state}", at.lock().expect("workos base"));
                    Response::json(200, &json!({ "url": url, "state": state }))
                }
                // the user's browser, consenting: the account is connected
                ("GET", "/pipes/consent") => match s.consents.remove(&q("state")) {
                    Some((user, provider)) => {
                        s.pipes.insert((user, provider.clone()), true);
                        Response::bytes(200, "text/html; charset=utf-8", format!("<!doctype html><title>Connected</title><p>{provider} is connected. You can close this window.</p>").into_bytes())
                    }
                    None => problem(400, "invalid_request", "this consent was used or never given"),
                },
                // a connected account's state, with no token in it
                ("GET", p) if p.starts_with("/user_management/users/") && p.contains("/connected_accounts/") => {
                    if req.header("authorization") != Some(format!("Bearer {key}").as_str()) {
                        return problem(401, "unauthorized", "the API key is wrong");
                    }
                    let rest = &p["/user_management/users/".len()..];
                    let Some((user, provider)) = rest.split_once("/connected_accounts/") else { return problem(404, "not_found", "no such route") };
                    s.states_read += 1;
                    match s.pipes.get(&(user.to_string(), provider.to_string())).copied() {
                        None => problem(404, "not_found", "no connected account"),
                        Some(authorized) => Response::json(
                            200,
                            &json!({
                                "object": "connected_account", "id": format!("conn_{user}_{provider}"), "user_id": user, "organization_id": null,
                                "scopes": [], "state": if authorized { "connected" } else { "needs_reauthorization" },
                                "created_at": "2026-10-01T00:00:00.000Z", "updated_at": "2026-10-01T00:00:00.000Z",
                            }),
                        ),
                    }
                }
                ("POST", p) if p.starts_with("/data-integrations/") && p.ends_with("/token") => {
                    if req.header("authorization") != Some(format!("Bearer {key}").as_str()) {
                        return problem(401, "unauthorized", "the API key is wrong");
                    }
                    let provider = p["/data-integrations/".len()..p.len() - "/token".len()].to_string();
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                    let Some(user) = body["user_id"].as_str().filter(|u| s.users.iter().any(|x| x.id == *u)).map(str::to_string) else {
                        return problem(404, "not_found", "no such user");
                    };
                    match s.pipes.get(&(user.clone(), provider.clone())).copied() {
                        None => Response::json(200, &json!({ "active": false, "error": "not_installed" })),
                        Some(false) => Response::json(200, &json!({ "active": false, "error": "needs_reauthorization" })),
                        Some(true) => {
                            let token = s.next(&format!("pipes_{provider}"));
                            s.tokens.entry(provider).or_default().push(token.clone());
                            let expires = crate::codestorage::iso(crate::codestorage::now_ms() + 3_600_000);
                            Response::json(
                                200,
                                &json!({ "active": true, "access_token": { "object": "access_token", "access_token": token, "expires_at": expires, "scopes": [], "missing_scopes": [] } }),
                            )
                        }
                    }
                }
                _ => problem(404, "not_found", "no such route"),
            }
        });
        let server = Server::start(port, handler)?;
        *base.lock().expect("workos base") = server.url.clone();
        Ok(WorkOs { url: server.url.clone(), client_id: client_id.to_string(), state, _server: server })
    }

    /// The user an email signs in as (made the first time).
    pub fn user(&self, email: &str) -> User {
        self.state.lock().expect("workos state").user_for(email)
    }

    /// A user's email changes; their id does not.
    pub fn set_email(&self, id: &str, email: &str) {
        let mut s = self.state.lock().expect("workos state");
        if let Some(u) = s.users.iter_mut().find(|u| u.id == id) {
            u.email = email.to_string();
        }
    }

    /// Whether WorkOS has verified a user's email.
    pub fn set_verified(&self, id: &str, verified: bool) {
        let mut s = self.state.lock().expect("workos state");
        if let Some(u) = s.users.iter_mut().find(|u| u.id == id) {
            u.verified = verified;
        }
    }

    /// The next authorize redirects back with this error.
    pub fn fail_next(&self, error: &str) {
        self.state.lock().expect("workos state").next_error = Some(error.to_string());
    }

    /// `email`'s account at `provider` is connected (`true`), or comes to
    /// need authorizing again (`false`).
    pub fn connect(&self, email: &str, provider: &str, authorized: bool) {
        let mut s = self.state.lock().expect("workos state");
        let user = s.user_for(email);
        s.pipes.insert((user.id, provider.to_string()), authorized);
    }

    /// How many connected accounts' states were read.
    pub fn states_read(&self) -> u64 {
        self.state.lock().expect("workos state").states_read
    }

    /// The access tokens handed out for `provider`, oldest first.
    pub fn tokens(&self, provider: &str) -> Vec<String> {
        self.state.lock().expect("workos state").tokens.get(provider).cloned().unwrap_or_default()
    }

    /// Sessions ended through the logout URL.
    pub fn ended(&self) -> Vec<String> {
        self.state.lock().expect("workos state").ended.clone()
    }
}
