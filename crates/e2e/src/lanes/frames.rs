//! Frames (docs/api.md, Frame sessions):
//! the platform's own page, the shell, frames fragments signed in, as its
//! tabs do, through its mint (`/auth/frame`), and nothing else mints. The
//! lane loads the shell at `/` and adds the frames with a script of its
//! own over DevTools, as the shell's code does (so each frame is one the
//! lane chose, whatever the shell opens itself).
//!
//! Over HTTP, as a browser sends it: the mint answers only a frame of the
//! platform's own page; its token works once, for its fragment only, and
//! only in a frame; the frame's cookie opens no top-level page; a public
//! fragment answers a frame with no session as it answers a stranger. In
//! the browser: the owner's members-only fragment frames signed in as them
//! (its page's `me()`, and whom its operation call acts as), its cookie
//! partitioned under the platform's page; a fragment shared with the
//! person frames signed in as them; a stranger's members-only fragment
//! does not (the frame says a tab must ask, and the page around it hears
//! why); a fragment's page framed there cannot mint for another; a public
//! fragment frames for someone signed out.
//!
//! It runs on the node as the lanes start it (the platform on 127.0.0.1,
//! cross-site from the fragments), in a Chrome of its own that blocks
//! third-party cookies, as Safari does, so a frame signs in only through
//! its partitioned cookie, and that keeps frames in the page's process,
//! so the lane can read them.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use super::isolation::{as_browser, cookie_of, frame, frame_says, label, made};
use super::templates::person;
use crate::api::{Api, Call, Reply};
use crate::browser::{Browser, Page};
use crate::Suite;

/// A page that shows who it is to its fragment (`me()`) and whom an
/// operation it calls acts as, once each answers.
const WHO_PAGE: &str = r#"<p>inside TEXT</p><p id="me">me?</p><p id="op">op?</p>
<script type="module">
import * as fragment from "./__fragment.js";
const show = (id, text) => { document.getElementById(id).textContent = text; };
fragment.me().then((m) => show("me", "me:" + m.principal));
fragment.call("whoami", {}).then((r) => show("op", "op:" + r.principal), (e) => show("op", "op refused " + e.status));
</script>"#;
const WHO_APP: &str = r#"import { DurableObject } from "cloudflare:workers";
export class App extends DurableObject {
  whoami(input, call) {
    return { principal: call.principal };
  }
}
"#;
const WHO_JSON: &str = r#"{ "operations": { "whoami": { "kind": "mutation", "role": "viewer", "input": { "type": "object", "additionalProperties": false } } } }"#;

/// The shell at `/settings`, once it has asked who is signed in and shows
/// them their settings (which a person with no chat yet sees too: `/`
/// would make their first agent).
pub(super) const SIGNED_IN: &str = "!document.getElementById('layout').hidden && !document.getElementById('settings-page').hidden";
/// The shell signed out: it asks them to sign in.
const SIGNED_OUT: &str = "!!document.querySelector('#first-run-card a[href^=\"/auth/login\"]')";

/// What a page around frames hears from them: every message, with its origin.
const LISTEN: &str = "(() => { window.heard = []; addEventListener('message', (e) => heard.push({ origin: e.origin, data: e.data })); return true; })()";

fn who(text: &str) -> Value {
    json!([
        { "path": "site/index.html", "text": WHO_PAGE.replace("TEXT", text) },
        { "path": "app.mjs", "text": WHO_APP },
        { "path": "fragment.json", "text": WHO_JSON },
    ])
}

/// The Set-Cookie line for `name`, whole.
fn cookie_line(r: &Reply, name: &str) -> String {
    r.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.starts_with(&format!("{name}="))).unwrap_or("").to_string()
}

/// A `?token=` from a redirect to `__signin`.
fn token_of(location: &str) -> String {
    location.split("token=").nth(1).unwrap_or("").split('&').next().unwrap_or("").to_string()
}

