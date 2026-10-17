//! The owner's browser (docs/explorations/owner-browser.md, a spike): a
//! computer's guest drives a browser its owner connected, over the Chrome
//! DevTools Protocol, through the Computer DO. The stub image's `browse`
//! command is the guest, as a browser tool would be: a new tab, attached,
//! read once it has loaded.
//!
//! - **Valid:** its owner mints a one-time ticket, which a socket from an
//!   extension spends on the computer's origin; that first connection
//!   makes the browser a link, given once on its socket, and the name the
//!   extension says is what its owner sees, connected. The guest's CDP
//!   reaches it, each frame named by the guest socket it came on, and the
//!   answers go back (the agent's reply reads the page's title); the
//!   guest socket's end is told to it. Asked first, as a CDP client asks a
//!   browser (`/json/version`: the check the managed skill
//!   owner-browser-finite gives an agent), the computer answers with the
//!   browser's name and the guest's socket. In a Chrome that loads the extension
//!   (`extension/`), Settings connects this browser and lists it by name
//!   (and another person signed in there is offered Connect, not told it
//!   is theirs), the agent opens a tab in it and reads it, and what it may
//!   use follows the person's choice in the extension's popup: only its
//!   own tabs, those and the tabs the person switches on, or every tab
//!   (where it closes one of the person's), and back.
//! - **Invalid:** no one else mints a ticket or forgets the browser; a
//!   socket from a page (no extension's origin), or naming no ticket, is
//!   refused, and so is a link's ticket asked from a page or with a link
//!   the computer never made; with no browser connected the guest is told
//!   so, in the platform's words, at its socket and at `/json/version`.
//!   The extension refuses a ticket for a computer it does not trust
//!   (another port than the platform's page, another zone than its
//!   manifest's), and stays connected.
//! - **Replay:** a ticket is spent once; a forgotten browser's link is
//!   refused.
//! - **Restart:** with its link a browser comes back on its own: a fake
//!   one asks for a ticket of its own and replaces the socket before it
//!   (told not to come back); the real extension, its connection dropped
//!   (a lever), comes back by itself; its worker stopped, as Chrome stops
//!   an idle one, it comes back once woken, still knowing the agent's tab;
//!   and reloaded, it does too (the Settings page open from before, its
//!   script cut off, says to load it again). Replaced by another browser,
//!   or forgotten (by its owner, from anywhere; in Settings), it does not:
//!   it keeps no link.
//! - **Replaced:** one that disconnects mid-call ends the guest's socket,
//!   which says so.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::computers::{agent_replies, phase, AGENT_JSON, CHAT_JSON};
use super::jobs::records;
use crate::api::{Api, Socket};
use crate::browser::Browser;
use crate::{Keys, Suite};

/// The fake extension's origin, as Chrome sends one on an extension's own
/// sockets (its id: 32 letters `a` to `p`).
const EXTENSION: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
/// A start of the stub, its restore, and its bridge's first follow
/// (computers.rs's `WAKE`).
const WAKE: Duration = Duration::from_secs(90);
/// The stub answers a message within seconds; a browse within its calls'.
const REPLY: Duration = Duration::from_secs(60);
/// The extension's worker, up after Chrome starts with it.
const EXTENSION_UP: Duration = Duration::from_secs(5);
/// The page the agent opens in the real extension's Chrome.
const PAGE: &str = "data:text/html,%3Ctitle%3EOpened%20by%20the%20agent%3C%2Ftitle%3E";
const PAGE_TITLE: &str = "Opened by the agent";
/// A tab the person opened themselves, and the platform's page's title
/// (cell/shell/index.html).
const PERSON_PAGE: &str = "data:text/html,%3Ctitle%3EA%20tab%20of%20the%20person%3C%2Ftitle%3E";
const PERSON_TITLE: &str = "A tab of the person";
const SHELL_TITLE: &str = "Finite.Computer";
const REAL: [&str; 19] = [
    "Settings offers to connect this browser, which has the extension",
    "connecting it: the extension holds the computer's socket",
    "another person signed in to this browser is offered Connect, not told it is connected",
    "Settings lists this browser by the name its extension gave, connected",
    "a ticket for a computer it does not trust (another port, another zone) is refused by the extension, which stays connected",
    "the agent opens a tab in it and reads it, in this browser",
    "a dropped connection: Settings says it is reconnecting, and it comes back on its own, with the extension's link",
    "its worker stopped, as Chrome stops an idle one: woken, it comes back with its link, and the agent's own tab is still its own",
    "at \"Only tabs they open\" (the default), the agent lists its own tab and none of the person's",
    "at \"Their tabs, plus tabs I pick\", the popup lists the person's tabs, and the one ticked joins the agent's",
    "at \"All my tabs and windows\", the agent lists every tab, and closes one of the person's",
    "back at \"Only tabs they open\", the person's tabs leave the agent's list",
    "the extension reloaded: Settings, open from before, is given its new script and shows this browser back on its own, with its link, with no reload",
    "replaced by another browser, it keeps no link (a worker started again later never takes the slot back): Settings says why, and connects it again",
    "Disconnect, on this browser's row in Settings, ends it: it does not come back, and Settings offers Connect again, with no error",
    "then the agent's browse is told its owner's browser is not connected, in the platform's words",
    "Connect again, on the same Settings page with no reload: it says it is connecting at once, then connected",
    "Disconnect from the extension, asked five times at once as closing Chrome's debugging bar asks once for each tab: Settings, open, offers Connect on its own, with no error, and it does not come back",
    "disconnected again and its worker stopped meanwhile, as Chrome stops an idle one, Connect on the same page still connects it",
];

