//! The fragment platform on celld, in Rust.
//!
//! The router (this file's `fetch`) verifies NIP-98, asks the registry
//! (`registry.rs`) which identity the signing key belongs to, bounds request
//! bodies, decides which fragment a request is for, and hands it to that
//! fragment's supervisor (`fragment.rs`) with the identity and the key.
//! A signed request the registry cannot answer for is refused (503), never
//! let through. Public routes:
//!
//!   POST   /api/identities                 register an agent the signer owns
//!                                          ({kind: agent, proof}: a key proof by its key)
//!   GET    /api/identities/<id|me>         an identity, as it or its owner sees it
//!   POST   /api/identities/<id|me>/keys    add a key ({proof}: a key proof by the new key)
//!   DELETE /api/identities/<id|me>/keys/<npub>   revoke one
//!   GET    /api/identities/<id>/keys/<npub>      {active}: for the identity and its agents
//!   GET    /api/budget                     the signer's month: allowance, spent, reserved, recent usage
//!   GET    /api/budget/usage?period=       a month's usage rows (FIN-10's shape)
//!   POST   /api/budget/<id>/top-up         {usd}: operators (FRAGMENT_OPERATORS)
//!   POST   /api/budget/key                 an agent: its owner's org OpenRouter key, for its turns
//!   POST   /api/fragments                  create (signed; the signer owns it)
//!   GET    /api/fragments                  the fragments the signer belongs to
//!   *      /api/agents, /api/a/...         the agents' script (agent/), co-hosted: passed through
//!   DELETE /api/f/<name>                   delete (owner)
//!   *      /api/f/<name>/<route>           the control API (signed; the code.storage webhook is HMAC,
//!                                          the inbox is its token)
//!   *      <name>.<suffix>/<path>          the fragment's site, on its own origin (every path)
//!   *      /f/<name>/<path>                the same, when no suffix is configured (dev)
//!   GET    /, /auth/…, /cli                sign-in on the platform origin (`auth.rs`)
//!
//! A browser on a fragment's origin is its person through that origin's own
//! session cookie (`__signin`), looked up live like a key.
//!
//! With a suffix configured, `/f/<name>/…` redirects to the fragment's own
//! host: fragments sharing one origin could act as each other's visitors.
//! `__watch` and `__live` stay reachable there for the CLI, which carries
//! no cookies.

mod ai;
mod auth;
mod blobs;
mod config;
mod channels;
mod deliveries;
mod cs;
mod error;
mod files;
mod fragment;
mod jobs;
mod js;
mod keys;
mod ledger;
mod live;
mod members;
mod ops;
mod plane;
mod principal;
mod publish;
mod push;
mod registry;
mod serve;
mod subscriptions;

use fragment_core::npub;
use fragment_proto::{limits, valid_fragment_name, AddKey, CreateFragment, ErrorBody, ErrorCode, IdentityKind, Register};
use serde_json::{json, Value};
use worker::*;

use config::Config;
use error::{CellError, CellResult};
use fragment::{KEY_HEADER, KIND_HEADER, NAME_HEADER, OWNER_HEADER, PRINCIPAL_HEADER, URL_HEADER};
use serve::MODE_HEADER;

pub use fragment::FragmentCell;
pub use principal::PrincipalCell;
pub use ledger::LedgerCell;
pub use registry::RegistryCell;

/// Client headers a fragment's supervisor sees; everything else, and any
/// `x-fragment-*` a client sends, stays at the router.
const PASSED_HEADERS: [&str; 8] =
    ["content-type", "cookie", "accept", "if-none-match", "upgrade", "x-pierre-event", "x-pierre-signature", "range"];

/// A WebSocket upgrade's own handshake, passed too: when the fragment's cell
/// lives on another node, celld tunnels the upgrade there and answers the
/// client's key itself, so it needs the client's handshake, not just
/// `Upgrade` (without these, every live socket that lands on the other node
/// fails: 502 "the cell accepted a WebSocket for a request that did not
/// upgrade").
const WEBSOCKET_HEADERS: [&str; 4] = ["sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"];

#[event(queue)]
async fn queue(batch: MessageBatch<deliveries::Delivery>, env: Env, _ctx: Context) -> Result<()> {
    deliveries::consume(batch, env).await
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match route(req, &env).await {
        Ok(resp) => Ok(resp),
        Err(e) => e.response(),
    }
}

