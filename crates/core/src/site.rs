//! Serving a fragment's `site/` from its live pin: file lookup, types,
//! caching, cookies, and the Open Graph tags `meta` asks for.

use crate::manifest::Meta;

pub fn mime_for_path(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "xml" => "application/xml",
        "csv" => "text/csv; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "webmanifest" => "application/manifest+json",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// The fragment's own organs, hidden from `__tree` and `__file`.
pub fn is_machinery(path: &str) -> bool {
    matches!(path, "fragment.json" | "app.mjs" | "rooms.mjs" | "_index.md")
        || ["workflows/", "applib/", "lib/", "node_modules/"].iter().any(|p| path.starts_with(p))
        || path.split('/').any(|seg| seg.starts_with('.'))
}

/// The repo paths a site path may name, in order: `a/` → `site/a/index.html`;
/// `a` → `site/a`, then `site/a/index.html`.
pub fn site_candidates(rest: &str) -> Vec<String> {
    if rest.is_empty() {
        return vec!["site/index.html".into()];
    }
    if let Some(dir) = rest.strip_suffix('/') {
        return vec![format!("site/{dir}/index.html")];
    }
    vec![format!("site/{rest}"), format!("site/{rest}/index.html")]
}

/// A name with a content hash in it (`app.3f9a1c2e.js`) never changes.
fn hashed(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.split('.').rev().skip(1).any(|part| part.len() >= 8 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Cache policy for a served file. Anything behind a share link or
/// membership is `private`: a shared cache must never hand it to someone else.
pub fn cache_control(path: &str, public_fragment: bool) -> &'static str {
    match (hashed(path), public_fragment) {
        (true, true) => "public, max-age=31536000, immutable",
        (true, false) => "private, max-age=31536000, immutable",
        (false, true) => "public, max-age=60",
        (false, false) => "private, no-cache",
    }
}

/// One cookie's value from a `Cookie` header.
pub fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then(|| v.trim())
    })
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Adds Open Graph tags to a page that has none.
pub fn inject_og(html: &str, name: &str, meta: &Meta, image_url: &str) -> String {
    if html.contains("og:title") {
        return html.to_string();
    }
    let title = html_escape(meta.title.as_deref().unwrap_or(name));
    let tags = format!(
        r#"<meta property="og:title" content="{title}"><meta property="og:description" content="{}"><meta property="og:image" content="{}"><meta name="twitter:card" content="summary_large_image"><title>{title}</title>"#,
        html_escape(meta.description.as_deref().unwrap_or("")),
        html_escape(meta.image.as_deref().unwrap_or(image_url)),
    );
    match html.find("<head>") {
        Some(i) => format!("{}{tags}{}", &html[..i + 6], &html[i + 6..]),
        None => format!("{tags}{html}"),
    }
}

/// The generated preview image a page without `meta.image` points at.
pub fn preview_svg(name: &str) -> String {
    let h = name.bytes().fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    let hue = h % 360;
    let letter = name.chars().next().unwrap_or('f').to_ascii_uppercase();
    let name = html_escape(name);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl({hue},65%,22%)"/><stop offset="1" stop-color="hsl({},70%,38%)"/></linearGradient></defs><rect width="1200" height="630" fill="url(#g)"/><circle cx="1050" cy="90" r="220" fill="hsl({},60%,45%)" opacity="0.25"/><text x="80" y="330" font-family="Georgia,serif" font-size="260" fill="hsl({hue},30%,92%)">{letter}</text><text x="84" y="520" font-family="ui-monospace,Menlo,monospace" font-size="44" fill="hsl({hue},25%,80%)">{name}</text></svg>"#,
        (hue + 60) % 360,
        (hue + 120) % 360,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_types() {
        assert_eq!(site_candidates(""), vec!["site/index.html"]);
        assert_eq!(site_candidates("docs/"), vec!["site/docs/index.html"]);
        assert_eq!(site_candidates("a.css"), vec!["site/a.css", "site/a.css/index.html"]);
        assert_eq!(mime_for_path("x/Y.PNG"), "image/png");
        assert_eq!(mime_for_path("noext"), "application/octet-stream");
        assert!(is_machinery("workflows/a.mjs") && is_machinery(".git/x") && is_machinery("a/.env"));
        assert!(!is_machinery("notes/a.md"));
    }

    #[test]
    fn caching() {
        assert_eq!(cache_control("app.3f9a1c2e.js", true), "public, max-age=31536000, immutable");
        assert_eq!(cache_control("index.html", false), "private, no-cache");
        assert_eq!(cache_control("index.html", true), "public, max-age=60");
    }

    #[test]
    fn cookies_and_og() {
        assert_eq!(cookie("a=1; fragview=tok; b=2", "fragview"), Some("tok"));
        assert_eq!(cookie("a=1", "fragview"), None);
        let meta = Meta { title: Some("T<x>".into()), description: None, image: None };
        let out = inject_og("<html><head></head></html>", "n", &meta, "https://n/__preview.svg");
        assert!(out.contains(r#"og:title" content="T&lt;x&gt;""#) && out.contains("__preview.svg"));
        assert_eq!(inject_og(&out, "n", &meta, "x"), out);
        assert!(preview_svg("todo").starts_with("<svg"));
    }
}
