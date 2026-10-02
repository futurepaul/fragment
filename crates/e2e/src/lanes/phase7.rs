//! Phase 7's acceptance (ROADMAP, phase 7; docs/phase-7.md, slice E): the
//! whole human flow in one run, in headless Chrome, with two people. Each
//! is in a browser of their own (a browser context) and signs in as a
//! person does: through WorkOS (the fake), a username taken on the
//! platform's page, and a CLI key approved on its page (the harness's
//! hands for what it reads back). The node is shaped as fragment.club is.
//!
//! 0. Each person's first sign-in lands on their desktop, their home, which
//!    the platform made then (docs/one-home.md, decision 7).
//! 1. The owner makes a chat from their desktop (New chat, answered by
//!    their agent) and shares it with the guest by username, in the share
//!    sheet the desktop opens.
//! 2. The guest accepts at `/join` and lands in the chat.
//! 3. Both see each other's messages arrive live, each labeled with its
//!    sender (the owner's agent's answers too).
//! 4. The guest asks the owner's agent about the chat, which is within the
//!    guest's reach: it answers there, its tool group shown above it.
//! 5. The guest asks it to read one of the owner's private apps: it cannot.
//! 6. The owner asks it to add to that same app (a list): it does.
//! 7. The owner removes the guest in the share sheet: the guest's socket
//!    closes, and their next request is a 403.
//! 8. A rewritten desktop cannot share without the sheet's click. The share
//!    lane proves it and it is not repeated here: "a rewritten desktop
//!    cannot read the sheet …", "nor fetch the signed API …", "(the signed
//!    API takes no session …)", "a rewritten desktop that frames the sheet
//!    gets a frame without it", "a window it opens on the sheet is severed
//!    from it …", "it takes nothing from its URL …", and "a form sent before
//!    its buttons arm is refused …".
//! 9. Phase 5 (docs/one-home.md): the owner's Hermes answers a chat their
//!    desktop makes for it (New chat, Hermes); they invite the guest from
//!    the chat's header, and the guest's first message is answered by name.
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
const DONE: &str = "Done: your list says the plan is on.";

/// Runs on a node restarted with the platform and the fragments on two domains
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
    format!("[...document.querySelectorAll('.msg.user.other')].some(m => m.querySelector('.bubble')?.textContent === {text:?} && !!m.querySelector('.who .face') && m.querySelector('.who')?.textContent.endsWith({who:?}))")
}

