//! Serving a fragment: its site from the `live` pin, the machine-read
//! plane (`__tree`, `__file`), browser calls (`__op`), and the change feed
//! (`__watch`). Who may see what follows the fragment's visibility:
//! members always; on a `link` or `public` fragment, whoever holds the
//! share link counts as a viewer (a `?view=` token sets a cookie on the
//! fragment's origin); on a `public` fragment, everyone else holds the
//! `public` floor. An unsigned browser calling an operation gets an
//! anonymous principal: a random cookie whose hash names it. A browser
//! signed in on this origin (`__signin`, the router's) is its person, and
//! accepts an invite at `__join?invite=<token>`.
//!
//! The router hands a site request's signer or session on unresolved: a
//! page or a file answers alike for everyone who may see the fragment, so
//! the registry is asked who is asking only when the anonymous standing may
//! not read (`reader`), or when the answer is someone's (`answers_someone`,
//! and the app's own routes).

use fragment_core::access::Purpose;
use fragment_core::{npub, site};
use fragment_proto::{valid_repo_path, ErrorCode, OpCall, Role, Visibility};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{as_themselves, decide, decode_segment, json_response, Caller, Facts, FragmentCell, MetaKey};
use crate::js;
use crate::routed::Mode;

/// The browser library pages import as `./__fragment.js`.
const CLIENT_JS: &str = include_str!("../client.mjs");
/// The scripts' entity tags, hashed at build time: a page view revalidates
/// them (`no-cache`) and gets 304 until a cell deploy changes their bytes.
const CLIENT_JS_HASH: u64 = site::content_hash(CLIENT_JS.as_bytes());
const SW_JS_HASH: u64 = site::content_hash(crate::push::SW_JS.as_bytes());
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
fn not_modified(req: &Request, etag: &str, cache: &str) -> CellResult<Option<Response>> {
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
    let etag = site::hash_etag(hash);
    if let Some(resp) = not_modified(req, &etag, "no-cache")? {
        return Ok(resp);
    }
    let h = Headers::new();
    h.set("content-type", "text/javascript; charset=utf-8")?;
    h.set("cache-control", "no-cache")?;
    h.set("etag", &etag)?;
    Ok(Response::ok(body)?.with_headers(h))
}

/// Whether a site path's answer is someone's, beyond whether they may see
/// the fragment: an operation's call, a socket (`__live`'s presence; a
/// member's `__watch` closes when they leave), an invite, a push
/// subscription, or the owner's fragments. Its caller is resolved first.
fn answers_someone(path: &str) -> bool {
    path.starts_with("__op/") || matches!(path, "__push-key" | "__push-sub" | "__push-unsub" | "__fragments" | "__join" | "__watch" | "__live")
}

fn with_cookies(mut resp: Response, cookies: &[String]) -> CellResult<Response> {
    for c in cookies {
        resp.headers_mut().append("set-cookie", c)?;
    }
    Ok(resp)
}

