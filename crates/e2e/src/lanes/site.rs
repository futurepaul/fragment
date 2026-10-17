//! Serving: each fragment on its own host, gated by visibility; the
//! machine-read plane; Open Graph pages; preview cards; and the change feed.
//!
//! A preview card (docs/cloudflare-v1.md, decision 31) is shot by
//! wrangler's local Browser Rendering: the binding's own local mode, a
//! Chrome for Testing it runs on this machine (a lower rung than
//! Cloudflare's browsers, which the hosted lane meets).

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{self, Api, Call, Reply};
use crate::Suite;

/// How long a card may take after its deploy: the fragment's alarm takes
/// the shot (a browser started, the page loaded and let settle) and keeps
/// it.
pub(super) const CARD_WAIT: Duration = Duration::from_secs(60);

/// A fragment's preview card once it shows `live` (the commit live moved
/// to), read as `keys`; `None` if it never did within `CARD_WAIT`.
pub(super) fn card_showing(s: &Suite, api: &Api, keys: &Keys, name: &str, live: &str) -> Option<Reply> {
    let mut got = None;
    s.eventually(CARD_WAIT, || {
        got = api.signed(keys, "GET", &format!("/api/f/{name}/card"), None).ok().filter(|r| r.status == 200 && r.header("x-fragment-ref") == live);
        got.is_some()
    });
    got
}

/// Whether a card's answer is one: a JPEG of the viewport, 1280×800, tagged
/// by its bytes' SHA-256, private to caches.
pub(super) fn is_card(r: &Reply) -> bool {
    let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&r.bytes));
    r.status == 200
        && r.header("content-type") == "image/jpeg"
        && fragment_core::media::jpeg_size(&r.bytes) == Some((1280, 800))
        && r.header("etag") == format!("\"{sha}\"")
        && r.header("cache-control") == "private, no-cache"
}

/// A card's answer, for a failure's detail (its bytes left out).
pub(super) fn card_detail(r: &Option<Reply>) -> Value {
    match r {
        Some(r) => json!({ "status": r.status, "type": r.header("content-type"), "etag": r.header("etag"), "ref": r.header("x-fragment-ref"), "bytes": r.bytes.len(), "size": fragment_core::media::jpeg_size(&r.bytes) }),
        None => Value::Null,
    }
}

/// The card and its schedule as the fragment keeps them (a test hook).
pub(super) fn cards(api: &Api, name: &str) -> Value {
    api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "cards" }))).map(|r| r.body).unwrap_or(Value::Null)
}

/// The kinds of a fragment's newest events, read as `keys`.
pub(super) fn event_kinds(api: &Api, keys: &Keys, name: &str) -> Vec<String> {
    let r = api.signed(keys, "GET", &format!("/api/f/{name}/events?tail=200"), None).map(|r| r.body).unwrap_or(Value::Null);
    r["events"].as_array().into_iter().flatten().filter_map(|e| e["kind"].as_str().map(str::to_string)).collect()
}

/// An SVG that runs script when opened as a document, unless sandboxed.
const SCRIPTED_SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>document.title = 'ran'</script></svg>";

/// Whether an answer carries the headers every answer on a fragment's
/// origin does (the router's `fragment_origin`).
fn guarded(r: &Reply) -> bool {
    r.header("x-content-type-options") == "nosniff" && r.header("referrer-policy") == "same-origin"
}

/// Whether any of an answer's policies sandboxes it (`sandbox`, a
/// directive of its own in any of its `Content-Security-Policy` headers).
fn sandboxed(r: &Reply) -> bool {
    r.headers.get_all("content-security-policy").iter().filter_map(|v| v.to_str().ok()).any(|v| v.split([',', ';']).any(|d| d.trim() == "sandbox"))
}

/// What a failed check of the origin's headers shows.
fn origin_detail(r: &Reply) -> Value {
    let csp: Vec<&str> = r.headers.get_all("content-security-policy").iter().filter_map(|v| v.to_str().ok()).collect();
    json!({ "status": r.status, "type": r.header("content-type"), "nosniff": r.header("x-content-type-options"), "referrer": r.header("referrer-policy"), "csp": csp })
}

