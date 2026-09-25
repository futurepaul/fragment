//! Serving a fragment's `site/` from its live pin: file lookup, types,
//! caching, cookies, and the Open Graph tags `meta` asks for; and where a
//! browser returns to after signing in.

use url::Url;

use crate::manifest::Meta;

/// A `return=` path the platform keeps for a browser's way back.
pub const RETURN_PATH_MAX_BYTES: usize = 2048;

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

/// A site file's entity tag: its tree row's last commit, which with its
/// path names its bytes. A strong validator: the file's bytes change only
/// with a commit that touches it.
pub fn file_etag(last_commit: &str) -> String {
    format!("\"{last_commit}\"")
}

/// A page served with Open Graph tags: its body also follows the live
/// commit's `fragment.json` (its `meta`), so the tag names the live pin
/// too, and is weak (the page is made, not stored).
pub fn page_etag(last_commit: &str, live: &str) -> String {
    format!("W/\"{last_commit}.{live}\"")
}

/// A hash of bytes compiled into the cell (the browser library, the
/// service worker), taken at build time for their entity tags: 64-bit
/// FNV-1a, which needs no dependency and runs in a `const`. Not for
/// anything an adversary chooses: these bytes are the platform's own.
pub const fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    // bounded: one turn per byte of a constant
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    hash
}

/// The entity tag for a `content_hash`.
pub fn hash_etag(hash: u64) -> String {
    format!("\"{hash:016x}\"")
}

/// Whether an `If-None-Match` header names `etag`, so the answer is 304:
/// `*`, or any of its tags by the weak comparison `If-None-Match` uses
/// (RFC 9110 13.1.2: a `W/` prefix on either side is ignored).
pub fn not_modified(if_none_match: &str, etag: &str) -> bool {
    let opaque = |tag: &str| {
        let tag = tag.trim();
        tag.strip_prefix("W/").unwrap_or(tag).to_string()
    };
    let wanted = opaque(etag);
    // bounded by the header's length: one turn per comma
    if_none_match.trim() == "*" || if_none_match.split(',').any(|tag| opaque(tag) == wanted)
}

/// One cookie's value from a `Cookie` header.
pub fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then(|| v.trim())
    })
}

/// The path a browser returns to after signing in, from the `return=` it
/// brought (a query value, so already percent-decoded once): the path when
/// it is one on this origin, and `/` otherwise.
///
/// Kept: a path that begins with exactly one `/` and holds no byte at or
/// below 0x20, no DEL, and no backslash, and that, percent-decoded once
/// more, still does not begin with `//`, `/\`, or `/` and a control byte.
/// The URL parser trims and drops control bytes and spaces, and a browser
/// reads `\` as `/`: `/ //evil.example` once became the scheme-relative
/// `//evil.example` and left the platform.
pub fn return_path(raw: Option<&str>) -> String {
    match raw {
        Some(path) if returnable(path) => path.to_string(),
        _ => "/".to_string(),
    }
}

/// Where a browser returns to on `base` (an origin, or a fragment's base
/// under one, ending in `/`), from the `return=` it brought: `return_path`
/// joined under `base`, and `base` itself should the join name another
/// origin.
pub fn return_url(base: &Url, raw: Option<&str>) -> Url {
    joined(base, &return_path(raw))
}

fn returnable(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.len() > RETURN_PATH_MAX_BYTES {
        return false;
    }
    if bytes.iter().any(|&b| b <= 0x20 || b == 0x7f || b == b'\\') {
        return false;
    }
    plain_start(bytes) && plain_start(&percent_decoded(bytes))
}

/// A path that begins with one `/`, not followed by another, a backslash,
/// or a control byte or space (which a parser would drop, leaving `//`).
fn plain_start(path: &[u8]) -> bool {
    match path {
        [b'/'] => true,
        [b'/', next, ..] => !(*next == b'/' || *next == b'\\' || *next <= 0x20 || *next == 0x7f),
        _ => false,
    }
}

/// `bytes` with each `%XX` decoded; a malformed escape stays as it is.
fn percent_decoded(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    // bounded: `i` grows by one or three each turn
    while i < bytes.len() {
        let escape = match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(high), Some(low)) => Some(high * 16 + low),
            _ => None,
        };
        match escape {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    out
}