impl FragmentCell {
    pub(crate) async fn serve(&self, mut req: Request, caller: &Caller, name: &str, rest: &str) -> CellResult<Response> {
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
        } else if path == "__fragments" {
            let answer = if req.method() == Method::Post {
                // a cross-site form cannot send JSON without a preflight
                if !req.headers().get("content-type")?.is_some_and(|c| c.starts_with("application/json")) {
                    return Err(CellError::invalid("send the fragment to make as application/json"));
                }
                let body: Value = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                self.owner_create(caller, body).await?
            } else {
                self.owner_fragments(caller).await?
            };
            json_response(&answer)?
        } else if path == "__people" {
            // names for a page: a person's username and picture, or whose agent
            self.reader(&mut facts, caller, link).await?;
            let ids: Vec<String> = url.query_pairs().filter(|(k, _)| k == "id").map(|(_, v)| v.into_owned()).collect();
            let mut answer = crate::ask_registry(&self.env, &crate::registry::calls::Profiles { ids }).await?;
            let platform = self.cfg.platform(&caller.url);
            for p in answer.profiles.values_mut() {
                p.picture = p.picture.take().map(|path| format!("{platform}{path}"));
            }
            json_response(&answer)?
        } else if path == "__sw.js" {
            script(&req, crate::push::SW_JS, SW_JS_HASH)?
        } else if path == "__join" {
            self.join_page(&mut req, caller, name, &url).await?
        } else if path == "__watch" {
            self.watch(&req, caller, &facts, link)?
        } else if path == "__live" {
            // an unsigned visitor without a cookie yet is anonymous for this socket only
            let principal = caller.principal().map(str::to_string).or(anon).unwrap_or_else(|| anon_principal(&js::random_hex::<32>()));
            self.live(&req, caller, &principal, link)?
        } else {
            match req.method() {
                Method::Get | Method::Head => self.site(&mut req, caller, &mut facts, &path, &url, link, anon).await?,
                // Only the app's own routes take other methods.
                _ => self.app_fetch(&mut req, caller, &facts, &path, &url, link, anon).await?,
            }
        };
        with_cookies(resp, &set)
    }

    /// An invite in a browser: sign in on this origin, then a button that
    /// posts back here (a cross-site form carries no SameSite=Lax cookie).
    async fn join_page(&self, req: &mut Request, caller: &Caller, name: &str, url: &url::Url) -> CellResult<Response> {
        let base = self.cfg.canonical(&caller.url, name);
        let form = |bytes: &[u8]| url::form_urlencoded::parse(bytes).find(|(k, _)| k == "invite").map(|(_, v)| v.into_owned());
        match req.method() {
            Method::Get => {
                let invite = url.query_pairs().find(|(k, _)| k == "invite").map(|(_, v)| v.into_owned()).unwrap_or_default();
                if caller.principal().is_none() {
                    let back: String = url::form_urlencoded::byte_serialize(format!("/__join?invite={invite}").as_bytes()).collect();
                    let mut resp = Response::empty()?.with_status(302);
                    resp.headers_mut().set("location", &format!("{base}__signin?return={back}"))?;
                    return Ok(resp);
                }
                let esc = |t: &str| t.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
                let html = format!(
                    r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Join {n}</title>
<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:34rem;margin:12vh auto;padding:0 16px}}button{{font:inherit;padding:.5em 1.1em;border-radius:8px}}</style>
<h1>Join {n}</h1><form method="post" action="{base}__join"><input type="hidden" name="invite" value="{i}"><button>Join</button></form>"#,
                    n = esc(name),
                    i = esc(&invite)
                );
                let h = Headers::new();
                h.set("content-type", "text/html; charset=utf-8")?;
                h.set("cache-control", "no-store")?;
                Ok(Response::ok(html)?.with_headers(h))
            }
            Method::Post => {
                let origin = req.headers().get("origin")?;
                if origin.is_some_and(|o| !base.starts_with(&o)) {
                    return Err(CellError::new(ErrorCode::Forbidden, "an invite is accepted from this fragment's own page"));
                }
                let token = form(&req.bytes().await?).ok_or_else(|| CellError::invalid("the form names no invite"))?;
                self.join(caller, fragment_proto::Join { token }).await?;
                let mut resp = Response::empty()?.with_status(302);
                resp.headers_mut().set("location", &base)?;
                Ok(resp)
            }
            _ => Err(CellError::invalid("__join takes GET or POST")),
        }
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
        let reader = self.reader(facts, caller, link).await?;
        let caller: &Caller = &reader;
        let head = req.method() == Method::Head;
        if path == "__fragment.js" {
            return script(req, CLIENT_JS, CLIENT_JS_HASH);
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
                let mut paths: Vec<String> = ["live", "main"]
                    .into_iter()
                    .map(|r| self.tree_rows(r))
                    .collect::<CellResult<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .map(|r| r.path)
                    .filter(|p| !site::is_machinery(p))
                    .collect();
                paths.sort();
                paths.dedup();
                return Ok(Response::ok(files_page(name, &paths))?.with_headers(headers("text/html; charset=utf-8", "no-store")?));
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

/// `__files`: a fragment's content files as links to `__file`. Framed (a
/// desktop's pane), a click asks the page around it to open the file
/// instead, as `{fragment: "open", url, title}`.
fn files_page(name: &str, paths: &[String]) -> String {
    let items: String = paths
        .iter()
        .map(|p| {
            let href = format!("__file?path={}", url::form_urlencoded::byte_serialize(p.as_bytes()).collect::<String>());
            format!("<li><a href=\"{}\">{}</a></li>", site::html_escape(&href), site::html_escape(p))
        })
        .collect();
    let list = if items.is_empty() { "<p>No files yet.</p>".to_string() } else { format!("<ul>{items}</ul>") };
    format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{n}: files</title>
<style>body{{font:14px/1.5 ui-sans-serif,system-ui,sans-serif;margin:0;padding:12px 16px;color:#1d2126;background:#fff}}ul{{list-style:none;padding:0;margin:0}}li{{padding:3px 0}}a{{color:#2a5bd7;text-decoration:none;font-family:ui-monospace,monospace}}a:hover{{text-decoration:underline}}p{{color:#6b7280}}
@media (prefers-color-scheme:dark){{body{{background:#15181b;color:#e6e8ea}}a{{color:#7aa2f7}}}}</style>
{list}
<script>if (parent !== window) document.addEventListener("click", (e) => {{ const a = e.target.closest("a"); if (!a) return; e.preventDefault(); parent.postMessage({{ fragment: "open", url: a.href, title: a.textContent }}, "*"); }});</script>"#,
        n = site::html_escape(name)
    )
}