/// The stub's own GET of the computer's `/json/version`, as a CDP client
/// asks a browser before it connects (and the managed skill's check does).
fn version_asked() -> String {
    use fragment_core::owner_browser::{GUEST_HOST, VERSION_PATH};
    format!("fetch http://{GUEST_HOST}{VERSION_PATH} with application/json in accept")
}

/// The chat the agent is in, said to by its owner, one reply a message.
struct Chat<'a> {
    api: &'a Api,
    owner: &'a Keys,
    name: String,
    agent: String,
    said: usize,
}

impl Chat<'_> {
    fn say(&mut self, text: &str) -> Result<()> {
        self.said += 1;
        let r = self.api.signed(self.owner, "POST", &format!("/api/f/{}/channels/chat", self.name), Some(&json!({ "id": format!("m{}", self.said), "body": { "text": text } })))?;
        anyhow::ensure!(r.status == 200, "saying {text:?}: {r}");
        Ok(())
    }

    /// Says `text`, and answers the agent's reply to it.
    fn ask(&mut self, s: &Suite, text: &str) -> Result<String> {
        self.say(text)?;
        Ok(self.reply(s))
    }

    /// The agent's reply to what was said last.
    fn reply(&self, s: &Suite) -> String {
        self.reply_within(s, REPLY)
    }

    /// The agent's reply to what was said last, waited for up to `within`.
    fn reply_within(&self, s: &Suite, within: Duration) -> String {
        let replies = || agent_replies(&records(self.api, self.owner, &self.name, "chat"), &self.agent);
        s.eventually(within, || replies().len() >= self.said);
        replies().get(self.said - 1).and_then(|r| r["body"]["text"].as_str()).unwrap_or("(no reply)").to_string()
    }
}

/// A ticket for `owner`'s browser on computer `id`, as `url` (http(s), as
/// `Socket::connect` takes one).
fn ticket(api: &Api, owner: &Keys, id: &str) -> Result<String> {
    let r = api.signed(owner, "POST", &format!("/api/computers/{id}/browser/ticket"), None)?;
    anyhow::ensure!(r.status == 200, "a browser ticket: {r}");
    Ok(r.body["url"].as_str().context("a ticket names its socket")?.replacen("ws", "http", 1))
}

/// The browsers computer `id`'s view lists.
fn browsers(api: &Api, owner: &Keys, id: &str) -> Vec<Value> {
    api.signed(owner, "GET", &format!("/api/computers/{id}"), None).ok().and_then(|r| r.body["browsers"].as_array().cloned()).unwrap_or_default()
}

/// A ticket a browser's `link` asks for on the computer's `origin`, from
/// `from` (an extension's origin, or a page's).
fn link_ticket(api: &Api, origin: &str, link: &str, from: &str) -> Result<crate::api::Reply> {
    let body = json!({ "link": link }).to_string().into_bytes();
    let call = crate::api::Call { method: "POST", url: format!("{origin}/__browser/ticket"), body: Some(body), content_type: Some("application/json"), extra: vec![("origin", from.to_string())], ..crate::api::Call::default() };
    api.call(call)
}

/// The next frame within `deadline`; a close is an error at once.
fn frame_by(ext: &mut Socket, deadline: Instant) -> Result<String> {
    loop {
        match ext.next_text() {
            Ok(t) => return Ok(t),
            Err(e) if e.to_string().starts_with("closed") || Instant::now() >= deadline => return Err(e),
            Err(_) => {}
        }
    }
}

/// Answers the guest's `browse` as a browser does, its page titled
/// `title`, until the guest socket ends: the methods it heard, and the
/// guest sockets they came on.
fn serve(ext: &mut Socket, title: &str) -> Result<(Vec<String>, BTreeSet<String>)> {
    let deadline = Instant::now() + REPLY;
    let (mut heard, mut sockets) = (vec![], BTreeSet::new());
    loop {
        let frame = frame_by(ext, deadline)?;
        let (socket, message) = frame.split_once(' ').context("a frame names its socket")?;
        sockets.insert(socket.to_string());
        if message == "close" {
            return Ok((heard, sockets));
        }
        let m: Value = serde_json::from_str(message)?;
        let method = m["method"].as_str().unwrap_or("").to_string();
        let result = match method.as_str() {
            "Target.createTarget" => json!({ "targetId": "T1" }),
            "Target.attachToTarget" => json!({ "sessionId": "S1" }),
            "Runtime.evaluate" => json!({ "result": { "type": "string", "value": title } }),
            _ => json!({}),
        };
        let mut answer = json!({ "id": m["id"], "result": result });
        if let Some(session) = m.get("sessionId") {
            answer["sessionId"] = session.clone();
        }
        ext.send_text(&format!("{socket} {answer}"))?;
        heard.push(method);
    }
}

/// What Settings' Browsers line says, for a check's detail.
const LINE: &str = "document.getElementById('settings-browser')?.innerText ?? document.body.innerText.slice(0, 300)";

/// The id Chrome gives the unpacked extension at `dir`: the first 16
/// bytes of its path's SHA-256, each hex digit `0`-`f` written as a letter
/// `a`-`p`. Its worker is looked for by it: Google Chrome runs workers of
/// its own extensions, and loads no unpacked one (from 137).
fn extension_id(dir: &std::path::Path) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(dir.to_string_lossy().as_bytes());
    hex::encode(&digest[..16]).chars().map(|c| (b'a' + c.to_digit(16).expect("hex") as u8) as char).collect()
}

