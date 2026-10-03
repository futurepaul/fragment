//! Serving a fragment: its site from the `live` pin, the machine-read
//! plane (`__tree`, `__file`), a blob by its hash (`__blob`, and an
//! editor's page uploading one there), browser calls (`__op`), who is in
//! it (`__people`, `__members`), and the change feed (`__watch`). Who may
//! see what follows the fragment's visibility:
//! members always; on a `link` or `public` fragment, whoever holds the
//! share link counts as a viewer (a `?view=` token sets a cookie on the
//! fragment's origin); on a `public` fragment, everyone else holds the
//! `public` floor. An unsigned browser calling an operation gets an
//! anonymous principal: a random cookie whose hash names it. A browser
//! signed in on this origin (`__signin`, the router's) is its person.
//! Invites are accepted on the platform's origin (`/join/<name>`,
//! share.rs), never here: a page here is the fragment's author's.
//!
//! The router hands a site request's signer or session on unresolved: a
//! page or a file answers alike for everyone who may see the fragment, so
//! the registry is asked who is asking only when the anonymous standing may
//! not read (`reader`), or when the answer is someone's (`answers_someone`,
//! and the app's own routes).

use fragment_core::access::Purpose;
use fragment_core::{npub, site};
use fragment_proto::{valid_repo_path, ErrorCode, OpCall, Role, Visibility};
use fragment_templates::blessed;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{as_themselves, decide, decode_segment, json_response, Caller, Facts, FragmentCell, MetaKey};
use crate::js;
use crate::routed::Mode;

/// The browser library pages import as `./__fragment.js`.
const CLIENT_JS: &str = include_str!("../client.mjs");
/// The files viewer (`__files`'s page, with `./__files.js`, `./__files.css`):
/// a fragment's files as a tree beside a reader.
const FILES_JS: &str = include_str!("../files.mjs");
const FILES_CSS: &str = include_str!("../files.css");
/// The scripts' entity tags, hashed at build time: a page view revalidates
/// them (`no-cache`) and gets 304 until a cell deploy changes their bytes.
const CLIENT_JS_HASH: u64 = site::content_hash(CLIENT_JS.as_bytes());
const FILES_JS_HASH: u64 = site::content_hash(FILES_JS.as_bytes());
const FILES_CSS_HASH: u64 = site::content_hash(FILES_CSS.as_bytes());
const SW_JS_HASH: u64 = site::content_hash(crate::push::SW_JS.as_bytes());
/// Marks a site request's refusal (401, 403) as the platform's, never an
/// app's answer: the router answers a browser's navigation to one as a
/// page (`auth::refused`), and drops the mark from any other answer. An
/// app that sent it would only get its own fragment's refusal page, which
/// a redirect of its own gets it anyway.
pub(crate) const REFUSAL: &str = "x-fragment-refusal";
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

/// 304 for a request whose `If-None-Match` names `etag`, with the headers
/// the full answer would carry; `None` otherwise.
pub(crate) fn not_modified(req: &Request, etag: &str, cache: &str) -> CellResult<Option<Response>> {
    match req.headers().get("if-none-match")? {
        Some(tags) if site::not_modified(&tags, etag) => {
            let h = Headers::new();
            h.set("etag", etag)?;
            h.set("cache-control", cache)?;
            Ok(Some(Response::empty()?.with_status(304).with_headers(h)))
        }
        _ => Ok(None),
    }
}

/// A script compiled into the cell, revalidated by its build-time hash.
fn script(req: &Request, body: &'static str, hash: u64) -> CellResult<Response> {
    compiled_in(req, body, hash, "text/javascript; charset=utf-8")
}

/// A file compiled into the cell, revalidated by its build-time hash.
fn compiled_in(req: &Request, body: &'static str, hash: u64, content_type: &str) -> CellResult<Response> {
    let etag = site::hash_etag(hash);
    if let Some(resp) = not_modified(req, &etag, "no-cache")? {
        return Ok(resp);
    }
    let h = Headers::new();
    h.set("content-type", content_type)?;
    h.set("cache-control", "no-cache")?;
    h.set("etag", &etag)?;
    Ok(Response::ok(body)?.with_headers(h))
}

/// Whether a site path's answer is someone's, beyond whether they may see
/// the fragment: an operation's call, a socket (`__live`'s presence; a
/// member's `__watch` closes when they leave), or a push subscription. Its
/// caller is resolved first.
fn answers_someone(path: &str) -> bool {
    path.starts_with("__op/") || matches!(path, "__push-key" | "__push-sub" | "__push-unsub" | "__watch" | "__live")
}

