//! An OpenID Connect provider, for the surface sign-in calls
//! (docs/self-host.md, seam 4): discovery, the JWKS, the authorization
//! endpoint, the token endpoint, and, where the provider has them, the
//! userinfo endpoint and RP-initiated logout (OpenID Connect Core 1.0,
//! Discovery 1.0, RP-Initiated Logout 1.0; PKCE, RFC 7636).
//!
//! It plays one of two providers (`Profile`):
//!
//! - **Strict**, as Keycloak or Dex are: PKCE with S256 required, the
//!   client authenticated with `client_secret_basic` or
//!   `client_secret_post`, id_tokens RS256 or ES256 (`sign_with`),
//!   RP-initiated logout, and people with a username and no email.
//! - **AuthKit**: WorkOS AuthKit's provider for an OAuth application
//!   (WorkOS Connect, https://workos.com/docs/authkit/connect/oauth), as
//!   its reference documents it at the AuthKit domain:
//!   - the metadata's fields exactly, with no `end_session_endpoint` and
//!     no client methods listed
//!     (https://workos.com/docs/reference/workos-connect/metadata/openid-configuration);
//!     the OAuth metadata beside it lists S256 and the client methods;
//!   - `/oauth2/authorize`: the nonce required; PKCE (S256 only) checked
//!     when the request carries it
//!     (https://workos.com/docs/reference/workos-connect/authorize);
//!   - `/oauth2/token`: the client's id and secret in the body (or Basic,
//!     which the OAuth metadata lists); an id_token RS256 with WorkOS's
//!     claims, its `sub` the user's id, `user_…`
//!     (https://workos.com/docs/reference/workos-connect/token), and an
//!     access token, a JWT;
//!   - `/oauth2/jwks`, and `/oauth2/userinfo`
//!     (https://workos.com/docs/reference/workos-connect/userinfo).
//!
//! Either provider holds a code to the redirect URI it was asked with and
//! spends it once.
//!
//! People are made on demand: an authorization request with `login_hint`
//! signs in as that person at once (an email makes one with that email; a
//! Strict provider's hint without `@` makes one with a username and no
//! email, as many directories have); without one, a browser gets a
//! one-field form. Levers: fail the next authorization, change an email,
//! rotate the keys, spoil the next id_token (`Spoil`), drop the logout
//! endpoint from the metadata, and the counts a test reads.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::Engine;
use rsa::pkcs1v15::SigningKey as RsaSigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::http::{Handler, Request, Response, Server};

/// An id_token lives this long.
const ID_TOKEN_TTL_S: i64 = 300;
/// The key generations published: the newest signs, the one before still
/// verifies (as Dex keeps its previous key).
const KEY_GENERATIONS: usize = 2;
/// The RSA keys' size.
const RSA_BITS: usize = 2048;

/// Which provider the fake plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// A strict provider, as Keycloak or Dex.
    Strict,
    /// WorkOS AuthKit's, for an OAuth application (WorkOS Connect).
    AuthKit,
}

/// Where a provider answers, under its issuer.
struct Paths {
    authorize: &'static str,
    /// The one-field form's own action (the fake's, no provider's).
    authorize_as: &'static str,
    token: &'static str,
    jwks: &'static str,
    userinfo: Option<&'static str>,
    logout: Option<&'static str>,
}

impl Profile {
    fn paths(self) -> Paths {
        match self {
            Profile::Strict => Paths { authorize: "/authorize", authorize_as: "/authorize/as", token: "/token", jwks: "/jwks", userinfo: None, logout: Some("/logout") },
            Profile::AuthKit => Paths {
                authorize: "/oauth2/authorize",
                authorize_as: "/oauth2/authorize/as",
                token: "/oauth2/token",
                jwks: "/oauth2/jwks",
                userinfo: Some("/oauth2/userinfo"),
                logout: None,
            },
        }
    }