/// Whether a chat's page shows an agent's answer `text`, labeled `who`.
fn from_agent(text: &str, who: &str) -> String {
    format!("[...document.querySelectorAll('.msg.agent')].some(m => m.querySelector('.md')?.textContent.includes({text:?}) && !!m.querySelector('.who .face.agent') && m.querySelector('.who')?.textContent === {who:?})")
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
    // taken, they are home: their desktop, made now, signed in on its origin
    let brand = format!("document.getElementById('brand')?.textContent === {:?}", format!("{username}'s desktop"));
    let landed = format!("location.host.startsWith('desktop--') && {brand}");
    anyhow::ensure!(chrome.until(&home, &landed, WAIT), "{email} took {username} and did not land on their desktop: {}", shown(chrome, &home));
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

/// New chat's menu, open: who answers the new chat. A click that a late
/// resize or a lost focus closed again (the desktop closes its menus so)
/// is made again.
fn ask_who_answers(s: &Suite, chrome: &mut Browser, desk: &Page) -> bool {
    let open = |chrome: &mut Browser| chrome.eval(desk, "!document.getElementById('menu').hidden && !!document.querySelector('#menu [data-answers]')").ok() == Some(json!(true));
    s.eventually(WAIT, || {
        if !open(chrome) {
            let _ = chrome.click(desk, "#new-chat");
            std::thread::sleep(Duration::from_millis(400));
        }
        open(chrome)
    })
}

/// The desktop opens the chat's share sheet as a person does, a click each
/// on its row's … menu and on Share…: a dialog on the desktop, framing the
/// platform's sheet at `sheet`, signed in through `__share`.
fn open_sheet(s: &Suite, chrome: &mut Browser, desk: &Page, chat: &str, sheet: &str) -> Result<()> {
    chrome.front(desk)?;
    chrome.click(desk, &format!(".more[data-fragment={chat:?}]"))?;
    anyhow::ensure!(chrome.until(desk, "!document.getElementById('menu').hidden", WAIT), "the chat's … menu did not open");
    chrome.click(desk, "#menu-share")?;
    let opened = chrome.until(desk, "document.getElementById('sheet').open", WAIT)
        && s.eventually(WAIT, || chrome.eval_in_frame(desk, sheet, "document.body.innerText.includes('General access')").ok() == Some(json!(true)));
    anyhow::ensure!(opened, "Share… showed no sheet at {sheet} in the desktop: {}", in_sheet_text(chrome, desk, sheet));
    Ok(())
}

/// The sheet in the desktop's dialog: where it is and what it says, for a FAIL's detail.
fn in_sheet_text(chrome: &mut Browser, desk: &Page, sheet: &str) -> String {
    let v = chrome.eval_in_frame(desk, sheet, "location.href + ' | ' + (document.body?.innerText ?? '').slice(-600)");
    v.map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_else(|e| format!("{e:#}"))
}

/// Done in the sheet: the desktop's dialog closes.
fn close_sheet(chrome: &mut Browser, desk: &Page, sheet: &str) -> Result<()> {
    chrome.eval_in_frame(desk, sheet, "(document.querySelector('button[data-done]').click(), true)")?;
    anyhow::ensure!(chrome.until(desk, "!document.getElementById('sheet').open", WAIT), "Done did not close the sheet's dialog");
    Ok(())
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
    let r = api.create_with(&owner.keys, json!({ "name": app, "template": "todo", "visibility": "members" }))?;
    anyhow::ensure!(r.status == 200, "making {app}: {r}");
    let secret = format!("the secret plan, {}", s.name("notes"));
    let r = api.signed(&owner.keys, "POST", &format!("/api/f/{app}/files"), Some(&json!({ "files": [{ "path": "notes.txt", "text": secret }] })))?;
    anyhow::ensure!(r.status == 200, "writing {app}'s notes: {r}");
    let notes = || api.signed(&owner.keys, "GET", &format!("/api/f/{app}/file?path=notes.txt"), None).map(|r| r.text).unwrap_or_default();

    // ---- 0. each landed on their desktop as they took their username (sign_in)
    let homes: Vec<Value> = [&owner, &guest].iter().map(|p| api.status(&p.keys, &format!("desktop.{}", p.username)).map(|r| r.body).unwrap_or_default()).collect();
    s.ok(
        "each person's first sign-in lands on their desktop, their home, made then: theirs alone (members only), showing their fragments inside it with no visit to the share sheet",
        homes.iter().all(|st| st["visibility"] == "members" && st["frame"] == json!(true)),
        json!(homes),
    );

    // ---- 1. the owner makes a chat from their desktop, and shares the
    // chat by username in the sheet
    let desk = home;
    chrome.viewport(&desk, 1440, 900, false)?;
    chrome.front(&desk)?;
    // the desktop closes a menu as its window resizes or loses focus: the new size first
    anyhow::ensure!(chrome.until(&desk, "innerWidth === 1440", WAIT), "the desktop did not take its new size");
    anyhow::ensure!(ask_who_answers(s, &mut chrome, &desk), "New chat did not ask who answers: {}", shown(&mut chrome, &desk));
    chrome.click(&desk, "#menu [data-answers=agent]")?;
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
        "the owner makes a chat from their desktop (New chat, their agent answering): a fragment of theirs, open in the middle, their agent in it",
        opened && owns && joined && ready,
        format!("{name:?}: opened {opened} owns {owns} agent {joined} ready {ready}: {}", shown(&mut chrome, &desk)),
    );
    let agent = agent_of().unwrap_or_default();
    let chat = Chat { api, owner: &owner.keys, name, agent };
    let agent_label = format!("{}'s agent", owner.username);

    let sheet = format!("{platform}/share/{}", chat.name);
    open_sheet(s, &mut chrome, &desk, &chat.name, &sheet)?;
    let in_sheet = |chrome: &mut Browser, js: &str| chrome.eval_in_frame(&desk, &sheet, js).ok() == Some(json!(true));
    let invite = "form:has(input[name=action][value=invite]) button[data-arm]";
    let can_invite = s.eventually(WAIT, || in_sheet(&mut chrome, &armed(invite)));
    chrome.eval_in_frame(&desk, &sheet, &format!("document.querySelector('input[name=username]').value = {:?}; document.querySelector({invite:?}).click(); true", guest.username))?;
    let got_link = s.eventually(WAIT, || in_sheet(&mut chrome, "!!document.getElementById('invite-link')?.value"));
    let link = chrome.eval_in_frame(&desk, &sheet, "document.getElementById('invite-link')?.value ?? ''")?.as_str().unwrap_or("").to_string();
    let invites = api.signed(&owner.keys, "GET", &format!("/api/f/{}/invites", chat.name), None)?;
    let pending: Vec<&Value> = invites.body["invites"].as_array().into_iter().flatten().filter(|i| i["invitee"] == guest.id.as_str()).collect();
    s.ok(
        "they share it with the guest by username, in the sheet the desktop opens (a dialog on it, the platform's sheet signed in through __share): it answers the link to send",
        can_invite && got_link && link.starts_with(&format!("{platform}/join/{}?token=", chat.name)) && pending.len() == 1 && pending[0]["role"] == "viewer",
        format!("{link:?} {invites} | {}", in_sheet_text(&mut chrome, &desk, &sheet)),
    );
    close_sheet(&mut chrome, &desk, &sheet)?;

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
    // to its owner, their own agent is "Your agent"
    let answers_owner = [WELCOME, GLAD].iter().all(|text| s.eventually(WAIT, || in_desk(&mut chrome, &from_agent(text, "Your agent"))));
    s.ok(
        "the owner's agent answers each of them, and both see its answers, labeled as the owner's agent (to the owner, as theirs)",
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

    // ---- 6. the owner asks it to add to that same app, a list
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__call".into(), json!({ "fragment": app, "operation": "add", "input": { "text": EDITED } }))]),
        Reply::Text(DONE.into()),
    ]);
    chrome.eval_in_frame(&desk, &chat_host, &send(&format!("Add to my list in {app}: {EDITED}")))?;
    let turn = chat.answered(s, DONE).unwrap_or_default();
    let work = chat.work(s, &turn);
    let (asker, step) = starter_and_step(&work);
    let list = || api.op(&owner.keys, &app, "list", "q", json!({})).map(|r| r.body["result"]["todos"].to_string()).unwrap_or_default();
    let added = s.eventually(WAIT, || list().contains(EDITED));
    let members = api.signed(&owner.keys, "GET", &format!("/api/f/{app}/members"), None)?;
    let owner_alone = members.body["members"].as_array().is_some_and(|m| m.len() == 1 && m[0]["principal"] == owner.id.as_str());
    s.ok(
        "the owner asks it to add to that same app: it does, for the owner (it is still not in the app)",
        !turn.is_empty() && asker == owner.id && step["tool"] == "platform__call" && step["ok"] == true && added && owner_alone,
        json!({ "list": list(), "members": members.body, "work": work }),
    );
    chrome.front(&desk)?;
    let grouped = s.eventually(WAIT, || in_desk(&mut chrome, &group_above(&turn, DONE, "platform__call", false)));
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
    open_sheet(s, &mut chrome, &desk, &chat.name, &sheet)?;
    let member = format!("input[name=member][value={:?}]", guest.id);
    // their role's menu ends in Remove access, sent as it is chosen
    let menu = format!("form:has({member}) select[name=role]");
    let can_remove = s.eventually(WAIT, || in_sheet(&mut chrome, &armed(&menu)));
    chrome.eval_in_frame(&desk, &sheet, &format!("(s => {{ s.value = 'remove'; s.dispatchEvent(new Event('change')); return true; }})(document.querySelector({menu:?}))"))?;
    let gone = s.eventually(WAIT, || {
        in_sheet(&mut chrome, &format!("document.readyState === 'complete' && !!document.querySelector('ul.people') && !document.querySelector({member:?})"))
    });
    s.ok(
        "the owner removes the guest in the sheet",
        can_remove && gone && chat.role_of(&guest.id).is_none(),
        in_sheet_text(&mut chrome, &desk, &sheet),
    );
    close_sheet(&mut chrome, &desk, &sheet)?;
    let said = "document.getElementById('banner-text')?.textContent ?? ''";
    chrome.front(&theirs)?;
    let closed = chrome.until(&theirs, &format!("!document.getElementById('banner').hidden && ({said}).includes('access to this chat changed')"), WAIT);
    s.ok("the guest's socket closes: their page says their access changed", closed, chrome.eval(&theirs, said).unwrap_or_default());
    let fetched = chrome.eval(&theirs, "fetch(location.origin + '/', { credentials: 'same-origin', cache: 'no-store' }).then(r => r.status, e => 'refused ' + e.name)")?;
    let signed = api.signed(&guest.keys, "GET", &format!("/api/f/{}/channels/chat", chat.name), None)?;
    chrome.reload(&theirs)?;
    let refused = chrome.until(&theirs, "document.contentType === 'text/html' && !!document.body?.innerText.includes(\"You don't have access to\")", WAIT);
    s.ok(
        "and their next request is a 403: their browser's, their key's, and the page reloaded",
        fetched == json!(403) && signed.status == 403 && refused,
        format!("{fetched} / {signed} / {}", shown(&mut chrome, &theirs)),
    );
    s.openrouter.clear_script();

    // ---- 9. phase 5: a chat the owner's Hermes answers, the guest invited from its header
    hermes_chat(s, &mut chrome, api, &owner, &guest, &desk)
}