/// A Chrome of the lane's own with the extension loaded, `owner` signed in,
/// and this browser connected in Settings' Browsers line: the first two of `labels` are
/// its checks; the extension's id beside them. `None` (each label a skip
/// saying why) when no Chrome is installed, or it loads no unpacked
/// extension.
fn extension_chrome(s: &mut Suite, api: &Api, owner: &Keys, labels: &[&str]) -> Result<Option<(Browser, crate::browser::Page, String)>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extension").canonicalize()?;
    let (load, only) = (format!("--load-extension={}", dir.display()), format!("--disable-extensions-except={}", dir.display()));
    let skip = |s: &mut Suite, why: &str| labels.iter().for_each(|l| s.skip(l, why));
    let Some(mut b) = Browser::launch_with(&s.scratch, &[&load, &only], None)? else {
        skip(s, "no Chrome is installed (set CHROME_BIN)");
        return Ok(None);
    };
    let id = extension_id(&dir);
    let ours = format!("chrome-extension://{id}/");
    // its worker starts as Chrome does; on a Chrome that loads none (CI's
    // Google Chrome) this wait runs out on every run, so it is short
    if !s.eventually(EXTENSION_UP, || b.extension_workers().is_ok_and(|w| w.iter().any(|u| u.starts_with(&ours)))) {
        skip(s, "this Chrome loads no unpacked extension (Google Chrome does not, from 137): set CHROME_BIN to Chromium or Chrome for Testing");
        return Ok(None);
    }
    let wait = Duration::from_secs(30);
    let session = api.sign_in(&Api::email_of(owner))?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let settings = b.open(&format!("{}/settings", api.base))?;
    let offered = b.until(&settings, "!!document.querySelector('[data-action=browser-connect]')", wait);
    s.ok(labels[0], offered, b.eval(&settings, LINE)?);
    b.click(&settings, "[data-action=browser-connect]")?;
    let connected = b.until(&settings, "document.getElementById('settings-browser')?.innerText.includes('(this browser): connected') === true", wait);
    s.ok(labels[1], connected, b.eval(&settings, LINE)?);
    Ok(Some((b, settings, id)))
}