    /// The metadata (Discovery 1.0, 3), as the provider publishes it.
    fn metadata(self, iss: &str, end_session: bool) -> Value {
        match self {
            Profile::Strict => {
                let mut doc = json!({
                    "issuer": iss,
                    "authorization_endpoint": format!("{iss}/authorize"),
                    "token_endpoint": format!("{iss}/token"),
                    "jwks_uri": format!("{iss}/jwks"),
                    "response_types_supported": ["code"],
                    "subject_types_supported": ["public"],
                    "id_token_signing_alg_values_supported": ["RS256", "ES256"],
                    "code_challenge_methods_supported": ["S256"],
                    "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
                    "scopes_supported": ["openid", "email", "profile"],
                    "claims_supported": ["sub", "iss", "aud", "exp", "iat", "nonce", "email", "email_verified", "name", "preferred_username"],
                });
                if end_session {
                    doc["end_session_endpoint"] = json!(format!("{iss}/logout"));
                }
                doc
            }
            // the reference's document, field for field
            Profile::AuthKit => json!({
                "issuer": iss,
                "authorization_endpoint": format!("{iss}/oauth2/authorize"),
                "grant_types_supported": ["authorization_code", "client_credentials", "refresh_token"],
                "id_token_signing_alg_values_supported": ["RS256"],
                "introspection_endpoint": format!("{iss}/oauth2/introspection"),
                "jwks_uri": format!("{iss}/oauth2/jwks"),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "token_endpoint": format!("{iss}/oauth2/token"),
                "userinfo_endpoint": format!("{iss}/oauth2/userinfo"),
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct User {
    /// The `sub`.
    pub id: String,
    pub email: Option<String>,
    pub username: String,
}

/// How the next id_token goes wrong (once), as a provider misconfigured,
/// or someone between it and the cell, would make it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spoil {
    /// Issued for another client.
    WrongAudience,
    /// Issued by another issuer.
    WrongIssuer,
    /// Past its `exp` (issued twelve minutes ago, for five).
    Expired,
    /// For another sign-in's nonce.
    WrongNonce,
    /// Its signature's bytes changed.
    BadSignature,
    /// `alg: none`, with no signature.
    AlgNone,
    /// HS256, keyed with the client's secret.
    Hs256,
    /// Signed by a key the JWKS never publishes.
    UnpublishedKey,
}

/// Which algorithm the provider signs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Es256,
}

/// A logout the provider was sent: its hint, and where it sent the browser.
#[derive(Clone, Debug)]
pub struct Logout {
    pub id_token_hint: Option<String>,
    pub client_id: Option<String>,
    pub back: Option<String>,
}

/// One key generation: an RSA key and an EC key, each with its kid.
struct Keys {
    generation: u32,
    rsa: rsa::RsaPrivateKey,
    ec: p256::ecdsa::SigningKey,
}

impl Keys {
    fn make(generation: u32) -> Keys {
        let rsa = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, RSA_BITS).expect("an RSA key");
        Keys { generation, rsa, ec: p256::ecdsa::SigningKey::random(&mut rand_core::OsRng) }
    }

    fn rsa_kid(&self) -> String {
        format!("rsa-{}", self.generation)
    }

    fn ec_kid(&self) -> String {
        format!("ec-{}", self.generation)
    }

    /// The published keys: an RS256 provider (AuthKit) publishes its RSA key alone.
    fn jwks(&self, profile: Profile) -> Vec<Value> {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let public = self.rsa.to_public_key();
        let mut keys = vec![json!({ "kty": "RSA", "use": "sig", "alg": "RS256", "kid": self.rsa_kid(), "n": b64.encode(public.n().to_bytes_be()), "e": b64.encode(public.e().to_bytes_be()) })];
        if profile == Profile::Strict {
            let point = self.ec.verifying_key().to_encoded_point(false);
            keys.push(json!({ "kty": "EC", "use": "sig", "alg": "ES256", "crv": "P-256", "kid": self.ec_kid(), "x": b64.encode(point.x().expect("x")), "y": b64.encode(point.y().expect("y")) }));
        }
        keys
    }

    /// A compact JWS of `claims` (`header` given), signed as its `alg` says.
    fn sign(&self, header: &Value, claims: &Value) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let input = format!("{}.{}", b64.encode(header.to_string()), b64.encode(claims.to_string()));
        let sig = match header["alg"].as_str() {
            Some("RS256") => RsaSigningKey::<Sha256>::new(self.rsa.clone()).sign(input.as_bytes()).to_vec(),
            Some("ES256") => {
                let s: p256::ecdsa::Signature = self.ec.sign(input.as_bytes());
                s.to_bytes().to_vec()
            }
            _ => panic!("the fake signs RS256 or ES256"),
        };
        format!("{input}.{}", b64.encode(sig))
    }
}

/// A code handed out at the authorization endpoint, spent at the token endpoint.
struct Code {
    user: String,
    redirect_uri: String,
    /// PKCE's S256 challenge (always, for a Strict provider).
    challenge: Option<String>,
    nonce: Option<String>,
}

struct State {
    profile: Profile,
    users: Vec<User>,
    codes: HashMap<String, Code>,
    /// Access tokens handed out → the user each names (`/oauth2/userinfo`).
    access: HashMap<String, String>,
    /// The newest last; at most `KEY_GENERATIONS`.
    keys: Vec<Keys>,
    /// A key never published (`Spoil::UnpublishedKey` signs with it).
    stray: Keys,
    alg: Alg,
    next_error: Option<String>,
    spoil: Option<Spoil>,
    end_session: bool,
    counter: u64,
    jwks_fetches: u64,
    /// How each token request authenticated, in order.
    auth: Vec<String>,
    logouts: Vec<Logout>,
}

impl State {
    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}-{:08}", self.counter)
    }

    /// The person a hint names, made the first time: `None` for a hint
    /// AuthKit does not take (its people are an email each).
    fn user_for(&mut self, hint: &str) -> Option<User> {
        let email = hint.contains('@').then(|| hint.to_string());
        if email.is_none() && self.profile == Profile::AuthKit {
            return None;
        }
        let found = self.users.iter().find(|u| match &email {
            Some(e) => u.email.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(e)),
            None => u.email.is_none() && u.username == hint,
        });
        if let Some(u) = found {
            return Some(u.clone());
        }
        let username = hint.split('@').next().unwrap_or(hint).to_string();
        let id = match self.profile {
            Profile::Strict => self.next("sub"),
            // WorkOS's user ids: `user_` and 26 characters
            Profile::AuthKit => {
                self.counter += 1;
                format!("user_{:026}", self.counter)
            }
        };
        let user = User { id, email, username };
        self.users.push(user.clone());
        Some(user)
    }

    fn signer(&self) -> &Keys {
        self.keys.last().expect("a key generation")
    }

    /// The claims about `user`, as the provider names them.
    fn person_claims(&self, user: &User, claims: &mut Value) {
        match self.profile {
            Profile::Strict => {
                claims["preferred_username"] = json!(user.username);
                claims["name"] = json!(format!("{} (fake)", user.username));
            }
            Profile::AuthKit => {
                claims["name"] = json!(format!("{} Fake", user.username));
                claims["given_name"] = json!(user.username);
                claims["family_name"] = json!("Fake");
            }
        }
        if let Some(email) = &user.email {
            claims["email"] = json!(email);
            claims["email_verified"] = json!(true);
        }
    }
}

