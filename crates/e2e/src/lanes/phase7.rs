//! Phase 7's acceptance (ROADMAP, phase 7; docs/phase-7.md, slice E): the
//! whole human flow in one run, in headless Chrome, with two people. Each
//! is in a browser of their own (a browser context) and signs in as a
//! person does: through WorkOS (the fake), a username taken on the
//! platform's page, and a CLI key approved on its page (the harness's
//! hands for what it reads back). The node is shaped as fragment.club is.
//!
//! 1. The owner makes a chat from their desktop (New chat) and shares it
//!    with the guest by username, in the share sheet the desktop opens.
//! 2. The guest accepts at `/join` and lands in the chat.
//! 3. Both see each other's messages arrive live, each labeled with its
//!    sender (the owner's agent's answers too).
//! 4. The guest asks the owner's agent about the chat, which is within the
//!    guest's reach: it answers there, its tool group shown above it.
//! 5. The guest asks it to read one of the owner's private apps: it cannot.
//! 6. The owner asks it to edit that same app: it does.
//! 7. The owner removes the guest in the share sheet: the guest's socket
//!    closes, and their next request is a 403.
//! 8. A rewritten desktop cannot share without the sheet's click. The share
//!    lane proves it and it is not repeated here: "a rewritten desktop
//!    cannot read the sheet …", "nor fetch the signed API …", "(the signed
//!    API takes no session …)", "a rewritten desktop that frames the sheet
//!    gets a frame without it", "a window it opens on the sheet is severed
//!    from it …", "it takes nothing from its URL …", and "a form sent before
//!    its buttons arm is refused …".
//!
//! Each step's details are other lanes': the share lane (the sheet's and the
//! join page's forms and refusals, the badges), the chat section's
//! `chats_apart` (a guest's turn reaches only what the guest could), its
//! work checks (the page's groups, the working line, Stop), and the desktop
//! lane (New chat, its frames). This lane proves the steps work together, as
//! people use them: it is phase 7's "done" evidence.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agents::chat_records;
use crate::api::{url_enc, Api};
use crate::browser::{Browser, BrowserContext, Page};
use crate::Suite;

const WAIT: Duration = Duration::from_secs(30);

const OWNER_SAYS: &str = "Glad you could join. Ask my agent anything.";
const GUEST_SAYS: &str = "Thanks for having me!";
const WELCOME: &str = "Welcome, both of you.";
const GLAD: &str = "Happy to help you both.";
const ASK_CHAT: &str = "What is this chat made of?";
const ABOUT: &str = "Two channels: chat for what we say, work for what I do.";
const CANNOT: &str = "I can't open that app for you: it isn't shared with you.";
const EDITED: &str = "The plan is on.";
const DONE: &str = "Done: your notes say the plan is on.";

/// Runs on a node restarted with its platform on the fragments' domain
/// (as fragment.club is), then restarts it as it was for the lanes after.
pub fn phase7(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("phase7") {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let result = run(s, &api);
    drop(api);
    s.stop()?;
    s.start(false, true)?;
    result
}

/// Someone in the flow.
struct Person {
    /// Their browser.
    browser: BrowserContext,
    /// The CLI key they approved in it: the harness reads with it.
    keys: Keys,
    id: String,
    username: String,
}

/// The chat as the harness reads it, with its owner's key.
struct Chat<'a> {
    api: &'a Api,
    owner: &'a Keys,
    name: String,
    /// The owner's agent, in it.
    agent: String,
}