pub fn owner_browser(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("owner-browser", &[crate::Need::Computers, crate::Need::LocalDocker]) {
        return Ok(());
    }
    let owner = api.person()?;
    let stranger = api.person()?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = r.body["computer"].as_str().unwrap_or("").to_string();
    s.ok("a person makes their computer", r.status == 200 && id.starts_with("computer:"), &r);
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    s.ok("its owner assigns an agent to it", r.status == 200 && !identity.is_empty(), &r);
    let chat_name = s.named(api, &owner, "chat")?;
    let created = s.create(api, &owner, &chat_name)?;
    s.commit(&created, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&created);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    let woke = r.status == 200 && s.eventually(WAKE, || phase(api, &owner, &id) == "awake");
    s.ok("the agent joins a chat, which wakes its computer", woke, phase(api, &owner, &id));
    let followed = s.eventually(WAKE, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    s.ok("its guest follows the chat", followed, "");
    let mut chat = Chat { api, owner: &owner, name: chat_name, agent: identity, said: 0 };

    // invalid: a ticket is its owner's alone; with none connected, the guest is told
    let r = api.signed(&stranger, "POST", &format!("/api/computers/{id}/browser/ticket"), None)?;
    s.ok("no one else mints a ticket for its browser", r.status == 404, &r);
    chat.say("browse https://example.test/one")?;
    let reply = chat.reply(s);
    s.ok("with no browser connected, the guest is told so, in the platform's words", reply.starts_with("browse failed") && reply.contains("your owner's browser is not connected"), &reply);
    let version = chat.ask(s, &version_asked())?;
    s.ok(
        "asked first, as a CDP client asks (`/json/version`), the computer says none is connected, in the same words",
        version.starts_with("fetched 403") && version.contains("not_connected") && version.contains("your owner's browser is not connected"),
        &version,
    );

    // valid: an extension's first connection makes the browser a link, and
    // carries the guest's CDP, named
    let url = ticket(api, &owner, &id)?;
    s.ok("its owner's ticket is a socket on the computer's own origin", url.contains("--computer.") && url.contains("/__browser?t="), &url);
    let origin = url.split("/__browser").next().unwrap_or("").to_string();
    let (mut ext, _) = Socket::connect(api, &url, None, None, Some(EXTENSION))?;
    let given: Value = frame_by(&mut ext, Instant::now() + Duration::from_secs(10)).ok().and_then(|f| serde_json::from_str(&f).ok()).unwrap_or_default();
    let (link_id, link) = (given["id"].as_str().unwrap_or("").to_string(), given["link"].as_str().unwrap_or("").to_string());
    s.ok(
        "its first connection makes the browser a link, given once, on its socket",
        given["type"] == "link" && fragment_core::owner_browser::valid_link_id(&link_id) && fragment_core::owner_browser::valid_link(&link),
        json!({ "type": given["type"], "id": link_id }),
    );
    ext.send_text(r#"{"type":"hello","name":"An e2e browser"}"#)?;
    let named = s.eventually(Duration::from_secs(10), || browsers(api, &owner, &id).iter().any(|b| b["id"] == link_id.as_str() && b["name"] == "An e2e browser" && b["connected"] == true));
    s.ok("its extension names it, and its owner sees it by that name, connected", named, json!(browsers(api, &owner, &id)));
    chat.say("browse https://example.test/one")?;
    let served = serve(&mut ext, "A page");
    let reply = chat.reply(s);
    let (heard, sockets) = served.as_ref().map(|(h, k)| (h.clone(), k.clone())).unwrap_or_default();
    s.ok(
        "the guest's CDP reaches its owner's browser, each frame named by its socket",
        heard == ["Target.createTarget", "Target.attachToTarget", "Runtime.evaluate"] && sockets.len() == 1 && sockets.iter().all(|k| fragment_core::owner_browser::valid_socket(k)),
        format!("{served:?}"),
    );
    s.ok("and the browser's answers reach the guest: the agent reads the page's title", reply == "browsed https://example.test/one: A page", &reply);
    s.ok("the guest socket's end is told to the browser", served.is_ok(), format!("{served:?}"));
    let socket = format!("ws://{}{}", fragment_core::owner_browser::GUEST_HOST, fragment_core::owner_browser::GUEST_PATH);
    let version = chat.ask(s, &version_asked())?;
    s.ok(
        "while it is connected, `/json/version` answers as a browser's DevTools does: the name its extension gave, and the guest's socket",
        version.starts_with("fetched 200") && version.contains(r#""Browser":"An e2e browser""#) && version.contains(&format!(r#""webSocketDebuggerUrl":"{socket}""#)),
        &version,
    );

    // invalid: a page's socket, one with no ticket, a link from a page or
    // no link at all
    let unspent = ticket(api, &owner, &id)?;
    let page = Socket::connect(api, &unspent, None, None, Some(&api.base)).err().map(|e| format!("{e:#}")).unwrap_or_default();
    s.ok("a socket from a page, not an extension, is refused", page.contains("403"), &page);
    let bare = unspent.split_once('?').map(|(u, _)| u.to_string()).unwrap_or_default();
    let none = Socket::connect(api, &bare, None, None, Some(EXTENSION)).err().map(|e| format!("{e:#}")).unwrap_or_default();
    s.ok("one that names no ticket is refused", none.contains("400"), &none);
    let (from_page, wrong) = (link_ticket(api, &origin, &link, &api.base)?, link_ticket(api, &origin, &"0".repeat(64), EXTENSION)?);
    s.ok("a link's ticket is refused to a page (403), and to a link the computer never made (401)", from_page.status == 403 && wrong.status == 401, format!("{} / {}", from_page.status, wrong.status));

    // coming back: its link's own ticket, which replaces the socket before
    // it (told not to come back), spent once
    let back = link_ticket(api, &origin, &link, EXTENSION)?;
    let back_url = back.body["url"].as_str().unwrap_or("").replacen("ws", "http", 1);
    let (mut newer, _) = Socket::connect(api, &back_url, None, None, Some(EXTENSION))?;
    let before = frame_by(&mut ext, Instant::now() + Duration::from_secs(10)).err().map(|e| e.to_string()).unwrap_or_default();
    s.ok(
        "with its link a browser asks for a ticket of its own and comes back, replacing the socket before it, which is told not to come back",
        back.status == 200 && back_url.contains("/__browser?t=") && before.starts_with(&format!("closed {}", fragment_core::owner_browser::REPLACED)),
        format!("{} / {before}", back.status),
    );
    let again = Socket::connect(api, &back_url, None, None, Some(EXTENSION)).err().map(|e| format!("{e:#}")).unwrap_or_default();
    s.ok("a ticket is spent once", again.contains("401"), &again);

    // replaced: a browser that goes mid-call ends the guest's socket
    chat.say("browse https://example.test/two")?;
    let asked = frame_by(&mut newer, Instant::now() + REPLY).unwrap_or_default();
    drop(newer);
    let reply = chat.reply(s);
    s.ok(
        "a browser that disconnects mid-call ends the guest's socket, which says so",
        asked.contains("Target.createTarget") && reply.starts_with("browse failed") && reply.contains("disconnected"),
        format!("{asked} / {reply}"),
    );

    // forgotten: from anywhere, by its owner alone; its socket ends, told
    // not to come back, and its link is refused from then on
    let came = link_ticket(api, &origin, &link, EXTENSION)?;
    let (mut held, _) = Socket::connect(api, &came.body["url"].as_str().unwrap_or("").replacen("ws", "http", 1), None, None, Some(EXTENSION))?;
    let r = api.signed(&stranger, "DELETE", &format!("/api/computers/{id}/browser/links/{link_id}"), None)?;
    s.ok("no one else forgets its browser", r.status == 404, &r);
    let r = api.signed(&owner, "DELETE", &format!("/api/computers/{id}/browser/links/{link_id}"), None)?;
    let ended = frame_by(&mut held, Instant::now() + Duration::from_secs(10)).err().map(|e| e.to_string()).unwrap_or_default();
    let refused = link_ticket(api, &origin, &link, EXTENSION)?;
    s.ok(
        "its owner forgets it: its socket ends, told not to come back, its link is refused, and its view lists it no more",
        r.status == 200 && ended.starts_with(&format!("closed {}", fragment_core::owner_browser::FORGOTTEN)) && refused.status == 401 && browsers(api, &owner, &id).is_empty(),
        format!("{} / {ended} / {}", r.status, refused.status),
    );

    // the real extension, in a Chrome that loads it
    let Some((mut b, settings, ext_id)) = extension_chrome(s, api, &owner, &REAL)? else {
        return Ok(());
    };
    let wait = Duration::from_secs(30);
    // the extension holds the owner's computer: another person's Settings
    // in this browser (their own computer, asleep) offers Connect, which
    // would replace it, and never says it is theirs
    let other = api.person()?;
    api.signed(&other, "POST", "/api/computers", Some(&json!({})))?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &api.sign_in(&Api::email_of(&other))?)?;
    let theirs = b.open(&format!("{}/settings", api.base))?;
    let offered = b.until(&theirs, "!!document.querySelector('[data-action=browser-connect]') && !document.querySelector('[data-browser]')", wait);
    s.ok(REAL[2], offered, b.eval(&theirs, LINE)?);
    b.close(theirs)?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &api.sign_in(&Api::email_of(&owner))?)?;
    let here = browsers(api, &owner, &id).first().cloned().unwrap_or_default();
    let mine = here["id"].as_str().unwrap_or("").to_string();
    let row = format!("[data-browser=\"{mine}\"]");
    b.reload(&settings)?;
    let listed = b.until(&settings, &format!("document.querySelector('{row}')?.innerText.includes('(this browser): connected') === true"), wait);
    s.ok(REAL[3], here["connected"] == true && here["name"].as_str().is_some_and(|n| n.contains(" on ")) && listed, format!("{here} / {}", b.eval(&settings, LINE)?));
    // invalid: a page that hands the extension a ticket for a computer it
    // does not trust (another port than the platform's page, or another
    // zone than its manifest's) is refused, and it stays connected
    let host = format!("{}--computer", "0".repeat(24));
    let mut refused = vec![];
    for forged in [format!("ws://{host}.fragment.localhost:1/__browser?t=x"), format!("wss://{host}.example.test/__browser?t=x")] {
        let ask = format!("new Promise((done) => {{ addEventListener('message', (e) => e.data?.fragmentBrowser === 'status' && e.data.error && done(e.data.error)); setTimeout(() => done('(no answer)'), 5000); postMessage({{ fragmentBrowser: 'connect', url: {} }}, location.origin); }})", json!(forged));
        refused.push(b.eval(&settings, &ask)?);
    }
    let stayed = browsers(api, &owner, &id).first().is_some_and(|b| b["connected"] == true && b["id"] == mine.as_str());
    s.ok(REAL[4], refused.iter().all(|r| r == "that is not a Fragment browser ticket") && stayed, format!("{refused:?} / {}", json!(browsers(api, &owner, &id))));
    chat.say(&format!("browse {PAGE}"))?;
    let reply = chat.reply(s);
    let pages = b.pages()?;
    let opened = pages.iter().any(|(_, url)| url.starts_with("data:text/html"));
    s.ok(REAL[5], reply == format!("browsed {PAGE}: {PAGE_TITLE}") && opened, format!("{reply} / {pages:?}"));
    // a dropped connection (the lever drops it as a network would): the
    // extension comes back on its own, with its link
    let r = super::computers::lever(api, &id, "drop-browser")?;
    let dropped = r.status == 200 && r.body["dropped"] == true;
    // the extension's first try is a second away: Settings says so meanwhile
    let reconnecting = b.until(&settings, "document.getElementById('settings-browser')?.innerText.includes('Reconnecting this browser') === true", Duration::from_secs(5));
    let came_back = s.eventually(wait, || browsers(api, &owner, &id).first().is_some_and(|b| b["connected"] == true && b["id"] == mine.as_str()));
    let works = chat.ask(s, "tabs")?;
    s.ok(REAL[6], dropped && reconnecting && came_back && works.contains(PAGE_TITLE), format!("{r} / {} / {works}", json!(browsers(api, &owner, &id))));
    // its worker stopped, as Chrome stops one idle past its socket's end (a
    // laptop's sleep, a long outage): the platform's page wakes it, its
    // link brings it back, and it still knows the tab the agent opened
    let stopped = b.stop_extension_worker(&format!("chrome-extension://{ext_id}/"))?;
    let gone = s.eventually(wait, || browsers(api, &owner, &id).first().is_some_and(|b| b["connected"] == false));
    b.reload(&settings)?;
    let woke = s.eventually(wait, || browsers(api, &owner, &id).first().is_some_and(|b| b["connected"] == true && b["id"] == mine.as_str()));
    let kept = chat.ask(s, "tabs")?;
    s.ok(REAL[7], stopped && gone && woke && kept.contains(PAGE_TITLE), format!("{stopped} {gone} {woke} / {kept}"));
    // what the agent may use is the person's choice, in the extension's
    // popup (its settings): the stub's `tabs` lists it, as a browser tool's
    // `tab list` would
    b.open(PERSON_PAGE)?;
    let own = chat.ask(s, "tabs")?;
    s.ok(REAL[8], own.contains(PAGE_TITLE) && !own.contains(PERSON_TITLE) && !own.contains(SHELL_TITLE), &own);
    let popup = b.open(&format!("chrome-extension://{ext_id}/popup.html"))?;
    let person_box = format!("input[data-title=\"{PERSON_TITLE}\"]");
    let at = |level: &str| format!("document.querySelector('input[value={level}]')?.checked === true");
    // shown once the worker answered its first view: a level is checked
    b.until(&popup, "!!document.querySelector('input[name=access]:checked')", wait);
    b.click(&popup, "input[value=chosen]")?;
    let listed = b.until(&popup, &format!("{} && !!document.querySelector('{person_box}')", at("chosen")), wait);
    b.click(&popup, &person_box)?;
    b.until(&popup, &format!("document.querySelector('{person_box}')?.checked === true"), wait);
    let chosen = chat.ask(s, "tabs")?;
    s.ok(REAL[9], listed && chosen.contains(PAGE_TITLE) && chosen.contains(PERSON_TITLE) && !chosen.contains(SHELL_TITLE), &chosen);
    b.click(&popup, "input[value=all]")?;
    b.until(&popup, &at("all"), wait);
    let all = chat.ask(s, "tabs")?;
    let closed = chat.ask(s, &format!("close-tab {PERSON_TITLE}"))?;
    let gone = s.eventually(Duration::from_secs(10), || b.pages().is_ok_and(|p| !p.iter().any(|(_, url)| url == PERSON_PAGE)));
    s.ok(REAL[10], all.contains(PERSON_TITLE) && all.contains(SHELL_TITLE) && closed == format!("closed {PERSON_TITLE}") && gone, format!("{all} / {closed}"));
    b.click(&popup, "input[value=own]")?;
    b.until(&popup, &at("own"), wait);
    let back = chat.ask(s, "tabs")?;
    s.ok(REAL[11], back.contains(PAGE_TITLE) && !back.contains(SHELL_TITLE), &back);
    b.close(popup)?;
    // the extension reloaded, as its owner's Reload in chrome://extensions
    // does it (an unpacked one stays on only in developer mode), or as an
    // update does: its script on the Settings page already open is cut off
    // from it, the new worker gives the page its own (the copy on the page
    // marks itself the newest), and Settings hears from it, with no reload,
    // that this browser is back on its own, its link
    const MARK: &str = "document.documentElement.dataset.fragmentExtension ?? ''";
    let before = b.eval(&settings, MARK)?;
    let extensions = b.open("chrome://extensions/")?;
    let reload = format!("chrome.developerPrivate.updateProfileConfiguration({{ inDeveloperMode: true }}).then(() => chrome.developerPrivate.reload('{ext_id}', {{ failQuietly: true }})).then(() => true)");
    let reloaded = b.eval(&extensions, &reload)?;
    b.close(extensions)?;
    let given = b.until(&settings, &format!("({MARK}) !== {before}"), wait);
    let back = b.until(
        &settings,
        &format!("(window.postMessage({{ fragmentBrowser: 'hello' }}, location.origin), document.querySelector('{row}')?.innerText.includes('(this browser): connected') === true && !document.getElementById('settings-browser').innerText.includes('Reload'))"),
        wait,
    );
    let linked = browsers(api, &owner, &id);
    let one = linked.len() == 1 && linked[0]["id"] == mine.as_str() && linked[0]["connected"] == true;
    s.ok(REAL[12], reloaded == true && given && back && one, format!("{before} → {} / {} / {}", b.eval(&settings, MARK)?, b.eval(&settings, LINE)?, json!(linked)));
    // replaced: another browser of its owner's connects. This one is told
    // not to come back and keeps no link, so a worker Chrome starts again
    // later never takes the slot back; Settings says why, and connects it
    // again (a link of its own) when asked
    let (other, _) = Socket::connect(api, &ticket(api, &owner, &id)?, None, None, Some(EXTENSION))?;
    let said = b.until(&settings, "document.getElementById('settings-browser')?.innerText.includes('Not connected: another browser connected in its place.') === true", wait);
    let popup = b.open(&format!("chrome-extension://{ext_id}/popup.html"))?;
    let unlinked = s.eventually(wait, || b.eval(&popup, "chrome.storage.local.get('link').then((v) => !v.link)").is_ok_and(|v| v == true));
    b.close(popup)?;
    drop(other);
    b.click(&settings, "[data-action=browser-connect]")?;
    let again = b.until(&settings, "document.getElementById('settings-browser')?.innerText.includes('(this browser): connected') === true", wait);
    s.ok(REAL[13], said && unlinked && again, b.eval(&settings, LINE)?);
    let mine = browsers(api, &owner, &id).into_iter().find(|b| b["connected"] == true).and_then(|b| b["id"].as_str().map(str::to_string)).unwrap_or_default();
    let row = format!("[data-browser=\"{mine}\"]");
    // disconnected in Settings (its row's Forget, said Disconnect on this
    // browser's own): the extension hears it must not come back (its link
    // let go), and Settings offers Connect again, with nothing to explain:
    // its owner said so
    let labelled = b.eval(&settings, &format!("document.querySelector('{row} [data-action=browser-forget]')?.innerText"))?;
    b.click(&settings, &format!("{row} [data-action=browser-forget]"))?;
    let told = format!("!document.querySelector('{row}') && !!document.querySelector('[data-action=browser-connect]') && !document.getElementById('settings-browser').innerText.includes('Not connected')");
    let off = b.until(&settings, &told, wait);
    // and stays so, once the extension's own word arrives
    std::thread::sleep(Duration::from_secs(1));
    let quiet = b.eval(&settings, &format!("({told})"))? == true;
    s.ok(REAL[14], labelled == "Disconnect" && off && quiet && !browsers(api, &owner, &id).iter().any(|b| b["id"] == mine.as_str()), format!("{labelled} / {}", b.eval(&settings, LINE)?));
    chat.say(&format!("browse {PAGE}"))?;
    let reply = chat.reply(s);
    s.ok(REAL[15], reply.starts_with("browse failed") && reply.contains("your owner's browser is not connected"), &reply);
    // connected again from the same Settings, no reload: what its owner
    // does next. Pressed, it says so at once, before the extension answers.
    let connected = "document.getElementById('settings-browser')?.innerText.includes('(this browser): connected') === true";
    b.click(&settings, "[data-action=browser-connect]")?;
    let at_once = b.eval(&settings, &format!("document.getElementById('settings-browser')?.innerText.includes('Connecting this browser') || {connected}"))? == true;
    let again = b.until(&settings, connected, wait);
    s.ok(REAL[16], at_once && again, b.eval(&settings, LINE)?);
    // disconnected from the extension's popup, as its owner might, asked
    // five times at once as closing Chrome's "debugging this browser" bar
    // asks once for each tab it held: each answer without an error, Settings
    // (open, on another page) offers Connect on its own with nothing to
    // explain, and it does not come back
    let mine = browsers(api, &owner, &id).into_iter().find(|b| b["connected"] == true).and_then(|b| b["id"].as_str().map(str::to_string)).unwrap_or_default();
    let popup = b.open(&format!("chrome-extension://{ext_id}/popup.html"))?;
    // shown once the worker answered its first view
    b.until(&popup, "!!document.querySelector('input[name=access]:checked')", wait);
    let answers = b.eval(&popup, "Promise.all(Array.from({ length: 5 }, () => chrome.runtime.sendMessage({ type: 'disconnect' }))).then((all) => all.map((s) => [s.connected, s.link, s.error]))")?;
    b.close(popup)?;
    let clean = answers.as_array().is_some_and(|all| all.len() == 5 && all.iter().all(|a| a[0] == false && a[1].is_null() && a[2].is_null()));
    let offered = b.until(&settings, "!!document.querySelector('[data-action=browser-connect]') && !document.getElementById('settings-browser').innerText.includes('Not connected')", wait);
    std::thread::sleep(Duration::from_secs(2));
    let gone = !browsers(api, &owner, &id).iter().any(|b| b["id"] == mine.as_str() || b["connected"] == true);
    s.ok(REAL[17], clean && offered && gone, format!("{answers} / {} / {}", b.eval(&settings, LINE)?, json!(browsers(api, &owner, &id))));
    // disconnected again, and its worker stopped meanwhile (Chrome stops an
    // idle one within 30 s once its socket is gone): Connect wakes it
    b.click(&settings, "[data-action=browser-connect]")?;
    let first = b.until(&settings, connected, wait);
    let mine = browsers(api, &owner, &id).into_iter().find(|b| b["connected"] == true).and_then(|b| b["id"].as_str().map(str::to_string)).unwrap_or_default();
    b.click(&settings, &format!("[data-browser=\"{mine}\"] [data-action=browser-forget]"))?;
    let offered = b.until(&settings, "!!document.querySelector('[data-action=browser-connect]')", wait);
    let stopped = b.stop_extension_worker(&format!("chrome-extension://{ext_id}/"))?;
    b.click(&settings, "[data-action=browser-connect]")?;
    let woke = b.until(&settings, connected, wait);
    s.ok(REAL[18], first && offered && stopped && woke, format!("{first} {offered} {stopped} / {}", b.eval(&settings, LINE)?));
    Ok(())
}

/// The same browser driven by our Hermes image as its managed skill
/// teaches (owner-browser-finite, templates/skills): the skill installed
/// from its owner's skills fragment and listed to the model by what its
/// owner calls the browser; its check and its agent-browser command, each
/// the skill's own line, run by Hermes' terminal tool (the scripted
/// model's `run:`), the tab opening in the extension's Chrome; and nothing
/// said of the browser beside its owner's message. It builds the Hermes
/// image (3.8 GB), so it runs only by name (`cargo xtask e2e --only
/// owner-browser-hermes`).
pub const HERMES_SECTION: &str = "owner-browser-hermes";
/// A Hermes start, and one of its turns (hermes.rs's bounds).
const HERMES_WAKE: Duration = Duration::from_secs(300);
const HERMES_TURN: Duration = Duration::from_secs(180);
/// The managed set's install, off the boot's path (hermes.rs waits as
/// long for one made while the computer is awake).
const SKILL_INSTALLED: Duration = Duration::from_secs(120);
/// The skill, in the release's managed set, and where a profile has it.
const SKILL: &str = "skills/productivity/owner-browser-finite/SKILL.md";
const SKILL_INSTALLED_AT: &str = "/data/hermes/managed-skills/productivity/owner-browser-finite/SKILL.md";
const HERMES: [&str; 7] = [
    "Settings offers to connect this browser, which has the extension",
    "connecting it: the extension holds the computer's socket",
    "its owner's skills fragment installs owner-browser-finite in the agent's profile",
    "the skill's check, run by Hermes' terminal, says the browser is connected: the name its extension gave, and the socket the skill drives",
    "the skill's agent-browser command, run by Hermes' terminal, opens the agent's tab in this browser, and the tool's answer reads its title",
    "Hermes lists the skill to its model, by what its owner calls the browser (\"my browser\")",
    "nothing is said of the browser beside its owner's message",
];

/// The skill's first line that `is`, as the release has it.
fn skill_line(is: impl Fn(&str) -> bool) -> Option<String> {
    let skill = fragment_templates::blessed::data_file("skills", SKILL)?;
    std::str::from_utf8(skill.bytes).ok()?.lines().find(|l| is(l)).map(str::to_string)
}

pub fn owner_browser_hermes(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section_by_name(HERMES_SECTION, &[crate::Need::Fakes, crate::Need::LocalDocker], "it builds and runs the Hermes image") {
        return Ok(());
    }
    s.ai.clear_script();
    s.ai.transcripts(true);
    let ran = hermes_run(s, api);
    s.ai.transcripts(false);
    ran
}

fn hermes_run(s: &mut Suite, api: &Api) -> Result<()> {
    let owner = api.person()?;
    // its owner's skills fragment, as the shell's setup makes one: the
    // managed set, the skill among it, installed as the computer starts
    let r = api.create_with(&owner, json!({ "label": s.name("skills"), "template": "skills" }))?;
    s.ok("its owner has a skills fragment on the blessed template", r.status == 200 && r.body["name"].is_string(), &r);
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = r.body["computer"].as_str().unwrap_or("").to_string();
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "hermes" })))?;
    s.ok("a person's computer, pinned to the Hermes image", r.status == 200 && r.body["image"] == "hermes", &r);
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON)), ("agent.json", Some(br#"{"tier":"cheap"}"#))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    s.ok("its owner assigns an agent to it", r.status == 200 && !identity.is_empty(), &r);
    let chat_name = s.named(api, &owner, "chat")?;
    let created = s.create(api, &owner, &chat_name)?;
    s.commit(&created, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&created);
    api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    let followed = s.eventually(HERMES_WAKE, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    s.ok("Hermes wakes and follows the chat", followed, phase(api, &owner, &id));
    let Some((mut b, _settings, _id)) = extension_chrome(s, api, &owner, &HERMES)? else {
        return Ok(());
    };
    let mut chat = Chat { api, owner: &owner, name: chat_name, agent: identity, said: 0 };

    // the managed set installs off the boot's path: looked for until it is there
    let there = |said: &str| said.contains(SKILL_INSTALLED_AT) && !said.contains("No such file");
    let looked = Instant::now();
    let mut installed = String::new();
    while looked.elapsed() < SKILL_INSTALLED {
        chat.say(&format!("run: ls {SKILL_INSTALLED_AT}"))?;
        installed = chat.reply_within(s, HERMES_TURN);
        if there(&installed) {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    s.ok(HERMES[2], there(&installed), &installed);

    // the skill's check, as it says it
    let socket = format!("ws://{}{}", fragment_core::owner_browser::GUEST_HOST, fragment_core::owner_browser::GUEST_PATH);
    let check = skill_line(|l| l.starts_with("curl ")).context("the skill says how to check")?;
    chat.say(&format!("run: {check}"))?;
    let checked = chat.reply_within(s, HERMES_TURN);
    let name = browsers(api, &owner, &id).first().and_then(|b| b["name"].as_str().map(str::to_string)).unwrap_or_default();
    s.ok(HERMES[3], !name.is_empty() && checked.contains(&name) && checked.contains("webSocketDebuggerUrl") && checked.contains(&socket), format!("{name}: {checked}"));

    // the skill's own command opens a page this Chrome reaches, with a title
    // of its own: the platform's (agent-browser navigates with
    // Page.navigate, which Chrome refuses for a `data:` URL through
    // chrome.debugger: net::ERR_ABORTED)
    let open = skill_line(|l| l.starts_with("agent-browser ") && l.contains(" open ")).context("the skill says how to open a page")?;
    let (command, _) = open.rsplit_once(' ').context("the skill's open names a page")?;
    let page = format!("{}/", api.base);
    let opening = format!("run: {command} {page}");
    chat.say(&opening)?;
    let reply = chat.reply_within(s, HERMES_TURN);
    let pages = b.pages()?;
    s.ok(HERMES[4], command.contains(&socket) && pages.iter().any(|(_, url)| *url == page) && reply.contains(SHELL_TITLE), format!("{reply} / {pages:?}"));

    // what the model was handed: the skill in its index, and its owner's
    // message as they said it, with no context before it
    let chats = s.ai.chats();
    let index = |c: &Value| {
        let system = c["messages"].as_array()?.iter().find(|m| m["role"] == "system")?["content"].to_string();
        let at = system.find("<available_skills>")?;
        Some(system[at..].to_string())
    };
    let indexed = chats.iter().filter_map(index).find(|i| i.contains("owner-browser-finite"));
    let listed = indexed.as_deref().and_then(|i| i.find("owner-browser-finite").map(|at| i[at..].chars().take(300).collect::<String>()));
    s.ok(
        HERMES[5],
        listed.as_deref().is_some_and(|l| l.starts_with("owner-browser-finite: Your owner's own browser") && l.contains(r#"\"my browser\""#)),
        listed.unwrap_or_else(|| {
            // not in an index: where the model was told of it, if anywhere, and the index it had
            let elsewhere = chats.iter().map(Value::to_string).find_map(|c| {
                let at = c.find("owner-browser-finite")?;
                let from = c.char_indices().map(|(i, _)| i).take_while(|i| *i <= at.saturating_sub(200)).last().unwrap_or(0);
                Some(c[from..].chars().take(500).collect::<String>())
            });
            format!("elsewhere: {elsewhere:?}; its index: {}", chats.last().and_then(index).map(|i| i.chars().take(3000).collect::<String>()).unwrap_or_default())
        }),
    );
    let asked = chats.iter().filter_map(|c| c["messages"].as_array()?.iter().rev().find(|m| m["role"] == "user").map(|m| m["content"].to_string())).find(|u| u.contains(&opening));
    s.ok(HERMES[6], asked.as_deref().is_some_and(|u| !u.contains("[Recent channel messages]")), asked.unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The managed skill's check and its commands name where the computer
    /// answers and the socket it carries (crates/core's), whole: the
    /// Hermes section runs these lines as they are.
    #[test]
    fn the_skill_names_the_computers_check_and_socket() {
        use fragment_core::owner_browser::{GUEST_HOST, GUEST_PATH, VERSION_PATH};
        assert_eq!(super::skill_line(|l| l.starts_with("curl ")).unwrap(), format!("curl -s http://{GUEST_HOST}{VERSION_PATH}"));
        let drives = format!("agent-browser --session owner --cdp ws://{GUEST_HOST}{GUEST_PATH} ");
        let open = super::skill_line(|l| l.starts_with("agent-browser ") && l.contains(" open ")).unwrap();
        assert!(open.starts_with(&format!("{drives}open ")), "{open}");
        assert!(super::skill_line(|l| l.starts_with("agent-browser ") && !l.starts_with(&drives)).is_none(), "every command on the owner's session and socket");
    }

    /// Hermes' skills index shows a description's first 57 characters, and
    /// "..." past 60 (`SKILL_PROMPT_DESC_LIMIT`, agent/skill_utils.py at
    /// v0.21.7): what its owner calls the browser is within them, and the
    /// description is one YAML scalar (no ": " in it).
    #[test]
    fn the_skills_index_shows_what_its_owner_calls_it() {
        let description = super::skill_line(|l| l.starts_with("description: ")).unwrap();
        let description = description.strip_prefix("description: ").unwrap();
        let shown: String = description.chars().take(57).collect();
        assert!(shown.starts_with("Your owner's own browser") && shown.contains("\"my browser\"") && shown.contains("\"my tabs\""), "{shown}");
        assert!(!description.contains(": "), "{description}");
    }

    /// An unpacked extension's id is its path's, as Chrome derives it (seen:
    /// the id Chrome 148 gave the extension loaded from this path).
    #[test]
    fn an_unpacked_extensions_id_is_its_paths() {
        let dir = std::path::Path::new("/Users/plebdev/Desktop/Projects/fragment-owner-browser/extension");
        assert_eq!(super::extension_id(dir), "pabppmmjgjppafdielninhopacngikok");
        assert!(fragment_core::owner_browser::extension_origin(&format!("chrome-extension://{}", super::extension_id(dir))));
    }
}
