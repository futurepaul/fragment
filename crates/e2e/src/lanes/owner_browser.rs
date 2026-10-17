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
//!   guest socket's end is told to it. While it is connected, its owner's
//!   turn is told so first, by the bridge (the browser's name and its
//!   socket, which the computer answers at `/json/version`; the stub
//!   echoes it, and Hermes hands it to its model). In a Chrome that loads the extension
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
//!   so, in the platform's words, and its owner's turns nothing of one.
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
/// What the stub echoes it was told before its owner's message while
/// their browser is connected (the bridge asks the computer first), after
/// its reply.
const TOLD: &str = "\n\n(told: its owner's browser, ";
const REAL: [&str; 16] = [
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
    "the extension reloaded: Settings, open from before, says to load it again, and loaded again shows this browser back on its own, with its link",
    "replaced by another browser, it keeps no link (a worker started again later never takes the slot back): Settings says why, and connects it again",
    "Forget, in Settings, ends it: it does not come back, and Settings offers to connect again",
    "then the agent is told its owner's browser is not connected, and its owner's turns nothing of it",
];

/// A stub's reply less what it echoes it was told.
fn answer(reply: &str) -> &str {
    reply.split(TOLD).next().unwrap_or(reply)
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
        let replies = || agent_replies(&records(self.api, self.owner, &self.name, "chat"), &self.agent);
        s.eventually(REPLY, || replies().len() >= self.said);
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
    s.ok("and its owner's turns are told nothing of a browser", !reply.contains(TOLD), &reply);

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
    s.ok("and the browser's answers reach the guest: the agent reads the page's title", answer(&reply) == "browsed https://example.test/one: A page", &reply);
    let socket = format!("ws://{}{}", fragment_core::owner_browser::GUEST_HOST, fragment_core::owner_browser::GUEST_PATH);
    s.ok("while it is connected, its owner's turn is told so first: the browser's name, and its socket", reply.ends_with(&format!("{TOLD}An e2e browser, at {socket})")), &reply);
    s.ok("the guest socket's end is told to the browser", served.is_ok(), format!("{served:?}"));

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
    s.ok(REAL[5], answer(&reply) == format!("browsed {PAGE}: {PAGE_TITLE}") && opened, format!("{reply} / {pages:?}"));
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
    s.ok(REAL[10], all.contains(PERSON_TITLE) && all.contains(SHELL_TITLE) && answer(&closed) == format!("closed {PERSON_TITLE}") && gone, format!("{all} / {closed}"));
    b.click(&popup, "input[value=own]")?;
    b.until(&popup, &at("own"), wait);
    let back = chat.ask(s, "tabs")?;
    s.ok(REAL[11], back.contains(PAGE_TITLE) && !back.contains(SHELL_TITLE), &back);
    b.close(popup)?;
    // the extension reloaded, as its owner's Reload in chrome://extensions
    // does it (an unpacked one stays on only in developer mode): its script
    // on the Settings page already open is cut off from it, and says so when
    // the page asks, as it does each time it shows Settings; the page loaded
    // again has the new one, and the extension is back on its own, its link
    let extensions = b.open("chrome://extensions/")?;
    let reload = format!("chrome.developerPrivate.updateProfileConfiguration({{ inDeveloperMode: true }}).then(() => chrome.developerPrivate.reload('{ext_id}', {{ failQuietly: true }})).then(() => true)");
    let reloaded = b.eval(&extensions, &reload)?;
    b.close(extensions)?;
    let cut_off = b.until(&settings, "(window.postMessage({ fragmentBrowser: 'hello' }, location.origin), document.getElementById('settings-browser')?.innerText.includes('reload this page'))", wait);
    let said = b.eval(&settings, LINE)?;
    b.reload(&settings)?;
    let back = b.until(&settings, &format!("document.querySelector('{row}')?.innerText.includes('(this browser): connected') === true"), wait);
    let linked = browsers(api, &owner, &id);
    let one = linked.len() == 1 && linked[0]["id"] == mine.as_str() && linked[0]["connected"] == true;
    s.ok(REAL[12], reloaded == true && cut_off && back && one, format!("{said} / {} / {}", b.eval(&settings, LINE)?, json!(linked)));
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
    // forgotten in Settings: the extension hears it must not come back (its
    // link let go, its status says so), and Settings offers Connect again
    b.click(&settings, &format!("{row} [data-action=browser-forget]"))?;
    let told = format!("!document.querySelector('{row}') && !!document.querySelector('[data-action=browser-connect]') && document.getElementById('settings-browser').innerText.includes('Not connected: this browser was forgotten in Settings.')");
    let off = b.until(&settings, &told, wait);
    s.ok(REAL[14], off && !browsers(api, &owner, &id).iter().any(|b| b["id"] == mine.as_str()), b.eval(&settings, LINE)?);
    chat.say(&format!("browse {PAGE}"))?;
    let reply = chat.reply(s);
    s.ok(REAL[15], reply.contains("your owner's browser is not connected") && !reply.contains(TOLD), &reply);
    Ok(())
}

