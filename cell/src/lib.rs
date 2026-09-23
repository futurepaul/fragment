//! The fragment platform on celld, in Rust.
//!
//! The router (this file's `fetch`) verifies NIP-98, bounds request bodies,
//! decides which fragment a request is for, and hands it to that
//! fragment's supervisor (`fragment.rs`) with the verified principal.
//! Public routes:
//!
//!   POST   /api/fragments                  create (signed; the signer owns it)
//!   GET    /api/fragments                  the fragments the signer belongs to
//!   DELETE /api/f/<name>                   delete (owner)
//!   *      /api/f/<name>/<route>           the control API (signed; the code.storage webhook is HMAC)
//!   *      <name>.<suffix>/<path>          the fragment's site, on its own origin
//!   *      /f/<name>/<path>                the same, when no suffix is configured (dev)
//!
//! With a suffix configured, `/f/<name>/…` redirects to the fragment's own
//! host: fragments sharing one origin could act as each other's visitors.
//! `__watch` stays reachable there for the CLI, which carries no cookies.

mod config;
mod cs;
mod error;
mod fragment;
mod js;
mod members;
mod ops;
mod plane;
mod principal;
mod serve;

use fragment_proto::{limits, valid_fragment_name, CreateFragment, ErrorCode};
use worker::*;

use config::Config;
use error::{CellError, CellResult};
use fragment::{NAME_HEADER, PRINCIPAL_HEADER, URL_HEADER};
use serve::MODE_HEADER;

pub use fragment::FragmentCell;
pub use principal::PrincipalCell;

/// Client headers a fragment's supervisor sees; everything else, and any
/// `x-fragment-*` a client sends, stays at the router.
const PASSED_HEADERS: [&str; 8] =
    ["content-type", "cookie", "accept", "if-none-match", "upgrade", "x-pierre-event", "x-pierre-signature", "range"];

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

fn authenticate(req: &Request, url: &Url, body: &[u8]) -> CellResult<String> {
    let header = req.headers().get("authorization")?;
    let now_s = js::now_ms() / 1000;
    fragment_nip98::verify(header.as_deref(), req.method().as_ref(), url.as_str(), body, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::new(ErrorCode::Unauthenticated, e.to_string()))
}

/// NIP-98 when the request carries it (a bad signature is still a 401).
fn authenticate_if_signed(req: &Request, url: &Url, body: &[u8]) -> CellResult<Option<String>> {
    match req.headers().get("authorization")? {
        Some(_) => authenticate(req, url, body).map(Some),
        None => Ok(None),
    }
}

fn check_name(name: &str) -> CellResult<()> {
    if valid_fragment_name(name) {
        Ok(())
    } else {
        Err(CellError::invalid("a fragment name must match ^[a-z0-9][a-z0-9-]{0,62}$"))
    }
}

struct Forward<'a> {
    name: &'a str,
    inner: String,
    principal: Option<String>,
    mode: Option<&'a str>,
}

/// Hands a request to the fragment's supervisor.
async fn forward(env: &Env, req: &Request, url: &Url, body: Vec<u8>, f: Forward<'_>) -> CellResult<Response> {
    let headers = Headers::new();
    for k in PASSED_HEADERS {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    headers.set(NAME_HEADER, f.name)?;
    headers.set(URL_HEADER, url.as_str())?;
    if let Some(p) = &f.principal {
        headers.set(PRINCIPAL_HEADER, p)?;
    }
    if let Some(m) = f.mode {
        headers.set(MODE_HEADER, m)?;
    }
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers);
    if !body.is_empty() {
        init.with_body(Some(worker::js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let inner = Request::new_with_init(&format!("https://fragment.internal{}{query}", f.inner), &init)?;
    let stub = env.durable_object("FRAGMENT")?.get_by_name(f.name)?;
    Ok(stub.fetch_with_request(inner).await?)
}

async fn serve(mut req: Request, env: &Env, url: &Url, name: &str, rest: &str, mode: &'static str) -> CellResult<Response> {
    check_name(name)?;
    let body = read_body(&mut req).await?;
    let principal = authenticate_if_signed(&req, url, &body)?;
    let f = Forward { name, inner: format!("/serve/{rest}"), principal, mode: Some(mode) };
    forward(env, &req, url, body, f).await
}

async fn route(mut req: Request, env: &Env) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    let url = req.url()?;
    let path = url.path().to_string();
    // A fragment's own host: everything but the platform API is its site.
    if let Some(name) = url.host_str().and_then(|h| cfg.fragment_of_host(h)) {
        if !path.starts_with("/api/") {
            let rest = path.trim_start_matches('/').to_string();
            return serve(req, env, &url, &name, &rest, "host").await;
        }
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (req.method(), segments.as_slice()) {
        (Method::Get, ["healthz"]) => Ok(Response::ok("ok")?),
        (Method::Post, ["api", "fragments"]) => {
            let body = read_body(&mut req).await?;
            let create: CreateFragment = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            check_name(&create.name)?;
            let principal = authenticate(&req, &url, &body)?;
            let f = Forward { name: &create.name, inner: "/create".into(), principal: Some(principal), mode: None };
            forward(env, &req, &url, body, f).await
        }
        (Method::Get, ["api", "fragments"]) => {
            let principal = authenticate(&req, &url, &[])?;
            let list = Request::new("https://principal.internal/list", Method::Get)?;
            Ok(env.durable_object("PRINCIPAL")?.get_by_name(&principal)?.fetch_with_request(list).await?)
        }
        (method, ["api", "f", name, rest @ ..]) => {
            check_name(name)?;
            let inner = match (method, rest) {
                (Method::Delete, [] | [""]) => "/delete".to_string(),
                (_, [] | [""]) => return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
                _ => format!("/api/{}", rest.join("/")),
            };
            let body = read_body(&mut req).await?;
            // code.storage signs its deliveries with the fragment's webhook secret instead.
            let principal = if inner == "/api/webhook" { None } else { Some(authenticate(&req, &url, &body)?) };
            let f = Forward { name, inner, principal, mode: None };
            forward(env, &req, &url, body, f).await
        }
        (_, ["f", name]) => {
            check_name(name)?;
            let to = Url::parse(&cfg.canonical(&url, name)).map_err(|e| CellError::host(e.to_string()))?;
            Ok(Response::redirect_with_status(to, 308)?)
        }
        (method, ["f", name, rest @ ..]) => {
            check_name(name)?;
            let rest = rest.join("/");
            if cfg.host_suffix.is_some() && rest != "__watch" {
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