/// The origin's own headers (audit, the lows beside H2), on the `site`
/// fragment as its first deploy left it, `cookie` its link: every answer
/// carries nosniff and a same-origin referrer policy, whoever made it (a
/// page, a file, the platform's script, the fragment's 404, the router's
/// refusal page, an error the router raised itself); scriptable media
/// (an SVG, at its site path or in `__file`) and `__file`'s HTML are
/// sandboxed; a page, a markdown file and a picture are not.
fn origin_headers(s: &mut Suite, api: &Api, name: &str, cookie: &str, view: &str) -> Result<()> {
    let page = |path: &str| api.page(name, path, Some(cookie));
    for (what, r) in [
        ("a page", page("")?),
        ("a site file", page("style.css")?),
        ("the browser library", page("__fragment.js")?),
        ("the fragment's 404", page("nope.html")?),
        ("a content file", page("__file?path=notes/a.md")?),
    ] {
        s.ok(&format!("{what} carries nosniff and a same-origin referrer policy"), guarded(&r), origin_detail(&r));
    }
    let r = api.call(Call { method: "GET", url: api.site_url(name, "?view=wrong"), extra: vec![("accept", "text/html".into())], ..Call::default() })?;
    s.ok(
        "so does the router's refusal page (a changed link)",
        r.status == 401 && r.header("content-type").starts_with("text/html") && guarded(&r),
        origin_detail(&r),
    );
    // the share link's navigation, which goes on without `view`, and a
    // worker's script, which only `__sw.js` is (#366): the router's
    // headers on both
    let r = api.call(Call {
        method: "GET",
        url: api.site_url(name, &format!("docs/?view={view}&x=1")),
        extra: vec![("accept", "text/html".into()), ("sec-fetch-mode", "navigate".into())],
        ..Call::default()
    })?;
    s.ok(
        "so does a share link's navigation, on to its URL without the link",
        r.status == 302 && !r.header("location").contains("view") && guarded(&r),
        origin_detail(&r),
    );
    let r = api.call(Call { method: "GET", url: api.site_url(name, "style.css"), cookie: Some(cookie.to_string()), extra: vec![("service-worker", "script".into())], ..Call::default() })?;
    s.ok("and the refusal of a worker's script that is not the platform's", r.status == 404 && guarded(&r), origin_detail(&r));
    let sha = "0".repeat(64);
    let r = api.call(Call { method: "PUT", url: api.site_url(name, &format!("__blob/{sha}")), body: Some(b"x".to_vec()), content_type: Some("text/plain"), ..Call::default() })?;
    s.ok(
        "and an error the router raises itself (an upload from no one), answered as JSON",
        r.status == 401 && r.message().contains("sign in on this fragment's page") && guarded(&r),
        origin_detail(&r),
    );
    for (what, path, kind) in [
        ("a site's SVG at its path", "logo.svg", "image/svg+xml"),
        ("the preview image", "__preview.svg", "image/svg+xml"),
        ("an SVG in __file", "__file?path=notes/x.svg", "image/svg+xml"),
        ("HTML in __file", "__file?path=notes/x.html", "text/html"),
    ] {
        let r = page(path)?;
        s.ok(&format!("{what} is sandboxed: opened directly, it runs no script on the origin"), r.status == 200 && r.header("content-type").starts_with(kind) && sandboxed(&r) && guarded(&r), origin_detail(&r));
    }
    let r = api.call(Call { method: "HEAD", url: api.site_url(name, "__file?path=notes/x.html"), cookie: Some(cookie.to_string()), ..Call::default() })?;
    s.ok("so is a HEAD of it", r.status == 200 && r.text.is_empty() && sandboxed(&r), origin_detail(&r));
    for (what, path) in [("a page", ""), ("a markdown file in __file", "__file?path=notes/a.md"), ("a picture", "dot.png")] {
        let r = page(path)?;
        s.ok(&format!("{what} is not sandboxed"), r.status == 200 && !sandboxed(&r) && guarded(&r), origin_detail(&r));
    }
    Ok(())
}

