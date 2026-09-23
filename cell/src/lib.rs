//! The fragment platform on celld, in Rust.
//!
//! The router (this file's `fetch`) verifies NIP-98, bounds request bodies,
//! and hands each call to the fragment's supervisor (`fragment.rs`) with
//! the verified principal. Routes:
//!
//!   POST /api/fragments                  create a fragment (the signer owns it)
//!   GET  /api/f/<name>/status
//!   PUT  /api/f/<name>/code              slice A: install app code
//!   POST /api/f/<name>/ops/<operation>   call an operation

mod error;
mod fragment;
mod js;

use fragment_proto::{limits, valid_fragment_name, CreateFragment, ErrorCode};
use worker::*;

use error::{CellError, CellResult};
use fragment::{NAME_HEADER, PRINCIPAL_HEADER};

pub use fragment::FragmentCell;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match route(req, &env).await {
        Ok(resp) => Ok(resp),
        Err(e) => e.response(),
    }
}

fn authenticate(req: &Request, url: &str, body: &[u8]) -> CellResult<String> {
    let header = req.headers().get("authorization")?;
    let now_s = js::now_ms() / 1000;
    fragment_nip98::verify(header.as_deref(), req.method().as_ref(), url, body, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::new(ErrorCode::Unauthenticated, e.to_string()))
}

async fn route(mut req: Request, env: &Env) -> CellResult<Response> {
    let url = req.url()?;
    let path = url.path().to_string();
    let body = req.bytes().await?;
    if body.len() > limits::BODY_MAX_BYTES {
        return Err(CellError::too_large("request body", body.len(), limits::BODY_MAX_BYTES));
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let (name, inner_path) = match (req.method(), segments.as_slice()) {
        (Method::Post, ["api", "fragments"]) => {
            let create: CreateFragment =
                serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            (create.name, "/create".to_string())
        }
        (_, ["api", "f", name, rest @ ..]) if !rest.is_empty() => (name.to_string(), format!("/{}", rest.join("/"))),
        _ => return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
    };
    if !valid_fragment_name(&name) {
        return Err(CellError::invalid("a fragment name must match ^[a-z0-9][a-z0-9-]{0,62}$"));
    }
    let principal = authenticate(&req, url.as_str(), &body)?;

    let headers = Headers::new();
    headers.set(PRINCIPAL_HEADER, &principal)?;
    headers.set(NAME_HEADER, &name)?;
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers);
    if !body.is_empty() {
        init.with_body(Some(worker::js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let inner = Request::new_with_init(&format!("https://fragment.internal{inner_path}"), &init)?;
    let stub = env.durable_object("FRAGMENT")?.get_by_name(&name)?;
    Ok(stub.fetch_with_request(inner).await?)
}
