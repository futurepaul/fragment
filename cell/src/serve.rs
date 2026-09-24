//! Serving a fragment: its site from the `live` pin, the machine-read
//! plane (`__tree`, `__file`), browser calls (`__op`), and the change feed
//! (`__watch`). Who may see what follows the fragment's visibility:
//! members always; on a `link` or `public` fragment, whoever holds the
//! share link counts as a viewer (a `?view=` token sets a cookie on the
//! fragment's origin); on a `public` fragment, everyone else holds the
//! `public` floor. An unsigned browser calling an operation gets an
//! anonymous principal: a random cookie whose hash names it.

use fragment_core::{npub, site};
use fragment_proto::{valid_repo_path, ErrorCode, OpCall, Role, Visibility};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{decode_segment, json_response, Caller, FragmentCell};
use crate::js;

pub const MODE_HEADER: &str = "x-fragment-mode";
/// The browser library pages import as `./__fragment.js`.
const CLIENT_JS: &str = include_str!("../client.mjs");
const VIEW_COOKIE: &str = "fragview";
const ANON_COOKIE: &str = "fragment_anon";
const VIEW_COOKIE_AGE_S: i64 = 7 * 24 * 3600;
const ANON_COOKIE_AGE_S: i64 = 365 * 24 * 3600;
/// Pages up to this size get Open Graph tags from `meta`.
const OG_MAX_BYTES: u64 = 1024 * 1024;