/// The change feed opened as `api::watch` opens it, with the upgrade's
/// answer: its status and headers.
fn watch_answered(api: &Api, name: &str, query: &str) -> Result<(tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::handshake::client::Response)> {
    use tungstenite::client::IntoClientRequest;
    let http = format!("{}/f/{name}/__watch{query}", api.base);
    let req = http.replacen("http", "ws", 1).into_client_request()?;
    let (socket, answer) = tungstenite::connect(req)?;
    let tcp = match socket.get_ref() {
        tungstenite::stream::MaybeTlsStream::Plain(s) => Some(s),
        tungstenite::stream::MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    if let Some(s) = tcp {
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
    }
    Ok((socket, answer))
}

pub fn site(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("site", &[crate::Need::Fakes, crate::Need::Deployment, crate::Need::Levers]) {
        return Ok(());
    }
    let owner = api.person()?;
    let viewer = api.person()?;
    let stranger = api.person()?;
    let name = s.named(api, &owner, "site")?;
    let c = s.create(api, &owner, &name)?;
    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    let r = api.page(&name, "", None)?;
    s.ok("visibility is checked before anything about the site", r.status == 401, &r);
    let r = api.page(&name, &format!("?view={view}"), None)?;
    s.ok("a fragment with nothing deployed says so", r.status == 404 && r.message().contains("deploy first"), &r);
    // The first request read the branches; later views ask code.storage
    // nothing (before, each asked for both). The poll backstop still reads
    // them every couple of seconds, far fewer than the views.
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let reads = s.fake.requests(&repo, "GET branch");
    let views = 20;
    let answered = (0..views).filter(|_| api.page(&name, &format!("?view={view}"), None).is_ok_and(|r| r.status == 404)).count();
    let asked = s.fake.requests(&repo, "GET branch") - reads;
    s.ok(
        "while nothing is deployed, page views do not ask code.storage for the branches",
        answered == views && (asked as usize) < views,
        format!("{answered} of {views} views answered 404; {asked} branch reads"),
    );
    s.commit(
        &c,
        &[
            ("site/index.html", Some(b"<!doctype html><html><head></head><body>front</body></html>")),
            ("site/docs/index.html", Some(b"<p>docs</p>")),
            ("site/app.3f9a1c2e.js", Some(b"console.log(1)")),
            ("site/style.css", Some(b"body{color:red}")),
            ("notes/a.md", Some(b"alpha")),
            ("site/logo.svg", Some(SCRIPTED_SVG)),
            ("site/dot.png", Some(b"\x89PNG\r\n\x1a\n")),
            ("notes/x.svg", Some(SCRIPTED_SVG)),
            ("notes/x.html", Some(b"<!doctype html><script>document.title = 'ran'</script>")),
            ("workflows/w.mjs", Some(b"code")),
            ("fragment.json", Some(br#"{"meta":{"title":"Site <Test>","description":"d"}}"#)),
        ],
    );
    let live = s.deploy(&c);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;

    // link (the default)
    let r = api.page(&name, "", None)?;
    s.ok("a link fragment refuses a visitor without the link", r.status == 401 && r.message().contains("share link"), &r);
    let r = api.page(&name, &format!("?view={view}"), None)?;
    let set = r.header("set-cookie");
    s.ok("the share link opens it", r.status == 200 && r.text.contains("front"), &r);
    s.ok("and sets a cookie for the fragment's origin", set.starts_with(&format!("fragview={view}; Path=/;")) && set.contains("HttpOnly"), &set);
    let cookie = format!("fragview={view}");
    // a page's navigation keeps the token in the cookie alone (the audit's
    // M2): on to its URL without `view`, where no page's code reads it
    let navigate = |path: &str, cookie: Option<&str>| {
        let extra = vec![
            ("accept", "text/html,application/xhtml+xml,*/*;q=0.8".to_string()),
            ("sec-fetch-dest", "document".to_string()),
            ("sec-fetch-mode", "navigate".to_string()),
            ("sec-fetch-site", "none".to_string()),
        ];
        api.call(Call { method: "GET", url: api.site_url(&name, path), cookie: cookie.map(str::to_string), extra, ..Call::default() })
    };
    let r = navigate(&format!("docs/?q=1&view={view}&b=x%20y"), None)?;
    let set = r.header("set-cookie");
    s.ok(
        "a navigation with the share link sets its cookie and goes on to its URL without it, uncached",
        r.status == 302 && r.header("location") == "/docs/?q=1&b=x%20y" && set.starts_with(&format!("fragview={view}; Path=/;")) && set.contains("HttpOnly") && r.header("cache-control") == "no-store",
        format!("{r} {set}"),
    );
    let r = navigate(&format!("?view={view}"), None)?;
    s.ok("one whose query was the token alone goes on to its path", r.status == 302 && r.header("location") == "/", &r);
    let r = navigate("docs/?q=1&b=x%20y", Some(&cookie))?;
    s.ok("where the cookie opens it", r.status == 200 && r.text.contains("docs"), &r);
    let r = navigate("?view=0123456789abcdef01234567", None)?;
    s.ok("a token that opens nothing is not followed on: its page says the link changed", r.status == 401 && r.header("location").is_empty() && r.header("set-cookie").is_empty(), &r);
    let r = navigate(&format!("/evil.example/?view={view}"), None)?;
    s.ok("the way on is a path on this origin, never another host (`//evil.example/`)", r.status == 302 && r.header("location") == "/evil.example/", &r);
    // a link unfurler sends no Fetch Metadata: it reads the page where it
    // asked, and its Open Graph tags (no redirect it may not follow)
    let unfurled = api.call(Call { method: "GET", url: api.site_url(&name, &format!("?view={view}")), extra: vec![("accept", "text/html".to_string())], ..Call::default() })?;
    s.ok("a share link's preview still unfurls: no Fetch Metadata, no redirect, the page and its tags", unfurled.status == 200 && unfurled.text.contains("og:title"), &unfurled);
    let r = api.page(&name, "docs/", Some(&cookie))?;
    s.ok("the cookie opens other pages", r.status == 200 && r.text.contains("docs"), &r);
    s.ok("a link fragment's pages are private to caches", r.header("cache-control") == "private, no-cache", r.header("cache-control"));
    let r = api.page(&name, "", Some("fragview=wrong"))?;
    s.ok("a wrong link is refused", r.status == 401, &r);
    let signed = |keys: &Keys| api.call(Call { method: "GET", url: api.site_url(&name, ""), keys: Some(keys), ..Call::default() });
    let r = signed(&viewer)?;
    s.ok("a signed member needs no link", r.status == 200, &r);
    let r = signed(&stranger)?;
    s.ok("a signed stranger without the link is 403", r.status == 403, &r);

    // its preview card: shot as a link holder sees the page, for its members
    let card = card_showing(s, api, &owner, &name, &live);
    s.ok("a deploy makes the fragment's preview card: a 1280×800 JPEG of its page, tagged by its bytes", card.as_ref().is_some_and(is_card), card_detail(&card));
    let etag = card.as_ref().map(|r| r.header("etag")).unwrap_or_default();
    let r = api.signed(&viewer, "GET", &format!("/api/f/{name}/card"), None)?;
    s.ok("a viewer gets the card", is_card(&r) && r.header("etag") == etag, card_detail(&Some(r)));
    let r = api.signed(&stranger, "GET", &format!("/api/f/{name}/card"), None)?;
    let anon = api.unsigned("GET", &format!("/api/f/{name}/card"), None)?;
    s.ok("a stranger does not (403), nor anyone unsigned (401)", r.status == 403 && anon.status == 401 && r.bytes.len() < 1000, format!("{r} | {anon}"));

    // the page itself
    let r = api.page(&name, "", Some(&cookie))?;
    s.ok("meta becomes Open Graph tags", r.text.contains(r#"og:title" content="Site &lt;Test&gt;""#) && r.text.contains("__preview.svg"), &r);
    let r = api.page(&name, "__preview.svg", Some(&cookie))?;
    s.ok("the preview image is served", r.status == 200 && r.header("content-type") == "image/svg+xml", &r);
    let r = api.page(&name, "app.3f9a1c2e.js", Some(&cookie))?;
    s.ok("hashed assets are immutable (privately, here)", r.header("cache-control") == "private, max-age=31536000, immutable", r.header("cache-control"));
    let r = api.page(&name, "nope.html", Some(&cookie))?;
    s.ok("a missing page is 404", r.status == 404, &r);
    let r = api.call(Call { method: "HEAD", url: api.site_url(&name, ""), cookie: Some(cookie.clone()), ..Call::default() })?;
    s.ok("HEAD answers without a body", r.status == 200 && r.text.is_empty(), &r);
    let r = api.call(Call { method: "DELETE", url: api.site_url(&name, ""), cookie: Some(cookie.clone()), ..Call::default() })?;
    s.ok("other methods are for the app's routes (none here: 404)", r.status == 404 && r.message().contains("no page or app route for DELETE /"), &r);
    origin_headers(s, api, &name, &cookie, &view)?;

    // revalidation: each file names its bytes in an ETag, and a conditional
    // GET naming it is answered before code.storage is asked for anything
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let conditional = |path: &str, tag: &str| {
        api.call(Call { method: "GET", url: api.site_url(&name, path), cookie: Some(cookie.clone()), extra: vec![("if-none-match", tag.to_string())], ..Call::default() })
    };
    let tag = |path: &str| api.page(&name, path, Some(&cookie)).map(|r| r.header("etag")).unwrap_or_default();
    let (css, hashed, page) = (tag("style.css"), tag("app.3f9a1c2e.js"), tag(""));
    s.ok("site files carry strong ETags, a page with Open Graph tags a weak one", css.starts_with('"') && hashed.starts_with('"') && page.starts_with("W/\""), format!("{css} {hashed} {page}"));
    let reads = s.fake.requests(&repo, "GET file");
    let r = conditional("style.css", &css)?;
    s.ok("a conditional GET naming a file's ETag is 304, without a body", r.status == 304 && r.text.is_empty() && r.header("etag") == css, &r);
    let r = conditional("", &format!("\"other\", {page}"))?;
    s.ok("a page revalidates too, by any tag in the list", r.status == 304 && r.header("cache-control") == "private, no-cache", &r);
    s.ok("revalidating asks code.storage for nothing", s.fake.requests(&repo, "GET file") == reads, s.fake.requests(&repo, "GET file") - reads);
    for file in ["__fragment.js", "__fragment.css", "__sw.js"] {
        let first = tag(file);
        let r = conditional(file, &first)?;
        s.ok(&format!("{file} revalidates by its build-time hash"), first.len() == 18 && r.status == 304, format!("{first} {r}"));
    }
    let r = api.page(&name, "__fragment.css", Some(&cookie))?;
    s.ok("__fragment.css is a stylesheet: the theme's variables", r.status == 200 && r.header("content-type") == "text/css; charset=utf-8" && r.text.contains("--accent:"), r.header("content-type"));
    s.commit(&c, &[("site/style.css", Some(b"body{color:blue}"))]);
    s.deploy(&c);
    let r = conditional("style.css", &css)?;
    s.ok("a deploy that changes a file changes its ETag: the old one gets the new bytes", r.status == 200 && r.text == "body{color:blue}" && r.header("etag") != css, &r);
    let r = conditional("", &page)?;
    s.ok("a deploy changes a page's ETag (its tags follow live's fragment.json)", r.status == 200 && r.text.contains("front") && r.header("etag") != page, &r);
    let r = conditional("app.3f9a1c2e.js", &hashed)?;
    s.ok("a file the deploy did not touch keeps its ETag", r.status == 304, &r);

    // A signed member's read waits on the Registry (without the link, the
    // anonymous standing may not read), and other requests run meanwhile.
    // A deploy that lands during that wait must not leave the answer
    // naming the new commit while it streams the old one's bytes.
    let blue = tag("style.css");
    s.commit(&c, &[("site/style.css", Some(b"body{color:green}"))]);
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "hold": 3000 })))?;
    s.ok("the registry holds its next answer (a test hook)", r.status == 200 && r.body["hold"] == 3000, &r);
    let asked = std::time::Instant::now();
    let (raced, sent) = std::thread::scope(|scope| {
        let read = scope.spawn(|| {
            let r = api.call(Call { method: "GET", url: api.site_url(&name, "style.css"), keys: Some(&viewer), ..Call::default() });
            (r, std::time::Instant::now())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        // the deploy is made now; when its refresh answers is the
        // cell's business, not the race's
        let sent = std::time::Instant::now();
        s.deploy(&c);
        (read.join().expect("the read's thread"), sent)
    });
    let ((r, answered), green) = (raced, tag("style.css"));
    let r = r?;
    s.ok(
        "the deploy was sent while the member's read waited on the registry",
        r.status == 200 && answered - asked >= std::time::Duration::from_millis(2_500) && sent < answered,
        format!("{}: answered after {:?}, the deploy sent at {:?}", r.status, answered - asked, sent - asked),
    );
    s.ok(
        "and its answer names the bytes it carries: the new commit's tag with the new bytes",
        r.header("etag") == green && r.text == "body{color:green}",
        format!("etag {} (new {green}, old {blue}): {:?}", r.header("etag"), r.text),
    );

    // the machine-read plane
    let r = api.page(&name, "__file?path=notes/a.md", Some(&cookie))?;
    s.ok("__file returns content", r.status == 200 && r.text == "alpha", &r);
    let r = api.page(&name, "__file?path=workflows/w.mjs", Some(&cookie))?;
    s.ok("__file refuses machinery", r.status == 400, &r);
    s.commit(&c, &[("data/new.json", Some(b"{}"))]);
    let r = api.page(&name, "__file?path=data/new.json", Some(&cookie))?;
    s.ok("__file reads a file only main has yet", r.status == 200 && r.text == "{}", &r);
    let r = api.page(&name, "__files", Some(&cookie))?;
    s.ok(
        "__files is the files viewer, the platform's page",
        r.status == 200 && r.header("content-type").starts_with("text/html") && r.text.contains("./__files.js") && r.text.contains("./__files.css"),
        &r,
    );
    let r = api.call(Call { method: "GET", url: api.site_url(&name, "__files"), cookie: Some(cookie.clone()), extra: vec![("accept", "application/json".into())], ..Call::default() })?;
    let listed: Vec<&str> = r.body["files"].as_array().map(|a| a.iter().filter_map(|f| f["path"].as_str()).collect()).unwrap_or_default();
    s.ok(
        "asked for JSON, it lists the content files of live and main, with their sizes, and not the machinery",
        r.status == 200
            && listed.contains(&"notes/a.md")
            && listed.contains(&"data/new.json")
            && !listed.iter().any(|p| p.starts_with("workflows/") || *p == "fragment.json")
            && r.body["files"].as_array().is_some_and(|a| a.iter().any(|f| f["path"] == "notes/a.md" && f["size"] == 5)),
        &r,
    );
    for asset in ["__files.js", "__files.css"] {
        let r = api.page(&name, asset, Some(&cookie))?;
        s.ok(&format!("{asset} is served, tagged with its build-time hash"), r.status == 200 && r.header("etag").len() == 18 && !r.text.is_empty(), &r);
    }
    let r = api.page(&name, "__files", None)?;
    s.ok("__files is gated like the site", r.status == 401, &r);

    // rotating the link
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({ "scopes": ["view"] })))?;
    let fresh = r.body["viewToken"].as_str().unwrap_or("").to_string();
    s.ok("the owner rotates the share link", r.status == 200 && fresh != view && r.body["rotated"] == json!(["view"]), &r);
    let r = api.page(&name, "", Some(&cookie))?;
    s.ok("the old link stops working", r.status == 401, &r);
    let r = api.page(&name, &format!("?view={fresh}"), None)?;
    s.ok("the new one works", r.status == 200, &r);
    let r = api.signed(&viewer, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({})))?;
    s.ok("only the owner rotates", r.status == 403, &r);

    // members and public
    let r = api.signed(&viewer, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    s.ok("only the owner changes visibility", r.status == 403, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let r = api.page(&name, &format!("?view={fresh}"), None)?;
    s.ok("a members fragment ignores the link", r.status == 401 && r.message().contains("members only"), &r);
    let r = signed(&viewer)?;
    s.ok("a members fragment serves its members", r.status == 200, &r);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let r = api.page(&name, "", None)?;
    s.ok("a public fragment serves anyone", r.status == 200 && r.header("cache-control") == "public, max-age=60", &r);
    let r = api.call(Call { method: "GET", url: api.site_url(&name, "__files"), extra: vec![("accept", "application/json".into())], ..Call::default() })?;
    s.ok("a public fragment's file list is public", r.status == 200 && r.body["files"].is_array(), &r);

    // /f/<name>/ on a fleet with hostnames
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}/docs/?x=1", api.base), ..Call::default() })?;
    s.ok(
        "a path URL redirects to the fragment's own origin",
        r.status == 308 && r.header("location") == api.site_url(&name, "docs/?x=1"),
        r.header("location"),
    );
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}", api.base), ..Call::default() })?;
    s.ok("the bare path redirects to its root", r.status == 308 && r.header("location") == api.site_url(&name, ""), &r);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/f/{name}/__op/x", api.base),
        body: Some(b"{}".to_vec()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    s.ok("a path URL never takes writes when fragments have hosts", r.status == 404, &r);
    let r = api.call(Call { method: "POST", url: format!("{}/f/{name}", api.base), body: Some(b"{}".to_vec()), content_type: Some("application/json"), ..Call::default() })?;
    s.ok("nor does the bare path: a write is no redirect (a 308 would send it on)", r.status == 404 && r.header("location").is_empty(), &r);
    preview_cards(s, api, &owner)
}

/// Preview cards beyond the first: who gets none, and a shot that fails.
fn preview_cards(s: &mut Suite, api: &Api, owner: &Keys) -> Result<()> {
    let page: &[u8] = b"<!doctype html><title>card</title><h1 style=\"font:64px system-ui\">a card</h1>";
    let lever = |name: &str, times: u64| api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "fail-cards", "times": times })));

    // a members-only fragment's page is a refusal to a visitor: it has no card
    let name = s.named(api, owner, "site-members")?;
    let c = s.create(api, owner, &name)?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    s.commit(&c, &[("site/index.html", Some(page))]);
    s.deploy(&c);
    let skipped = s.eventually(CARD_WAIT, || event_kinds(api, owner, &name).iter().any(|k| k == "card.skipped"));
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/card"), None)?;
    s.ok(
        "a members-only fragment gets no card (a visitor without an account sees only its refusal): 404, saying none was made",
        skipped && r.status == 404 && r.message().contains("no card yet") && cards(api, &name)["cards"]["wanted"].is_null(),
        json!({ "card": r.status, "cards": cards(api, &name), "events": event_kinds(api, owner, &name) }),
    );

    // a shot that fails is tried again, with a wait between, and the card comes
    let name = s.named(api, owner, "site-retry")?;
    let c = s.create(api, owner, &name)?;
    lever(&name, 2)?;
    s.commit(&c, &[("site/index.html", Some(page))]);
    let live = s.deploy(&c);
    let card = card_showing(s, api, owner, &name, &live);
    let kept = cards(api, &name);
    s.ok(
        "a shot that fails is tried again: two failed tries, then the card, from the third",
        card.as_ref().is_some_and(is_card) && kept["cards"]["card"]["attempt"] == 3 && kept["failCardsLeft"] == 0 && !event_kinds(api, owner, &name).iter().any(|k| k == "card.failed"),
        json!({ "card": card_detail(&card), "cards": kept }),
    );

    // and one that keeps failing ends quietly: one event, no card, nothing else changed
    let name = s.named(api, owner, "site-giveup")?;
    let c = s.create(api, owner, &name)?;
    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    lever(&name, 9)?;
    s.commit(&c, &[("site/index.html", Some(page))]);
    s.deploy(&c);
    let ended = s.eventually(Duration::from_secs(120), || event_kinds(api, owner, &name).iter().any(|k| k == "card.failed"));
    let kept = cards(api, &name);
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/card"), None)?;
    let failed = event_kinds(api, owner, &name).iter().filter(|k| *k == "card.failed").count();
    s.ok(
        "a shot that keeps failing gives up after five tries: one card.failed event, no card, and no more tries",
        ended && failed == 1 && r.status == 404 && kept["cards"]["wanted"].is_null() && kept["failCardsLeft"] == 4,
        json!({ "card": r.status, "cards": kept, "failed": failed }),
    );
    let site = api.page(&name, &format!("?view={view}"), None)?;
    s.ok("and the fragment serves its page all the same", site.status == 200 && site.text.contains("a card"), &site);
    page_errors(s, api, owner, page)
}