fn with_cookies(mut resp: Response, cookies: &[String]) -> CellResult<Response> {
    for c in cookies {
        resp.headers_mut().append("set-cookie", c)?;
    }
    Ok(resp)
}

impl FragmentCell {
    /// A site request's answer, its refusal marked as the platform's (`REFUSAL`).
    pub(crate) async fn serve(&self, req: Request, caller: &Caller, name: &str, rest: &str) -> CellResult<Response> {
        match self.answer(req, caller, name, rest).await {
            Err(e) if crate::auth::is_refusal(e.code) => {
                let mut resp = e.response()?;
                resp.headers_mut().set(REFUSAL, "1")?;
                Ok(resp)
            }
            answered => answered,
        }
    }

    async fn answer(&self, mut req: Request, caller: &Caller, name: &str, rest: &str) -> CellResult<Response> {
        // the fragment's facts, read once for the whole request
        let mut facts = self.facts()?;
        // the query string, as the request arrived (the router's URL)
        let url = caller.url.clone();
        let origin = Origin {
            cookie_path: if caller.mode == Some(Mode::Host) { "/".into() } else { format!("/f/{name}/") },
            secure: caller.url.scheme() == "https",
        };
        let cookies = req.headers().get("cookie")?.unwrap_or_default();
        let via_query = url.query_pairs().any(|(k, v)| k == "view" && eq_ct(&v, &facts.view_token));
        let via_cookie = site::cookie(&cookies, VIEW_COOKIE).is_some_and(|v| eq_ct(v, &facts.view_token));
        let link = via_query || via_cookie;
        let mut set = vec![];
        if via_query {
            set.push(origin.cookie(VIEW_COOKIE, &facts.view_token, VIEW_COOKIE_AGE_S));
        }
        let path: String = rest.split('/').map(decode_segment).collect::<Vec<_>>().join("/");
        let anon = site::cookie(&cookies, ANON_COOKIE).filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).map(anon_principal);
        // who a socket connected as is asked again later (live.rs)
        let credential = if path == "__live" { caller.unresolved.clone() } else { None };
        let resolved;
        let caller = if answers_someone(&path) {
            resolved = self.identified(caller, name).await?;
            &*resolved
        } else {
            caller
        };
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
            let principal = match (caller.principal(), anon) {
                (Some(p), _) => p.to_string(),
                (None, Some(a)) => a,
                (None, None) => {
                    let fresh = js::random_hex::<32>();
                    set.push(origin.cookie(ANON_COOKIE, &fresh, ANON_COOKIE_AGE_S));
                    anon_principal(&fresh)
                }
            };
            let result = self.call_op(caller, &facts, &principal, link, op, body).await?;
            json_response(&result)?
        } else if let Some(op) = path.strip_prefix("__push-").filter(|op| matches!(*op, "key" | "sub" | "unsub")) {
            // web push: anyone who can see the fragment (push.rs)
            self.admit(&facts, caller, link, Role::Public)?;
            let answer = if op == "key" {
                self.push_key().await?
            } else {
                if req.method() != Method::Post || !req.headers().get("content-type")?.is_some_and(|c| c.starts_with("application/json")) {
                    return Err(CellError::invalid("POST the subscription as application/json"));
                }
                let body: Value = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                let principal = caller.principal().map(str::to_string).or(anon).unwrap_or_else(|| "anonymous".into());
                if op == "sub" {
                    self.push_subscribe(&body, &principal)?
                } else {
                    self.push_unsubscribe(&body)?
                }
            };
            json_response(&answer)?
        } else if path == "__people" {
            // names for a page: a person's username and picture, or whose agent
            self.reader(&mut facts, caller, link, Role::Public).await?;
            let ids: Vec<String> = url.query_pairs().filter(|(k, _)| k == "id").map(|(_, v)| v.into_owned()).collect();
            let mut answer = crate::ask_registry(&self.env, &crate::registry::calls::Profiles { ids }).await?;
            let platform = self.cfg.platform(&caller.url);
            // bounded: the registry answers at most 64 profiles
            for p in answer.profiles.values_mut() {
                p.picture = p.picture.take().map(|path| format!("{platform}{path}"));
                if let Some(fragment) = p.fragment.clone() {
                    let face = crate::fragment::ask(&self.env, &fragment, "computer/face", &json!({})).await.unwrap_or(Value::Null);
                    p.title = face["title"].as_str().map(str::to_string);
                }
            }
            json_response(&answer)?
        } else if path == "__members" {
            // who is in it, for a page (a chat's agents, its lead the first
            // added): viewers and up, as the API's list
            self.reader(&mut facts, caller, link, Role::Viewer).await?;
            json_response(&self.member_list()?)?
        } else if let Some(sha) = path.strip_prefix("__blob/") {
            match req.method() {
                // one of this fragment's blobs by its hash (blobs.rs): viewers and up
                Method::Get | Method::Head => {
                    self.reader(&mut facts, caller, link, Role::Viewer).await?;
                    self.serve_blob(&req, sha).await?
                }
                // a page's upload (a chat's attachment), its bytes streamed
                // through the router: editors, as the API's (`put_blob`)
                Method::Put => match self.identified(caller, name).await {
                    Ok(caller) => self.put_blob(&caller, sha, &req).await?,
                    Err(e) => {
                        // its bytes are read all the same (js::drain)
                        js::drain(&req).await?;
                        return Err(e);
                    }
                },
                _ => return Err(CellError::invalid("read a blob with GET or HEAD, or upload one with PUT")),
            }
        } else if path == "__sw.js" {
            script(&req, crate::push::SW_JS, SW_JS_HASH)?
        } else if path == "__watch" {
            self.watch(&req, caller, &facts, link)?
        } else if path == "__live" {
            // an unsigned visitor without a cookie yet gets one, as a call
            // does: its sockets share one principal, and one query budget
            let principal = match (caller.principal(), anon) {
                (Some(p), _) => p.to_string(),
                (None, Some(a)) => a,
                (None, None) => {
                    let fresh = js::random_hex::<32>();
                    set.push(origin.cookie(ANON_COOKIE, &fresh, ANON_COOKIE_AGE_S));
                    anon_principal(&fresh)
                }
            };
            let resp = self.live(&req, caller, credential, &principal, link)?;
            // a page following this fragment may soon post to a computer's channel
            self.prewake();
            resp
        } else {
            match req.method() {
                Method::Get | Method::Head => self.site(&mut req, caller, &mut facts, &path, &url, link, anon).await?,
                // Only the app's own routes take other methods.
                _ => self.app_fetch(&mut req, caller, &facts, &path, &url, link, anon).await?,
            }
        };
        with_cookies(resp, &set)
    }

    /// The author's `fetch` for a path that is not a site file: it sees the
    /// fragment's public URL and who is asking (`x-fragment-principal`,
    /// `x-fragment-role`), so they are resolved first.
    #[allow(clippy::too_many_arguments)]
    async fn app_fetch(&self, req: &mut Request, caller: &Caller, facts: &Facts, path: &str, url: &url::Url, link: bool, anon: Option<String>) -> CellResult<Response> {
        let caller = self.identified(caller, &facts.name).await?;
        let role = self.admit(facts, &caller, link, Role::Public)?;
        let who = caller.principal().map(npub::display).or(anon).unwrap_or_else(|| "anonymous".into());
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
        headers.set("x-fragment-principal", &who)?;
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
        let target = format!("{}{path}{query}", self.cfg.canonical(&caller.url, &facts.name));
        facet.fetch(Request::new_with_init(&target, &init)?).await
    }

    /// The change feed for `fragment sync --watch`: a frame per external
    /// move of main. Viewers and up; a revoked member's feed closes.
    fn watch(&self, req: &Request, caller: &Caller, facts: &Facts, link: bool) -> CellResult<Response> {
        if !req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("__watch is a WebSocket; send Upgrade: websocket"));
        }
        let standing = self.standing(caller, link)?;
        decide(facts.visibility, standing, Purpose::Read, Role::Viewer)?;
        let pair = WebSocketPair::new()?;
        let who = match caller.principal() {
            Some(p) if as_themselves(standing) => format!("p:{p}"),
            _ => "view".to_string(),
        };
        self.state.accept_websocket_with_tags(&pair.server, &["watch", &who]);
        pair.server.send_with_str(json!({ "type": "hello", "ref": "main", "sha": facts.pin_main }).to_string())?;
        Ok(Response::from_websocket(pair.client)?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn site(&self, req: &mut Request, caller: &Caller, facts: &mut Facts, path: &str, url: &url::Url, link: bool, anon: Option<String>) -> CellResult<Response> {
        // a page or a file reads alike for everyone who may see the fragment
        let reader = self.reader(facts, caller, link, Role::Public).await?;
        let caller: &Caller = &reader;
        let head = req.method() == Method::Head;
        if path == "__fragment.js" {
            return script(req, CLIENT_JS, CLIENT_JS_HASH);
        }
        if path == "__files.js" {
            return script(req, FILES_JS, FILES_JS_HASH);
        }
        if path == "__files.css" {
            return compiled_in(req, FILES_CSS, FILES_CSS_HASH, "text/css; charset=utf-8");
        }
        self.ensure_pins(facts).await?;
        let facts = &*facts;
        let name = facts.name.as_str();
        let live = facts.pin_live.as_deref().ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("{name} has no live commit yet: deploy first")))?;
        let public = facts.visibility == Visibility::Public;
        let headers = |ct: &str, cache: &str| -> CellResult<Headers> {
            let h = Headers::new();
            h.set("content-type", ct)?;
            h.set("cache-control", cache)?;
            h.set("x-fragment-ref", live)?;
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
                    .filter(|r| !site::is_machinery(&r.path))
                    .map(|r| {
                        let size = blobs.get(&r.path).copied().unwrap_or(r.size);
                        json!({ "path": r.path, "size": size, "mode": r.mode, "lastCommitSha": r.last_commit })
                    })
                    .collect();
                return json_response(&json!({ "type": "tree", "ref": "live", "sha": live, "count": files.len(), "files": files }));
            }
            "__files" => {
                if !req.headers().get("accept")?.is_some_and(|a| a.contains("application/json")) {
                    return Ok(Response::ok(files_page(name))?.with_headers(headers("text/html; charset=utf-8", "no-store")?));
                }
                // a path on both is live's, as `__file` reads it
                let mut sizes = std::collections::BTreeMap::new();
                for which in ["main", "live"] {
                    let blobs = self.pointer_sizes(which)?;
                    for r in self.tree_rows(which)?.into_iter().filter(|r| !site::is_machinery(&r.path)) {
                        let size = blobs.get(&r.path).copied().unwrap_or(r.size);
                        sizes.insert(r.path, size);
                    }
                }
                let files: Vec<Value> = sizes.into_iter().map(|(path, size)| json!({ "path": path, "size": size })).collect();
                return json_response(&json!({ "type": "files", "count": files.len(), "files": files }));
            }
            "__file" => {
                let p = url.query_pairs().find(|(k, _)| k == "path").map(|(_, v)| v.into_owned()).unwrap_or_default();
                if !valid_repo_path(&p) || site::is_machinery(&p) {
                    return Err(CellError::invalid("path must be a content path (not the fragment's own machinery)"));
                }
                // Data an app writes lands on main; a path not yet on live reads from there.
                let (which, row) = match self.tree_row("live", &p)? {
                    Some(row) => ("live", row),
                    None => match self.tree_row("main", &p)? {
                        Some(row) => ("main", row),
                        None => return Err(CellError::new(ErrorCode::NotFound, format!("no file {p}"))),
                    },
                };
                if head {
                    return Ok(Response::empty()?.with_headers(headers(site::mime_for_path(&p), "no-store")?));
                }
                let range = req.headers().get("range")?;
                let mut resp = self.stream_file(facts, which, &row, range.as_deref()).await?;
                resp.headers_mut().set("cache-control", "no-store")?;
                return Ok(resp);
            }
            _ => {}
        }
        if !path.is_empty() && !valid_repo_path(path.trim_end_matches('/')) {
            return Err(CellError::new(ErrorCode::NotFound, "no such page"));
        }
        // a blessed template's fragment: its site is the platform release's (decision 40)
        if let Some(installed) = self.meta(MetaKey::Blessed)? {
            let (t, release) = installed.split_once('@').unwrap_or((installed.as_str(), ""));
            for candidate in site::site_candidates(path) {
                if let Some(bytes) = blessed::site_file(t, &candidate) {
                    return self.blessed_page(req, caller, facts, &candidate, bytes, release, public).await;
                }
            }
        }
        let mut found = None;
        for candidate in site::site_candidates(path) {
            if let Some(row) = self.tree_row("live", &candidate)? {
                found = Some(row);
                break;
            }
        }
        let Some(row) = found else {
            if !path.starts_with("__") {
                return self.app_fetch(req, caller, facts, path, url, link, anon).await;
            }
            return Err(CellError::new(ErrorCode::NotFound, format!("no page {path:?}")));
        };
        let mime = site::mime_for_path(&row.path);
        let cache = site::cache_control(&row.path, public);
        // a file of a pointer's size may be one: its bytes are a blob's
        let pointer = if crate::blobs::maybe_pointer(row.size) { self.pointer("live", &row.path)? } else { None };
        // Only a page gets Open Graph tags, so only a page reads `meta`.
        let page = pointer.is_none() && mime.starts_with("text/html") && row.size <= OG_MAX_BYTES;
        let stored = if page { self.meta(MetaKey::MetaLive)? } else { None };
        let og: Option<fragment_core::manifest::Meta> = match stored {
            Some(text) => Some(serde_json::from_str(&text).map_err(|e| CellError::host(format!("the stored meta does not decode: {e}")))?),
            None => None,
        };
        // Revalidation is answered here, before code.storage is asked for
        // anything: the tree row already names the bytes.
        let etag = match og {
            Some(_) => site::page_etag(&row.last_commit, live),
            None => site::file_etag(&row.last_commit),
        };
        if let Some(resp) = not_modified(req, &etag, cache)? {
            return Ok(resp);
        }
        let with_etag = |mut resp: Response| -> CellResult<Response> {
            resp.headers_mut().set("content-type", mime)?;
            resp.headers_mut().set("cache-control", cache)?;
            resp.headers_mut().set("etag", &etag)?;
            Ok(resp)
        };
        if head {
            let size = pointer.map_or(row.size, |(_, size)| size);
            let h = headers(mime, cache)?;
            h.set("content-length", &size.to_string())?;
            return with_etag(Response::empty()?.with_headers(h));
        }
        let range = req.headers().get("range")?;
        if let Some((sha, _)) = pointer {
            return with_etag(self.stream_blob(&sha, mime, range.as_deref()).await?);
        }
        if let Some(meta) = og {
            if let Some(bytes) = self.cs()?.read(&facts.repo, live, &row.path, OG_MAX_BYTES as usize).await? {
                let html = String::from_utf8_lossy(&bytes);
                let image = format!("{}__preview.svg", self.cfg.canonical(&caller.url, name));
                let page = site::inject_og(&html, name, &meta, &image);
                return with_etag(Response::from_html(page)?.with_headers(headers(mime, cache)?));
            }
        }
        with_etag(self.stream_git(facts, "live", &row.path).await?)
    }
}