pub(crate) fn eq_ct(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Origin {
    cookie_path: String,
    secure: bool,
}

impl Origin {
    fn cookie(&self, name: &str, value: &str, max_age_s: i64) -> String {
        let secure = if self.secure { "; Secure" } else { "" };
        format!("{name}={value}; Path={}; Max-Age={max_age_s}; HttpOnly; SameSite=Lax{secure}", self.cookie_path)
    }
}

/// The anonymous principal a cookie names.
fn anon_principal(cookie_value: &str) -> String {
    format!("{}{}", npub::ANON_PREFIX, &hex::encode(Sha256::digest(cookie_value.as_bytes()))[..32])
}

fn with_cookies(mut resp: Response, cookies: &[String]) -> CellResult<Response> {
    for c in cookies {
        resp.headers_mut().append("set-cookie", c)?;
    }
    Ok(resp)
}

impl FragmentCell {
    pub(crate) async fn serve(&self, mut req: Request, caller: &Caller, name: &str, rest: &str) -> CellResult<Response> {
        self.name()?;
        let url = req.url()?;
        let origin = Origin {
            cookie_path: if req.headers().get(MODE_HEADER)?.as_deref() == Some("host") { "/".into() } else { format!("/f/{name}/") },
            secure: caller.url.scheme() == "https",
        };
        let cookies = req.headers().get("cookie")?.unwrap_or_default();
        let view_token = self.must("view_token")?;
        let via_query = url.query_pairs().any(|(k, v)| k == "view" && eq_ct(&v, &view_token));
        let via_cookie = site::cookie(&cookies, VIEW_COOKIE).is_some_and(|v| eq_ct(v, &view_token));
        let link = via_query || via_cookie;
        let mut set = vec![];
        if via_query {
            set.push(origin.cookie(VIEW_COOKIE, &view_token, VIEW_COOKIE_AGE_S));
        }
        let path: String = rest.split('/').map(decode_segment).collect::<Vec<_>>().join("/");
        let anon = site::cookie(&cookies, ANON_COOKIE).filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).map(anon_principal);
        let resp = if let Some(op) = path.strip_prefix("__op/") {
            if req.method() != Method::Post {
                return Err(CellError::invalid("call an operation with POST"));
            }
            let is_json = req.headers().get("content-type")?.is_some_and(|c| c.starts_with("application/json"));
            if !is_json {
                // A cross-site form cannot send JSON without a preflight.
                return Err(CellError::invalid("send the call as application/json"));
            }
            let body: OpCall = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let principal = match (&caller.principal, anon) {
                (Some(p), _) => p.clone(),
                (None, Some(a)) => a,
                (None, None) => {
                    let fresh = js::random_hex::<32>();
                    set.push(origin.cookie(ANON_COOKIE, &fresh, ANON_COOKIE_AGE_S));
                    anon_principal(&fresh)
                }
            };
            let result = self.call_op(caller, &principal, link, op, body).await?;
            json_response(&result)?
        } else if let Some(op) = path.strip_prefix("__push-").filter(|op| matches!(*op, "key" | "sub" | "unsub")) {
            // web push: anyone who can see the fragment (push.rs)
            self.require(caller, link, Role::Public)?;
            let answer = if op == "key" {
                self.push_key()?
            } else {
                if req.method() != Method::Post || !req.headers().get("content-type")?.is_some_and(|c| c.starts_with("application/json")) {
                    return Err(CellError::invalid("POST the subscription as application/json"));
                }
                let body: Value = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                let principal = caller.principal.clone().or(anon).unwrap_or_else(|| "anonymous".into());
                if op == "sub" {
                    self.push_subscribe(&body, &principal)?
                } else {
                    self.push_unsubscribe(&body)?
                }
            };
            json_response(&answer)?
        } else if path == "__sw.js" {
            let h = Headers::new();
            h.set("content-type", "text/javascript; charset=utf-8")?;
            h.set("cache-control", "no-cache")?;
            Response::ok(crate::push::SW_JS)?.with_headers(h)
        } else if path == "__watch" {
            self.watch(&req, caller, link)?
        } else if path == "__live" {
            // an unsigned visitor without a cookie yet is anonymous for this socket only
            let principal = caller.principal.clone().or(anon).unwrap_or_else(|| anon_principal(&js::random_hex::<32>()));
            self.live(&req, caller, &principal, link)?
        } else {
            let role = self.require(caller, link, Role::Public)?;
            let who = caller.principal.as_deref().map(npub::display).or(anon).unwrap_or_else(|| "anonymous".into());
            match req.method() {
                Method::Get | Method::Head => self.site(&mut req, caller, name, &path, &url, role, &who).await?,
                // Only the app's own routes take other methods.
                _ => self.app_fetch(&mut req, caller, name, &path, &url, role, &who).await?,
            }
        };
        with_cookies(resp, &set)
    }

    /// The author's `fetch` for a path that is not a site file: it sees the
    /// fragment's public URL and who is asking (`x-fragment-principal`,
    /// `x-fragment-role`).
    #[allow(clippy::too_many_arguments)]
    async fn app_fetch(&self, req: &mut Request, caller: &Caller, name: &str, path: &str, url: &url::Url, role: Role, who: &str) -> CellResult<Response> {
        let facet = match self.facet() {
            Ok(f) => f,
            Err(e) if e.code == ErrorCode::NoCode => {
                return Err(CellError::new(ErrorCode::NotFound, format!("no page or app route for {} /{path}", req.method().as_ref())))
            }
            Err(e) => return Err(e),
        };
        let headers = Headers::new();
        for k in ["content-type", "accept", "if-none-match", "range"] {
            if let Some(v) = req.headers().get(k)? {
                headers.set(k, &v)?;
            }
        }
        headers.set("x-fragment-principal", who)?;
        headers.set("x-fragment-role", role.as_str())?;
        let mut init = RequestInit::new();
        init.with_method(req.method()).with_headers(headers);
        if !matches!(req.method(), Method::Get | Method::Head) {
            let body = req.bytes().await?;
            if !body.is_empty() {
                init.with_body(Some(worker::js_sys::Uint8Array::from(body.as_slice()).into()));
            }
        }
        let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
        let target = format!("{}{path}{query}", self.cfg.canonical(&caller.url, name));
        facet.fetch(Request::new_with_init(&target, &init)?).await
    }

    /// The change feed for `fragment sync --watch`: a frame per external
    /// move of main. Viewers and up; a revoked member's feed closes.
    fn watch(&self, req: &Request, caller: &Caller, link: bool) -> CellResult<Response> {
        if !req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("__watch is a WebSocket; send Upgrade: websocket"));
        }
        self.require(caller, link, Role::Viewer)?;
        let pair = WebSocketPair::new()?;
        let who = match &caller.principal {
            Some(p) if self.has_standing(caller)? => format!("p:{p}"),
            _ => "view".to_string(),
        };
        self.state.accept_websocket_with_tags(&pair.server, &["watch", &who]);
        pair.server.send_with_str(json!({ "type": "hello", "ref": "main", "sha": self.pin("main")? }).to_string())?;
        Ok(Response::from_websocket(pair.client)?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn site(&self, req: &mut Request, caller: &Caller, name: &str, path: &str, url: &url::Url, role: Role, who: &str) -> CellResult<Response> {
        let head = req.method() == Method::Head;
        if path == "__fragment.js" {
            let h = Headers::new();
            h.set("content-type", "text/javascript; charset=utf-8")?;
            h.set("cache-control", "no-cache")?;
            return Ok(Response::ok(CLIENT_JS)?.with_headers(h));
        }
        self.ensure_pins().await?;
        let live = self.pin("live")?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("{name} has no live commit yet: deploy first")))?;
        let public = self.visibility()? == Visibility::Public;
        let headers = |ct: &str, cache: &str| -> CellResult<Headers> {
            let h = Headers::new();
            h.set("content-type", ct)?;
            h.set("cache-control", cache)?;
            h.set("x-fragment-ref", &live)?;
            Ok(h)
        };
        match path {
            "__preview.svg" => {
                let svg = site::preview_svg(name);
                return Ok(Response::ok(svg)?.with_headers(headers("image/svg+xml", if public { "public, max-age=3600" } else { "private, max-age=3600" })?));
            }
            "__tree" => {
                let blobs = self.pointer_sizes("live")?;
                let files: Vec<Value> = self
                    .tree_rows("live")?
                    .into_iter()
                    .filter(|r| !site::is_machinery(r["path"].as_str().unwrap_or("")))
                    .map(|r| {
                        let size = r["path"].as_str().and_then(|p| blobs.get(p)).map_or(r["size"].clone(), |s| json!(s));
                        json!({ "path": r["path"], "size": size, "mode": r["mode"], "lastCommitSha": r["last_commit"] })
                    })
                    .collect();
                return json_response(&json!({ "type": "tree", "ref": "live", "sha": live, "count": files.len(), "files": files }));
            }
            "__file" => {
                let p = url.query_pairs().find(|(k, _)| k == "path").map(|(_, v)| v.into_owned()).unwrap_or_default();
                if !valid_repo_path(&p) || site::is_machinery(&p) {
                    return Err(CellError::invalid("path must be a content path (not the fragment's own machinery)"));
                }
                // Data an app writes lands on main; a path not yet on live reads from there.
                let which = if self.tree_row("live", &p)?.is_some() {
                    "live"
                } else if self.tree_row("main", &p)?.is_some() {
                    "main"
                } else {
                    return Err(CellError::new(ErrorCode::NotFound, format!("no file {p}")));
                };
                if head {
                    return Ok(Response::empty()?.with_headers(headers(site::mime_for_path(&p), "no-store")?));
                }
                let range = req.headers().get("range")?;
                let mut resp = self.stream_file(which, &p, range.as_deref()).await?;
                resp.headers_mut().set("cache-control", "no-store")?;
                return Ok(resp);
            }
            _ => {}
        }
        if !path.is_empty() && !valid_repo_path(path.trim_end_matches('/')) {
            return Err(CellError::new(ErrorCode::NotFound, "no such page"));
        }
        let Some((file, row)) = site::site_candidates(path).into_iter().find_map(|c| self.tree_row("live", &c).ok().flatten().map(|r| (c, r))) else {
            if !path.starts_with("__") {
                return self.app_fetch(req, caller, name, path, url, role, who).await;
            }
            return Err(CellError::new(ErrorCode::NotFound, format!("no page {path:?}")));
        };
        let mime = site::mime_for_path(&file);
        let cache = site::cache_control(&file, public);
        if head {
            let h = headers(mime, cache)?;
            let size = match self.pointer("live", &file)? {
                Some((_, size)) => size,
                None => row["size"].as_u64().unwrap_or(0),
            };
            h.set("content-length", &size.to_string())?;
            return Ok(Response::empty()?.with_headers(h));
        }
        let meta: Option<fragment_core::manifest::Meta> = self.meta("meta_live")?.and_then(|m| serde_json::from_str(&m).ok());
        if let (Some(meta), true) = (meta, mime.starts_with("text/html") && row["size"].as_u64().unwrap_or(u64::MAX) <= OG_MAX_BYTES) {
            if let Some(bytes) = self.cs()?.read(&self.must("repo")?, &live, &file, OG_MAX_BYTES as usize).await? {
                let html = String::from_utf8_lossy(&bytes);
                let image = format!("{}__preview.svg", self.cfg.canonical(&caller.url, name));
                let page = site::inject_og(&html, name, &meta, &image);
                return Ok(Response::from_html(page)?.with_headers(headers(mime, cache)?));
            }
        }
        let range = req.headers().get("range")?;
        let mut resp = self.stream_file("live", &file, range.as_deref()).await?;
        resp.headers_mut().set("content-type", mime)?;
        resp.headers_mut().set("cache-control", cache)?;
        Ok(resp)
    }
}
