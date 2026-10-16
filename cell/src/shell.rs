//! The shell (docs/cloudflare-v1.md, decisions 6–12): the platform's one
//! page, at `/`, `/settings` and `/welcome`, and its files at
//! `/__shell/<file>`, the release's (cell/shell/), read from its Static
//! Assets (assets.rs). It holds no key: its script calls the API with
//! the person's platform session (lib.rs `shell_session`) and frames their
//! fragments, each signed in on its own origin by the platform's frame mint.
//!
//! Its policy: scripts and styles from this origin only (and Google
//! Fonts' stylesheet and files); frames of this origin (the share sheet,
//! the frame mint's redirects) and of the fragments' and computers' hosts
//! under the suffix; never framed itself.

use fragment_proto::ErrorCode;
use fragment_templates::File;
use worker::*;

use crate::config::Config;
use crate::error::CellResult;

/// A file's validator: its bytes' hash (a release changes them).
fn etag(f: &File) -> String {
    format!("\"s-{}\"", &f.sha256[..20])
}

fn not_modified(req: &Request, tag: &str) -> Result<bool> {
    Ok(req.headers().get("if-none-match")?.is_some_and(|v| v.split(',').any(|t| t.trim() == tag)))
}

/// One of the shell's pages, its file in the release.
fn page_file(name: &str) -> &'static File {
    fragment_templates::shell(name).unwrap_or_else(|| panic!("the shell's {name} is in the release's index"))
}

/// Where the shell may frame: this origin, and every fragment's and
/// computer's host under the suffix.
fn frame_src(cfg: &Config, url: &Url) -> String {
    let platform = cfg.platform();
    let scheme = url.scheme();
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!("'self' {platform} {scheme}://*.{}{port}", cfg.host_suffix)
}

/// `GET /`, `GET /settings` and `GET /welcome`: the shell's page, for
/// anyone (signed out, it asks them to sign in; the shell opens the view
/// its path names).
pub async fn page(env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    h.set(
        "content-security-policy",
        &format!(
            "default-src 'self'; script-src 'self'; style-src 'self' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' data: blob:; connect-src 'self'; frame-src {}; manifest-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'",
            frame_src(cfg, url)
        ),
    )?;
    h.set("x-frame-options", "DENY")?;
    // a page that opens it in a window keeps no hold on it, as on every platform page
    h.set("cross-origin-opener-policy", "same-origin")?;
    h.set("referrer-policy", "strict-origin-when-cross-origin")?;
    crate::assets::body(env, page_file("index.html"), h).await
}

/// `GET /admin`: the operators' admin page (decision 59), to the platform
/// session of one of the deployment's operators alone, by the rule the
/// admin's API holds the shell's session to (orgs.rs `session_operator`).
/// To anyone else, signed out, signed in, or with a session that ended,
/// it is the router's 404 for a path it lacks (lib.rs `no_route`), word
/// for word: no one else learns it is here. Its API decides again. It
/// frames nothing and is never framed.
pub async fn admin_page(req: &Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let missing = || crate::no_route(url.path());
    let Some(token) = crate::auth::platform_session_token(req, url)? else { return Err(missing()) };
    match crate::orgs::session_operator(env, cfg, token).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(missing()),
        // a session not live (401), or a person being wiped (403), is no operator's
        Err(e) if matches!(e.code, ErrorCode::Unauthenticated | ErrorCode::Forbidden) => return Err(missing()),
        Err(e) => return Err(e),
    }
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    h.set(
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-src 'none'; form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'",
    )?;
    h.set("x-frame-options", "DENY")?;
    h.set("cross-origin-opener-policy", "same-origin")?;
    h.set("referrer-policy", "same-origin")?;
    crate::assets::body(env, page_file("admin.html"), h).await
}

/// `GET /__shell/<file>`: one of the files it publishes
/// (`fragment_templates::shell_published`), or `None`. A 304 and a HEAD
/// are answered from the index, before any read.
pub async fn asset(req: &Request, env: &Env, name: &str) -> CellResult<Option<Response>> {
    let Some((f, mime)) = fragment_templates::shell_published(name) else { return Ok(None) };
    let tag = etag(f);
    let h = Headers::new();
    h.set("content-type", mime)?;
    // revalidated each time: a release changes them under the same names
    h.set("cache-control", "no-cache")?;
    h.set("etag", &tag)?;
    h.set("x-content-type-options", "nosniff")?;
    if not_modified(req, &tag)? {
        return Ok(Some(Response::empty()?.with_status(304).with_headers(h)));
    }
    if req.method() == Method::Head {
        h.set("content-length", &f.size.to_string())?;
        return Ok(Some(Response::empty()?.with_headers(h)));
    }
    Ok(Some(crate::assets::body(env, f, h).await?))
}
