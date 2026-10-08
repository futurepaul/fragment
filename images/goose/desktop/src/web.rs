//! Reading and searching the web without a browser: the fast way, for
//! pages that need no clicking (docs/computers.md, "Our images": goose).
//!
//! - `web_read {url, mode?, start?, links?}`: the page as Markdown. `article`
//!   (the default) is its main text, as Firefox's reader view finds it
//!   (dom_smoothie, Mozilla's Readability ported); `page` is all of its
//!   body. Long pages come in parts of `PART_MAX_CHARS`, the next named by
//!   `start`. A PDF or another file is saved to the work's downloads, and
//!   its path said.
//! - `web_search {query, max?}`: DuckDuckGo's HTML results (no key): title,
//!   address and snippet of each.
//!
//! The transport is curl (the image's, with its CA bundle and the
//! computer's egress), bounded in time and size.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::mcp::{self, cut};

/// A page is fetched within this.
pub const FETCH_MS_MAX: u64 = 25_000;
/// A page is at most this many bytes (curl's `--max-filesize`).
pub const PAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// A part of a page's text the model is given at once.
pub const PART_MAX_CHARS: usize = 24_000;
/// A part asked for is at most this, and at least `PART_MIN_CHARS`.
pub const PART_ASKED_MAX_CHARS: usize = 60_000;
pub const PART_MIN_CHARS: usize = 1_000;
/// Links listed with a page, at most.
pub const LINKS_MAX: usize = 150;
/// Search results, at most and by default.
pub const RESULTS_MAX: usize = 20;
pub const RESULTS_DEFAULT: usize = 8;
/// A query is at most this long.
pub const QUERY_MAX_BYTES: usize = 500;
/// An address is at most this long.
pub const URL_MAX_BYTES: usize = 4096;
/// Where a file that is no page is saved.
pub const DOWNLOADS: &str = "/data/work/downloads";
/// A browser's agent string: some sites answer curl's with a refusal.
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// What a fetch got.
#[derive(Debug)]
pub struct Fetched {
    pub status: u16,
    pub content_type: String,
    pub url: String,
    pub body: Vec<u8>,
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("web-{}-{}", std::process::id(), fragment_bridge::log::now_ms()))
}

/// One fetch with curl: GET, or POST of `form` (`name=value`, urlencoded).
pub async fn fetch(url: &str, form: &[(&str, &str)]) -> Result<Fetched, String> {
    let file = scratch();
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-sS", "-L", "--compressed", "--max-redirs", "8", "--proto", "=http,https", "--proto-redir", "=http,https"]);
    cmd.args(["--max-time", &(FETCH_MS_MAX / 1000).to_string(), "--max-filesize", &PAGE_MAX_BYTES.to_string(), "-A", USER_AGENT]);
    cmd.args(["-H", "accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8", "-H", "accept-language: en-US,en;q=0.9"]);
    for (k, v) in form {
        cmd.arg("--data-urlencode").arg(format!("{k}={v}"));
    }
    cmd.arg("-o").arg(&file).args(["-w", "%{http_code}\t%{content_type}\t%{url_effective}"]).arg("--").arg(url);
    let out = tokio::time::timeout(Duration::from_millis(FETCH_MS_MAX + 5_000), cmd.kill_on_drop(true).output()).await.map_err(|_| "no answer in time".to_string())?.map_err(|e| format!("curl: {e}"))?;
    let body = std::fs::read(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let w = String::from_utf8_lossy(&out.stdout);
    let mut parts = w.splitn(3, '\t');
    let status = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let content_type = parts.next().unwrap_or("").to_ascii_lowercase();
    let url = parts.next().unwrap_or(url).to_string();
    Ok(Fetched { status, content_type, url, body })
}

/// Whether a page is an address the tools fetch: http or https.
pub fn url_ok(url: &str) -> bool {
    url.len() <= URL_MAX_BYTES && (url.starts_with("https://") || url.starts_with("http://")) && !url.chars().any(char::is_whitespace)
}

/// `href` made absolute against the page at `base`.
pub fn absolute(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") || href.starts_with("data:") {
        return None;
    }
    if href.starts_with("https://") || href.starts_with("http://") {
        return Some(href.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(net) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{net}"));
    }
    let host_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..host_end]);
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    let path = &rest[host_end..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
    let dir = if dir.is_empty() { "/" } else { dir };
    Some(format!("{origin}{dir}{href}"))
}