async fn read_body(req: &mut Request) -> CellResult<Vec<u8>> {
    let declared: usize = req.headers().get("content-length")?.and_then(|l| l.parse().ok()).unwrap_or(0);
    if declared > limits::BODY_MAX_BYTES {
        return Err(CellError::too_large("request body", declared, limits::BODY_MAX_BYTES));
    }
    let body = req.bytes().await?;
    if body.len() > limits::BODY_MAX_BYTES {
        return Err(CellError::too_large("request body", body.len(), limits::BODY_MAX_BYTES));
    }
    Ok(body)
}

/// The key that signed the request (NIP-98), not yet resolved.
fn authenticate(req: &Request, url: &Url, body: &[u8]) -> CellResult<String> {
    let header = req.headers().get("authorization")?;
    let now_s = js::now_ms() / 1000;
    fragment_nip98::verify(header.as_deref(), req.method().as_ref(), url.as_str(), body, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::new(ErrorCode::Unauthenticated, e.to_string()))
}

/// Who is asking: the identity the registry says holds the signing key,
/// or a browser's session names (no key then).
pub(crate) struct Signer {
    pub key: Option<String>,
    pub id: String,
    pub kind: IdentityKind,
    pub owner: Option<String>,
    /// A person's username; an agent's owner's: where its fragments go.
    pub username: Option<String>,
}

/// Asks the registry cell. Its refusals pass through; not reaching it, or
/// a failure inside it, is `registry_unavailable`: nothing signed is
/// decided without it (docs/finite-integration.md, rule 7).
pub(crate) async fn ask_registry(env: &Env, path: &str, body: &Value) -> CellResult<Value> {
    let unavailable = |why: String| CellError::new(ErrorCode::RegistryUnavailable, format!("the identity registry did not answer ({why}); try again shortly"));
    let asked = async {
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(body.to_string().into()));
        let req = Request::new_with_init(&format!("https://registry.internal{path}"), &init)?;
        let mut resp = env.durable_object("REGISTRY")?.get_by_name(registry::NAME)?.fetch_with_request(req).await?;
        let status = resp.status_code();
        let bytes = resp.bytes().await?;
        Ok::<_, worker::Error>((status, bytes))
    };
    let (status, bytes) = asked.await.map_err(|e| unavailable(e.to_string()))?;
    if status == 200 {
        return serde_json::from_slice(&bytes).map_err(|e| unavailable(format!("its answer: {e}")));
    }
    match serde_json::from_slice::<ErrorBody>(&bytes) {
        Ok(e) if status < 500 => Err(CellError::new(e.error, e.message)),
        Ok(e) => Err(unavailable(e.message)),
        Err(_) => Err(unavailable(format!("status {status}"))),
    }
}

pub(crate) fn facts_of(v: &Value) -> CellResult<(String, IdentityKind, Option<String>)> {
    let id = v["id"].as_str().filter(|i| npub::is_identity(i)).ok_or_else(|| CellError::host("the registry named no identity"))?;
    let kind = v["kind"].as_str().and_then(IdentityKind::parse).ok_or_else(|| CellError::host("the registry named no kind"))?;
    Ok((id.to_string(), kind, v["owner"].as_str().map(str::to_string)))
}

/// The signer the registry's facts describe.
pub(crate) fn signer_of(v: &Value, key: Option<String>) -> CellResult<Signer> {
    let (id, kind, owner) = facts_of(v)?;
    Ok(Signer { key, id, kind, owner, username: v["username"].as_str().map(str::to_string) })
}

async fn resolve(env: &Env, key: String) -> CellResult<Signer> {
    let v = ask_registry(env, "/resolve", &json!({ "key": key })).await?;
    signer_of(&v, Some(key))
}

/// The signer of a request that must be signed, resolved.
async fn signer(env: &Env, req: &Request, url: &Url, body: &[u8]) -> CellResult<Signer> {
    let key = authenticate(req, url, body)?;
    resolve(env, key).await
}

/// The signer when the request carries a signature (a bad one is still a 401).
async fn signer_if_signed(env: &Env, req: &Request, url: &Url, body: &[u8]) -> CellResult<Option<Signer>> {
    match req.headers().get("authorization")? {
        Some(_) => signer(env, req, url, body).await.map(Some),
        None => Ok(None),
    }
}

