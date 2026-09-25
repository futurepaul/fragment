//! What the router hands a fragment's supervisor, and how it travels. The
//! router decides a request's fragment, the URL it arrived on, how its
//! site was addressed, and who is asking; `Routed` carries that in headers
//! only the router sets (`fragment_proto::routed`; a client's are dropped),
//! written once by `to_headers` and read once by `from_headers`, side by
//! side here so the two cannot drift.
//!
//! Who is asking arrives resolved (`Signed`) on the control API, and
//! unresolved (`Credential`) on a site request: most of a site's requests
//! answer alike for everyone who may see the fragment, so the fragment asks
//! the registry only when its answer depends on who is asking.
//!
//! Calls from inside the platform (a run's Workflow, the app's `Files`
//! capability, the delivery consumer, the router's test hooks) are not
//! routed requests: they carry no caller and no URL, and the supervisor
//! answers them before it decodes a `Routed`. `internal_request` builds
//! them, for Rust and for `entry.mjs`.

use std::ops::Deref;

use fragment_core::npub;
use fragment_proto::{routed, valid_fragment_name, ErrorCode, Identity};
use serde::{Deserialize, Serialize};
// the macro's generated code names `wasm_bindgen`: worker's re-export
use worker::wasm_bindgen::{self, prelude::*};
use worker::{Env, Headers, Method, Request, RequestInit, Url};

use crate::error::{CellError, CellResult};
use crate::registry::calls;

/// How a fragment's site was addressed: its own origin, or `/f/<name>/`
/// on the platform's (a dev fleet without hostnames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Host,
    Path,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Host => "host",
            Mode::Path => "path",
        }
    }

    fn parse(s: &str) -> Option<Mode> {
        match s {
            "host" => Some(Mode::Host),
            "path" => Some(Mode::Path),
            _ => None,
        }
    }
}

/// Who is asking: an identity the registry vouched for, and the key it
/// signed with (none for a browser's session). It reads as its identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signed {
    #[serde(flatten)]
    pub identity: Identity,
    /// 64 hex.
    pub key: Option<String>,
}

impl Deref for Signed {
    type Target = Identity;
    fn deref(&self) -> &Identity {
        &self.identity
    }
}

/// Who is asking a site request, as the router found them: not yet
/// resolved, since most of a site's requests answer alike for everyone who
/// may see the fragment (a page, a file). The fragment resolves it when its
/// answer depends on who is asking (`FragmentCell::identified`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Credential {
    /// The key a NIP-98 signature over the request was verified for (64
    /// hex): checking a signature needs no registry, so the router did.
    Key(String),
    /// The token of the fragment origin's session cookie (64 hex).
    Session(String),
}

impl Credential {
    fn well_formed(&self) -> bool {
        match self {
            Credential::Key(key) => npub::is_hex_key(key),
            Credential::Session(token) => token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        }
    }

    /// Who the credential names on `fragment`, asked of the registry: a key
    /// no one holds, or a revoked one, is 401; a session that is not live
    /// is nobody (a stale cookie leaves the request anonymous); a registry
    /// that cannot answer is 503, never anonymous (rule 7).
    pub async fn resolve(self, env: &Env, fragment: &str) -> CellResult<Option<Signed>> {
        match self {
            Credential::Key(key) => {
                let identity = crate::ask_registry(env, &calls::Resolve { key: key.clone() }).await?;
                Ok(Some(Signed { identity, key: Some(key) }))
            }
            Credential::Session(token) => match crate::ask_registry(env, &calls::Session { token, fragment: Some(fragment.to_string()) }).await {
                Ok(identity) => Ok(Some(Signed { identity, key: None })),
                Err(e) if e.code == ErrorCode::Unauthenticated => Ok(None),
                Err(e) => Err(e),
            },
        }
    }
}

/// A request the router hands a fragment's supervisor.
pub struct Routed {
    /// The fragment's full name (`<label>.<username>`).
    pub name: String,
    /// The URL it arrived on: canonical URLs, cookies, and the query string
    /// all come from here.
    pub url: Url,
    /// Set on site requests only.
    pub mode: Option<Mode>,
    /// Who is asking, resolved (the control API). `None` with no
    /// `credential`: anonymous.
    pub signed: Option<Signed>,
    /// Who is asking a site request, unresolved. Never beside `signed`.
    pub credential: Option<Credential>,
}