pub fn frames(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("frames", &[crate::Need::Chrome, crate::Need::TwoSites]) {
        return Ok(());
    }
    let (owner, owner_session) = person(api)?;
    let (guest, guest_session) = person(api)?;
    let (stranger, _) = person(api)?;
    let (owner_id, guest_id) = (api.identity(&owner)?, api.identity(&guest)?);
    let own = made(api, &owner, &s.name("fown"), "blank", who("own"), "members")?;
    let shared = made(api, &owner, &s.name("fshared"), "blank", who("shared"), "members")?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{shared}/members/{guest_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "sharing {shared}: {r}");
    let theirs = made(api, &stranger, &s.name("ftheirs"), "blank", who("theirs"), "members")?;
    let open = made(api, &stranger, &s.name("fopen"), "blank", who("open"), "public")?;
    let names = Names { own: &own, shared: &shared, theirs: &theirs, open: &open };
    over_http(s, api, &owner_session, &names)?;
    in_chrome(s, api, &names, (&owner_id, &owner_session), (&guest_id, &guest_session))
}

struct Names<'a> {
    own: &'a str,
    shared: &'a str,
    theirs: &'a str,
    open: &'a str,
}

fn over_http(s: &mut Suite, api: &Api, owner_session: &str, n: &Names<'_>) -> Result<()> {
    let platform = api.base.clone();
    let session = format!("fragment_session={owner_session}");
    let mint_url = |name: &str| format!("{platform}/auth/frame?name={name}&return=/x");
    let mint = |name: &str, dest: &str, site: &str| as_browser(api, "GET", mint_url(name), dest, site, &session);

    // ---- only a frame of the platform's own page mints
    let mut refused = vec![];
    for (dest, site) in [("document", "same-origin"), ("document", "none"), ("empty", "same-origin"), ("object", "same-origin"), ("iframe", "same-site"), ("iframe", "cross-site"), ("iframe", "none")] {
        let r = mint(n.own, dest, site)?;
        if r.status != 403 || r.header("location").contains("__signin") {
            refused.push(format!("{dest} from {site}: {r}"));
        }
    }
    let bare = api.call(Call { method: "GET", url: mint_url(n.own), cookie: Some(session.clone()), ..Call::default() })?;
    if bare.status != 403 {
        refused.push(format!("no Fetch Metadata: {bare}"));
    }
    s.ok(
        "the mint answers only a frame of the platform's own page: a top-level visit, a fetch, an object, a frame of another site's page or of a fragment's (one site with the rest), and a request with no Fetch Metadata are refused (403), signed in or not",
        refused.is_empty(),
        format!("{refused:?}"),
    );

    // ---- its token: once, for its fragment, in a frame
    let r = mint(n.own, "iframe", "same-origin")?;
    let to = r.header("location");
    s.ok(
        "a frame of the platform's own page, signed in, is sent to its fragment's __signin with a token, cached nowhere, its referrer dropped",
        r.status == 302 && to.starts_with(&api.site_url(n.own, "__signin?token=")) && r.header("cache-control") == "no-store" && r.header("referrer-policy") == "no-referrer",
        format!("{r} → {to}"),
    );
    let top = as_browser(api, "GET", to.clone(), "document", "cross-site", "")?;
    let after = as_browser(api, "GET", to, "iframe", "cross-site", "")?;
    s.ok(
        "a top-level navigation with it is refused (401) and spends it: a frame cannot redeem it after",
        top.status == 401 && top.cookies().is_empty() && after.status == 401 && after.cookies().is_empty(),
        format!("{top} / {after}"),
    );
    let to = mint(n.own, "iframe", "same-origin")?.header("location");
    let elsewhere = as_browser(api, "GET", api.site_url(n.shared, &format!("__signin?token={}", token_of(&to))), "iframe", "cross-site", "")?;
    let redeemed = as_browser(api, "GET", to.clone(), "iframe", "cross-site", "")?;
    let line = cookie_line(&redeemed, "fragment_frame");
    s.ok(
        "another fragment refuses it (401) and leaves it unspent; its own fragment's frame redeems it into a partitioned frame cookie, then checks the browser kept it",
        elsewhere.status == 401
            && redeemed.status == 302
            && redeemed.header("location").starts_with(&api.site_url(n.own, "__signin?check=frame&return="))
            && ["HttpOnly", "Secure", "SameSite=None", "Partitioned"].iter().all(|a| line.contains(a))
            && cookie_line(&redeemed, "fragment_site").is_empty(),
        format!("{elsewhere} / {redeemed} {line:?}"),
    );
    let again = as_browser(api, "GET", to, "iframe", "cross-site", "")?;
    s.ok("and it works once (401 again)", again.status == 401 && again.cookies().is_empty(), &again);

    // ---- the frame's cookie
    let value = line.split(';').next().unwrap_or("").to_string();
    let framed = as_browser(api, "GET", api.site_url(n.own, ""), "iframe", "cross-site", &value)?;
    let top = as_browser(api, "GET", api.site_url(n.own, ""), "document", "cross-site", &value)?;
    let fetched = as_browser(api, "GET", api.site_url(n.own, ""), "empty", "same-site", &value)?;
    s.ok(
        "its cookie signs the fragment's frames in, shown in the platform's page alone (frame-ancestors names its exact origin); it opens no top-level page, nor counts on another page's fetch (401)",
        framed.status == 200
            && framed.text.contains("inside own")
            && framed.header("content-security-policy") == format!("frame-ancestors {platform}")
            && top.status == 401
            && fetched.status == 401,
        format!("{framed} {:?} / {top} / {fetched}", framed.header("content-security-policy")),
    );

    // ---- signed out, or not known: a note in the frame, nothing minted
    let consent = mint(n.theirs, "iframe", "same-origin")?;
    let out = as_browser(api, "GET", mint_url(n.open), "iframe", "same-origin", "")?;
    let note = |r: &Reply, why: &str| {
        r.status == 200
            && r.header("content-security-policy").ends_with("frame-ancestors 'self'")
            && r.header("x-frame-options") == "SAMEORIGIN"
            && r.text.contains(&format!("why: \"{why}\""))
            && r.text.contains(&format!("{platform:?})"))
            && !r.text.contains("__signin?token=")
    };
    s.ok(
        "a stranger's members-only fragment mints nothing for the owner's frame, nor does any fragment for someone signed out: a note only the platform's page may frame, which tells that page why, and no other",
        note(&consent, "consent") && note(&out, "signed-out"),
        format!("{consent} / {out}"),
    );
    let public = as_browser(api, "GET", api.site_url(n.open, ""), "iframe", "cross-site", "")?;
    s.ok(
        "a public fragment answers a frame with no session as it answers a stranger, shown in its own pages and the platform's",
        public.status == 200 && public.text.contains("inside open") && public.header("content-security-policy") == format!("frame-ancestors 'self' {platform}"),
        format!("{public} {:?}", public.header("content-security-policy")),
    );
    Ok(())
}