/// What a page reports as its card's shot loads it: on status and in
/// events, and cleared by a clean page's deploy.
fn page_errors(s: &mut Suite, api: &Api, owner: &Keys, clean: &[u8]) -> Result<()> {
    let name = s.named(api, owner, "site-errors")?;
    let c = s.create(api, owner, &name)?;
    let status = || api.signed(owner, "GET", &format!("/api/f/{name}/status"), None).map(|r| r.body).unwrap_or(Value::Null);
    let before = status();
    s.ok("before its first shot, a fragment's status has no page report", before["name"] == name.as_str() && before.get("page").is_none(), &before);
    let view = c["viewToken"].as_str().unwrap_or("-").to_string();
    // its shot is a link holder's: what it reports must hold no token,
    // whatever the page logs (`location.href`) or throws from (its URL)
    let throws: &[u8] = b"<!doctype html><meta http-equiv=\"Content-Security-Policy\" content=\"img-src 'none'\"><title>throws</title>\
        <script src=\"missing.js\"></script><img src=\"blocked.png\">\
        <script>console.error(\"e2e console\", 7, location.href, document.cookie); throw new Error(\"e2e boom at \" + location.href)</script>";
    s.commit(&c, &[("site/index.html", Some(throws))]);
    let live = s.deploy(&c);
    let mut page = Value::Null;
    s.eventually(CARD_WAIT, || {
        page = status()["page"].clone();
        page["live"] == live.as_str()
    });
    let errors = page["errors"].as_array().cloned().unwrap_or_default();
    let has = |kind: &str, f: &dyn Fn(&Value) -> bool| errors.iter().any(|e| e["kind"] == kind && f(e));
    let text = |e: &Value, s: &str| e["text"].as_str().is_some_and(|t| t.contains(s));
    // the missing script's 404 is JSON, which nosniff (on every answer of
    // the origin) refuses to run: a load that fails says so twice
    s.ok(
        "a page that throws as it loads shows on status, tagged with its live: the exception, the console error, the failed load and its refusal as a script, the CSP violation",
        has("exception", &|e| text(e, "Error: e2e boom"))
            && has("console", &|e| text(e, "e2e console 7"))
            && has("network", &|e| e["source"].as_str().is_some_and(|u| u.ends_with("/missing.js")))
            && has("security", &|e| text(e, "strict MIME type checking"))
            && has("security", &|e| text(e, "img-src"))
            && errors.len() == 5
            && page["dropped"] == 0
            && page["at"].as_i64().is_some_and(|at| at > 0),
        &page,
    );
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=200"), None)?;
    let said: Vec<Value> = r.body["events"].as_array().into_iter().flatten().filter(|e| e["kind"] == "page.errors").cloned().collect();
    s.ok(
        "and as one page.errors event, saying the first",
        said.len() == 1 && said[0]["data"] == page && said[0]["summary"].as_str().is_some_and(|t| t.contains("5 errors") && t.contains(&live[..12])),
        json!({ "said": said, "page": page }),
    );
    s.ok(
        "a link holder's shot reports no token: it set the cookie and opened the page at a URL without it",
        has("console", &|e| text(e, "e2e console 7 http"))
            && !page.to_string().contains(&view)
            && !json!(said).to_string().contains(&view)
            && !page.to_string().contains("view="),
        json!({ "page": page, "said": said }),
    );
    s.commit(&c, &[("site/index.html", Some(clean))]);
    let fixed = s.deploy(&c);
    s.eventually(CARD_WAIT, || {
        page = status()["page"].clone();
        page["live"] == fixed.as_str()
    });
    let said = event_kinds(api, owner, &name).iter().filter(|k| *k == "page.errors").count();
    s.ok(
        "a later deploy of a clean page clears it, and says nothing",
        page["errors"] == json!([]) && page["dropped"] == 0 && said == 1,
        json!({ "page": page, "said": said }),
    );
    Ok(())
}