impl Chat<'_> {
    fn said_by(&self, who: &str, text: &str) -> bool {
        chat_records(self.api, self.owner, &self.name).iter().any(|r| r["principal"] == who && r["body"]["text"] == text)
    }

    fn role_of(&self, id: &str) -> Option<String> {
        let r = self.api.signed(self.owner, "GET", &format!("/api/f/{}/members", self.name), None).ok()?;
        r.body["members"].as_array()?.iter().find(|m| m["principal"] == id).and_then(|m| m["role"].as_str().map(str::to_string))
    }

    /// The turn the agent's answer `text` closed, once the answer is in the
    /// chat and the agent is idle again (the next message starts a turn of
    /// its own, and steers none).
    fn answered(&self, s: &Suite, text: &str) -> Option<String> {
        let turn = || {
            chat_records(self.api, self.owner, &self.name)
                .into_iter()
                .find(|r| r["principal"] == self.agent.as_str() && r["body"]["text"] == text)
                .and_then(|r| r["body"]["turn"].as_str().map(str::to_string))
        };
        let idle = || {
            let v = self.api.signed(self.owner, "GET", "/api/a/agent", None).map(|r| r.body).unwrap_or_default();
            v["active"] == false && v["driving"] == false && v["waiting"].as_array().is_none_or(Vec::is_empty)
        };
        (s.eventually(WAIT, || turn().is_some()) && s.eventually(WAIT, idle)).then(turn).flatten()
    }

    /// Turn `turn`'s records on `work`, once its end is there.
    fn work(&self, s: &Suite, turn: &str) -> Vec<Value> {
        let of_turn = || -> Vec<Value> {
            let r = self.api.signed(self.owner, "GET", &format!("/api/f/{}/channels/work?after=0", self.name), None);
            let all = r.ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
            all.into_iter().filter(|r| r["body"]["turn"] == turn).collect()
        };
        s.eventually(WAIT, || of_turn().last().is_some_and(|r| r["body"]["kind"] == "turn.end"));
        of_turn()
    }
}

/// Who started a turn (its `turn.start`), and its one step.
fn starter_and_step(work: &[Value]) -> (String, Value) {
    let body = |kind: &str| work.iter().find(|r| r["body"]["kind"] == kind).map(|r| r["body"].clone()).unwrap_or_default();
    (body("turn.start")["asker"].as_str().unwrap_or("").to_string(), body("turn.step"))
}

fn label(name: &str) -> &str {
    name.split('.').next().unwrap_or("")
}

/// Where a page is and what it says, for a FAIL's detail.
fn shown(chrome: &mut Browser, page: &Page) -> String {
    let v = chrome.eval(page, "location.href + ' (' + document.visibilityState + ') | ' + (document.body?.innerText ?? '').slice(-600)");
    v.map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_else(|e| format!("{e:#}"))
}

/// What a chat's page shows, for a FAIL's detail.
const MESSAGES: &str = "document.visibilityState + ' | ' + (document.getElementById('messages')?.innerText.slice(-600) ?? '')";

/// JavaScript that sends `text` from a chat's page, as its composer does.
fn send(text: &str) -> String {
    format!("document.getElementById('text').value = {text:?}; document.getElementById('say').requestSubmit(); true")
}

/// Whether a chat's page shows someone else's message `text`, labeled `who`.
fn from_other(text: &str, who: &str) -> String {
    format!("[...document.querySelectorAll('.msg.user.other')].some(m => m.querySelector('.bubble')?.textContent === {text:?} && m.querySelector('.who')?.textContent === {who:?})")
}

/// Whether a chat's page shows an agent's answer `text`, labeled `who`.
fn from_agent(text: &str, who: &str) -> String {
    format!("[...document.querySelectorAll('.msg.agent')].some(m => m.querySelector('.md')?.textContent.includes({text:?}) && m.querySelector('.who')?.textContent === {who:?})")
}

/// Whether a chat's page shows turn `turn`'s tool group, done and folded,
/// above its answer `text`, its one step a call of `tool` that `failed` or not.
fn group_above(turn: &str, text: &str, tool: &str, failed: bool) -> String {
    let group = format!("details.tools[data-turn={turn:?}]");
    let step = if failed { ".step.error" } else { ".step:not(.error)" };
    format!(
        "(() => {{ const g = document.querySelector({group:?}); const a = [...document.querySelectorAll('.msg.agent')].find(m => m.textContent.includes({text:?})); \
         return !!g && !!a && !g.open && g.textContent.includes('Worked through 1 step') && !!g.querySelector({step:?})?.textContent.includes({tool:?}) \
         && !!(g.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING); }})()"
    )
}