fn test_hooks(env: &Env) -> bool {
    Config::from_env(env).test_hooks
}

/// The identity a path names: `me` is the signer.
fn named_identity(who: &str, signer: &Signer) -> CellResult<String> {
    if who == "me" {
        return Ok(signer.id.clone());
    }
    if npub::is_identity(who) {
        return Ok(who.to_string());
    }
    Err(CellError::invalid(format!("{who:?} is not an identity (id:…) or `me`")))
}

/// A create's name under the creator's username: a bare label goes under
/// it, and a qualified name must already be under it.
/// Makes a fragment for a person, under their username: the API's create
/// and the platform's "new" page.
pub(crate) async fn create_fragment(env: &Env, cfg: &Config, url: &Url, mut create: CreateFragment, principal: Signer) -> CellResult<Response> {
    if principal.kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "fragments are made by people"));
    }
    let username = principal.username.clone().ok_or_else(|| CellError::invalid(format!("choose a username first (sign in at {}/)", cfg.platform(url))))?;
    create.name = qualify(&create.name, &username)?;
    let body = serde_json::to_vec(&create).map_err(|e| CellError::host(e.to_string()))?;
    // a fresh request: nothing of the caller's but what the router decided
    let bare = Request::new(url.as_str(), Method::Post)?;
    let f = Forward { name: &create.name, inner: "/create".into(), principal: Some(principal), mode: None, extra: vec![] };
    forward(env, &bare, url, bytes_body(body), f).await
}

fn qualify(name: &str, username: &str) -> CellResult<String> {
    if fragment_proto::valid_label(name) {
        return Ok(fragment_proto::fragment_name(name, username));
    }
    match fragment_proto::split_fragment_name(name) {
        Some((_, u)) if u == username => Ok(name.to_string()),
        Some(_) => Err(CellError::new(ErrorCode::Forbidden, format!("you make fragments under your own username ({username})"))),
        None => Err(CellError::invalid(
            "a fragment's name is a label (lowercase letters, digits, and single dashes, at most 63), optionally followed by .<your username>",
        )),
    }
}

/// A picture's type from its first bytes (a request's content-type is not trusted).
pub(crate) fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// `GET /api/users/<username>` and `.../picture`: anyone may see who a
/// username is, and their picture.
async fn users(env: &Env, rest: &[&str]) -> CellResult<Response> {
    let [username, tail @ ..] = rest else { return Err(CellError::new(ErrorCode::NotFound, "name a username")) };
    if !fragment_proto::valid_username(username) {
        return Err(CellError::new(ErrorCode::NotFound, format!("no one is {username}")));
    }
    let v = ask_registry(env, "/username/lookup", &json!({ "username": username })).await?;
    let picture = v["picture"]["sha"].as_str().map(|sha| format!("/api/users/{username}/picture?v={}", &sha[..12]));
    match tail {
        [] => json_answer(&json!({ "id": v["id"], "kind": v["kind"], "username": username, "picture": picture })),
        ["picture"] => {
            let (Some(sha), Some(mime)) = (v["picture"]["sha"].as_str(), v["picture"]["mime"].as_str()) else {
                return Err(CellError::new(ErrorCode::NotFound, format!("{username} has no picture")));
            };
            let blob = js::blob_get(env.as_ref(), &format!("pictures/{sha}"), None).await?.ok_or_else(|| CellError::host("a picture's bytes are missing"))?;
            let headers = Headers::new();
            headers.set("content-type", mime)?;
            headers.set("cache-control", "public, max-age=300")?;
            headers.set("x-content-type-options", "nosniff")?;
            Ok(Response::from_body(ResponseBody::Stream(blob.body))?.with_headers(headers))
        }
        _ => Err(CellError::new(ErrorCode::NotFound, "no such route")),
    }
}

fn key_in_path(k: &str) -> CellResult<String> {
    npub::parse(k).ok_or_else(|| CellError::invalid(format!("{k:?} is not an npub or a 64-hex key")))
}

