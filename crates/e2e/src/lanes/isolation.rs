//! Isolation between fragments, and the desktop's frames
//! (docs/fragment-boats.md, slice 1). Which of a browser's cookies count on
//! a fragment's origin follows the Fetch Metadata the browser sends, so
//! another fragment's page (one site with it) reaches it only as a
//! stranger would; a frame signs in only through the page that frames it
//! (`__frame`, a capability its owner allows); signing in and out cannot
//! be set off from another page; and a fragment that is not yours, nor
//! shared with you, asks before it learns who you are.
//!
//! `isolation` runs as fragment.club is shaped (the platform and every
//! fragment one site), in Chrome where the browser is the point: each open
//! bug is an attack page on another fragment. `frames` runs as a domain on
//! the Public Suffix List is (every fragment its own site), in a Chrome
//! that blocks third-party cookies, as Safari does.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use fragment_core::form;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::desktop::DOT_PNG;
use super::signin::{consent, signout, unframed, with_session};
use super::templates::person;
use crate::api::{url_enc, Api, Call, Reply};
use crate::browser::{Browser, Page};
use crate::Suite;

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

fn label(name: &str) -> &str {
    name.split('.').next().unwrap_or("")
}

/// A fragment of `owner`'s from `template`, `files` written and deployed
/// over it, opened to `visibility`.
fn made(api: &Api, owner: &Keys, label: &str, template: &str, files: Value, visibility: &str) -> Result<String> {
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
fn cookie_of(chrome: &mut Browser, api: &Api, name: &str, cookie: &str) -> Option<Value> {
    let host = api.site_url(name, "").split("//").nth(1)?.split(':').next()?.to_string();
    chrome.cookies().ok()?.into_iter().find(|c| c["name"] == cookie && c["domain"] == host.as_str())
}

/// Appends a frame of `src` to the page.
fn frame(chrome: &mut Browser, page: &Page, src: &str) -> Result<Value> {
    chrome.eval(page, &format!("(() => {{ const f = document.createElement('iframe'); f.src = {src:?}; document.body.append(f); return true; }})()"))
}

/// The text of the page's frame at `url_part`, once it holds `text`.
fn frame_says(s: &Suite, chrome: &mut Browser, page: &Page, url_part: &str, text: &str, wait: Duration) -> bool {
    let expr = format!("(document.body?.innerText ?? '').includes({text:?})");
    s.eventually(wait, || chrome.eval_in_frame(page, url_part, &expr).ok() == Some(json!(true)))
}

/// A request as a browser sends it for `dest` (`document`, `iframe`,
/// `image`, `empty`) from a page on `site` (`same-origin`, `same-site`,
/// `cross-site`), with `cookie`.
fn as_browser(api: &Api, method: &str, url: String, dest: &str, site: &str, cookie: &str) -> Result<Reply> {
    let mode = if matches!(dest, "document" | "iframe") { "navigate" } else { "no-cors" };
    let extra = vec![("sec-fetch-dest", dest.to_string()), ("sec-fetch-site", site.to_string()), ("sec-fetch-mode", mode.to_string())];
    api.call(Call { method, url, cookie: Some(cookie.to_string()), extra, ..Call::default() })
}

/// On a node shaped as fragment.club is, then back as it was.
pub fn isolation(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("isolation") {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let result = attacks(s, &api);
    drop(api);
    s.stop()?;
    s.start(false, true)?;
    result
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
    let todo = made(api, &owner, &s.name("itodo"), "todo", json!([]), "link")?;
    let desk = made(api, &owner, &s.name("idesk"), "desktop", json!([]), "link")?;
    let evil = made(api, &mallory, &s.name("ievil"), "blank", json!([]), "public")?;
    let r = api.signed(&member, "GET", "/api/identities/me", None)?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{y}/members/{}", r.body["id"].as_str().unwrap_or("")), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a member to {y}: {r}");
    let now_ms = || SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);

    // ---- the grant: the owner allows the desktop's frames in its share sheet
    let sheet = |name: &str| with_session(api, "GET", &format!("/share/{name}"), &session);
    let (desk_sheet, todo_sheet) = (sheet(&desk)?, sheet(&todo)?);
    s.ok(
        "the owner's share sheet offers the frame grant for a fragment that asks for it (the desktop), and not for one that does not",
        desk_sheet.text.contains("Your fragments inside it") && desk_sheet.text.contains(">Allow<") && !todo_sheet.text.contains("Your fragments inside it"),
        &desk_sheet,
    );
    let grant = form::issue(&session, &format!("share:{desk}"), now_ms() - form::DELAY_MS - 50);
    let body = format!("form={}&action=frame&granted=yes", url_enc(&grant));
    let post = |origin: String| {
        let cookie = Some(format!("fragment_session={session}"));
        let url = format!("{}/share/{desk}", api.base);
        api.call(Call { method: "POST", url, body: Some(body.clone().into_bytes()), content_type: Some("application/x-www-form-urlencoded"), cookie, extra: vec![("origin", origin)], ..Call::default() })
    };
    let refused = post(api.site_origin(&desk))?;
    let status = |name: &str| api.status(&owner, name).map(|r| r.body["frame"].clone());
    s.ok("the desktop's own page cannot grant it (403)", refused.status == 403 && status(&desk)? == json!(false), &refused);
    let r = post(api.base.clone())?;
    s.ok("the owner allows it from the sheet", r.status == 303 && status(&desk)? == json!(true), &r);

    // ---- the owner, signed in on x's own page and in the desktop's frame of it
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the isolation lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let inside = "(document.body?.innerText ?? '').includes('inside x')";
    let top = chrome.open(&api.site_url(&x, "__signin?return=/"))?;
    s.ok("the owner reads x (members only) signed in on its own page", chrome.until(&top, inside, wait), "");
    let d = chrome.open(&api.site_url(&desk, "__signin?return=/"))?;
    chrome.viewport(&d, 1440, 900, false)?;
    let row = format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?})", format!("app:{x}"));
    let listed = chrome.until(&d, &format!("!!{row}"), wait);
    chrome.eval(&d, &format!("{row}?.click(); true"))?;
    let x_host = format!("{}--", label(&x));
    let panel = listed && s.eventually(wait, || chrome.eval_in_frame(&d, &x_host, inside).ok() == Some(json!(true)));
    let framed = cookie_of(&mut chrome, api, &x, "fragment_frame");
    s.ok(
        "and in the desktop's pane of it, signed in through the desktop's __frame, in a partitioned cookie (SameSite=None, Secure)",
        panel && framed.as_ref().is_some_and(|c| c["sameSite"] == "None" && c["secure"] == true && c["partitionKey"].is_object()),
        json!(framed),
    );
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
    s.ok("a frame of it on another fragment's page (an iframe, an object) shows nothing: its answer names only the desktop that framed it", !shown, "");
    let frame_x = framed.as_ref().and_then(|c| c["value"].as_str()).unwrap_or_default();
    let (with, without) = (
        as_browser(api, "GET", api.site_url(&x, ""), "iframe", "same-site", &format!("fragment_frame={frame_x}"))?,
        as_browser(api, "GET", api.site_url(&x, ""), "iframe", "same-site", &format!("fragment_site={site_x}"))?,
    );
    s.ok(
        "(frame-ancestors: the desktop's origin with its frame session, else this origin's own pages; never cached without it)",
        with.header("content-security-policy") == format!("frame-ancestors {}", api.site_origin(&desk))
            && with.header("cache-control") == "private, no-cache"
            && without.header("content-security-policy") == "frame-ancestors 'self'"
            && without.status == 401,
        format!("{} {:?} / {} {:?}", with.status, with.header("content-security-policy"), without.status, without.header("content-security-policy")),
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

    // ---- framing without the capability
    frame(&mut chrome, &e, &api.site_url(&evil, &format!("__frame?name={x}")))?;
    let t = chrome.open(&api.site_url(&todo, "__signin?return=/"))?;
    let on_todo = format!("location.host.startsWith({:?}) && location.pathname === '/' && document.readyState === 'complete'", format!("{}--", label(&todo)));
    anyhow::ensure!(chrome.until(&t, &on_todo, wait), "the owner's todo did not open");
    frame(&mut chrome, &t, &api.site_url(&todo, &format!("__frame?name={x}")))?;
    let refused = frame_says(s, &mut chrome, &t, "__frame?name=", "does not ask for the frame capability", wait);
    let stranger = frame_says(s, &mut chrome, &e, "__frame?name=", "does not ask for the frame capability", wait);
    let shown = s.eventually(Duration::from_secs(3), || chrome.eval_in_frame(&t, &x_host, inside).ok() == Some(json!(true)));
    s.ok("a fragment whose fragment.json does not ask for frame (the owner's own todo, a stranger's page) cannot frame x signed in", refused && stranger && !shown, "");
    let desk_site = cookie_of(&mut chrome, api, &desk, "fragment_site").and_then(|c| c["value"].as_str().map(str::to_string)).unwrap_or_default();
    let at_desk = |dest: &str| as_browser(api, "GET", api.site_url(&desk, &format!("__frame?name={x}")), dest, "same-origin", &format!("fragment_site={desk_site}"));
    let (fetched, opened, frame_ok) = (at_desk("empty")?, at_desk("document")?, at_desk("iframe")?);
    s.ok(
        "__frame is only a frame of the page's own: a fetch of it, or a tab of it, is refused (403), even on the desktop",
        fetched.status == 403 && opened.status == 403 && frame_ok.status == 302,
        format!("{fetched} / {opened} / {frame_ok}"),
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

/// On a node shaped as a listed domain is (every fragment its own site),
/// in a Chrome of its own, then back as it was.
pub fn frames(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("frames") {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_listed()?;
    let result = listed(s, &api);
    drop(api);
    s.stop()?;
    s.start(false, true)?;
    result
}

fn listed(s: &mut Suite, api: &Api) -> Result<()> {
    let wait = Duration::from_secs(20);
    let (owner, session) = person(api)?;
    let desk = made(api, &owner, &s.name("fdesk"), "desktop", json!([]), "link")?;
    let files = json!([{ "path": "site/index.html", "text": "<p>inside the app</p>" }, { "path": "notes/hello.txt", "text": "hello from a file" }]);
    let app = made(api, &owner, &s.name("fapp"), "blank", files, "members")?;
    let shut = made(api, &owner, &s.name("fshut"), "blank", json!([{ "path": "site/index.html", "text": "<p>inside shut</p>" }]), "members")?;
    // Third-party cookies blocked (Chrome's own switch), as Safari blocks
    // them; and every cookie of `shut`'s blocked (a site setting), as a
    // browser that keeps a fragment signed out in frames does. Frames stay
    // in the page's process, so the lane can read them.
    let blocked = json!({ "profile": { "content_settings": { "exceptions": { "cookies": { format!("{},*", api.site_origin(&shut)): { "setting": 2 } } } } } });
    let args = ["--test-third-party-cookie-phaseout", "--disable-site-isolation-trials"];
    let Some(mut chrome) = Browser::launch_with(&s.scratch, &args, Some(&blocked))? else {
        s.ok("Chrome is installed for the frames lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = chrome.open(&api.site_url(&desk, "__signin?return=/"))?;
    chrome.viewport(&page, 1440, 900, false)?;
    let open = |chrome: &mut Browser, name: &str| {
        let row = format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?})", format!("app:{name}"));
        chrome.until(&page, &format!("!!{row}"), wait) && chrome.eval(&page, &format!("{row}.click(); true")).is_ok()
    };
    let asked = open(&mut chrome, &app) && frame_says(s, &mut chrome, &page, "__frame?name=", "allow it in its share sheet", wait);
    s.ok("until its owner allows it, a desktop's pane says so, and signs nothing in", asked && cookie_of(&mut chrome, api, &app, "fragment_frame").is_none(), "");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{desk}/grants/frame"), Some(&json!({ "granted": true })))?;
    s.ok("(the owner allows it)", r.status == 200, &r);
    chrome.reload(&page)?;
    let app_host = format!("{}--", label(&app));
    let inside = frame_says(s, &mut chrome, &page, &app_host, "inside the app", wait);
    let (framed, own) = (cookie_of(&mut chrome, api, &app, "fragment_frame"), cookie_of(&mut chrome, api, &app, "fragment_site"));
    let desk_host = api.site_origin(&desk).trim_start_matches("http://").split(':').next().unwrap_or("").to_string();
    s.ok(
        "with third-party cookies blocked, the desktop's pane of a members-only app, another site, signs in through __frame",
        inside && framed.as_ref().is_some_and(|c| c["partitionKey"]["topLevelSite"].as_str().is_some_and(|t| t.ends_with(&desk_host))) && own.is_none(),
        json!({ "frame": framed, "site": own }),
    );
    let token = framed.as_ref().and_then(|c| c["value"].as_str()).unwrap_or_default();
    let r = as_browser(api, "GET", api.site_url(&app, ""), "iframe", "cross-site", &format!("fragment_frame={token}"))?;
    let top = as_browser(api, "GET", api.site_url(&app, ""), "document", "cross-site", &format!("fragment_frame={token}"))?;
    s.ok(
        "its page answers the frame with frame-ancestors naming the desktop alone; the frame's cookie opens no top-level page (members only: 401)",
        r.status == 200 && r.header("content-security-policy") == format!("frame-ancestors {}", api.site_origin(&desk)) && top.status == 401,
        format!("{r} {:?} / {top}", r.header("content-security-policy")),
    );
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Files]').click(); true", format!("app:{app}")))?;
    let clicked = s.eventually(wait, || {
        let click = "(() => { const a = [...document.querySelectorAll('a')].find(a => a.textContent === 'notes/hello.txt'); if (!a) return false; a.click(); return true; })()";
        chrome.eval_in_frame(&page, "/__files", click).ok() == Some(json!(true))
    });
    s.ok("a file pane opens through __frame too", clicked && frame_says(s, &mut chrome, &page, "__file?path=", "hello from a file", wait), "");
    let told = open(&mut chrome, &shut) && frame_says(s, &mut chrome, &page, &format!("{}--", label(&shut)), "in a tab", wait);
    let heard = chrome.until(&page, "document.getElementById('notice').textContent.includes('signed out inside this page')", wait);
    chrome.screenshot(&page, &s.scratch.join("frames-blocked.png"))?;
    s.ok("a pane whose fragment the browser keeps signed out in frames offers it in a tab, and the desktop hears so", told && heard, "");
    Ok(())
}