/// Whether the button `selector` names is there, and armed.
fn armed(selector: &str) -> String {
    format!("(b => !!b && !b.disabled)(document.querySelector({selector:?}))")
}

/// Someone signs in, in a browser of their own, as a person does: WorkOS
/// (the fake) for `email`, the username the platform asks for, then a CLI
/// key approved on the platform's page. Answers them, and their page on the
/// platform's home.
fn sign_in(chrome: &mut Browser, api: &Api, email: &str, username: &str) -> Result<(Person, Page)> {
    let browser = chrome.another_context()?;
    let home = chrome.open_in(&browser, &format!("{}/auth/login?return=/&login_hint={}", api.base, url_enc(email)))?;
    let take = "form[action=\"/auth/username\"]";
    anyhow::ensure!(chrome.until(&home, &format!("!!document.querySelector({take:?})"), WAIT), "{email} was not asked for a username: {}", shown(chrome, &home));
    let input = format!("{take} input[name=username]");
    chrome.eval(&home, &format!("document.querySelector({input:?}).value = {username:?}; true"))?;
    chrome.click(&home, &format!("{take} button"))?;
    let signed_in = format!("!!document.body?.innerText.includes({:?})", format!("Signed in as {username}"));
    anyhow::ensure!(chrome.until(&home, &signed_in, WAIT), "{email} did not take {username}: {}", shown(chrome, &home));
    let keys = Keys::generate();
    let approve = chrome.open_in(&browser, &api.approval_link(&keys, 0))?;
    let button = "form[action=\"/cli/approve\"] button";
    anyhow::ensure!(chrome.until(&approve, &format!("!!document.querySelector({button:?})"), WAIT), "no key approval for {email}: {}", shown(chrome, &approve));
    chrome.click(&approve, button)?;
    anyhow::ensure!(chrome.until(&approve, "document.title === 'Key added'", WAIT), "{email}'s key was not added: {}", shown(chrome, &approve));
    chrome.close(approve)?;
    let me = api.signed(&keys, "GET", "/api/identities/me", None)?;
    anyhow::ensure!(me.status == 200 && me.body["kind"] == "person" && me.body["username"] == username, "{email}'s key: {me}");
    let id = me.body["id"].as_str().context("an identity has an id")?.to_string();
    Ok((Person { browser, keys, id, username: username.to_string() }, home))
}

/// The desktop opens the chat's share sheet as a person does, a click each
/// on its row's … menu and on Share…: a window of its own, on the platform's
/// origin at `sheet`.
fn open_sheet(s: &Suite, chrome: &mut Browser, desk: &Page, chat: &str, sheet: &str) -> Result<Page> {
    chrome.front(desk)?;
    let before: Vec<String> = chrome.pages()?.into_iter().map(|(target, _)| target).collect();
    chrome.click(desk, &format!(".more[data-fragment={chat:?}]"))?;
    anyhow::ensure!(chrome.until(desk, "!document.getElementById('menu').hidden", WAIT), "the chat's … menu did not open");
    chrome.click(desk, "#menu-share")?;
    let mut found = None;
    s.eventually(WAIT, || {
        found = chrome.pages().ok().and_then(|p| p.into_iter().find(|(target, url)| url == sheet && !before.contains(target)));
        found.is_some()
    });
    let (target, _) = found.with_context(|| format!("Share… opened no window on {sheet}: {:?}", chrome.pages()))?;
    let popup = chrome.attach(&target)?;
    chrome.front(&popup)?;
    Ok(popup)
}

