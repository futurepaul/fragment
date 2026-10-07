//! Isolation between fragments (docs/fragment-boats.md,
//! slice 1). Which of a browser's cookies count on a fragment's
//! origin follows the Fetch Metadata the browser sends, so another
//! fragment's page (one site with it) reaches it only as a stranger would;
//! a frame of a fragment signs in only through the platform's mint, for a
//! frame of the platform's own page (the frames section), so another
//! fragment's page signs no frame in, by the mint or without it;
//! signing in and out cannot be set off from another page; and a fragment
//! that is not yours, nor shared with you, asks before it learns who you
//! are.
//!
//! `isolation` runs as fragment.club is shaped (the platform cross-site
//! from the fragments, which are one site with each other), in Chrome where
//! the browser is the point: each open bug is an attack page on another
//! fragment.

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::signin::{consent, signout, unframed, with_session};
use super::templates::person;
use crate::api::{url_enc, Api, Call, Reply};
use crate::browser::{Browser, Page};
use crate::Suite;

/// A one-pixel PNG.
const DOT_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// An app whose one route records who posted to it, and a query that says.
const POSTED_APP: &str = r#"import { DurableObject } from "cloudflare:workers";
export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS posts (who TEXT NOT NULL)");
  }
  posts() {
    return { posts: this.ctx.storage.sql.exec("SELECT who FROM posts").toArray().map((r) => r.who) };
  }
  async fetch(request) {
    if (request.method !== "POST") return new Response("no such route", { status: 404 });
    this.ctx.storage.sql.exec("INSERT INTO posts (who) VALUES (?)", request.headers.get("x-fragment-principal"));
    return new Response("posted");
  }
}
"#;
const POSTED_JSON: &str = r#"{ "operations": { "posts": { "kind": "query" } } }"#;

pub(super) fn label(name: &str) -> &str {
    name.split('.').next().unwrap_or("")
}

/// A fragment of `owner`'s from `template`, `files` written and deployed
/// over it, opened to `visibility`.
pub(super) fn made(api: &Api, owner: &Keys, label: &str, template: &str, files: Value, visibility: &str) -> Result<String> {
    let name = api.qualified(owner, label)?;
    let r = api.create_with(owner, json!({ "name": name, "template": template }))?;
    anyhow::ensure!(r.status == 200, "making {name}: {r}");
    if files.as_array().is_some_and(|f| !f.is_empty()) {
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/files"), Some(&json!({ "files": files })))?;
        anyhow::ensure!(r.status == 200, "writing {name}'s files: {r}");
        let r = api.signed(owner, "POST", &format!("/api/f/{name}/deploy"), None)?;
        anyhow::ensure!(r.status == 200, "deploying {name}: {r}");
    }
    let r = api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": visibility })))?;
    anyhow::ensure!(r.status == 200, "{name}'s visibility: {r}");
    Ok(name)
}

/// The cookie `cookie` the browser holds for `name`'s host.
pub(super) fn cookie_of(chrome: &mut Browser, api: &Api, name: &str, cookie: &str) -> Option<Value> {
    let host = api.site_url(name, "").split("//").nth(1)?.split(':').next()?.to_string();
    chrome.cookies().ok()?.into_iter().find(|c| c["name"] == cookie && c["domain"] == host.as_str())
}

/// Appends a frame of `src` to the page.
pub(super) fn frame(chrome: &mut Browser, page: &Page, src: &str) -> Result<Value> {
    chrome.eval(page, &format!("(() => {{ const f = document.createElement('iframe'); f.src = {src:?}; document.body.append(f); return true; }})()"))
}

/// The text of the page's frame at `url_part`, once it holds `text`.
pub(super) fn frame_says(s: &Suite, chrome: &mut Browser, page: &Page, url_part: &str, text: &str, wait: Duration) -> bool {
    let expr = format!("(document.body?.innerText ?? '').includes({text:?})");
    s.eventually(wait, || chrome.eval_in_frame(page, url_part, &expr).ok() == Some(json!(true)))
}