pub fn watch(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("watch", &[]) {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "watch")?;
    let c = s.create(api, &owner, &name)?;
    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    s.commit(&c, &[("a.md", Some(b"1"))]);
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}/__watch", api.base), keys: Some(&owner), ..Call::default() })?;
    s.ok("__watch without an upgrade is 400", r.status == 400, &r);
    let refused = api::watch(api, &name, "", None).is_err();
    s.ok("an anonymous feed on a link fragment is refused", refused, "");
    let mut feed = api::watch(api, &name, &format!("?view={view}"), None)?;
    let hello: serde_json::Value = serde_json::from_str(feed.read()?.to_text()?)?;
    s.ok("the link opens the feed; hello names main's pin", hello["type"] == "hello" && hello["sha"].as_str().is_some_and(|s| s.len() == 40), &hello);
    // the router sets the origin's headers on every answer, a socket's
    // upgrade too: rebuilt with them, it must still upgrade
    let (mut answered, answer) = watch_answered(api, &name, &format!("?view={view}"))?;
    let hello: serde_json::Value = serde_json::from_str(answered.read()?.to_text()?)?;
    let nosniff = answer.headers().get("x-content-type-options").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    s.ok(
        "the feed's upgrade, carrying the origin's headers (nosniff), still answers 101 and delivers its hello",
        answer.status().as_u16() == 101 && nosniff == "nosniff" && hello["type"] == "hello",
        json!({ "status": answer.status().as_u16(), "nosniff": nosniff, "hello": hello }),
    );
    // done with: the feed above is the one the frames below are read from
    let _ = answered.close(None);
    drop(answered);
    s.commit(&c, &[("b.md", Some(b"2"))]);
    let changed: serde_json::Value = serde_json::from_str(feed.read()?.to_text()?)?;
    s.ok("a push arrives as a changed frame with its paths", changed["type"] == "changed" && changed["paths"] == json!(["b.md"]), &changed);
    api.signed(&owner, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({ "scopes": ["view"] })))?;
    let closed = matches!(feed.read(), Ok(tungstenite::Message::Close(Some(f))) if u16::from(f.code) == 4003);
    s.ok("rotating the link closes link holders' feeds", closed, "");
    let mut signed = api::watch(api, &name, "", Some(&owner))?;
    let hello = signed.read()?;
    s.ok("a signed member opens the feed without the link", hello.to_text().unwrap_or("").contains("hello"), &hello);
    Ok(())
}