/// `path` under `base`, as a path relative to it (`./…`: a colon in its
/// first segment is never read as a scheme, nor a leading `//` as a host),
/// or `base` itself when the result would name another origin. Neither the
/// filter in `return_path` nor the `./` lets that happen; the origin check
/// is the tripwire should both miss a form.
fn joined(base: &Url, path: &str) -> Url {
    assert!(path.starts_with('/'), "a return path begins with /");
    match base.join(&format!(".{path}")) {
        Ok(url) if url.origin() == base.origin() => url,
        _ => base.clone(),
    }
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
    let page_title = if html.contains("<title") { String::new() } else { format!("<title>{title}</title>") };
    let tags = format!(
        r#"<meta property="og:title" content="{title}"><meta property="og:description" content="{}"><meta property="og:image" content="{}"><meta name="twitter:card" content="summary_large_image">{page_title}"#,
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

    /// Goal: only a name with a content hash in it is cached for a year,
    /// and a public fragment's such file by shared caches too. The e2e's
    /// site section reads the other three rows off served pages. Method:
    /// names at the edges of the hash rule, on a public fragment.
    #[test]
    fn only_a_hashed_name_is_immutable() {
        for (path, hashed) in [
            ("app.3f9a1c2e.js", true),
            ("assets/app.3F9A1C2E.min.css", true),
            ("app.0123456789abcdef.js", true),
            ("app.3f9a1c2.js", false), // seven hex digits: too short to be a hash
            ("app.3f9a1c2g.js", false),
            ("deadbeef", false), // the last part is the extension, never the hash
            ("app.deadbeef", false),
            ("3f9a1c2e.d/app.js", false), // a hashed folder names no file's bytes
        ] {
            let expected = if hashed { "public, max-age=31536000, immutable" } else { "public, max-age=60" };
            assert_eq!(cache_control(path, true), expected, "{path}");
        }
    }

    /// Goal: a conditional GET is answered 304 exactly when it names the
    /// file's current tag. Method: tags as browsers and caches send them
    /// (weak, strong, lists, `*`) against strong and weak tags, and the
    /// ones that must miss (another commit, a tag cut short, no tag).
    #[test]
    fn entity_tags() {
        let file = file_etag("3f9a1c2e3f9a1c2e3f9a1c2e3f9a1c2e3f9a1c2e");
        assert_eq!(file, "\"3f9a1c2e3f9a1c2e3f9a1c2e3f9a1c2e3f9a1c2e\"");
        let page = page_etag("aaaa", "bbbb");
        assert_eq!(page, "W/\"aaaa.bbbb\"");
        for header in [file.as_str(), &format!("W/{file}"), &format!("\"x\", {file}"), &format!(" {file} ,\"y\""), "*"] {
            assert!(not_modified(header, &file), "{header}");
        }
        for header in ["W/\"aaaa.bbbb\"", "\"aaaa.bbbb\"", "\"zz\", W/\"aaaa.bbbb\""] {
            assert!(not_modified(header, &page), "{header}");
        }
        for header in ["", "\"3f9a1c2e\"", "\"aaaa.cccc\"", "W/\"aaaa\"", "3f9a1c2e3f9a1c2e3f9a1c2e3f9a1c2e3f9a1c2e", "**"] {
            assert!(!not_modified(header, &file) && !not_modified(header, &page), "{header}");
        }
        // a tag names a live pin: another deploy is another tag
        assert!(!not_modified(&page_etag("aaaa", "bbbb"), &page_etag("aaaa", "cccc")));
    }

    /// Goal: the build-time hash is FNV-1a 64 (so its tags are stable
    /// across builds of the same bytes). Method: the algorithm's published
    /// test vectors, and a `const` use.
    #[test]
    fn content_hashes() {
        const EMPTY: u64 = content_hash(b"");
        assert_eq!(EMPTY, 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(content_hash(b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(hash_etag(0xaf63_dc4c_8601_ec8c), "\"af63dc4c8601ec8c\"");
    }

    #[test]
    fn cookies_and_og() {
        assert_eq!(cookie("a=1; fragview=tok; b=2", "fragview"), Some("tok"));
        assert_eq!(cookie("a=1", "fragview"), None);
        let meta = Meta { title: Some("T<x>".into()), description: None, image: None };
        let out = inject_og("<html><head></head></html>", "n", &meta, "https://n/__preview.svg");
        assert!(out.contains(r#"og:title" content="T&lt;x&gt;""#) && out.contains("__preview.svg"));
        assert_eq!(inject_og(&out, "n", &meta, "x"), out);
        assert!(out.contains("<title>T&lt;x&gt;</title>"));
        let titled = inject_og("<head><title>Own</title></head>", "n", &meta, "x");
        assert_eq!(titled.matches("<title>").count(), 1, "a page's own title stays the only one");
        assert!(preview_svg("todo").starts_with("<svg"));
    }

    /// Goal: no `return=` a browser can bring leaves the platform's origin
    /// (audit R1). Method: every form that once did (a space, tab, or CR/LF
    /// before `//`, an encoded slash or backslash, a scheme in the first
    /// segment), and the legitimate paths that must survive, against a host
    /// base and a dev fleet's path base, with the expected URL written out.
    #[test]
    fn return_paths_stay_on_the_origin() {
        let club = Url::parse("https://fragment.club/").unwrap();
        let dev = Url::parse("http://127.0.0.1:8790/f/todo/").unwrap();
        let hostile = [
            "/ //evil.example",
            "/\t//evil.example",
            "/\r\n//evil.example",
            "/\n//evil.example",
            "/\u{7f}//evil.example",
            "//evil.example",
            "///evil.example",
            "/\\evil.example",
            "/\\/evil.example",
            "/%2F/evil.example",
            "/%2f%2fevil.example",
            "/%5Cevil.example",
            "/%09//evil.example",
            "/%20//evil.example",
            "/%0D%0A//evil.example",
            "/ok\\..\\evil",
            "/a b",
            "https://evil.example/",
            "evil.example",
            "",
        ];
        for raw in hostile {
            assert_eq!(return_path(Some(raw)), "/", "{raw:?}");
            assert_eq!(return_url(&club, Some(raw)).as_str(), "https://fragment.club/", "{raw:?}");
            assert_eq!(return_url(&dev, Some(raw)).as_str(), "http://127.0.0.1:8790/f/todo/", "{raw:?}");
        }
        assert_eq!(return_path(None), "/");
        assert_eq!(return_path(Some(&format!("/{}", "a".repeat(RETURN_PATH_MAX_BYTES)))), "/", "too long");
        let longest = format!("/{}", "a".repeat(RETURN_PATH_MAX_BYTES - 1));
        assert_eq!(return_path(Some(&longest)), longest);

        // a scheme in the first segment is a path here, never a scheme
        // (joined bare, `http:evil.example` under an https base was http://evil.example/)
        assert_eq!(return_url(&club, Some("/http:evil.example")).as_str(), "https://fragment.club/http:evil.example");
        assert_eq!(return_url(&club, Some("/javascript:alert(1)")).as_str(), "https://fragment.club/javascript:alert(1)");
        assert_eq!(return_url(&dev, Some("/https:evil.example")).as_str(), "http://127.0.0.1:8790/f/todo/https:evil.example");

        let kept = [
            ("/", "https://fragment.club/", "http://127.0.0.1:8790/f/todo/"),
            ("/x", "https://fragment.club/x", "http://127.0.0.1:8790/f/todo/x"),
            ("/a%20b", "https://fragment.club/a%20b", "http://127.0.0.1:8790/f/todo/a%20b"),
            ("/c++/notes%2Fold", "https://fragment.club/c++/notes%2Fold", "http://127.0.0.1:8790/f/todo/c++/notes%2Fold"),
            ("/__join?invite=abc", "https://fragment.club/__join?invite=abc", "http://127.0.0.1:8790/f/todo/__join?invite=abc"),
            ("/auth/fragment?name=f&return=%2Fx", "https://fragment.club/auth/fragment?name=f&return=%2Fx", "http://127.0.0.1:8790/f/todo/auth/fragment?name=f&return=%2Fx"),
            ("/doc#part", "https://fragment.club/doc#part", "http://127.0.0.1:8790/f/todo/doc#part"),
        ];
        for (raw, on_club, on_dev) in kept {
            assert_eq!(return_path(Some(raw)), raw);
            assert_eq!(return_url(&club, Some(raw)).as_str(), on_club);
            assert_eq!(return_url(&dev, Some(raw)).as_str(), on_dev);
        }
    }

    /// Goal: the join alone keeps a path on the origin, should the filter in
    /// `return_path` ever miss a form. Method: paths the filter refuses,
    /// handed to the join directly: each stays a path under the base.
    #[test]
    fn the_join_alone_stays_on_the_origin() {
        let club = Url::parse("https://fragment.club/").unwrap();
        for (path, expected) in [
            ("//evil.example", "https://fragment.club//evil.example"),
            ("/\\evil.example", "https://fragment.club//evil.example"),
            ("/\t//evil.example", "https://fragment.club///evil.example"),
            ("/http://evil.example/", "https://fragment.club/http://evil.example/"),
        ] {
            assert_eq!(joined(&club, path).as_str(), expected, "{path:?}");
        }
    }
}