/// The provider's people, for a service that shares them (the WorkOS
/// fake's Pipes, whose users are AuthKit's).
#[derive(Clone)]
pub struct Directory(Arc<Mutex<State>>);

impl Directory {
    /// Whether `id` is one of the provider's people.
    pub fn knows(&self, id: &str) -> bool {
        self.0.lock().expect("oidc state").users.iter().any(|u| u.id == id)
    }

    /// The person a hint signs in as (made the first time).
    pub fn user(&self, hint: &str) -> User {
        self.0.lock().expect("oidc state").user_for(hint).unwrap_or_else(|| panic!("this provider takes no person named {hint:?}"))
    }
}

pub struct Oidc {
    /// The issuer: the base URL, exactly as its id_tokens' `iss`.
    pub url: String,
    pub client_id: String,
    pub client_secret: String,
    pub profile: Profile,
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
    let mut u = url::Url::parse(base).expect("a redirect URI is a URL");
    u.query_pairs_mut().extend_pairs(pairs);
    u.into()
}

fn now_s() -> i64 {
    crate::codestorage::now_ms() / 1000
}

/// The client's id and secret, as the request authenticates with them,
/// and how: `client_secret_basic` (RFC 6749 2.3.1: each form-encoded, then
/// base64), `client_secret_post`, or `none`.
fn client_of(req: &Request, form: &HashMap<String, String>) -> Option<(String, Option<String>, &'static str)> {
    if let Some(basic) = req.header("authorization").and_then(|a| a.strip_prefix("Basic ")) {
        let decoded = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(basic).ok()?).ok()?;
        let (id, secret) = decoded.split_once(':')?;
        let dec = |s: &str| url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned());
        return Some((dec(id)?, Some(dec(secret)?), "client_secret_basic"));
    }
    let id = form.get("client_id")?.clone();
    match form.get("client_secret") {
        Some(secret) => Some((id, Some(secret.clone()), "client_secret_post")),
        None => Some((id, None, "none")),
    }
}