/// A request as a browser sends it for `dest` (`document`, `iframe`,
/// `image`, `empty`) from a page on `site` (`same-origin`, `same-site`,
/// `cross-site`), with `cookie`.
pub(super) fn as_browser(api: &Api, method: &str, url: String, dest: &str, site: &str, cookie: &str) -> Result<Reply> {
    let mode = if matches!(dest, "document" | "iframe") { "navigate" } else { "no-cors" };
    let extra = vec![("sec-fetch-dest", dest.to_string()), ("sec-fetch-site", site.to_string()), ("sec-fetch-mode", mode.to_string())];
    api.call(Call { method, url, cookie: Some(cookie.to_string()), extra, ..Call::default() })
}

/// On a node shaped as fragment.club is, then back as it was.
pub fn isolation(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("isolation", &[crate::Need::Node, crate::Need::Chrome]) {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let result = attacks(s, &api);
    let opened = by_url(s, &api);
    let hosts = hosts(s, &api);
    let result = result.and(opened).and(hosts);
    drop(api);
    s.stop()?;
    s.start(false)?;
    result
}

/// The fragments' suffix's own name, with the platform elsewhere, is the
/// platform's, cached nowhere; any other name under it is no one's.
fn hosts(s: &mut Suite, api: &Api) -> Result<()> {
    let get = |url: String| api.call(Call { method: "GET", url, ..Call::default() });
    let r = get(format!("http://{}:{}/cli?key=x", crate::BOATS, api.port))?;
    let to = r.header("location");
    s.ok(
        "the fragments' suffix's own name answers 308 to the platform, cached nowhere",
        r.status == 308 && to == format!("{}/cli?key=x", api.base) && r.header("cache-control") == "no-store",
        format!("{r} → {to}"),
    );
    let r = get(format!("http://a.{}:{}/", crate::BOATS, api.port))?;
    s.ok("another name under it is no one's (404)", r.status == 404 && r.message().contains("no fragment here"), &r);
    Ok(())
}

fn attacks(s: &mut Suite, api: &Api) -> Result<()> {
    let wait = Duration::from_secs(20);
    let (owner, session) = person(api)?;
    let (mallory, _) = person(api)?;
    let (member, member_session) = person(api)?;
    let owner_id = api.identity(&owner)?;
    let files = json!([
        { "path": "site/index.html", "text": "<p>inside x</p>" },
        { "path": "site/secret.js", "text": "window.secretX = 'leaked';" },
        { "path": "site/pic.png", "base64": DOT_PNG },
        { "path": "app.mjs", "text": POSTED_APP },
        { "path": "fragment.json", "text": POSTED_JSON },
    ]);
    let x = made(api, &owner, &s.name("ix"), "blank", files, "members")?;
    let y = made(api, &owner, &s.name("iy"), "blank", json!([]), "members")?;
    let z = made(api, &owner, &s.name("iz"), "blank", json!([]), "members")?;
    let evil = made(api, &mallory, &s.name("ievil"), "blank", json!([]), "public")?;
    let r = api.signed(&member, "GET", "/api/identities/me", None)?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{y}/members/{}", r.body["id"].as_str().unwrap_or("")), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a member to {y}: {r}");

    // ---- the owner, signed in on x's own page
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the isolation lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let inside = "(document.body?.innerText ?? '').includes('inside x')";
    let top = chrome.open(&api.site_url(&x, "__signin?return=/"))?;
    s.ok("the owner reads x (members only) signed in on its own page", chrome.until(&top, inside, wait), "");
    let site_x = cookie_of(&mut chrome, api, &x, "fragment_site").and_then(|c| c["value"].as_str().map(str::to_string)).unwrap_or_default();

    // ---- an attack page: a stranger's fragment, one site with x
    let e = chrome.open(&api.site_url(&evil, ""))?;
    let on_evil = format!("location.host.startsWith({:?}) && document.readyState === 'complete'", format!("{}--", label(&evil)));
    anyhow::ensure!(chrome.until(&e, &on_evil, wait), "the attack page did not open");
    let run = |chrome: &mut Browser, js: String| chrome.eval(&e, &js).unwrap_or_else(|err| json!(err.to_string()));
    let image = run(&mut chrome, format!("new Promise((done) => {{ const i = new Image(); i.onload = () => done('loaded'); i.onerror = () => done('refused'); i.src = {:?}; }})", api.site_url(&x, "pic.png")));
    s.ok("another fragment's page cannot load a members-only fragment's image with the owner's cookie", image == "refused", &image);
    let script = run(
        &mut chrome,
        format!(
            "new Promise((done) => {{ const t = document.createElement('script'); t.onload = () => done(String(window.secretX)); t.onerror = () => done('refused'); t.src = {:?}; document.head.append(t); }})",
            api.site_url(&x, "secret.js")
        ),
    );
    s.ok("nor run its script as the owner (it defines nothing there)", script != "leaked", &script);
    let to_route = api.site_url(&x, "post");
    run(
        &mut chrome,
        format!(
            "(() => {{ const sink = document.createElement('iframe'); sink.name = 'sink'; const f = document.createElement('form'); f.method = 'post'; f.action = {to_route:?}; f.target = 'sink'; document.body.append(sink, f); f.submit(); return fetch({to_route:?}, {{ method: 'POST', mode: 'no-cors', credentials: 'include', body: 'x' }}).then(() => 'sent', (e) => e.name); }})()"
        ),
    );
    std::thread::sleep(Duration::from_secs(1));
    // the fragment's own page posts once, as the owner
    chrome.eval(&top, "fetch('post', { method: 'POST', body: 'mine' }).then((r) => r.status)")?;
    let posts = api.op(&owner, &x, "posts", "p", json!({}))?;
    s.ok(
        "a form and a fetch posted to its app's route from another fragment's page are not the owner's (refused there, as to a stranger); only its own page's post is",
        posts.body["result"]["posts"] == json!([owner_id]),
        &posts,
    );
    frame(&mut chrome, &e, &api.site_url(&x, "?attack"))?;
    run(&mut chrome, format!("(() => {{ const o = document.createElement('object'); o.data = {:?}; o.type = 'text/html'; document.body.append(o); return true; }})()", api.site_url(&x, "?object")));
    let shown = s.eventually(Duration::from_secs(5), || ["?attack", "?object"].iter().any(|at| chrome.eval_in_frame(&e, at, inside).ok() == Some(json!(true))));
    s.ok("a frame of it on another fragment's page (an iframe, an object) shows nothing", !shown, "");
    let without = as_browser(api, "GET", api.site_url(&x, ""), "iframe", "same-site", &format!("fragment_site={site_x}"))?;
    s.ok(
        "(frame-ancestors: this origin's own pages and the platform's; a frame with the site cookie alone is a stranger's, 401)",
        without.header("content-security-policy") == format!("frame-ancestors 'self' {}", api.base) && without.status == 401,
        format!("{} {:?}", without.status, without.header("content-security-policy")),
    );

    // ---- sign-out and sign-in set off from another page
    run(&mut chrome, format!("new Promise((done) => {{ const i = new Image(); i.onload = i.onerror = () => done(true); i.src = {:?}; }})", api.site_url(&x, "__signout")));
    let still = api.page(&x, "", Some(&format!("fragment_site={site_x}")))?;
    s.ok("an image of its __signout on another page leaves the owner signed in there", still.status == 200 && cookie_of(&mut chrome, api, &x, "fragment_site").is_some(), &still);
    let r = as_browser(api, "GET", api.site_url(&x, "__signout"), "document", "same-site", &format!("fragment_site={site_x}"))?;
    let still = api.page(&x, "", Some(&format!("fragment_site={site_x}")))?;
    s.ok("(a GET of __signout, even a link's, only shows a button)", r.status == 200 && r.text.contains("<button>Sign out") && unframed(&r) && still.status == 200, &r);
    let r = api.call(Call { method: "POST", url: api.site_url(&x, "__signout"), cookie: Some(format!("fragment_site={site_x}")), extra: vec![("origin", api.site_origin(&evil))], ..Call::default() })?;
    let still = api.page(&x, "", Some(&format!("fragment_site={site_x}")))?;
    s.ok("and a POST to it from another fragment's page is refused (403)", r.status == 403 && still.status == 200, &r);
    run(&mut chrome, format!("new Promise((done) => {{ const i = new Image(); i.onload = i.onerror = () => done(true); i.src = {:?}; }})", api.site_url(&y, "__signin?return=/")));
    std::thread::sleep(Duration::from_millis(500));
    s.ok("an image of another fragment's __signin signs the owner in nowhere", cookie_of(&mut chrome, api, &y, "fragment_site").is_none(), json!(cookie_of(&mut chrome, api, &y, "fragment_site")));
    frame(&mut chrome, &e, &api.site_url(&z, "__signin?return=/"))?;
    let told = frame_says(s, &mut chrome, &e, &format!("{}--", label(&z)), "can't sign you in inside this page", wait);
    s.ok(
        "nor does a frame of it: the frame offers the fragment in a tab of its own instead",
        told && cookie_of(&mut chrome, api, &z, "fragment_site").is_none() && cookie_of(&mut chrome, api, &z, "fragment_frame").is_none(),
        "",
    );
    // the platform's mint, framed by this page: a frame of another site's
    // page, which it refuses (its refusal shows in no frame)
    frame(&mut chrome, &e, &format!("{}/auth/frame?name={z}&return=/", api.base))?;
    std::thread::sleep(Duration::from_secs(2));
    s.ok(
        "nor can it frame the platform's mint for one: nothing signs in there",
        cookie_of(&mut chrome, api, &z, "fragment_frame").is_none() && cookie_of(&mut chrome, api, &z, "fragment_site").is_none(),
        json!(cookie_of(&mut chrome, api, &z, "fragment_frame")),
    );

    // ---- asked once: a fragment that is not yours, nor shared with you
    let q = chrome.open(&api.site_url(&evil, "__signin?return=/"))?;
    let asked = chrome.until(&q, "location.pathname === '/auth/fragment' && document.body.innerText.includes('Continue to')", wait);
    s.ok(
        "a stranger's fragment that sends the owner to its __signin gets the platform's question, not who they are",
        asked && cookie_of(&mut chrome, api, &evil, "fragment_site").is_none(),
        chrome.eval(&q, "location.href + ' ' + document.body.innerText.slice(0, 200)").unwrap_or_default(),
    );
    let armed = chrome.until(&q, "document.querySelector('button[data-arm]')?.disabled === false", Duration::from_secs(3));
    if armed {
        chrome.click(&q, "button[data-arm]")?;
    }
    let landed = chrome.until(&q, &format!("location.host.startsWith({:?})", format!("{}--", label(&evil))), wait);
    s.ok("they say yes, once its button arms, and are signed in there", armed && landed && cookie_of(&mut chrome, api, &evil, "fragment_site").is_some(), "");
    drop(chrome);

    let fragment = |who: &str, name: &str| with_session(api, "GET", &format!("/auth/fragment?name={name}&return=/"), who);
    let (own, shared) = (fragment(&session, &x)?, fragment(&member_session, &y)?);
    s.ok(
        "silent on the person's own fragments and on those shared with them: straight to the fragment's sign-in",
        own.status == 302 && own.header("location").starts_with(&api.site_url(&x, "__signin?token=")) && shared.status == 302 && shared.header("location").starts_with(&api.site_url(&y, "__signin?token=")),
        format!("{own} / {shared}"),
    );
    let ask = fragment(&member_session, &evil)?;
    s.ok(
        "on anyone else's, a platform page asks first: unframed, its button arming after a moment, nothing minted",
        ask.status == 200 && ask.text.contains("Continue to") && ask.text.contains("data-arm disabled") && unframed(&ask) && !ask.text.contains("__signin?token="),
        &ask,
    );
    let (foreign, bare) = (
        consent(api, &member_session, &evil, &api.site_origin(&evil))?,
        api.call(Call {
            method: "POST",
            url: format!("{}/auth/fragment?name={evil}&return=/", api.base),
            cookie: Some(format!("fragment_session={member_session}")),
            extra: vec![("origin", api.base.clone())],
            ..Call::default()
        })?,
    );
    s.ok("its yes is refused from another page (403), and without the page's own form token (403)", foreign.status == 403 && bare.status == 403 && fragment(&member_session, &evil)?.status == 200, format!("{foreign} / {bare}"));
    let yes = consent(api, &member_session, &evil, &api.base)?;
    let remembered = fragment(&member_session, &evil)?;
    s.ok(
        "said yes, it signs them in, and is remembered: the next sign-in there asks nothing",
        yes.status == 303 && yes.header("location").starts_with(&api.site_url(&evil, "__signin?token=")) && remembered.status == 302,
        format!("{yes} / {remembered}"),
    );
    let redeemed = api.call(Call { method: "GET", url: remembered.header("location"), ..Call::default() })?;
    let site = redeemed.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_site=").map(str::to_string)).unwrap_or_default();
    let r = signout(api, &evil, &format!("fragment_site={site}"))?;
    let asks = fragment(&member_session, &evil)?;
    s.ok("signing out of it there forgets the yes: the next sign-in asks again", r.status == 303 && asks.status == 200 && asks.text.contains("Continue to"), format!("{r} / {asks}"));
    Ok(())
}

