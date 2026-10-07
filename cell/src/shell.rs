//! The shell (docs/cloudflare-v1.md, decisions 6–12): the platform's one
//! page, at `/` and `/settings`, and its files at `/__shell/<file>`, compiled into the
//! release (cell/shell/). It holds no key: its script calls the API with
//! the person's platform session (lib.rs `shell_session`) and frames their
//! fragments, each signed in on its own origin by the platform's frame mint.
//!
//! Its policy: scripts and styles from this origin only (and Google
//! Fonts' stylesheet and files); frames of this origin (the share sheet,
//! the frame mint's redirects) and of the fragments' and computers' hosts
//! under the suffix; never framed itself.

use worker::*;

use crate::config::Config;

/// The shell's files: published name, content type, and bytes.
const FILES: [(&str, &str, &[u8]); 13] = [
    ("shell.js", "text/javascript; charset=utf-8", include_bytes!("../shell/shell.js")),
    ("shell.css", "text/css; charset=utf-8", include_bytes!("../shell/shell.css")),
    ("layout.js", "text/javascript; charset=utf-8", include_bytes!("../shell/layout.js")),
    ("viewer.js", "text/javascript; charset=utf-8", include_bytes!("../shell/viewer.js")),
    ("agent-identity.js", "text/javascript; charset=utf-8", include_bytes!("../shell/agent-identity.js")),
    ("app-icons.js", "text/javascript; charset=utf-8", include_bytes!("../shell/app-icons.js")),
    ("lucide-icons.js", "text/javascript; charset=utf-8", include_bytes!("../shell/lucide-icons.js")),
    ("tooltips.js", "text/javascript; charset=utf-8", include_bytes!("../shell/tooltips.js")),
    ("vendor/split-grid.js", "text/javascript; charset=utf-8", include_bytes!("../shell/vendor/split-grid.js")),
    ("manifest.webmanifest", "application/manifest+json", include_bytes!("../shell/manifest.webmanifest")),
    ("icon.svg", "image/svg+xml", include_bytes!("../shell/icon.svg")),
    // the viewer's wallpaper: Teo Badini's photograph on Pexels (cell/shell/CREDITS.md)
    ("wallpaper.jpg", "image/jpeg", include_bytes!("../shell/wallpaper.jpg")),
    // every agent's image, tinted to its colour (shell.css; CREDITS.md)
    ("agent.png", "image/png", include_bytes!("../shell/agent.png")),
];
const PAGE: &str = include_str!("../shell/index.html");

/// A file's validator: its bytes' hash (a release changes them).
fn etag(body: &[u8]) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(body);
    format!("\"s-{}\"", hex::encode(&digest[..10]))
}

fn not_modified(req: &Request, tag: &str) -> Result<bool> {
    Ok(req.headers().get("if-none-match")?.is_some_and(|v| v.split(',').any(|t| t.trim() == tag)))
}

/// Where the shell may frame: this origin, and every fragment's and
/// computer's host under the suffix.
fn frame_src(cfg: &Config, url: &Url) -> String {
    let platform = cfg.platform();
    let scheme = url.scheme();
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!("'self' {platform} {scheme}://*.{}{port}", cfg.host_suffix)
}

/// `GET /` and `GET /settings`: the shell's page, for anyone (signed out,
/// it asks them to sign in; the shell opens the view its path names).
pub fn page(req: &Request, cfg: &Config, url: &Url) -> Result<Response> {
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
    let _ = req;
    Ok(Response::ok(PAGE)?.with_headers(h))
}

/// `GET /__shell/<file>`: one of its files, or `None`.
pub fn asset(req: &Request, name: &str) -> Result<Option<Response>> {
    let Some((_, mime, body)) = FILES.iter().find(|(n, _, _)| *n == name) else { return Ok(None) };
    let tag = etag(body);
    let h = Headers::new();
    h.set("content-type", mime)?;
    // revalidated each time: a release changes them under the same names
    h.set("cache-control", "no-cache")?;
    h.set("etag", &tag)?;
    h.set("x-content-type-options", "nosniff")?;
    if not_modified(req, &tag)? {
        return Ok(Some(Response::empty()?.with_status(304).with_headers(h)));
    }
    Ok(Some(Response::from_bytes(body.to_vec())?.with_headers(h)))
}