fn run(s: &mut Suite, api: &Api) -> Result<()> {
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the phase7 lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let platform = api.base.clone();
    let (owner, home) = sign_in(&mut chrome, api, &format!("{}@e2e.test", s.name("owner")), &s.name("own"))?;
    let (guest, _) = sign_in(&mut chrome, api, &format!("{}@e2e.test", s.name("guest")), &s.name("gst"))?;
    s.ok(
        "two people sign in, each in a browser of their own: WorkOS (the fake), a username, and a CLI key approved there",
        owner.id != guest.id && owner.username != guest.username,
        format!("{} @{} / {} @{}", owner.id, owner.username, guest.id, guest.username),
    );
    // one of the owner's apps, private: only the people in it may open it
    // (the owner alone: their agent is not in it either)
    let app = api.qualified(&owner.keys, &s.name("private"))?;
    let r = api.create_with(&owner.keys, json!({ "name": app, "template": "blank", "visibility": "members" }))?;
    anyhow::ensure!(r.status == 200, "making {app}: {r}");
    let secret = format!("the secret plan, {}", s.name("notes"));
    let r = api.signed(&owner.keys, "POST", &format!("/api/f/{app}/files"), Some(&json!({ "files": [{ "path": "notes.txt", "text": secret }] })))?;
    anyhow::ensure!(r.status == 200, "writing {app}'s notes: {r}");
    let notes = || api.signed(&owner.keys, "GET", &format!("/api/f/{app}/file?path=notes.txt"), None).map(|r| r.text).unwrap_or_default();

    // ---- 1. the owner makes a desktop (the platform's New fragment form),
    // a chat from it, and shares the chat by username in the sheet
    let desk_label = s.name("desk");
    let new = "form[action=\"/auth/new\"]";
    chrome.viewport(&home, 1440, 900, false)?;
    chrome.eval(
        &home,
        &format!("(() => {{ const f = document.querySelector({new:?}); f.querySelector('input[name=template][value=desktop]').checked = true; f.querySelector('input[name=label]').value = {desk_label:?}; return true; }})()"),
    )?;
    chrome.click(&home, &format!("{new} button"))?;
    let desk = home;
    let brand = format!("document.getElementById('brand')?.textContent === {:?}", format!("{}'s desktop", owner.username));
    let on_desk = chrome.until(&desk, &format!("location.host.startsWith({:?}) && {brand}", format!("{desk_label}--")), WAIT);
    chrome.click(&desk, "#new-chat")?;
    let opened = chrome.until(&desk, "document.querySelectorAll('#chats .row').length === 1 && !!document.querySelector('#frames iframe:not([hidden])')?.dataset.fragment", WAIT);
    let name = chrome.eval(&desk, "document.querySelector('#frames iframe:not([hidden])')?.dataset.fragment ?? ''")?.as_str().unwrap_or("").to_string();
    let chat_host = format!("{}--", label(&name));
    let agent_of = || -> Option<String> {
        let r = api.signed(&owner.keys, "GET", &format!("/api/f/{name}/members"), None).ok()?;
        r.body["members"].as_array()?.iter().find(|m| m["kind"] == "agent").and_then(|m| m["principal"].as_str().map(str::to_string))
    };
    let listening = || api.signed(&owner.keys, "GET", &format!("/api/f/{name}/subscriptions"), None).map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, Vec::len));
    let joined = !name.is_empty() && s.eventually(WAIT, || agent_of().is_some() && listening() == 1);
    let in_desk = |chrome: &mut Browser, js: &str| chrome.eval_in_frame(&desk, &chat_host, js).ok() == Some(json!(true));
    let ready = s.eventually(WAIT, || in_desk(&mut chrome, "document.getElementById('say')?.dataset.ready === '1'"));
    let mine = api.signed(&owner.keys, "GET", "/api/fragments", None)?;
    let owns = mine.body["fragments"].as_array().is_some_and(|a| a.iter().any(|f| f["name"] == name.as_str() && f["role"] == "owner"));
    s.ok(
        "the owner makes a desktop, and a chat from it (New chat): a fragment of theirs, open in the middle, their agent in it",
        on_desk && opened && owns && joined && ready,
        format!("{name:?}: desk {on_desk} opened {opened} owns {owns} agent {joined} ready {ready}: {}", shown(&mut chrome, &desk)),
    );
    let agent = agent_of().unwrap_or_default();
    let chat = Chat { api, owner: &owner.keys, name, agent };
    let agent_label = format!("{}'s agent", owner.username);

    let sheet = format!("{platform}/share/{}", chat.name);
    let popup = open_sheet(s, &mut chrome, &desk, &chat.name, &sheet)?;
    let invite = "form:has(input[name=action][value=invite]) button[data-arm]";
    let can_invite = chrome.until(&popup, &armed(invite), WAIT);
    chrome.eval(&popup, &format!("document.querySelector('input[name=username]').value = {:?}; true", guest.username))?;
    chrome.click(&popup, invite)?;
    let got_link = chrome.until(&popup, "!!document.getElementById('invite-link')?.value", WAIT);
    let link = chrome.eval(&popup, "document.getElementById('invite-link')?.value ?? ''")?.as_str().unwrap_or("").to_string();
    let invites = api.signed(&owner.keys, "GET", &format!("/api/f/{}/invites", chat.name), None)?;
    let pending: Vec<&Value> = invites.body["invites"].as_array().into_iter().flatten().filter(|i| i["invitee"] == guest.id.as_str()).collect();
    s.ok(
        "they share it with the guest by username, in the sheet the desktop opens (a window on the platform's origin): it answers the link to send",
        can_invite && got_link && link.starts_with(&format!("{platform}/join/{}?token=", chat.name)) && pending.len() == 1 && pending[0]["role"] == "viewer",
        format!("{link:?} {invites} | {}", shown(&mut chrome, &popup)),
    );
    chrome.close(popup)?;

    // ---- 2. the guest accepts at /join, and lands in the chat
    let theirs = chrome.open_in(&guest.browser, &link)?;
    chrome.front(&theirs)?;
    let join = "button[data-arm]";
    let can_join = chrome.until(&theirs, &armed(join), WAIT);
    chrome.click(&theirs, join)?;
    let landed = chrome.until(&theirs, &format!("location.host.startsWith({chat_host:?}) && document.title === 'Chat'"), WAIT);
    let ready = chrome.until(&theirs, "document.getElementById('say')?.dataset.ready === '1'", WAIT);
    let role = chat.role_of(&guest.id);
    s.ok(
        "the guest opens the link, accepts at /join, and lands in the chat, signed in on its origin, as a viewer",
        can_join && landed && ready && role.as_deref() == Some("viewer"),
        format!("{role:?} | {}", shown(&mut chrome, &theirs)),
    );

    // ---- 3. both see each other's messages arrive live, labeled
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text(WELCOME.into()), Reply::Text(GLAD.into())]);
    chrome.eval_in_frame(&desk, &chat_host, &send(OWNER_SAYS))?;
    chrome.front(&theirs)?;
    let guest_sees = chrome.until(&theirs, &from_other(OWNER_SAYS, &owner.username), WAIT);
    s.ok(
        "the guest sees the owner's message arrive live, labeled with the owner's username",
        guest_sees && chat.said_by(&owner.id, OWNER_SAYS),
        chrome.eval(&theirs, MESSAGES).unwrap_or_default(),
    );
    let welcomed = chat.answered(s, WELCOME).is_some();
    chrome.eval(&theirs, &send(GUEST_SAYS))?;
    chrome.front(&desk)?;
    let owner_sees = s.eventually(WAIT, || in_desk(&mut chrome, &from_other(GUEST_SAYS, &guest.username)));
    s.ok(
        "the owner sees the guest's arrive live, labeled with the guest's",
        owner_sees && chat.said_by(&guest.id, GUEST_SAYS),
        chrome.eval_in_frame(&desk, &chat_host, MESSAGES).unwrap_or_default(),
    );
    let glad = chat.answered(s, GLAD).is_some();
    chrome.front(&theirs)?;
    let answers_both = [WELCOME, GLAD].iter().all(|text| chrome.until(&theirs, &from_agent(text, &agent_label), WAIT));
    chrome.front(&desk)?;
    let answers_owner = [WELCOME, GLAD].iter().all(|text| s.eventually(WAIT, || in_desk(&mut chrome, &from_agent(text, &agent_label))));
    s.ok(
        "the owner's agent answers each of them, and both see its answers, labeled as the owner's agent",
        welcomed && glad && answers_both && answers_owner,
        format!("{welcomed} {glad} {answers_both} {answers_owner} | {}", chrome.eval(&theirs, MESSAGES).unwrap_or_default()),
    );

    // ---- 4. the guest asks the owner's agent about the chat (theirs to
    // read): it reads the chat's file for them, and answers there
    s.openrouter.script(&[Reply::Tools(vec![("platform__read_file".into(), json!({ "fragment": chat.name, "path": "fragment.json" }))]), Reply::Text(ABOUT.into())]);
    let asked = s.openrouter.chats().len();
    chrome.eval(&theirs, &send(ASK_CHAT))?;
    let turn = chat.answered(s, ABOUT).unwrap_or_default();
    let work = chat.work(s, &turn);
    let (asker, step) = starter_and_step(&work);
    let read = s.openrouter.chats()[asked..].iter().any(|c| c["messages"].to_string().contains("A chat, live for everyone in it"));
    s.ok(
        "the guest asks the owner's agent about the chat, which is within the guest's reach: it reads the chat's file for them, and answers there",
        !turn.is_empty() && asker == guest.id && step["tool"] == "platform__read_file" && step["ok"] == true && read,
        json!({ "turn": turn, "read": read, "work": work }),
    );
    chrome.front(&theirs)?;
    let grouped = chrome.until(&theirs, &group_above(&turn, ABOUT, "platform__read_file", false), WAIT) && chrome.until(&theirs, &from_agent(ABOUT, &agent_label), WAIT);
    s.ok("the guest's page shows its tool group, folded above the answer", grouped, chrome.eval(&theirs, MESSAGES).unwrap_or_default());

    // ---- 5. the guest asks it to read one of the owner's private apps
    s.openrouter.script(&[Reply::Tools(vec![("platform__read_file".into(), json!({ "fragment": app, "path": "notes.txt" }))]), Reply::Text(CANNOT.into())]);
    let asked = s.openrouter.chats().len();
    let theirs_to_open = api.status(&guest.keys, &app)?.status;
    chrome.eval(&theirs, &send(&format!("Read me the notes in {app}")))?;
    let turn = chat.answered(s, CANNOT).unwrap_or_default();
    let work = chat.work(s, &turn);
    let (asker, step) = starter_and_step(&work);
    let leaked = s.openrouter.chats()[asked..].iter().any(|c| c.to_string().contains(&secret));
    s.ok(
        "the guest asks it to read one of the owner's private apps (not the guest's to open): it cannot, and nothing of the app reaches the model",
        theirs_to_open == 403 && !turn.is_empty() && asker == guest.id && step["tool"] == "platform__read_file" && step["ok"] == false && !leaked && notes() == secret,
        json!({ "guest opens it": theirs_to_open, "leaked": leaked, "work": work }),
    );
    chrome.front(&theirs)?;
    let failed = chrome.until(&theirs, &group_above(&turn, CANNOT, "platform__read_file", true), WAIT);
    s.ok("the guest's page shows that step failed, above the answer", failed, chrome.eval(&theirs, MESSAGES).unwrap_or_default());
    chrome.screenshot(&theirs, &s.scratch.join("phase7-guest.png"))?;

    // ---- 6. the owner asks it to edit that same app
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__write_files".into(), json!({ "fragment": app, "files": [{ "path": "notes.txt", "text": EDITED }] }))]),
        Reply::Text(DONE.into()),
    ]);
    chrome.eval_in_frame(&desk, &chat_host, &send(&format!("Change the notes in {app} to say: {EDITED}")))?;
    let turn = chat.answered(s, DONE).unwrap_or_default();
    let work = chat.work(s, &turn);
    let (asker, step) = starter_and_step(&work);
    let edited = s.eventually(WAIT, || notes() == EDITED);
    let members = api.signed(&owner.keys, "GET", &format!("/api/f/{app}/members"), None)?;
    let owner_alone = members.body["members"].as_array().is_some_and(|m| m.len() == 1 && m[0]["principal"] == owner.id.as_str());
    s.ok(
        "the owner asks it to edit that same app: it does, for the owner (it is still not in the app)",
        !turn.is_empty() && asker == owner.id && step["tool"] == "platform__write_files" && step["ok"] == true && edited && owner_alone,
        json!({ "notes": notes(), "members": members.body, "work": work }),
    );
    chrome.front(&desk)?;
    let grouped = s.eventually(WAIT, || in_desk(&mut chrome, &group_above(&turn, DONE, "platform__write_files", false)));
    s.ok("the owner's page shows its tool group above the answer", grouped, chrome.eval_in_frame(&desk, &chat_host, MESSAGES).unwrap_or_default());
    chrome.screenshot(&desk, &s.scratch.join("phase7-owner.png"))?;

    // ---- 7. the owner removes the guest in the sheet, opened again from the desktop
    let badge = format!(
        "[...document.querySelectorAll('#chats .row')].find(r => r.querySelector('.label')?.textContent === {:?})?.querySelector('.shared')?.textContent === '1'",
        label(&chat.name)
    );
    chrome.front(&desk)?;
    let badged = chrome.until(&desk, &badge, WAIT);
    s.ok("the owner's desktop shows the chat shared with one person", badged, chrome.eval(&desk, "document.getElementById('chats')?.innerHTML ?? ''").unwrap_or_default());
    let popup = open_sheet(s, &mut chrome, &desk, &chat.name, &sheet)?;
    let member = format!("input[name=member][value={:?}]", guest.id);
    let remove = format!("form:has(input[name=action][value=remove]):has({member}) button[data-arm]");
    let can_remove = chrome.until(&popup, &armed(&remove), WAIT);
    chrome.click(&popup, &remove)?;
    let gone = chrome.until(&popup, &format!("document.readyState === 'complete' && !!document.querySelector('ul.people') && !document.querySelector({member:?})"), WAIT);
    s.ok(
        "the owner removes the guest in the sheet",
        can_remove && gone && chat.role_of(&guest.id).is_none(),
        shown(&mut chrome, &popup),
    );
    let said = "document.getElementById('banner-text')?.textContent ?? ''";
    chrome.front(&theirs)?;
    let closed = chrome.until(&theirs, &format!("!document.getElementById('banner').hidden && ({said}).includes('access to this chat changed')"), WAIT);
    s.ok("the guest's socket closes: their page says their access changed", closed, chrome.eval(&theirs, said).unwrap_or_default());
    let fetched = chrome.eval(&theirs, "fetch(location.origin + '/', { credentials: 'same-origin', cache: 'no-store' }).then(r => r.status, e => 'refused ' + e.name)")?;
    let signed = api.signed(&guest.keys, "GET", &format!("/api/f/{}/channels/chat", chat.name), None)?;
    chrome.reload(&theirs)?;
    let refused = chrome.until(&theirs, "!!document.body?.innerText.includes('forbidden')", WAIT);
    s.ok(
        "and their next request is a 403: their browser's, their key's, and the page reloaded",
        fetched == json!(403) && signed.status == 403 && refused,
        format!("{fetched} / {signed} / {}", shown(&mut chrome, &theirs)),
    );
    s.openrouter.clear_script();
    Ok(())
}