/// A request as a browser's page sends it: `dest` and `mode` as Fetch
/// Metadata names them, and an `Accept` that names HTML, as a navigation's
/// does (and a script may send on a fetch).
fn html_request(api: &Api, url: String, dest: &str, mode: &str, site: &str, cookie: Option<String>) -> Result<Reply> {
    let extra = vec![
        ("sec-fetch-dest", dest.to_string()),
        ("sec-fetch-mode", mode.to_string()),
        ("sec-fetch-site", site.to_string()),
        ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string()),
    ];
    api.call(Call { method: "GET", url, cookie, extra, ..Call::default() })
}

/// Opening a fragment by its own URL (ROADMAP decision 4): a browser
/// signed in to the platform lands on the fragment signed in, silently on
/// the person's own and on those shared with them, asked once on anyone
/// else's; signed out, it signs in first and comes back to the path it
/// asked for. A refusal a browser navigates to is a page in the platform's
/// look; any other request's (an API call, a fetch) stays JSON, and a
/// fetch from another page still carries no session.
fn by_url(s: &mut Suite, api: &Api) -> Result<()> {
    let wait = Duration::from_secs(20);
    let owner = Keys::generate();
    let owner_email = format!("u-{}@e2e.test", &owner.pubkey_hex()[..12]);
    let owner_session = api.sign_in(&owner_email)?;
    api.approve(&owner_session, &owner)?;
    let (guest, guest_session) = person(api)?;
    let (stranger, _) = person(api)?;
    let guest_id = api.identity(&guest)?;
    let pages = |text: &str| json!([{ "path": "site/index.html", "text": format!("<p>{text}</p>") }, { "path": "site/page.html", "text": format!("<p>{text}, deeper</p>") }]);
    let own = made(api, &owner, &s.name("uown"), "blank", pages("inside own"), "link")?;
    let own_too = made(api, &owner, &s.name("uowntoo"), "blank", pages("inside own too"), "members")?;
    let shared = made(api, &owner, &s.name("ushared"), "blank", pages("inside shared"), "members")?;
    let theirs = made(api, &stranger, &s.name("utheirs"), "blank", pages("inside theirs"), "link")?;
    let open = made(api, &stranger, &s.name("uopen"), "blank", pages("inside open"), "public")?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{shared}/members/{guest_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "sharing {shared}: {r}");
    let host_of = |name: &str| format!("{}--", label(name));
    let says = |text: &str| format!("document.contentType === 'text/html' && (document.body?.innerText ?? '').includes({text:?})");
    let shown = |chrome: &mut Browser, page: &Page| chrome.eval(page, "location.href + ' ' + document.contentType + ' ' + (document.body?.innerText ?? '').slice(0, 300)").unwrap_or_default();

    // ---- the requests, as a browser sends them
    let asked = html_request(api, api.site_url(&own_too, "page.html?x=1"), "document", "navigate", "none", None)?;
    let signin = format!("{}/auth/fragment?name={own_too}&return={}", api.base, url_enc("/page.html?x=1"));
    s.ok(
        "a browser's top-level visit to a members-only fragment, signed in there as no one, goes to the platform's sign-in for it, to come back to the page it asked for",
        asked.status == 302 && asked.header("location") == signin,
        format!("{asked} {:?}", asked.header("location")),
    );
    let r = html_request(api, api.site_url(&open, ""), "document", "navigate", "none", None)?;
    s.ok("a public fragment just serves", r.status == 200 && r.text.contains("inside open"), &r);
    let navigation = || vec![("sec-fetch-dest", "document".to_string()), ("sec-fetch-mode", "navigate".to_string()), ("sec-fetch-site", "none".to_string())];
    let bare = api.call(Call { method: "GET", url: api.site_url(&own_too, ""), extra: navigation(), ..Call::default() })?;
    let fetched = html_request(api, api.site_url(&own_too, ""), "empty", "cors", "same-origin", None)?;
    let called = api.call(Call {
        method: "POST",
        url: api.site_url(&own_too, "__op/nothing"),
        body: Some(br#"{"id":"n","input":{}}"#.to_vec()),
        content_type: Some("application/json"),
        extra: vec![("sec-fetch-dest", "empty".into()), ("sec-fetch-mode", "cors".into()), ("sec-fetch-site", "same-origin".into()), ("accept", "text/html".into())],
        ..Call::default()
    })?;
    let api_call = api.call(Call { method: "GET", url: format!("{}/api/f/{own_too}/status", api.base), extra: vec![("accept", "text/html".into())], ..Call::default() })?;
    let json = |r: &Reply| r.header("content-type").starts_with("application/json") && r.body["error"] == "unauthenticated";
    s.ok(
        "an API call, a fetch, and an operation keep their JSON refusals, even asking for HTML; so does a navigation that asks for none (not a browser's)",
        [&bare, &fetched, &called, &api_call].iter().all(|r| r.status == 401 && json(r)),
        format!("{bare} / {fetched} / {called} / {api_call}"),
    );
    let site_owner = super::signin::site_cookie(api, &owner_session, &own_too)?;
    let from_page = |site: &str| html_request(api, api.site_url(&own_too, ""), "empty", "cors", site, Some(format!("fragment_site={site_owner}")));
    let (other, own_page) = (from_page("same-site")?, from_page("same-origin")?);
    s.ok(
        "a fetch from another fragment's page, with the owner's cookie, still counts no session (401 JSON; from the fragment's own page, 200)",
        other.status == 401 && json(&other) && own_page.status == 200,
        format!("{other} / {own_page}"),
    );
    let framed = html_request(api, api.site_url(&own_too, ""), "iframe", "navigate", "same-site", None)?;
    s.ok(
        "a frame's navigation is not sent to sign in (a frame cannot): it answers a page that says to, shown only in this origin's own pages and the platform's",
        framed.status == 401
            && framed.header("content-type").starts_with("text/html")
            && framed.text.contains("You need to sign in")
            && framed.header("content-security-policy") == format!("frame-ancestors 'self' {}", api.base),
        format!("{framed} {:?}", framed.header("content-security-policy")),
    );
    let stale = html_request(api, api.site_url(&own, "?view=0123456789abcdef"), "document", "navigate", "none", None)?;
    s.ok("a share link that no longer opens it answers a page saying the link has changed", stale.status == 401 && stale.text.contains("This link has changed"), &stale);

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the isolation lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };

    // ---- signed out: sign in (the WorkOS fake's form), then back
    let nobody = chrome.another_context()?;
    let p = chrome.open_in(&nobody, &api.site_url(&own, ""))?;
    let form = chrome.until(&p, "!!document.querySelector('input[name=login_hint]')", wait);
    if form {
        chrome.eval(&p, &format!("document.querySelector('input[name=login_hint]').value = {owner_email:?}; document.querySelector('form').submit(); true"))?;
    }
    let back = chrome.until(&p, &format!("location.host.startsWith({:?}) && location.pathname === '/' && {}", host_of(&own), says("inside own")), wait);
    s.ok("someone signed out who opens their fragment's URL signs in, and lands back on it signed in", form && back, shown(&mut chrome, &p));
    let p = chrome.open_in(&nobody, &api.site_url(&own_too, "page.html?x=1"))?;
    let landed = chrome.until(
        &p,
        &format!("location.host.startsWith({:?}) && location.pathname + location.search === '/page.html?x=1' && {}", host_of(&own_too), says("inside own too, deeper")),
        wait,
    );
    s.ok("signed in to the platform, the URL of another of their own fragments lands on the page it names, signed in, asking nothing", landed, shown(&mut chrome, &p));

    // ---- the guest, signed in to the platform
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &guest_session)?;
    let g = chrome.open(&api.site_url(&shared, ""))?;
    let in_shared = chrome.until(&g, &format!("location.host.startsWith({:?}) && {}", host_of(&shared), says("inside shared")), wait);
    s.ok("the URL of a fragment shared with them lands on it signed in, asking nothing", in_shared && cookie_of(&mut chrome, api, &shared, "fragment_site").is_some(), shown(&mut chrome, &g));
    let t = chrome.open(&api.site_url(&theirs, ""))?;
    let question = chrome.until(&t, "location.pathname === '/auth/fragment' && document.body.innerText.includes('Continue to')", wait);
    s.ok(
        "a stranger's fragment's URL asks first, on the platform, before it learns who they are",
        question && cookie_of(&mut chrome, api, &theirs, "fragment_site").is_none(),
        shown(&mut chrome, &t),
    );
    let armed = chrome.until(&t, "document.querySelector('button[data-arm]')?.disabled === false", Duration::from_secs(3));
    if armed {
        chrome.click(&t, "button[data-arm]")?;
    }
    let refused = format!("location.host.startsWith({:?}) && {}", host_of(&theirs), says("You don't have access to"));
    let told = chrome.until(&t, &refused, wait);
    s.ok(
        "said yes, on a fragment shared by link they hold no link for, a page says they have no access and whom to ask: not JSON, and no sign-in loop",
        armed && told && cookie_of(&mut chrome, api, &theirs, "fragment_site").is_some(),
        shown(&mut chrome, &t),
    );
    let again = chrome.open(&api.site_url(&theirs, ""))?;
    let once = chrome.until(&again, &refused, wait);
    s.ok("and it asked once: its URL again goes straight there", once, shown(&mut chrome, &again));

    // ---- the owner removes the guest; the guest reloads
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{shared}/members/{guest_id}"), None)?;
    anyhow::ensure!(r.status == 200, "removing the guest from {shared}: {r}");
    chrome.reload(&g)?;
    let gone = chrome.until(&g, &format!("location.host.startsWith({:?}) && {}", host_of(&shared), says("You don't have access to")), wait);
    s.ok("a guest the owner removed who reloads gets a page saying they have no access, not JSON", gone, shown(&mut chrome, &g));
    Ok(())
}