/// The same browser driven by our Hermes image's own CDP client: the
/// agent-browser on its PATH, run by Hermes' terminal tool (the scripted
/// model's `run:`), its tab opened in the extension's Chrome. Nothing in
/// the image changes for it. It builds the Hermes image (3.8 GB), so it
/// runs only by name (`cargo xtask e2e --only owner-browser-hermes`).
pub const HERMES_SECTION: &str = "owner-browser-hermes";
/// A Hermes start, and one of its turns (hermes.rs's bounds).
const HERMES_WAKE: Duration = Duration::from_secs(300);
const HERMES_TURN: Duration = Duration::from_secs(180);
const HERMES: [&str; 5] = [
    "Settings offers to connect this browser, which has the extension",
    "connecting it: the extension holds the computer's socket",
    "Hermes' terminal runs agent-browser against it, and the agent's tab opens in this browser",
    "the tool's answer reads the page's title",
    "the model is handed its owner's browser before their message: connected, and how its terminal reaches it",
];

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
    // a page this Chrome reaches, with a title of its own: the platform's
    // (agent-browser navigates with Page.navigate, which Chrome refuses
    // for a `data:` URL through chrome.debugger: net::ERR_ABORTED)
    let page = format!("{}/", api.base);
    let mut chat = Chat { api, owner: &owner, name: chat_name, agent: identity, said: 0 };
    let socket = format!("ws://{}{}", fragment_core::owner_browser::GUEST_HOST, fragment_core::owner_browser::GUEST_PATH);
    chat.say(&format!("run: agent-browser --session owner --cdp {socket} open {page}"))?;
    let replies = || agent_replies(&records(api, &owner, &chat.name, "chat"), &chat.agent);
    s.eventually(HERMES_TURN, || !replies().is_empty());
    let reply = replies().first().and_then(|r| r["body"]["text"].as_str()).unwrap_or("(no reply)").to_string();
    let pages = b.pages()?;
    s.ok(HERMES[2], pages.iter().any(|(_, url)| *url == page), format!("{pages:?}"));
    s.ok(HERMES[3], reply.contains(SHELL_TITLE), &reply);
    let chats = s.ai.chats();
    let handed = chats.iter().any(|c| {
        let c = c.to_string();
        c.contains("Your owner's browser is connected: ") && c.contains(&format!("--cdp {socket}"))
    });
    s.ok(HERMES[4], handed, chats.last().map(|c| c.to_string().chars().take(600).collect::<String>()).unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    /// An unpacked extension's id is its path's, as Chrome derives it (seen:
    /// the id Chrome 148 gave the extension loaded from this path).
    #[test]
    fn an_unpacked_extensions_id_is_its_paths() {
        let dir = std::path::Path::new("/Users/plebdev/Desktop/Projects/fragment-owner-browser/extension");
        assert_eq!(super::extension_id(dir), "pabppmmjgjppafdielninhopacngikok");
        assert!(fragment_core::owner_browser::extension_origin(&format!("chrome-extension://{}", super::extension_id(dir))));
    }
}
