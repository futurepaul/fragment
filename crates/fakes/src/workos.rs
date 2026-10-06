//! WorkOS, for the surface the platform calls: two services, as WorkOS
//! runs them.
//!
//! - **AuthKit's OpenID Connect provider** at its own domain (`authkit`),
//!   for an OAuth application (WorkOS Connect): sign-in is one more issuer
//!   (`oidc::Profile::AuthKit`, docs/self-host.md, seam 4). Its people are
//!   the environment's users, `user_…`.
//! - **Pipes** on the API (`url`), its access tokens
//!   (https://workos.com/docs/reference/pipes/access-token): a user's
//!   account at a provider is connected by the test (`connect`), or by the
//!   user's browser through the consent URL the authorize call gives
//!   (`POST /data-integrations/{provider}/authorize`, then `GET
//!   /pipes/consent`), and may come to need authorizing again; each token
//!   asked for is new, and lasts an hour. Its state is read with no token
//!   (`GET /user_management/users/{user}/connected_accounts/{provider}`,
//!   https://workos.com/docs/reference/pipes/connected-account):
//!   `connected` or `needs_reauthorization`, 404 when there is none. Pipes
//!   knows AuthKit's users and no one else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};
use crate::oidc::{Directory, Oidc, Profile, User};

#[derive(Default)]
struct State {
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
}

pub struct WorkOs {
    /// The API (`WORKOS_API_URL`): Pipes.
    pub url: String,
    /// The environment (`WORKOS_CLIENT_ID`): its people are kept under
    /// `workos:<client_id>`.
    pub client_id: String,
    /// AuthKit's OpenID Connect provider (the AuthKit domain), for the
    /// OAuth application `authkit.client_id`.
    pub authkit: Oidc,
    users: Directory,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn problem(status: u16, error: &str, description: &str) -> Response {
    Response::json(status, &json!({ "error": error, "error_description": description }))
}

impl WorkOs {
    /// Serves on free ports for one environment (its client id and API
    /// key) and its OAuth application (its client id and secret).
    pub fn start(client_id: &str, api_key: &str, app_client: &str, app_secret: &str) -> std::io::Result<WorkOs> {
        Self::start_on(0, 0, client_id, api_key, app_client, app_secret)
    }

    /// The API on `port`, AuthKit on `authkit_port` (0: a free one).
    pub fn start_on(port: u16, authkit_port: u16, client_id: &str, api_key: &str, app_client: &str, app_secret: &str) -> std::io::Result<WorkOs> {
        assert!(client_id != app_client, "the OAuth application is a client of its own, not the environment's");
        let authkit = Oidc::start_as(Profile::AuthKit, authkit_port, app_client, app_secret)?;
        let state: Arc<Mutex<State>> = Arc::default();
        let (st, key, users) = (Arc::clone(&state), api_key.to_string(), authkit.directory());
        // where the fake answers, for the consent URLs it gives (known once it listens)
        let base: Arc<Mutex<String>> = Arc::default();
        let at = Arc::clone(&base);
        let handler: Handler = Arc::new(move |req: &Request| {
            let q = |k: &str| req.query.get(k).cloned().unwrap_or_default();
            let keyed = req.header("authorization") == Some(format!("Bearer {key}").as_str());
            let mut s = st.lock().expect("workos state");
            match (req.method.as_str(), req.path.as_str()) {
                ("POST", p) if p.starts_with("/data-integrations/") && p.ends_with("/authorize") => {
                    if !keyed {
                        return problem(401, "unauthorized", "the API key is wrong");
                    }
                    let provider = p["/data-integrations/".len()..p.len() - "/authorize".len()].to_string();
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                    let Some(user) = body["user_id"].as_str().filter(|u| users.knows(u)).map(str::to_string) else {
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
                    if !keyed {
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
                    if !keyed {
                        return problem(401, "unauthorized", "the API key is wrong");
                    }
                    let provider = p["/data-integrations/".len()..p.len() - "/token".len()].to_string();
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                    let Some(user) = body["user_id"].as_str().filter(|u| users.knows(u)).map(str::to_string) else {
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
        let users = authkit.directory();
        Ok(WorkOs { url: server.url.clone(), client_id: client_id.to_string(), authkit, users, state, _server: server })
    }

    /// The user an email signs in as (made the first time).
    pub fn user(&self, email: &str) -> User {
        self.users.user(email)
    }

    /// `email`'s account at `provider` is connected (`true`), or comes to
    /// need authorizing again (`false`).
    pub fn connect(&self, email: &str, provider: &str, authorized: bool) {
        let user = self.users.user(email);
        self.state.lock().expect("workos state").pipes.insert((user.id, provider.to_string()), authorized);
    }

    /// How many connected accounts' states were read.
    pub fn states_read(&self) -> u64 {
        self.state.lock().expect("workos state").states_read
    }

    /// The access tokens handed out for `provider`, oldest first.
    pub fn tokens(&self, provider: &str) -> Vec<String> {
        self.state.lock().expect("workos state").tokens.get(provider).cloned().unwrap_or_default()
    }
}