/// The key a key proof in a body proves, for this request and its signer.
fn proven_key(proof: &str, req: &Request, url: &Url, signer_key: &str) -> CellResult<String> {
    let now_s = js::now_ms() / 1000;
    let key = fragment_nip98::verify_proof(proof, req.method().as_ref(), url.as_str(), signer_key, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::invalid(format!("proof: {e}")))?;
    if key == signer_key {
        return Err(CellError::invalid("proof: the new key must not be the key that signs the request"));
    }
    Ok(key)
}

fn json_answer(v: &Value) -> CellResult<Response> {
    Ok(Response::from_json(v)?)
}

/// Whose budget a signer sees: a person's own org; an agent's owner's.
fn billing_org(who: &Signer) -> CellResult<String> {
    let person = match who.kind {
        IdentityKind::Person => who.id.as_str(),
        IdentityKind::Agent => who.owner.as_deref().ok_or_else(|| CellError::host("an agent without an owner"))?,
    };
    ledger::org_of(person).ok_or_else(|| CellError::host("no billing org"))
}

/// `/api/budget…`: a billing org's month (ledger.rs).
async fn budget_route(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let body = read_body(&mut req).await?;
    let who = signer(env, &req, url, &body).await?;
    match (req.method(), rest) {
        (Method::Get, []) => json_answer(&ledger::ask(env, &billing_org(&who)?, Method::Get, "/status", None).await?),
        (Method::Get, ["usage"]) => {
            let period = url.query_pairs().find(|(k, _)| k == "period").map(|(_, v)| v.into_owned());
            let path = match period {
                Some(p) if p.len() == 7 && p.as_bytes()[4] == b'-' && p.bytes().enumerate().all(|(i, b)| i == 4 || b.is_ascii_digit()) => format!("/usage?period={p}"),
                Some(_) => return Err(CellError::invalid("period is YYYY-MM")),
                None => "/usage".to_string(),
            };
            json_answer(&ledger::ask(env, &billing_org(&who)?, Method::Get, &path, None).await?)
        }
        // an agent's turns spend its owner's month: their org's OpenRouter
        // key, whose limit is the allowance (OpenRouter stops it there)
        (Method::Post, ["key"]) => {
            if who.kind != IdentityKind::Agent {
                return Err(CellError::new(ErrorCode::Forbidden, "only an agent asks for its owner's model key"));
            }
            json_answer(&ledger::ask(env, &billing_org(&who)?, Method::Post, "/key", None).await?)
        }
        (Method::Post, [id, "top-up"]) => {
            if !cfg.is_operator(who.key.as_deref(), &who.id)? {
                return Err(CellError::new(ErrorCode::Forbidden, "only the fleet's operators top up budgets"));
            }
            let id = named_identity(id, &who)?;
            let org = ledger::org_of(&id).ok_or_else(|| CellError::invalid("name a person"))?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let usd = v["usd"].as_f64().filter(|u| u.is_finite() && *u > 0.0).ok_or_else(|| CellError::invalid("usd is a positive number of dollars"))?;
            let micros = fragment_core::budget::micros(usd);
            json_answer(&ledger::ask(env, &org, Method::Post, "/top-up", Some(&json!({ "micros": micros, "by": who.id }))).await?)
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// `/api/identities…`: the registry's public face.
async fn identities(mut req: Request, env: &Env, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let body = read_body(&mut req).await?;
    let method = req.method();
    if let (Method::Post, []) = (&method, rest) {
        let reg: Register = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
        return match reg.kind {
            IdentityKind::Person => Err(CellError::invalid("people sign in: `fragment login` adds a key to you")),
            // FIN-11's trusted initial registration: the owner signs, and the
            // agent's key proves itself inside
            IdentityKind::Agent => {
                let owner = signer(env, &req, url, &body).await?;
                let owner_key = owner.key.clone().expect("a signed request has a key");
                let proof = reg.proof.ok_or_else(|| CellError::invalid("registering an agent needs a proof by its key"))?;
                let key = proven_key(&proof, &req, url, &owner_key)?;
                json_answer(&ask_registry(env, "/agents", &json!({ "owner": owner.id, "key": key })).await?)
            }
        };
    }
    let who = signer(env, &req, url, &body).await?;
    match (method, rest) {
        (Method::Put, ["me", "username"]) => {
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let username = v["username"].as_str().ok_or_else(|| CellError::invalid("{username}"))?;
            json_answer(&ask_registry(env, "/username/claim", &json!({ "identity": who.id, "username": username })).await?)
        }
        (Method::Put, ["me", "picture"]) => {
            if body.len() > limits::PICTURE_MAX_BYTES {
                return Err(CellError::too_large("a picture", body.len(), limits::PICTURE_MAX_BYTES));
            }
            let mime = picture_type(&body).ok_or_else(|| CellError::invalid("a picture is a PNG, JPEG, WebP, or GIF"))?;
            let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&body));
            js::blob_put_bytes(env.as_ref(), &format!("pictures/{sha}"), &body).await?;
            json_answer(&ask_registry(env, "/picture/set", &json!({ "identity": who.id, "sha": sha, "mime": mime })).await?)
        }
        (Method::Get, [id]) => {
            let id = named_identity(id, &who)?;
            json_answer(&ask_registry(env, "/view", &json!({ "identity": id, "by": who.id })).await?)
        }
        (Method::Post, [id, "keys"]) => {
            let id = named_identity(id, &who)?;
            let add: AddKey = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let key = proven_key(&add.proof, &req, url, who.key.as_deref().expect("a signed request has a key"))?;
            json_answer(&ask_registry(env, "/keys", &json!({ "identity": id, "key": key, "by": who.id })).await?)
        }
        (Method::Delete, [id, "keys", k]) => {
            let id = named_identity(id, &who)?;
            let key = key_in_path(k)?;
            json_answer(&ask_registry(env, "/revoke", &json!({ "identity": id, "key": key, "by": who.id })).await?)
        }
        (Method::Get, [id, "keys", k]) => {
            let id = named_identity(id, &who)?;
            let key = key_in_path(k)?;
            json_answer(&ask_registry(env, "/check", &json!({ "identity": id, "key": key, "by": who.id })).await?)
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// A fragment named in an API path: `<label>.<username>`, or a bare label
/// for a signed caller's own (under its username; an agent's owner's).
fn named_fragment(name: &str, signer: Option<&Signer>) -> CellResult<String> {
    if valid_fragment_name(name) {
        return Ok(name.to_string());
    }
    if !fragment_proto::valid_label(name) {
        return Err(CellError::invalid("a fragment's name is <label>.<username>"));
    }
    match signer.and_then(|s| s.username.as_deref()) {
        Some(username) => Ok(fragment_proto::fragment_name(name, username)),
        None => Err(CellError::new(ErrorCode::NotFound, format!("no fragment {name}: name it as {name}.<username>"))),
    }
}

fn check_name(name: &str) -> CellResult<()> {
    if valid_fragment_name(name) {
        Ok(())
    } else {
        Err(CellError::invalid("a fragment's name is <label>.<username>"))
    }
}

struct Forward<'a> {
    name: &'a str,
    inner: String,
    principal: Option<Signer>,
    mode: Option<&'a str>,
    /// Headers this route passes on purpose (the inbox's token and hop count).
    extra: Vec<(&'static str, String)>,
}

/// Hands a request to the fragment's supervisor.
/// Bytes the router read, as a body to forward.
fn bytes_body(body: Vec<u8>) -> Option<worker::wasm_bindgen::JsValue> {
    (!body.is_empty()).then(|| worker::js_sys::Uint8Array::from(body.as_slice()).into())
}

async fn forward(env: &Env, req: &Request, url: &Url, body: Option<worker::wasm_bindgen::JsValue>, f: Forward<'_>) -> CellResult<Response> {
    let headers = Headers::new();
    for k in PASSED_HEADERS {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    if req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
        headers.set("connection", "Upgrade")?;
        for k in WEBSOCKET_HEADERS {
            if let Some(v) = req.headers().get(k)? {
                headers.set(k, &v)?;
            }
        }
    }
    headers.set(NAME_HEADER, f.name)?;
    headers.set(URL_HEADER, url.as_str())?;
    if let Some(p) = &f.principal {
        headers.set(PRINCIPAL_HEADER, &p.id)?;
        if let Some(k) = &p.key {
            headers.set(KEY_HEADER, k)?;
        }
        headers.set(KIND_HEADER, p.kind.as_str())?;
        if let Some(o) = &p.owner {
            headers.set(OWNER_HEADER, o)?;
        }
    }
    if let Some(m) = f.mode {
        headers.set(MODE_HEADER, m)?;
    }
    for (k, v) in &f.extra {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers);
    if body.is_some() {
        init.with_body(body);
    }
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let inner = Request::new_with_init(&format!("https://fragment.internal{}{query}", f.inner), &init)?;
    let stub = env.durable_object("FRAGMENT")?.get_by_name(f.name)?;
    Ok(stub.fetch_with_request(inner).await?)
}

async fn serve(mut req: Request, env: &Env, url: &Url, name: &str, rest: &str, mode: &'static str) -> CellResult<Response> {
    check_name(name)?;
    if auth::is_fragment_route(rest) {
        return auth::fragment(&req, env, &Config::from_env(env), url, name, rest, mode == "path").await;
    }
    let body = read_body(&mut req).await?;
    // a signature names its key's identity; a browser, its session here
    let principal = match signer_if_signed(env, &req, url, &body).await? {
        Some(s) => Some(s),
        None => auth::site_session(&req, env, name).await?,
    };
    let f = Forward { name, inner: format!("/serve/{rest}"), principal, mode: Some(mode), extra: vec![] };
    forward(env, &req, url, bytes_body(body), f).await
}

/// The URL a request arrived on, as its client named it. A proxy that ends
/// TLS in front of celld (Fly's) forwards plain HTTP and says so in
/// `x-forwarded-proto`; signatures, links, and cookies name the https URL.
fn arrived_url(req: &Request) -> CellResult<Url> {
    let mut url = req.url()?;
    if url.scheme() == "http" && req.headers().get("x-forwarded-proto")?.as_deref() == Some("https") {
        url.set_scheme("https").map_err(|_| CellError::host("could not name the https URL"))?;
    }
    Ok(url)
}

async fn route(mut req: Request, env: &Env) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let url = arrived_url(&req)?;
    let path = url.path().to_string();
    // A fragment's own host is all its own (`/api/…` included: apps have
    // routes there); the platform API answers on the platform's host.
    if let Some(name) = url.host_str().and_then(|h| cfg.fragment_of_host(h)) {
        let rest = path.trim_start_matches('/').to_string();
        return serve(req, env, &url, &name, &rest, "host").await;
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (req.method(), segments.as_slice()) {
        (_, [""] | ["auth", ..] | ["cli"] | ["cli", "approve"]) => {
            let segs = segments.clone();
            auth::platform(req, env, &cfg, &url, &segs).await
        }
        (Method::Get, ["healthz"]) => {
            let mut resp = Response::ok("ok")?;
            resp.headers_mut().set("x-fragment-deploy", &cfg.deploy_id)?;
            Ok(resp)
        }
        (Method::Post, ["api", "fragments"]) => {
            let body = read_body(&mut req).await?;
            let create: CreateFragment = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let principal = signer(env, &req, &url, &body).await?;
            create_fragment(env, &cfg, &url, create, principal).await
        }
        // the agents' script, co-hosted: it checks its own requests (NIP-98
        // by the owner, or an inbox's token), so they pass through as they came
        (_, ["api", "agents"]) | (_, ["api", "a", ..]) => js::service_fetch(env.as_ref(), "AGENTS", req).await,
        (Method::Get, ["api", "fragments"]) => {
            let principal = signer(env, &req, &url, &[]).await?;
            let list = Request::new("https://principal.internal/list", Method::Get)?;
            Ok(env.durable_object("PRINCIPAL")?.get_by_name(&principal.id)?.fetch_with_request(list).await?)
        }
        (_, ["api", "budget", rest @ ..]) => {
            let rest = rest.to_vec();
            budget_route(req, env, &cfg, &url, &rest).await
        }
        (Method::Post, ["api", "test", "ledger"]) if test_hooks(env) => {
            let body = read_body(&mut req).await?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let org = v["identity"].as_str().and_then(ledger::org_of).ok_or_else(|| CellError::invalid("name an identity"))?;
            json_answer(&ledger::ask(env, &org, Method::Post, "/test", Some(&json!({ "offsetMs": v["offsetMs"] }))).await?)
        }
        (Method::Get, ["api", "users", rest @ ..]) => {
            let rest = rest.to_vec();
            users(env, &rest).await
        }
        (_, ["api", "identities", rest @ ..]) => {
            let rest = rest.to_vec();
            identities(req, env, &url, &rest).await
        }
        (Method::Get, ["api", "test", "env"]) if test_hooks(env) => json_answer(&Value::Object(js::env_vars(env.as_ref())?)),
        (Method::Post, ["api", "test", "keys"]) if test_hooks(env) => {
            let body = read_body(&mut req).await?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let name = v["fragment"].as_str().unwrap_or("");
            check_name(name)?;
            let headers = Headers::new();
            headers.set(fragment::NAME_HEADER, name)?;
            headers.set(fragment::URL_HEADER, url.as_str())?;
            let mut init = RequestInit::new();
            init.with_method(Method::Post).with_headers(headers).with_body(Some(v.to_string().into()));
            let inner = Request::new_with_init("https://fragment.internal/test/keys", &init)?;
            Ok(env.durable_object("FRAGMENT")?.get_by_name(name)?.fetch_with_request(inner).await?)
        }
        (Method::Post, ["api", "test", "registry"]) if test_hooks(env) => {
            let body = read_body(&mut req).await?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_answer(&ask_registry(env, "/test", &v).await?)
        }
        // A blob's bytes stream through: the router never holds them. The
        // signature covers the URL, which names the bytes' hash; the
        // fragment checks the hash as they arrive.
        (Method::Put, ["api", "f", name, "blobs", sha]) => {
            let declared: Option<u64> = req.headers().get("content-length")?.and_then(|l| l.parse().ok());
            match declared {
                None => return Err(CellError::invalid("a blob upload declares its content-length")),
                Some(n) if n > limits::BLOB_MAX_BYTES => return Err(CellError::too_large("a blob", n as usize, limits::BLOB_MAX_BYTES as usize)),
                Some(_) => {}
            }
            let principal = signer(env, &req, &url, &[]).await?;
            let name = named_fragment(name, Some(&principal))?;
            let body = req.inner().body().map(worker::wasm_bindgen::JsValue::from);
            let f = Forward { name: &name, inner: format!("/api/blobs/{sha}"), principal: Some(principal), mode: None, extra: vec![] };
            forward(env, &req, &url, body, f).await
        }
        (method, ["api", "f", name, rest @ ..]) => {
            if !valid_fragment_name(name) && !fragment_proto::valid_label(name) {
                return Err(CellError::invalid("a fragment's name is <label>.<username>"));
            }
            let inner = match (method, rest) {
                (Method::Delete, [] | [""]) => "/delete".to_string(),
                (_, [] | [""]) => return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
                _ => format!("/api/{}", rest.join("/")),
            };
            let body = read_body(&mut req).await?;
            // code.storage signs its deliveries with the fragment's webhook
            // secret, and the inbox takes its token, instead of a signature.
            let mut extra = vec![];
            let principal = match inner.as_str() {
                "/api/webhook" => None,
                "/api/inbox" => {
                    for k in ["x-fragment-inbox-token", jobs::HOPS_HEADER] {
                        if let Some(v) = req.headers().get(k)? {
                            extra.push((k, v));
                        }
                    }
                    None
                }
                _ => Some(signer(env, &req, &url, &body).await?),
            };
            let name = named_fragment(name, principal.as_ref())?;
            let f = Forward { name: &name, inner, principal, mode: None, extra };
            forward(env, &req, &url, bytes_body(body), f).await
        }
        (_, ["f", name]) => {
            check_name(name)?;
            let to = Url::parse(&cfg.canonical(&url, name)).map_err(|e| CellError::host(e.to_string()))?;
            Ok(Response::redirect_with_status(to, 308)?)
        }
        (method, ["f", name, rest @ ..]) => {
            check_name(name)?;
            let rest = rest.join("/");
            if cfg.host_suffix.is_some() && rest != "__watch" && rest != "__live" {
                if !matches!(method, Method::Get | Method::Head) {
                    return Err(CellError::new(ErrorCode::NotFound, "fragments are served from their own origin"));
                }
                let mut to = Url::parse(&cfg.canonical(&url, name)).map_err(|e| CellError::host(e.to_string()))?;
                to.set_path(&format!("/{rest}"));
                to.set_query(url.query());
                return Ok(Response::redirect_with_status(to, 308)?);
            }
            serve(req, env, &url, name, &rest, "path").await
        }
        _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
    }
}