impl Routed {
    /// Writes the route into `headers` (the router's side).
    pub fn to_headers(&self, headers: &Headers) -> CellResult<()> {
        assert!(valid_fragment_name(&self.name), "the router routes to a valid name");
        assert!(self.signed.is_none() || self.credential.is_none(), "who is asking is resolved or not, never both");
        assert!(self.credential.as_ref().is_none_or(Credential::well_formed), "the router forwards a credential it checked");
        headers.set(routed::NAME, &self.name)?;
        headers.set(routed::URL, self.url.as_str())?;
        if let Some(mode) = self.mode {
            headers.set(routed::MODE, mode.as_str())?;
        }
        if let Some(signed) = &self.signed {
            let json = serde_json::to_string(signed).map_err(|e| CellError::host(format!("the signer: {e}")))?;
            headers.set(routed::SIGNED, &json)?;
        }
        if let Some(credential) = &self.credential {
            let json = serde_json::to_string(credential).map_err(|e| CellError::host(format!("the credential: {e}")))?;
            headers.set(routed::CREDENTIAL, &json)?;
        }
        Ok(())
    }

    /// Reads the route back (the supervisor's side). Only the router sets
    /// these headers, so a missing or malformed one is a host fault.
    pub fn from_headers(headers: &Headers) -> CellResult<Routed> {
        let name = headers.get(routed::NAME)?.filter(|n| valid_fragment_name(n)).ok_or_else(|| CellError::host("no fragment name from the router"))?;
        let url = headers.get(routed::URL)?.and_then(|u| Url::parse(&u).ok()).ok_or_else(|| CellError::host("no URL from the router"))?;
        let mode = match headers.get(routed::MODE)? {
            Some(m) => Some(Mode::parse(&m).ok_or_else(|| CellError::host(format!("the router named a mode {m:?}")))?),
            None => None,
        };
        let signed = match headers.get(routed::SIGNED)? {
            Some(json) => {
                let signed: Signed = serde_json::from_str(&json).map_err(|e| CellError::host(format!("the router's signer: {e}")))?;
                if !npub::is_identity(&signed.id) || signed.key.as_deref().is_some_and(|k| !npub::is_hex_key(k)) {
                    return Err(CellError::host("the router named a malformed signer"));
                }
                Some(signed)
            }
            None => None,
        };
        let credential = match headers.get(routed::CREDENTIAL)? {
            Some(json) => {
                let credential: Credential = serde_json::from_str(&json).map_err(|e| CellError::host(format!("the router's credential: {e}")))?;
                if !credential.well_formed() || signed.is_some() {
                    return Err(CellError::host("the router named a malformed credential"));
                }
                Some(credential)
            }
            None => None,
        };
        Ok(Routed { name, url, mode, signed, credential })
    }
}

/// The marker an internal route's caller carries, and its value: the router
/// never passes these headers, so a request with one came from inside.
/// `None`: the route needs none (the router's test hooks, which fleets
/// without them do not answer).
fn marker(path: &str) -> CellResult<Option<(&'static str, &'static str)>> {
    if path.starts_with("job/") {
        Ok(Some((crate::jobs::JOB_HEADER, "1")))
    } else if path.starts_with("cap/files/") {
        Ok(Some((crate::files::CAP_HEADER, "files")))
    } else if path == "deliver/report" {
        Ok(Some((crate::deliveries::REPORT_HEADER, "1")))
    } else if path.starts_with("test/") {
        Ok(None)
    } else {
        Err(CellError::host(format!("{path} is not an internal route")))
    }
}

/// A call into a fragment's supervisor from inside the platform: `path`
/// (`job/advance`, `cap/files/read`, `deliver/report`, `test/keys`) with a
/// JSON body, marked as that route expects.
pub(crate) fn internal_request(path: &str, body: &str) -> CellResult<Request> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    if let Some((header, value)) = marker(path)? {
        headers.set(header, value)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    Ok(Request::new_with_init(&format!("https://fragment.internal/{path}"), &init)?)
}

/// `internal_request` for `entry.mjs` (the job driver and the `Files`
/// capability), so the JavaScript names no header. A class with a static
/// method, because worker-build exports classes by name and a free
/// function only as a method of the Worker's entrypoint.
#[wasm_bindgen(wasm_bindgen = worker::wasm_bindgen)]
pub struct InternalRoute;

#[wasm_bindgen]
impl InternalRoute {
    pub fn request(path: &str, body: &str) -> Result<worker::web_sys::Request, JsValue> {
        let req = internal_request(path, body).map_err(|e| JsValue::from_str(&e.message))?;
        // the handle, not JavaScript's `Request.clone()` (which tees the body)
        Ok(Clone::clone(req.inner()))
    }
}