impl FragmentCell {
    /// A file of a blessed template's site, from the release: a page gets
    /// the fragment's own Open Graph tags, as a site's page does.
    #[allow(clippy::too_many_arguments)]
    async fn blessed_page(&self, req: &Request, caller: &Caller, facts: &Facts, path: &str, bytes: &'static [u8], release: &str, public: bool) -> CellResult<Response> {
        let mime = site::mime_for_path(path);
        let cache = site::cache_control(path, public);
        let page = mime.starts_with("text/html");
        let og: Option<fragment_core::manifest::Meta> = match page.then(|| self.meta(MetaKey::MetaLive)).transpose()?.flatten() {
            Some(text) => Some(serde_json::from_str(&text).map_err(|e| CellError::host(format!("the stored meta does not decode: {e}")))?),
            None => None,
        };
        let live = facts.pin_live.as_deref().unwrap_or("");
        let etag = match og {
            Some(_) => format!("\"b-{release}-{}\"", &live[..live.len().min(12)]),
            None => format!("\"b-{release}\""),
        };
        if let Some(resp) = not_modified(req, &etag, cache)? {
            return Ok(resp);
        }
        let h = Headers::new();
        h.set("content-type", mime)?;
        h.set("cache-control", cache)?;
        h.set("etag", &etag)?;
        h.set("x-fragment-ref", live)?;
        if req.method() == Method::Head {
            h.set("content-length", &bytes.len().to_string())?;
            return Ok(Response::empty()?.with_headers(h));
        }
        if let Some(meta) = og {
            let html = String::from_utf8_lossy(bytes);
            let image = format!("{}__preview.svg", self.cfg.canonical(&caller.url, &facts.name));
            return Ok(Response::from_html(site::inject_og(&html, &facts.name, &meta, &image))?.with_headers(h));
        }
        Ok(Response::from_bytes(bytes.to_vec())?.with_headers(h))
    }
}

/// `__files`: the files viewer's page (`files.mjs`), which lists the
/// fragment's content files by asking `__files` for JSON.
fn files_page(name: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{n}: files</title>
<link rel="stylesheet" href="./__files.css">
</head>
<body data-fragment="{n}">
<script type="module" src="./__files.js"></script>
</body>
</html>"#,
        n = site::html_escape(name)
    )
}