/// A page's Markdown: its main text (`article`), or its whole body.
pub fn markdown(html: &str, url: &str, article: bool) -> (String, String) {
    if article {
        let cfg = dom_smoothie::Config { text_mode: dom_smoothie::TextMode::Markdown, ..Default::default() };
        if let Ok(mut r) = dom_smoothie::Readability::new(html, Some(url), Some(cfg)) {
            if let Ok(a) = r.parse() {
                if a.text_content.trim().len() > 200 {
                    return (a.title, a.text_content.to_string());
                }
            }
        }
    }
    let doc = dom_query::Document::from(html);
    let title = doc.select("title").text().trim().to_string();
    let skip = ["script", "style", "noscript", "svg", "template", "iframe", "head", "meta"];
    let text = match doc.select("body").nodes().first() {
        Some(body) => body.md(Some(&skip)).to_string(),
        None => doc.md(Some(&skip)).to_string(),
    };
    (title, text)
}

/// The page's links, absolute, each once, at most `LINKS_MAX`.
pub fn links(html: &str, url: &str) -> Vec<(String, String)> {
    let doc = dom_query::Document::from(html);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for a in doc.select("a[href]").iter() {
        let Some(href) = a.attr("href").and_then(|h| absolute(url, &h)) else { continue };
        if !seen.insert(href.clone()) {
            continue;
        }
        let text: String = a.text().split_whitespace().collect::<Vec<_>>().join(" ");
        out.push((cut(&text, 120).to_string(), href));
        if out.len() >= LINKS_MAX {
            break;
        }
    }
    out
}

/// A part of `text` from char `start`, at most `max` chars: the part, and
/// where the next starts (none at the end).
pub fn part(text: &str, start: usize, max: usize) -> (String, Option<usize>, usize) {
    let total = text.chars().count();
    let s: String = text.chars().skip(start).take(max).collect();
    let end = (start + max).min(total);
    (s, (end < total).then_some(end), total)
}

/// A search result.
#[derive(Debug, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// DuckDuckGo's address for a result: its redirect's `uddg`, decoded.
fn unwrap_ddg(href: &str) -> String {
    let Some(q) = href.split_once("uddg=").map(|(_, q)| q) else { return href.to_string() };
    let enc = q.split('&').next().unwrap_or(q);
    percent_decode(enc)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    // bounded by the string: each pass takes a byte or three
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// DuckDuckGo's HTML results page, read: its organic results, ads left out.
pub fn ddg_hits(html: &str) -> Vec<Hit> {
    let doc = dom_query::Document::from(html);
    let mut out = Vec::new();
    for r in doc.select(".result").iter() {
        if r.has_class("result--ad") {
            continue;
        }
        let a = r.select("a.result__a");
        let Some(href) = a.attr("href") else { continue };
        let url = unwrap_ddg(&href);
        if !url.starts_with("http") || url.contains("duckduckgo.com/y.js") {
            continue;
        }
        let squash = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push(Hit { title: squash(a.text().to_string()), url, snippet: squash(r.select(".result__snippet").text().to_string()) });
    }
    out
}

pub async fn read(args: &Value) -> Value {
    let Some(url) = args["url"].as_str().filter(|u| url_ok(u)) else { return mcp::text_result("web_read needs `url`, an http(s) address", true) };
    let article = args["mode"].as_str() != Some("page");
    let start = args["start"].as_u64().unwrap_or(0) as usize;
    let max = (args["max_chars"].as_u64().unwrap_or(PART_MAX_CHARS as u64) as usize).clamp(PART_MIN_CHARS, PART_ASKED_MAX_CHARS);
    let f = match fetch(url, &[]).await {
        Ok(f) => f,
        Err(e) => return mcp::text_result(&format!("could not fetch {url}: {e}"), true),
    };
    if f.status >= 400 || f.status == 0 {
        let excerpt = String::from_utf8_lossy(&f.body[..f.body.len().min(600)]).into_owned();
        return mcp::text_result(&format!("{url} answered {}: {}\n(a page that refuses a plain fetch may open in the browser tools)", f.status, excerpt.trim()), true);
    }
    let is_html = f.content_type.contains("html") || (f.content_type.is_empty() && f.body.starts_with(b"<"));
    let is_text = f.content_type.starts_with("text/") || f.content_type.contains("json") || f.content_type.contains("xml") || f.content_type.contains("javascript");
    if !is_html && !is_text {
        return match save(&f) {
            Ok(path) => mcp::text_result(&format!("{} is no page ({}, {} bytes): saved to {}. Read it with a tool for its kind (a PDF: `pdftotext {} -`).", f.url, f.content_type, f.body.len(), path.display(), path.display()), false),
            Err(e) => mcp::text_result(&format!("{} is no page ({}), and saving it failed: {e}", f.url, f.content_type), true),
        };
    }
    let html = String::from_utf8_lossy(&f.body).into_owned();
    let (title, text) = if is_html { markdown(&html, &f.url, article) } else { (String::new(), html.clone()) };
    let (body, next, total) = part(&text, start, max);
    let mut said = format!("# {}\n{}\n\n{}", if title.is_empty() { &f.url } else { &title }, f.url, body.trim());
    match next {
        Some(n) => said.push_str(&format!("\n\n(characters {start} to {n} of {total}: the rest with web_read start={n})")),
        None if start > 0 => said.push_str(&format!("\n\n(characters {start} to {total} of {total}: the end)")),
        None => {}
    }
    if args["links"].as_bool() == Some(true) && is_html {
        let l = links(&html, &f.url);
        said.push_str(&format!("\n\n## Links ({})\n", l.len()));
        for (text, href) in l {
            said.push_str(&format!("- [{}]({href})\n", if text.is_empty() { "-" } else { &text }));
        }
    }
    mcp::text_result(&said, false)
}

/// A file that is no page, saved under `DOWNLOADS` by its address's last
/// part.
fn save(f: &Fetched) -> std::io::Result<PathBuf> {
    let name: String = f.url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).take(80).collect();
    let name = if name.is_empty() || name.starts_with('.') { format!("download-{}", fragment_bridge::log::now_ms()) } else { name };
    std::fs::create_dir_all(DOWNLOADS)?;
    let path = Path::new(DOWNLOADS).join(name);
    std::fs::write(&path, &f.body)?;
    Ok(path)
}