/// RFC 8414's metadata, which AuthKit publishes beside the OpenID one
/// (https://workos.com/docs/reference/workos-connect/metadata).
fn authkit_oauth_metadata(iss: &str) -> Value {
    json!({
        "authorization_endpoint": format!("{iss}/oauth2/authorize"),
        "code_challenge_methods_supported": ["S256"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "introspection_endpoint": format!("{iss}/oauth2/introspection"),
        "issuer": iss,
        "registration_endpoint": format!("{iss}/oauth2/register"),
        "scopes_supported": ["email", "offline_access", "openid", "profile"],
        "response_modes_supported": ["query"],
        "response_types_supported": ["code"],
        "token_endpoint": format!("{iss}/oauth2/token"),
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
    })
}

impl Oidc {
    /// A Strict provider on a free port for one client: its id and secret.
    pub fn start(client_id: &str, client_secret: &str) -> std::io::Result<Oidc> {
        Self::start_as(Profile::Strict, 0, client_id, client_secret)
    }

    pub fn start_on(port: u16, client_id: &str, client_secret: &str) -> std::io::Result<Oidc> {
        Self::start_as(Profile::Strict, port, client_id, client_secret)
    }

    /// `profile`'s provider on `port` (0: a free one) for one client.
    pub fn start_as(profile: Profile, port: u16, client_id: &str, client_secret: &str) -> std::io::Result<Oidc> {
        assert!(!client_id.is_empty() && !client_secret.is_empty(), "a provider's client has an id and a secret");
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let issuer = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let state = Arc::new(Mutex::new(State {
            profile,
            users: vec![],
            codes: HashMap::new(),
            access: HashMap::new(),
            keys: vec![Keys::make(1)],
            stray: Keys::make(0),
            alg: Alg::Rs256,
            next_error: None,
            spoil: None,
            end_session: profile.paths().logout.is_some(),
            counter: 0,
            jwks_fetches: 0,
            auth: vec![],
            logouts: vec![],
        }));
        let (st, client, secret, iss) = (Arc::clone(&state), client_id.to_string(), client_secret.to_string(), issuer.clone());
        let paths = profile.paths();
        let handler: Handler = Arc::new(move |req: &Request| {
            let mut s = st.lock().expect("oidc state");
            let path = req.path.as_str();
            match req.method.as_str() {
                "GET" if path == "/.well-known/openid-configuration" => Response::json(200, &profile.metadata(&iss, s.end_session)),
                "GET" if path == "/.well-known/oauth-authorization-server" && profile == Profile::AuthKit => Response::json(200, &authkit_oauth_metadata(&iss)),
                "GET" if path == paths.jwks => {
                    s.jwks_fetches += 1;
                    let keys: Vec<Value> = s.keys.iter().flat_map(|k| k.jwks(profile)).collect();
                    Response::json(200, &json!({ "keys": keys }))
                }
                "GET" if path == paths.authorize || path == paths.authorize_as => authorize(&mut s, req, &client, &paths),
                "POST" if path == paths.token => token(&mut s, req, &client, &secret, &iss),
                "GET" if Some(path) == paths.userinfo => {
                    let token = req.header("authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or_default();
                    let Some(user) = s.access.get(token).and_then(|id| s.users.iter().find(|u| u.id == *id)).cloned() else {
                        return problem(401, "invalid_token", "no such access token");
                    };
                    let mut claims = json!({ "sub": user.id });
                    s.person_claims(&user, &mut claims);
                    Response::json(200, &claims)
                }
                "GET" if Some(path) == paths.logout => {
                    let opt = |k: &str| req.query.get(k).cloned();
                    let logout = Logout { id_token_hint: opt("id_token_hint"), client_id: opt("client_id"), back: opt("post_logout_redirect_uri") };
                    let back = logout.back.clone();
                    s.logouts.push(logout);
                    match back {
                        Some(b) => redirect(&b),
                        None => Response::bytes(200, "text/plain", b"signed out".to_vec()),
                    }
                }
                _ => problem(404, "not_found", "no such route"),
            }
        });
        let server = Server::serve(listener, handler)?;
        assert_eq!(server.url, issuer, "the issuer is where the fake answers");
        Ok(Oidc { url: issuer, client_id: client_id.to_string(), client_secret: client_secret.to_string(), profile, state, _server: server })
    }

    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.state.lock().expect("oidc state"))
    }

    /// Its people, shared with a service that knows them too.
    pub fn directory(&self) -> Directory {
        Directory(Arc::clone(&self.state))
    }

    /// The person a hint signs in as (made the first time).
    pub fn user(&self, hint: &str) -> User {
        self.directory().user(hint)
    }

    /// A person's email changes; their `sub` does not.
    pub fn set_email(&self, id: &str, email: &str) {
        self.with(|s| {
            if let Some(u) = s.users.iter_mut().find(|u| u.id == id) {
                u.email = Some(email.to_string());
            }
        })
    }

    /// The next authorization redirects back with this error.
    pub fn fail_next(&self, error: &str) {
        self.with(|s| s.next_error = Some(error.to_string()))
    }

    /// The next id_token goes wrong this way.
    pub fn spoil_next(&self, how: Spoil) {
        self.with(|s| s.spoil = Some(how))
    }

    /// Signs with this algorithm from now on (ES256: a Strict provider's alone).
    pub fn sign_with(&self, alg: Alg) {
        assert!(alg == Alg::Rs256 || self.profile == Profile::Strict, "AuthKit signs RS256 alone");
        self.with(|s| s.alg = alg)
    }

    /// A new key generation signs from now on, published at once beside
    /// the one before (the oldest beyond `KEY_GENERATIONS` withdrawn).
    pub fn rotate_keys(&self) {
        let generation = self.with(|s| s.signer().generation + 1);
        let fresh = Keys::make(generation);
        self.with(|s| {
            s.keys.push(fresh);
            while s.keys.len() > KEY_GENERATIONS {
                s.keys.remove(0);
            }
        })
    }

    /// Whether the metadata offers RP-initiated logout (a Strict provider's).
    pub fn offer_logout(&self, on: bool) {
        assert!(self.profile == Profile::Strict, "AuthKit offers no RP-initiated logout");
        self.with(|s| s.end_session = on)
    }

    /// How many times the JWKS was fetched.
    pub fn jwks_fetches(&self) -> u64 {
        self.with(|s| s.jwks_fetches)
    }

    /// How each token request authenticated, oldest first.
    pub fn token_auth(&self) -> Vec<String> {
        self.with(|s| s.auth.clone())
    }

    /// The logouts the provider was sent, oldest first.
    pub fn logouts(&self) -> Vec<Logout> {
        self.with(|s| s.logouts.clone())
    }

    /// Codes handed out and not yet spent.
    pub fn codes_outstanding(&self) -> usize {
        self.with(|s| s.codes.len())
    }

    /// Where its authorization endpoint is.
    pub fn authorize_url(&self) -> String {
        format!("{}{}", self.url, self.profile.paths().authorize)
    }

    /// Where its RP-initiated logout is, when it has one.
    pub fn logout_url(&self) -> Option<String> {
        self.profile.paths().logout.map(|p| format!("{}{p}", self.url))
    }
}