/// Phase 5's acceptance (docs/one-home.md): the owner makes a Hermes (the
/// hermes template, on the sandcastle fake's node, whose gateway answers
/// as Hermes does: `echo: [<name>] <text>`); their desktop's New chat
/// offers it, and makes a chat it answers; Invite, in the chat's header,
/// opens its share sheet, where they invite the guest by username; the
/// guest joins, writes, and Hermes answers them by name, live on both
/// pages.
fn hermes_chat(s: &mut Suite, chrome: &mut Browser, api: &Api, owner: &Person, guest: &Person, desk: &Page) -> Result<()> {
    let hermes = api.qualified(&owner.keys, &s.name("herm"))?;
    let r = api.create_with(&owner.keys, json!({ "name": hermes, "template": "hermes" }))?;
    anyhow::ensure!(r.status == 200, "making {hermes}: {r}");
    // ready once it answers its own chat: its computer's identity a member there
    let computer_of = |name: &str| -> Option<String> {
        let r = api.signed(&owner.keys, "GET", &format!("/api/f/{name}/members"), None).ok()?;
        r.body["members"].as_array()?.iter().find(|m| m["kind"] == "computer").and_then(|m| m["principal"].as_str().map(str::to_string))
    };
    let ready = s.eventually(WAIT, || computer_of(&hermes).is_some());
    chrome.front(desk)?;
    let row = format!("!!document.querySelector('#computers .row[data-key={:?}]')", format!("app:{hermes}"));
    let listed = chrome.until(desk, &row, WAIT);
    ask_who_answers(s, chrome, desk);
    let offered = format!("[...document.querySelectorAll('#menu [data-answers=computer]')].map(b => b.dataset.computer).join() === {hermes:?}");
    let offers = chrome.until(desk, &offered, WAIT);
    s.ok(
        "the owner's Hermes is made, listed among their computers, and New chat offers it",
        ready && listed && offers,
        format!("ready {ready} listed {listed} offers {offers}: {}", chrome.eval(desk, "document.getElementById('menu').innerText").unwrap_or_default()),
    );
    let before = chrome.eval(desk, "document.querySelector('#frames iframe:not([hidden])')?.dataset.fragment ?? ''")?;
    chrome.click(desk, "#menu [data-answers=computer]")?;
    let opened = chrome.until(desk, &format!("(document.querySelector('#frames iframe:not([hidden])')?.dataset.fragment ?? '') !== {before} && document.getElementById('chat-title').textContent.startsWith('chat-')"), WAIT);
    let name = chrome.eval(desk, "document.querySelector('#frames iframe:not([hidden])')?.dataset.fragment ?? ''")?.as_str().unwrap_or("").to_string();
    let manifest = api.signed(&owner.keys, "GET", &format!("/api/f/{name}/manifest"), None)?;
    let joined = s.eventually(WAIT, || computer_of(&name).is_some() && computer_of(&name) == computer_of(&hermes));
    s.ok(
        "Hermes there makes a chat it answers: its fragment.json names the owner's Hermes, which joins it",
        opened && manifest.body["agent"] == json!({ "channel": "chat", "computer": hermes }) && joined,
        format!("{name:?} {opened} {joined} {manifest}"),
    );
    let chat_host = format!("{}--", label(&name));
    let platform = api.base.clone();
    let sheet = format!("{platform}/share/{name}");
    chrome.click(desk, "#invite")?;
    let opened = chrome.until(desk, "document.getElementById('sheet').open", WAIT)
        && s.eventually(WAIT, || chrome.eval_in_frame(desk, &sheet, "document.body.innerText.includes('General access')").ok() == Some(json!(true)));
    let in_sheet = |chrome: &mut Browser, js: &str| chrome.eval_in_frame(desk, &sheet, js).ok() == Some(json!(true));
    let invite = "form:has(input[name=action][value=invite]) button[data-arm]";
    let can_invite = s.eventually(WAIT, || in_sheet(chrome, &armed(invite)));
    chrome.eval_in_frame(desk, &sheet, &format!("document.querySelector('input[name=username]').value = {:?}; document.querySelector({invite:?}).click(); true", guest.username))?;
    let got_link = s.eventually(WAIT, || in_sheet(chrome, "!!document.getElementById('invite-link')?.value"));
    let link = chrome.eval_in_frame(desk, &sheet, "document.getElementById('invite-link')?.value ?? ''")?.as_str().unwrap_or("").to_string();
    s.ok(
        "Invite, in the chat's header, opens its share sheet, where the owner invites the guest by username",
        opened && can_invite && got_link && link.starts_with(&format!("{platform}/join/{name}?token=")),
        format!("{link:?} | {}", in_sheet_text(chrome, desk, &sheet)),
    );
    close_sheet(chrome, desk, &sheet)?;

    let theirs = chrome.open_in(&guest.browser, &link)?;
    chrome.front(&theirs)?;
    let join = "button[data-arm]";
    let can_join = chrome.until(&theirs, &armed(join), WAIT);
    chrome.click(&theirs, join)?;
    let landed = chrome.until(&theirs, &format!("location.host.startsWith({chat_host:?}) && document.getElementById('say')?.dataset.ready === '1'"), WAIT);
    let hello = "Hello Hermes, it's the guest";
    chrome.eval(&theirs, &send(hello))?;
    let answer = format!("echo: [{}] {hello}", guest.username);
    let by_hermes = format!(
        "[...document.querySelectorAll('.msg.agent:not(.streaming)')].some(m => m.querySelector('.md')?.textContent === {answer:?} && !!m.querySelector('.who .face.hermes') && m.querySelector('.who')?.textContent.endsWith('Hermes'))"
    );
    let guest_sees = chrome.until(&theirs, &by_hermes, WAIT);
    s.ok(
        "the guest joins, writes, and the owner's Hermes answers them by name, labeled as Hermes",
        can_join && landed && guest_sees,
        format!("{can_join} {landed} (its gateway dialed {} times) | {}", s.sandcastle.relay_dials(), chrome.eval(&theirs, MESSAGES).unwrap_or_default()),
    );
    chrome.front(desk)?;
    let owner_sees = s.eventually(WAIT, || chrome.eval_in_frame(desk, &chat_host, &by_hermes).ok() == Some(json!(true)));
    s.ok("and the owner sees it live in their desktop", owner_sees, chrome.eval_in_frame(desk, &chat_host, MESSAGES).unwrap_or_default());
    chrome.close(theirs)?;
    // its Hermes goes with its fragment: the node is as the lanes after expect it
    let r = api.signed(&owner.keys, "DELETE", &format!("/api/f/{hermes}"), None)?;
    let gone = r.status == 200 && s.eventually(WAIT, || !s.sandcastle.computers().values().any(|c| c.spec["service"]["env"]["HERMES_DASHBOARD"] == "1"));
    anyhow::ensure!(gone, "{hermes}, deleted, left its computer on the node: {r}");
    Ok(())
}