pub async fn search(args: &Value) -> Value {
    let Some(q) = args["query"].as_str().map(str::trim).filter(|q| !q.is_empty() && q.len() <= QUERY_MAX_BYTES) else {
        return mcp::text_result(&format!("web_search needs `query`, at most {QUERY_MAX_BYTES} bytes"), true);
    };
    let max = (args["max"].as_u64().unwrap_or(RESULTS_DEFAULT as u64) as usize).clamp(1, RESULTS_MAX);
    let f = match fetch("https://html.duckduckgo.com/html/", &[("q", q)]).await {
        Ok(f) => f,
        Err(e) => return mcp::text_result(&format!("the search did not answer: {e}"), true),
    };
    let html = String::from_utf8_lossy(&f.body).into_owned();
    let hits = ddg_hits(&html);
    if hits.is_empty() {
        let refused = f.status != 200 || html.contains("anomaly") || html.contains("challenge");
        let why = if refused { format!("the search engine refused this computer ({}): try again in a minute, or search in the browser tools", f.status) } else { "no results".to_string() };
        return mcp::text_result(&format!("{why} for {q:?}"), refused);
    }
    let mut said = format!("Results for {q:?}:\n");
    for (i, h) in hits.iter().take(max).enumerate() {
        said.push_str(&format!("\n{}. {}\n   {}\n   {}\n", i + 1, h.title, h.url, h.snippet));
    }
    said.push_str("\n(read one with web_read url=…)");
    mcp::text_result(&said, false)
}