/// The authorization endpoint: a code for the person `login_hint` names,
/// or the one-field form that asks.
fn authorize(s: &mut State, req: &Request, client: &str, paths: &Paths) -> Response {
    let q = |k: &str| req.query.get(k).cloned().unwrap_or_default();
    if q("client_id") != client {
        return problem(400, "invalid_client", "unknown client_id");
    }
    let back = q("redirect_uri");
    if q("response_type") != "code" || url::Url::parse(&back).is_err() {
        return problem(400, "invalid_request", "response_type=code and a redirect_uri");
    }
    if !q("scope").split(' ').any(|x| x == "openid") {
        return problem(400, "invalid_scope", "openid is required");
    }
    let refuse = |why: &str| redirect(&with_query(&back, &[("error", "invalid_request"), ("error_description", why), ("state", &q("state"))]));
    let challenge = Some(q("code_challenge")).filter(|c| !c.is_empty());
    let pkce_ok = match (&challenge, s.profile) {
        (None, Profile::Strict) => false,
        (None, Profile::AuthKit) => true,
        (Some(c), _) => q("code_challenge_method") == "S256" && c.len() == 43,
    };
    if !pkce_ok {
        return refuse(match s.profile {
            Profile::Strict => "PKCE with S256 is required",
            Profile::AuthKit => "code_challenge_method: only S256 is supported",
        });
    }
    let nonce = Some(q("nonce")).filter(|n| !n.is_empty());
    if nonce.is_none() && s.profile == Profile::AuthKit {
        return refuse("nonce is required");
    }
    if let Some(error) = s.next_error.take() {
        return redirect(&with_query(&back, &[("error", &error), ("error_description", "a failure the test asked for"), ("state", &q("state"))]));
    }
    let hint = q("login_hint");
    if hint.is_empty() {
        // a person at a browser: who are you?
        let fields: String = ["client_id", "redirect_uri", "response_type", "scope", "state", "nonce", "code_challenge", "code_challenge_method"]
            .iter()
            .map(|k| format!(r#"<input type="hidden" name="{k}" value="{}">"#, q(k).replace('&', "&amp;").replace('"', "&quot;")))
            .collect();
        let (title, who) = match s.profile {
            Profile::Strict => ("OpenID Connect (fake)", "an email, or a username with no email"),
            Profile::AuthKit => ("WorkOS AuthKit (fake)", "an email"),
        };
        let page = format!(
            r#"<!doctype html><title>{title}</title><form action="{}" method="get">
<p>{title}: sign in as ({who})</p>{fields}<input name="login_hint" value="dev@fragment.localhost" size="32"> <button>Sign in</button></form>"#,
            paths.authorize_as
        );
        return Response::bytes(200, "text/html; charset=utf-8", page.into_bytes());
    }
    let Some(user) = s.user_for(&hint) else { return refuse("AuthKit signs people in by their email") };
    let code = s.next("code");
    s.codes.insert(code.clone(), Code { user: user.id, redirect_uri: back.clone(), challenge, nonce });
    redirect(&with_query(&back, &[("code", &code), ("state", &q("state"))]))
}

/// The token endpoint: a code spent, for an id_token (spoiled, when a test
/// asked) and an access token.
fn token(s: &mut State, req: &Request, client: &str, secret: &str, iss: &str) -> Response {
    if !req.header("content-type").is_some_and(|c| c.starts_with("application/x-www-form-urlencoded")) {
        return problem(400, "invalid_request", "a token request is form-encoded");
    }
    let form: HashMap<String, String> = url::form_urlencoded::parse(&req.body).into_owned().collect();
    let Some((id, given, how)) = client_of(req, &form) else {
        return problem(401, "invalid_client", "no client authentication");
    };
    s.auth.push(how.to_string());
    if id != client || given.as_deref() != Some(secret) {
        return problem(401, "invalid_client", "the client id or secret is wrong");
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return problem(400, "unsupported_grant_type", "authorization_code only");
    }
    let Some(code) = form.get("code").and_then(|c| s.codes.remove(c)) else {
        return problem(400, "invalid_grant", "the code is invalid or was used");
    };
    if form.get("redirect_uri") != Some(&code.redirect_uri) {
        return problem(400, "invalid_grant", "the redirect_uri is not the authorization request's");
    }
    if let Some(challenge) = &code.challenge {
        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
        if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != *challenge {
            return problem(400, "invalid_grant", "the code_verifier does not match the code_challenge");
        }
    }
    let user = s.users.iter().find(|u| u.id == code.user).cloned().expect("a code names a user");
    let spoil = s.spoil.take();
    let now = now_s();
    let (iat, exp) = match spoil {
        Some(Spoil::Expired) => (now - 720, now - 420),
        _ => (now, now + ID_TOKEN_TTL_S),
    };
    let mut claims = json!({
        "iss": if spoil == Some(Spoil::WrongIssuer) { "https://another-issuer.example" } else { iss },
        "sub": user.id,
        "aud": if spoil == Some(Spoil::WrongAudience) { "another-client" } else { client },
        "exp": exp,
        "iat": iat,
    });
    if s.profile == Profile::Strict {
        claims["auth_time"] = json!(iat);
    }
    s.person_claims(&user, &mut claims);
    if let Some(nonce) = code.nonce {
        claims["nonce"] = json!(if spoil == Some(Spoil::WrongNonce) { "another-sign-in".to_string() } else { nonce });
    }
    let keys = if spoil == Some(Spoil::UnpublishedKey) { &s.stray } else { s.signer() };
    let (alg, kid) = match s.alg {
        Alg::Rs256 => ("RS256", keys.rsa_kid()),
        Alg::Es256 => ("ES256", keys.ec_kid()),
    };
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let id_token = match spoil {
        Some(Spoil::AlgNone) => format!("{}.{}.", b64.encode(json!({ "alg": "none", "typ": "JWT" }).to_string()), b64.encode(claims.to_string())),
        Some(Spoil::Hs256) => {
            use hmac::{Hmac, Mac};
            let input = format!("{}.{}", b64.encode(json!({ "alg": "HS256", "typ": "JWT", "kid": kid }).to_string()), b64.encode(claims.to_string()));
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("an HMAC key");
            mac.update(input.as_bytes());
            format!("{input}.{}", b64.encode(mac.finalize().into_bytes()))
        }
        _ => keys.sign(&json!({ "alg": alg, "typ": "JWT", "kid": kid }), &claims),
    };
    let id_token = match spoil {
        Some(Spoil::BadSignature) => {
            let (input, sig) = id_token.rsplit_once('.').expect("a JWS");
            let mut bytes = b64.decode(sig).expect("base64url");
            bytes[0] ^= 0x55;
            format!("{input}.{}", b64.encode(bytes))
        }
        _ => id_token,
    };
    let answer = match s.profile {
        Profile::Strict => {
            let access = s.next("access");
            s.access.insert(access.clone(), user.id.clone());
            json!({ "access_token": access, "token_type": "Bearer", "expires_in": ID_TOKEN_TTL_S, "id_token": id_token, "scope": "openid email profile" })
        }
        // a JWT, its claims the reference's access token's (its `sid` the consent)
        Profile::AuthKit => {
            let consent = s.next("consent");
            let signer = s.signer();
            let access = signer.sign(
                &json!({ "alg": "RS256", "typ": "JWT", "kid": signer.rsa_kid() }),
                &json!({ "iss": iss, "sub": user.id, "aud": client, "client_id": client, "sid": consent, "scope": "openid profile email", "exp": now + ID_TOKEN_TTL_S, "iat": now }),
            );
            let refresh = s.next("refresh");
            s.access.insert(access.clone(), user.id.clone());
            json!({ "access_token": access, "expires_in": ID_TOKEN_TTL_S, "id_token": id_token, "refresh_token": refresh, "token_type": "Bearer" })
        }
    };
    Response::json(200, &answer).with_header("cache-control", "no-store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_core::oidc::{self as rules, ClientAuth, Expect, Jwks, Provider};

    /// An answer: its status, its `location`, and its body.
    struct Answer {
        status: u16,
        location: String,
        body: Vec<u8>,
    }

    /// One request to the fake, over a connection of its own.
    fn call(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8]) -> Answer {
        use std::io::{Read, Write};
        let u = url::Url::parse(url).unwrap();
        let authority = format!("{}:{}", u.host_str().unwrap(), u.port().unwrap());
        let target = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_string(),
        };
        let mut stream = std::net::TcpStream::connect(&authority).unwrap();
        let mut head = format!("{method} {target} HTTP/1.1\r\nhost: {authority}\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        stream.write_all(format!("{head}\r\n").as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let location = head.lines().find_map(|l| l.strip_prefix("location: ")).unwrap_or_default().to_string();
        Answer { status, location, body: raw[split + 4..].to_vec() }
    }

    fn get(url: &str) -> (u16, Vec<u8>, String) {
        let r = call("GET", url, &[], b"");
        (r.status, r.body, r.location)
    }

    const BACK: &str = "http://127.0.0.1:1/auth/callback";

    /// The cell's half of a sign-in, by hand: the authorization request
    /// (PKCE, the nonce, `hint`), then the code exchanged as `auth` says,
    /// and spent again (refused). Answers the token answer's JSON.
    fn sign_in(fake: &Oidc, provider: &Provider, auth: ClientAuth, hint: &str, nonce: &str) -> Value {
        let verifier = "v".repeat(64);
        let to = rules::authorize_url(&rules::Authorize {
            provider,
            client_id: &fake.client_id,
            redirect_uri: BACK,
            scopes: rules::DEFAULT_SCOPES,
            state: "st",
            nonce,
            challenge: &rules::challenge(&verifier),
            login_hint: Some(hint),
        });
        let (status, _, back) = get(&to);
        assert_eq!(status, 302);
        let code = url::Url::parse(&back).unwrap().query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
        let req = rules::token_request(auth, &fake.client_id, Some(&fake.client_secret), &code, BACK, &verifier);
        let mut headers = vec![("content-type".to_string(), "application/x-www-form-urlencoded".to_string())];
        headers.extend(req.authorization.map(|a| ("authorization".to_string(), a)));
        let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let r = call("POST", &provider.token_endpoint, &headers, req.body.as_bytes());
        let again = call("POST", &provider.token_endpoint, &headers, req.body.as_bytes());
        assert_eq!(again.status, 400, "a code is spent once");
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        serde_json::from_slice(&r.body).unwrap()
    }

    fn verify(fake: &Oidc, provider: &Provider, answer: &Value, nonce: &str) -> Result<rules::Verified, rules::IdTokenError> {
        let token = rules::id_token_of(200, answer.to_string().as_bytes()).unwrap();
        let (_, keys, _) = get(&provider.jwks_uri);
        let expect = Expect { issuer: &fake.url, client_id: &fake.client_id, nonce, now_s: now_s(), algs: &provider.algs };
        rules::verify(&token, &Jwks::parse(&keys).unwrap(), &expect)
    }

    // Goal: the Strict fake is a provider the cell's own rules accept: its
    // metadata parses, its code exchange takes PKCE and Basic, and its
    // id_token verifies against its JWKS, with RS256, with ES256, and
    // after a rotation. Method: the cell's half of the flow, by hand.
    #[test]
    fn its_id_tokens_verify_by_the_cells_rules() {
        let fake = Oidc::start("client-1", "secret-1").unwrap();
        let (_, doc, _) = get(&rules::discovery_url(&fake.url));
        let provider = Provider::parse(&fake.url, &doc).unwrap();
        let auth = provider.client_auth(None, true).unwrap();
        let v = verify(&fake, &provider, &sign_in(&fake, &provider, auth, "jane", "n1"), "n1").unwrap();
        let person = rules::ClaimMap::default().person(&v);
        assert_eq!((person.email, person.handle.as_str()), (None, "jane"), "a username, and no email assumed");
        fake.sign_with(Alg::Es256);
        assert!(verify(&fake, &provider, &sign_in(&fake, &provider, auth, "jane", "n2"), "n2").is_ok());
        fake.rotate_keys();
        assert!(verify(&fake, &provider, &sign_in(&fake, &provider, auth, "jane", "n3"), "n3").is_ok());
        assert_eq!(fake.token_auth().first().map(String::as_str), Some("client_secret_basic"));
        fake.spoil_next(Spoil::WrongAudience);
        assert_eq!(verify(&fake, &provider, &sign_in(&fake, &provider, auth, "jane", "n4"), "n4").unwrap_err(), rules::IdTokenError::Audience);
        fake.spoil_next(Spoil::UnpublishedKey);
        assert!(matches!(verify(&fake, &provider, &sign_in(&fake, &provider, auth, "jane", "n5"), "n5").unwrap_err(), rules::IdTokenError::UnknownKey(_)));
    }

    // Goal: the AuthKit fake is WorkOS's documented provider, and the
    // cell's rules take it as one more issuer: its metadata (no logout, no
    // client methods listed) parses; the client in the body; an id_token
    // RS256 whose `sub` is a WorkOS user id, with WorkOS's claims; the
    // userinfo endpoint names the same user. Method: the cell's half of
    // the flow, by hand; then what AuthKit refuses.
    #[test]
    fn authkit_is_one_more_issuer_by_the_cells_rules() {
        let fake = Oidc::start_as(Profile::AuthKit, 0, "client_app_1", "sk_app_1").unwrap();
        let (_, doc, _) = get(&rules::discovery_url(&fake.url));
        let provider = Provider::parse(&fake.url, &doc).unwrap();
        assert!(provider.end_session_endpoint.is_none() && provider.algs == vec![rules::Alg::Rs256]);
        assert_eq!(provider.authorization_endpoint, format!("{}/oauth2/authorize", fake.url));
        let (status, oauth, _) = get(&format!("{}/.well-known/oauth-authorization-server", fake.url));
        let oauth: Value = serde_json::from_slice(&oauth).unwrap();
        assert!(status == 200 && oauth["code_challenge_methods_supported"] == json!(["S256"]));
        let auth = provider.client_auth(Some(ClientAuth::Post), true).unwrap();
        let answer = sign_in(&fake, &provider, auth, "jane@example.com", "n1");
        let v = verify(&fake, &provider, &answer, "n1").unwrap();
        assert!(v.subject.starts_with("user_") && v.subject.len() == 31, "{}", v.subject);
        assert_eq!(v.subject, fake.user("jane@example.com").id, "the same person signs in again under the same id");
        let person = rules::ClaimMap::default().person(&v);
        assert_eq!((person.email.as_deref(), person.name.as_deref()), (Some("jane@example.com"), Some("jane Fake")));
        assert_eq!(v.claims["given_name"], "jane");
        assert!(fake.token_auth().iter().all(|a| a == "client_secret_post"), "{:?}", fake.token_auth());
        let access = answer["access_token"].as_str().unwrap();
        assert_eq!(access.split('.').count(), 3, "AuthKit's access token is a JWT");
        let info = call("GET", &format!("{}/oauth2/userinfo", fake.url), &[("authorization", &format!("Bearer {access}"))], b"");
        let info: Value = serde_json::from_slice(&info.body).unwrap();
        assert_eq!((info["sub"].as_str(), info["email"].as_str()), (Some(v.subject.as_str()), Some("jane@example.com")));
        // the nonce is required; PKCE need not come, and only S256 does
        let base = format!("{}?response_type=code&client_id=client_app_1&redirect_uri={}&scope=openid&state=s&login_hint=jane%40example.com", fake.authorize_url(), url::form_urlencoded::byte_serialize(BACK.as_bytes()).collect::<String>());
        let (_, _, to) = get(&base);
        assert!(to.contains("error=invalid_request"), "{to}");
        let (_, _, to) = get(&format!("{base}&nonce=n"));
        assert!(to.contains("code="), "{to}");
        let (_, _, to) = get(&format!("{base}&nonce=n&code_challenge={}&code_challenge_method=plain", "a".repeat(43)));
        assert!(to.contains("error=invalid_request"), "{to}");
        let (_, _, to) = get(&format!("{base}&nonce=n").replace("jane%40example.com", "jdoe"));
        assert!(to.contains("error=invalid_request"), "AuthKit's people are an email each: {to}");
        assert!(fake.logout_url().is_none());
    }
}