/// A Chrome of a lane's own that blocks third-party cookies (Chrome's own
/// switch and setting), as Safari blocks them, and keeps frames in the
/// page's process, so a lane reads them where they are (`None`: no Chrome
/// is installed).
pub(super) fn safari_like(s: &Suite) -> Result<Option<Browser>> {
    let args = ["--test-third-party-cookie-phaseout", "--disable-site-isolation-trials"];
    let blocking = json!({ "profile": { "cookie_controls_mode": 1, "block_third_party_cookies": true } });
    Browser::launch_with(&s.scratch, &args, Some(&blocking))
}

/// The shell's page at `/settings`, loaded (`ready`: `SIGNED_IN` or `SIGNED_OUT`),
/// listening to its frames.
pub(super) fn platform_page(chrome: &mut Browser, api: &Api, ready: &str) -> Result<Page> {
    let page = chrome.open(&format!("{}/settings", api.base))?;
    let loaded = chrome.until(&page, &format!("document.readyState === 'complete' && ({ready})"), Duration::from_secs(20));
    anyhow::ensure!(loaded, "the shell did not open ({ready}): {}", chrome.eval(&page, "document.body.innerText.slice(0, 300)")?);
    chrome.eval(&page, LISTEN)?;
    Ok(page)
}

/// A computer's ports in a frame of the platform's page (decisions 11 and
/// 41: the shell's tab onto its screen), from the computers section, which
/// has one awake: every answer on its origin may be framed by the
/// platform's page alone; a ticket redeemed in a frame signs that frame in
/// with a partitioned cookie of its own, which opens no top-level page;
/// the top-level cookie counts on no other page's frame, fetch, or socket
/// (every fragment's page is one site with the computer's origin); and in
/// a Chrome that blocks third-party cookies, the owner's platform page
/// shows the screen through a ticket while a fragment's page shows nothing.
/// `site` is the top-level cookie the section's own ticket set.
pub(super) fn computer_ports(s: &mut Suite, api: &Api, owner: &fragment_nip98::Keys, id: &str, origin: &str, site: &str) -> Result<()> {
    let wait = Duration::from_secs(20);
    let platform = api.base.clone();
    let ancestors = format!("frame-ancestors {platform}");
    let at = |path: &str| format!("{origin}{path}");
    let ticket = || -> Result<String> {
        let r = api.signed(owner, "POST", &format!("/api/computers/{id}/ports/6080/ticket"), Some(&json!({})))?;
        anyhow::ensure!(r.status == 200, "a ticket: {r}");
        Ok(r.body["url"].as_str().unwrap_or("").to_string())
    };
    let plain = |url: String, cookie: Option<&str>| api.call(Call { method: "GET", url, cookie: cookie.map(str::to_string), ..Call::default() });
    let answers = [plain(at("/p/6080/"), Some(site))?, plain(at("/p/6080/"), None)?, plain(at("/elsewhere"), None)?, plain(ticket()?, None)?];
    s.ok(
        "every answer on its origin (its screen, a refusal, a ticket's redirect) may be framed by the platform's page alone",
        answers.iter().all(|r| r.header("content-security-policy") == ancestors) && answers[0].status == 200 && answers[3].status == 303,
        answers.iter().map(|r| format!("{} {:?}", r.status, r.header("content-security-policy"))).collect::<Vec<_>>().join(" / "),
    );
    let framed = as_browser(api, "GET", ticket()?, "iframe", "cross-site", "")?;
    let line = cookie_line(&framed, "fragment_computer_frame");
    s.ok(
        "a ticket redeemed in a frame (the platform's tab) signs that frame in with a partitioned cookie of its own, never the top-level one",
        framed.status == 303
            && framed.header("location") == "/p/6080/"
            && ["HttpOnly", "Secure", "SameSite=None", "Partitioned"].iter().all(|a| line.contains(a))
            && cookie_line(&framed, "fragment_computer").is_empty(),
        format!("{framed} {line:?}"),
    );
    let frame_cookie = line.split(';').next().unwrap_or("").to_string();
    let screen = |dest: &str, from: &str, cookie: &str| as_browser(api, "GET", at("/p/6080/"), dest, from, cookie);
    let (in_frame, at_top, bare) = (screen("iframe", "cross-site", &frame_cookie)?, screen("document", "cross-site", &frame_cookie)?, screen("iframe", "cross-site", "")?);
    s.ok(
        "its frame cookie opens the screen in a frame, which only the platform's page may show; it opens no top-level page, and a frame without it is no one's (401)",
        in_frame.status == 200 && in_frame.header("content-security-policy") == ancestors && at_top.status == 401 && bare.status == 401,
        format!("{in_frame} / {at_top} / {bare}"),
    );
    let fragments_page = api.site_origin(&format!("page.{}", api.username(owner)?));
    let (framed, fetched, own) = (
        screen("iframe", "same-site", site)?,
        as_browser(api, "GET", at("/p/6080/version.txt"), "empty", "same-site", site)?,
        as_browser(api, "GET", at("/p/6080/version.txt"), "empty", "same-origin", site)?,
    );
    let socket = match crate::api::Socket::connect(api, &at("/p/6080/websockify"), None, Some(site), Some(&fragments_page)) {
        Ok(_) => "opened".to_string(),
        Err(e) => format!("{e:#}"),
    };
    s.ok(
        "its top-level cookie counts only on its own page's requests and a visit of its own: a fragment's page, one site with it, framing or fetching the screen with it is a stranger (401), and its socket is refused (403)",
        framed.status == 401 && fetched.status == 401 && own.status == 200 && socket.contains("403"),
        format!("{framed} / {fetched} / {own} / {socket}"),
    );

    // ---- in a browser, as the owner
    let Some(mut chrome) = safari_like(s)? else {
        s.ok("Chrome is installed for the computer's frames (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let session = api.sign_in(&Api::email_of(owner))?;
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", &session)?;
    let shell = platform_page(&mut chrome, api, SIGNED_IN)?;
    frame(&mut chrome, &shell, &ticket()?)?;
    let computer = origin.split("//").nth(1).unwrap_or("").to_string();
    let shown = frame_says(s, &mut chrome, &shell, &computer, "This computer has no screen", wait);
    s.ok("the owner's platform page shows the computer's screen in a frame, through a ticket, with third-party cookies blocked", shown, chrome.eval(&shell, "document.body.innerText.slice(0, 200)").unwrap_or_default());
    let label = s.name("fscreen");
    let page = made(api, owner, &label, "blank", json!([{ "path": "site/index.html", "text": "<p>a fragment's page</p>" }]), "public")?;
    let host_page = chrome.open(&api.site_url(&page, ""))?;
    anyhow::ensure!(chrome.until(&host_page, "(document.body?.innerText ?? '').includes(\"a fragment's page\")", wait), "the fragment's page did not open");
    frame(&mut chrome, &host_page, &ticket()?)?;
    let seen = s.eventually(Duration::from_secs(5), || chrome.eval_in_frame(&host_page, &computer, "(document.body?.innerText ?? '').includes('no screen')").ok() == Some(json!(true)));
    s.ok("a fragment's page that frames the screen, even through a ticket, shows nothing", !seen, "");
    Ok(())
}

fn in_chrome(s: &mut Suite, api: &Api, n: &Names<'_>, (owner_id, owner_session): (&str, &str), (guest_id, guest_session): (&str, &str)) -> Result<()> {
    let wait = Duration::from_secs(20);
    let platform = api.base.clone();
    let Some(mut chrome) = safari_like(s)? else {
        s.ok("Chrome is installed for the frames lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let mint = |name: &str| format!("{platform}/auth/frame?name={name}&return=/");
    let heard = |chrome: &mut Browser, page: &Page, name: &str, why: &str| {
        let expr = format!("heard.some((m) => m.origin === {platform:?} && m.data.fragment === 'signin-blocked' && m.data.name === {name:?} && m.data.why === {why:?})");
        chrome.until(page, &expr, wait)
    };
    let host = |name: &str| format!("{}--", label(name));

    // ---- the owner's page frames their own members-only fragment
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", owner_session)?;
    let shell = platform_page(&mut chrome, api, SIGNED_IN)?;
    frame(&mut chrome, &shell, &mint(n.own))?;
    let me = frame_says(s, &mut chrome, &shell, &host(n.own), &format!("me:{owner_id}"), wait);
    let op = me && frame_says(s, &mut chrome, &shell, &host(n.own), &format!("op:{owner_id}"), wait);
    let text = chrome.eval_in_frame(&shell, &host(n.own), "document.body.innerText").unwrap_or_default();
    s.ok("the platform's page frames the owner's members-only fragment signed in through its mint: its page's me() is the owner, and so is whom its operation call acts as", me && op, &text);
    let top_site = reqwest::Url::parse(&platform).ok().and_then(|u| u.host_str().map(|h| format!("http://{h}"))).unwrap_or_default();
    let partitioned = |chrome: &mut Browser, name: &str| {
        let own_host = api.site_url(name, "").split("//").nth(1).and_then(|h| h.split(':').next()).unwrap_or("").to_string();
        chrome.cookies().ok().into_iter().flatten().find(|c| c["name"] == "fragment_frame" && c["domain"] == own_host.as_str())
    };
    let kept = partitioned(&mut chrome, n.own);
    let under_platform = kept.as_ref().is_some_and(|c| c["partitionKey"]["topLevelSite"] == top_site.as_str());
    s.ok(
        "its session is the frame's own cookie, partitioned under the platform's page; the fragment's top-level cookie is never set",
        under_platform && cookie_of(&mut chrome, api, n.own, "fragment_site").is_none(),
        json!({ "frame": kept }),
    );
    let access = chrome.eval_in_frame(&shell, &host(n.own), "document.hasStorageAccess()").unwrap_or_default();
    s.ok("(this Chrome blocks third-party cookies, as Safari does: the frame has no storage access of its own)", access == json!(false), &access);

    // ---- a stranger's members-only fragment: a tab must ask first
    frame(&mut chrome, &shell, &mint(n.theirs))?;
    let told = frame_says(s, &mut chrome, &shell, &format!("auth/frame?name={}", n.theirs), "in a tab", wait);
    let why = heard(&mut chrome, &shell, n.theirs, "consent");
    s.ok(
        "a stranger's members-only fragment does not sign in: the frame says a tab must ask first, and the page around it hears why, from the platform's origin",
        told && why && partitioned(&mut chrome, n.theirs).is_none(),
        chrome.eval(&shell, "JSON.stringify(heard)").unwrap_or_default(),
    );

    // ---- the fragment's page, framed there, frames the mint for another
    let nested = format!("(() => {{ const f = document.createElement('iframe'); f.src = {:?}; document.body.append(f); return true; }})()", mint(n.shared));
    chrome.eval_in_frame(&shell, &host(n.own), &nested)?;
    std::thread::sleep(Duration::from_secs(2));
    let any_partition = chrome.cookies()?.into_iter().find(|c| c["name"] == "fragment_frame" && c["domain"].as_str().is_some_and(|d| d.starts_with(&host(n.shared))));
    s.ok(
        "a fragment's page framed there that frames the mint for another of the owner's fragments signs nothing in (its frame is another site's page's, which the mint refuses: above)",
        any_partition.is_none(),
        json!(any_partition),
    );

    // ---- a fragment shared with the guest, in the guest's page
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", guest_session)?;
    let theirs_page = platform_page(&mut chrome, api, SIGNED_IN)?;
    frame(&mut chrome, &theirs_page, &mint(n.shared))?;
    let me = frame_says(s, &mut chrome, &theirs_page, &host(n.shared), &format!("me:{guest_id}"), wait);
    let op = me && frame_says(s, &mut chrome, &theirs_page, &host(n.shared), &format!("op:{guest_id}"), wait);
    s.ok(
        "a fragment shared with the person frames signed in as them, asking nothing",
        me && op,
        chrome.eval_in_frame(&theirs_page, &host(n.shared), "document.body.innerText").unwrap_or_default(),
    );

    // ---- signed out: a public fragment frames as a stranger sees it
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", "signed-out")?;
    let home = platform_page(&mut chrome, api, SIGNED_OUT)?;
    frame(&mut chrome, &home, &api.site_url(n.open, ""))?;
    let shown = frame_says(s, &mut chrome, &home, &host(n.open), "inside open", wait) && frame_says(s, &mut chrome, &home, &host(n.open), "me:anon:", wait);
    s.ok(
        "a public fragment frames for someone signed out, as a stranger sees it, with no mint",
        shown,
        chrome.eval_in_frame(&home, &host(n.open), "document.body.innerText").unwrap_or_default(),
    );
    frame(&mut chrome, &home, &mint(n.open))?;
    let told = frame_says(s, &mut chrome, &home, &format!("auth/frame?name={}", n.open), "signed out", wait);
    let why = heard(&mut chrome, &home, n.open, "signed-out");
    s.ok("signed out, the mint's frame asks them to sign in, and the page around it hears why", told && why, chrome.eval(&home, "JSON.stringify(heard)").unwrap_or_default());
    chrome.screenshot(&home, &s.scratch.join("frames-signed-out.png"))?;
    Ok(())
}