pub fn tools() -> Vec<Value> {
    vec![
        mcp::tool(
            "web_read",
            "Read a web page as Markdown, fast, without the browser: for pages that need no clicking or login (docs, articles, wikis, lists, APIs that answer JSON). `mode` \"article\" (default) is its main text; \"page\" is everything on it (use for lists, tables, search pages, when article misses things). Long pages come in parts: pass `start` for the next. `links: true` lists the page's links (to crawl). A PDF or other file is saved to /data/work/downloads. If a page needs JavaScript or a login, or comes back empty, use the browser tools instead.",
            json!({ "type": "object", "required": ["url"], "properties": {
                "url": { "type": "string", "description": "the http(s) address" },
                "mode": { "type": "string", "enum": ["article", "page"] },
                "start": { "type": "integer", "minimum": 0, "description": "the character to start from (from the last part's note)" },
                "max_chars": { "type": "integer", "minimum": PART_MIN_CHARS, "maximum": PART_ASKED_MAX_CHARS },
                "links": { "type": "boolean", "description": "list the page's links too" }
            } }),
        ),
        mcp::tool(
            "web_search",
            "Search the web (DuckDuckGo): the top results' titles, addresses and snippets. Then web_read the ones worth reading.",
            json!({ "type": "object", "required": ["query"], "properties": {
                "query": { "type": "string" },
                "max": { "type": "integer", "minimum": 1, "maximum": RESULTS_MAX }
            } }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_made_absolute() {
        let base = "https://example.com/docs/guide/intro.html?x=1";
        assert_eq!(absolute(base, "https://a.b/c").as_deref(), Some("https://a.b/c"));
        assert_eq!(absolute(base, "//cdn.x/y").as_deref(), Some("https://cdn.x/y"));
        assert_eq!(absolute(base, "/root").as_deref(), Some("https://example.com/root"));
        assert_eq!(absolute(base, "next.html").as_deref(), Some("https://example.com/docs/guide/next.html"));
        assert_eq!(absolute("https://example.com", "a").as_deref(), Some("https://example.com/a"));
        for none in ["#top", "javascript:void(0)", "mailto:a@b", ""] {
            assert_eq!(absolute(base, none), None, "{none}");
        }
    }

    #[test]
    fn only_web_addresses_are_read() {
        assert!(url_ok("https://example.com/a?b=c"));
        for bad in ["file:///etc/passwd", "ftp://x", "example.com", "https://a b"] {
            assert!(!url_ok(bad), "{bad}");
        }
    }

    /// A page comes in parts, each naming where the next starts.
    #[test]
    fn long_pages_come_in_parts() {
        let text = "é".repeat(2500);
        let (p, next, total) = part(&text, 0, 1000);
        assert_eq!((p.chars().count(), next, total), (1000, Some(1000), 2500));
        let (p, next, _) = part(&text, 2000, 1000);
        assert_eq!((p.chars().count(), next), (500, None));
    }

    /// DuckDuckGo's HTML: organic results with their real addresses; ads
    /// left out.
    #[test]
    fn search_results_are_read() {
        let html = r#"<html><body>
        <div class="result results_links result--ad"><a class="result__a" href="https://duckduckgo.com/y.js?ad=1">Ad</a></div>
        <div class="result results_links"><h2><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=abc">Rust  Programming
        Language</a></h2><a class="result__snippet" href="x">A language empowering <b>everyone</b>.</a></div>
        <div class="result results_links"><a class="result__a" href="https://doc.rust-lang.org/book/">The Book</a><div class="result__snippet">Learn Rust</div></div>
        </body></html>"#;
        let hits = ddg_hits(html);
        assert_eq!(hits, vec![
            Hit { title: "Rust Programming Language".into(), url: "https://www.rust-lang.org/".into(), snippet: "A language empowering everyone.".into() },
            Hit { title: "The Book".into(), url: "https://doc.rust-lang.org/book/".into(), snippet: "Learn Rust".into() },
        ]);
    }

    /// An article's main text, and a whole page's, as Markdown.
    #[test]
    fn pages_read_as_markdown() {
        let para = "Rust is a language for reliable and efficient software. ".repeat(12);
        let html = format!(r#"<html><head><title>On Rust</title></head><body><nav><a href="/">Home</a> <a href="/about">About</a></nav><article><h1>On Rust</h1><p>{para}</p><p>See <a href="/book">the book</a>.</p></article><script>var x = 1;</script></body></html>"#);
        let (title, text) = markdown(&html, "https://example.com/post", true);
        assert_eq!(title, "On Rust");
        assert!(text.contains("reliable and efficient") && !text.contains("var x"), "{text}");
        let (_, page) = markdown(&html, "https://example.com/post", false);
        assert!(page.contains("About") && page.contains("reliable") && !page.contains("var x"), "{page}");
        let l = links(&html, "https://example.com/post");
        assert!(l.contains(&("the book".to_string(), "https://example.com/book".to_string())), "{l:?}");
        assert_eq!(percent_decode("a%20b+c%2F"), "a b c/");
    }
}
