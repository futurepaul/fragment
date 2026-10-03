//! The blessed chat template (`templates/chat`, docs/chat-records.md) in a
//! browser, against the stub image's scripted agent (`images/bridge`'s
//! `script` runtime) on a computer under `wrangler dev` with Docker.
//!
//! A person's computer runs an agent fragment that is in a chat whose files
//! are the template's. The page's own routes answer as the API's rules
//! say: an agent's name (`__people`), the members (`__members`), and an
//! editor's upload (`PUT __blob/<sha256>`). Then the owner, signed in on
//! the chat's page, types and presses Enter, and sees the agent's draft
//! live, then its reply; a tool step as a card; an approval card they
//! answer with its button, which another member sees but may not press;
//! Stop, which ends a slow turn; a picture they attach, in their message
//! and as the chat's blob; a reply's file. At a phone's width nothing
//! scrolls sideways. Screenshots (desktop light and dark, phone) stay in
//! the run's scratch (`chat/`).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::computers::{agent_replies, turn_of, work_of, AGENT_JSON};
use super::jobs::records;
use super::signin::site_cookie;
use crate::api::{Api, Call};
use crate::browser::{Browser, Page};
use crate::Suite;

/// A start of the stub and its bridge's first follow (as the computers lane's).
const WAKE: Duration = Duration::from_secs(90);
/// A turn of the scripted agent, once awake, as a page sees it.
const TURN: Duration = Duration::from_secs(30);

/// `cond`, as a page's JavaScript, true within `TURN`.
fn shows(chrome: &mut Browser, page: &Page, cond: &str) -> bool {
    chrome.until(page, cond, TURN)
}

/// The template's files, as a CLI sync would commit them.
fn template_files() -> Vec<(&'static str, Option<&'static [u8]>)> {
    fragment_templates::CHAT.iter().map(|(path, bytes)| (*path, Some(*bytes))).collect()
}

/// A JS string literal.
fn js(s: &str) -> String {
    serde_json::to_string(s).expect("a string encodes")
}

pub fn chat(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("chat") {
        return Ok(());
    }
    let owner = api.person()?;
    let member = api.person()?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && computer.starts_with("computer:"), "making the computer: {r}");

    // an agent fragment on the computer, and a chat whose files are the template's
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{computer}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && identity.starts_with("id:"), "assigning the agent: {r}");
    let chat_name = s.named(api, &owner, "talk")?;
    let chat = s.create(api, &owner, &chat_name)?;
    s.commit(&chat, &template_files());
    s.deploy(&chat);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("the agent joins the chat on the template", r.status == 200, &r);
    let joined = json!({ "id": "joined-chat", "body": { "kind": "joined", "fragment": chat_name } });
    api.signed(&owner, "POST", &format!("/api/f/{agent_name}/channels/tasks"), Some(&joined))?;
    let member_id = api.identity(&member)?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{member_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a viewer: {r}");

    // ---- the page's own routes, as a browser signed in there calls them
    let owner_session = api.sign_in(&Api::email_of(&owner))?;
    let member_session = api.sign_in(&Api::email_of(&member))?;
    let owner_site = format!("fragment_site={}", site_cookie(api, &owner_session, &chat_name)?);
    let member_site = format!("fragment_site={}", site_cookie(api, &member_session, &chat_name)?);
    let served = s.eventually(Duration::from_secs(20), || api.page(&chat_name, "", Some(&owner_site)).is_ok_and(|r| r.status == 200 && r.text.contains("./chat.js")));
    s.ok("the chat serves the template's page", served, "");
    let label = agent_name.split('.').next().unwrap_or("").to_string();
    let r = api.page(&chat_name, &format!("__people?id={identity}"), Some(&owner_site))?;
    let profile = &r.body["profiles"][identity.as_str()];
    s.ok(
        "__people names an agent made from an agent fragment: its label, and its fragment",
        r.status == 200 && profile["kind"] == "agent" && profile["name"] == label.as_str() && profile["fragment"] == agent_name.as_str(),
        &r,
    );
    let r = api.page(&chat_name, "__members", Some(&owner_site))?;
    let listed = r.body["members"].as_array().cloned().unwrap_or_default();
    s.ok(
        "__members lists the chat's members, its agent among them",
        r.status == 200 && listed.iter().any(|m| m["principal"] == identity.as_str() && m["kind"] == "agent" && m["role"] == "editor"),
        &r,
    );
    let r = api.page(&chat_name, "__members", Some(&member_site))?;
    s.ok("a viewer lists them too", r.status == 200, &r);
    let r = api.page(&chat_name, "__members", None)?;
    s.ok("and no one who may not see the chat", r.status == 401, &r);
    let bytes = b"a page's upload".to_vec();
    let sha = hex::encode(Sha256::digest(&bytes));
    let put = |sha: &str, cookie: Option<&str>, extra: Vec<(&'static str, String)>| {
        api.call(Call { method: "PUT", url: api.site_url(&chat_name, &format!("__blob/{sha}")), body: Some(bytes.clone()), content_type: Some("text/plain"), cookie: cookie.map(str::to_string), extra, ..Call::default() })
    };
    let r = put(&sha, Some(&owner_site), vec![])?;
    s.ok("an editor's page uploads a blob at __blob/<sha256>", r.status == 200 && r.body["sha"] == sha.as_str() && r.body["stored"] == true, &r);
    let again = put(&sha, Some(&owner_site), vec![])?;
    s.ok("the same upload again stores nothing new", again.status == 200 && again.body["stored"] == false, &again);
    let r = api.page(&chat_name, &format!("__blob/{sha}"), Some(&owner_site))?;
    s.ok("and the page reads it there", r.status == 200 && r.bytes == bytes, &r);
    let wrong = hex::encode(Sha256::digest(b"something else"));
    let r = put(&wrong, Some(&owner_site), vec![])?;
    s.ok("bytes that are not what their hash says are refused", r.status == 400 && r.text.contains("hash to"), &r);
    let r = put(&sha, Some(&member_site), vec![])?;
    s.ok("a viewer may not upload", r.status == 403, &r);
    let r = put(&sha, None, vec![])?;
    s.ok("nor anyone not signed in", r.status == 401, &r);
    let other_page = vec![("sec-fetch-site", "same-site".to_string()), ("sec-fetch-mode", "cors".to_string()), ("sec-fetch-dest", "empty".to_string())];
    let r = put(&sha, Some(&owner_site), other_page)?;
    s.ok("another fragment's page cannot upload with the owner's cookie", r.status == 401, &r);

    // ---- awake, the guest follows the chat as the agent
    let r = api.signed(&owner, "POST", &format!("/api/computers/{computer}/wake"), Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "waking the computer: {r}");
    let subscribed = s.eventually(WAKE, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    s.ok("the computer's guest follows the chat", subscribed, "");

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the chat section (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let shots = s.dir("chat");
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &owner_session)?;
    let page = chrome.open(&api.site_url(&chat_name, "__signin?return=/"))?;
    chrome.viewport(&page, 1280, 860, false)?;
    let ready = chrome.until(&page, "document.getElementById('say')?.dataset.ready === '1'", TURN);
    let role = chrome.eval(&page, "import('./__fragment.js').then((f) => f.me()).then((m) => m.role)").unwrap_or_default();
    s.ok("the owner, signed in on the chat's page, may write", ready && role == "owner" && chrome.eval(&page, "!document.getElementById('text').disabled")? == true, &role);
    let named = format!("document.title === {} && document.getElementById('text').placeholder === {}", js(&capital(&label)), js(&format!("Message {}", capital(&label))));
    s.ok("it names the chat by its agent, and the composer says whom it messages", shows(&mut chrome, &page, &named), chrome.eval(&page, "[document.title, document.getElementById('text').placeholder]")?);
    s.ok("a new chat shows its empty start", chrome.eval(&page, "!!document.querySelector('.empty .agent-avatar.large')")? == true, "");

    // the page notes every draft it shows, as a person would see them
    chrome.eval(
        &page,
        "(() => { window.__drafts = []; new MutationObserver(() => { for (const d of document.querySelectorAll('.msg.agent.streaming .md')) { const t = d.textContent; if (t && window.__drafts[window.__drafts.length - 1] !== t) window.__drafts.push(t); } }).observe(document.getElementById('messages'), { childList: true, subtree: true, characterData: true }); return true; })()",
    )?;
    let say = |chrome: &mut Browser, text: &str| -> Result<()> {
        chrome.click(&page, "#text")?;
        chrome.type_text(&page, text)?;
        chrome.press(&page, "Enter", false)
    };
    let mine = |text: &str| format!("[...document.querySelectorAll('.msg.user.mine .bubble')].some((b) => b.textContent === {})", js(text));
    let replied = |text: &str| format!("[...document.querySelectorAll('.msg.agent:not(.streaming) .md')].some((m) => m.textContent.includes({}))", js(text));

    // typed, Enter: the message, the agent's draft live, then its reply
    let first = "hello there, slowly";
    chrome.click(&page, "#text")?;
    chrome.type_text(&page, "hello there,")?;
    chrome.press(&page, "Enter", true)?;
    let lines = chrome.eval(&page, "document.getElementById('text').value")?;
    s.ok("Shift+Enter adds a line, and sends nothing", lines == "hello there,\n" && chrome.eval(&page, "document.querySelectorAll('.msg.user').length")? == 0, &lines);
    chrome.eval(&page, "(() => { const t = document.getElementById('text'); t.value = ''; t.dispatchEvent(new Event('input')); return true; })()")?;
    say(&mut chrome, first)?;
    s.ok("typed and sent with Enter, the message shows as the owner's", shows(&mut chrome, &page, &mine(first)), "");
    s.ok("and the composer is empty again", chrome.eval(&page, "document.getElementById('text').value")? == "", "");
    let r = records(api, &owner, &chat_name, "chat");
    let sent = r.iter().find(|x| x["body"]["text"] == first);
    s.ok("the page posted it to chat as the owner, `{text}`", sent.is_some_and(|x| x["principal"] == api.identity(&owner).unwrap_or_default().as_str()), json!(r));
    let answered = shows(&mut chrome, &page, &format!("{} && !document.querySelector('.msg.agent.streaming')", replied(first)));
    let reply = chrome.eval(&page, &format!("[...document.querySelectorAll('.msg.agent .md')].map((m) => m.textContent).find((t) => t.includes({})) ?? ''", js(first)))?;
    let reply = reply.as_str().unwrap_or("").to_string();
    let drafts = chrome.eval(&page, "window.__drafts")?;
    // each draft the whole text so far: a beginning of the reply
    let drafted = drafts.as_array().is_some_and(|d| !d.is_empty() && d.iter().all(|t| t.as_str().is_some_and(|t| reply.starts_with(t) && t.len() < reply.len())));
    s.ok("the agent's draft shows live, as it writes", drafted, json!({ "drafts": drafts, "reply": reply }));
    s.ok("then its reply replaces the draft", answered && reply.starts_with("echo:"), chrome.eval(&page, "document.getElementById('messages').innerText")?);
    s.ok("the agent's reply is under its name", chrome.eval(&page, &format!("[...document.querySelectorAll('.msg.agent .who')].every((w) => w.textContent === {})", js(&capital(&label))))? == true, "");

    // a tool step, as a card
    say(&mut chrome, "tool please")?;
    let stepped = shows(&mut chrome, &page, &format!("[...document.querySelectorAll('details.tools')].some((d) => d.textContent.includes('search') && d.textContent.includes('3 results')) && {}", replied("tool please")));
    s.ok("a tool step shows as a card before the reply", stepped, chrome.eval(&page, "document.getElementById('messages').innerText")?);

    // an @mention: the composer offers the chat's agents, and the message is `to` the one picked
    chrome.click(&page, "#text")?;
    chrome.type_text(&page, &format!("@{}", &label[..3]))?;
    let offered = shows(&mut chrome, &page, &format!("!document.getElementById('mentions').hidden && document.getElementById('mentions').textContent === {}", js(&capital(&label))));
    chrome.press(&page, "Tab", false)?;
    chrome.type_text(&page, "are you there")?;
    let typed = chrome.eval(&page, "document.getElementById('text').value")?;
    s.ok("typing @ offers the chat's agents, and Tab picks one", offered && typed == format!("@{label} are you there").as_str(), &typed);
    chrome.press(&page, "Enter", false)?;
    let addressed = format!("@{label} are you there");
    s.ok("the agent answers it", shows(&mut chrome, &page, &replied(&addressed)), "");
    let r = records(api, &owner, &chat_name, "chat");
    let to = r.iter().find(|x| x["body"]["text"] == addressed.as_str()).map(|x| x["body"]["to"].clone()).unwrap_or_default();
    s.ok("its record is `to` the agent its @mention named", to == json!([identity]), &to);

    // an approval: a card with buttons for the owner, which a member sees but may not press
    say(&mut chrome, "approve this")?;
    let card = "document.querySelector('.prompt:not(.closed) button[data-option=\"once\"]')";
    s.ok("an approval shows as a card with its buttons, enabled for the owner", shows(&mut chrome, &page, &format!("{card} && !{card}.disabled")), chrome.eval(&page, "document.getElementById('messages').innerText")?);
    let elsewhere = chrome.another_context()?;
    chrome.set_cookie_in(&elsewhere, &format!("{}/", api.base), "fragment_session", &member_session)?;
    let theirs = chrome.open_in(&elsewhere, &api.site_url(&chat_name, "__signin?return=/"))?;
    chrome.viewport(&theirs, 1280, 860, false)?;
    let seen = chrome.until(&theirs, "document.querySelectorAll('.prompt:not(.closed) .options button').length === 2", TURN);
    let pressable = chrome.eval(&theirs, "[...document.querySelectorAll('.prompt:not(.closed) .options button')].filter((b) => !b.disabled).length")?;
    let note = chrome.eval(&theirs, "document.querySelector('.prompt .prompt-note')?.textContent ?? ''")?;
    s.ok("another member sees the card, its buttons disabled, saying who may answer", seen && pressable == 0 && note.as_str().is_some_and(|n| n.contains("can answer")), json!({ "pressable": pressable, "note": note }));
    s.ok("and may not attach files (a blob is an editor's)", chrome.eval(&theirs, "document.getElementById('attach').disabled")? == true, "");
    chrome.click(&page, ".prompt:not(.closed) button[data-option=\"once\"]")?;
    let closed = shows(&mut chrome, &page, "document.querySelector('.prompt.closed .outcome.answered')?.textContent.startsWith('Allow once')");
    s.ok("its owner answers by clicking, and the card says how it closed", closed, chrome.eval(&page, "document.querySelector('.prompt')?.innerText")?);
    s.ok("the turn goes on approved", shows(&mut chrome, &page, &replied("(approved)")), "");
    let r = records(api, &owner, &chat_name, "chat");
    let answer = r.iter().find(|x| x["body"]["kind"] == "prompt_response").cloned().unwrap_or_default();
    s.ok("the page answered with a prompt_response for the card's option", answer["body"]["option"] == "once" && answer["body"]["prompt"].is_string(), &answer);
    s.ok("the member's card closes too", chrome.until(&theirs, "!!document.querySelector('.prompt.closed .outcome.answered')", TURN), "");
    chrome.close(theirs)?;

    // Stop: the send circle becomes Stop while the owner's turn runs
    say(&mut chrome, "slow, then stopped")?;
    let stoppable = chrome.until(&page, "!document.getElementById('stop').hidden && !document.getElementById('stop').disabled", TURN);
    let turn = chrome.eval(&page, "document.getElementById('stop').dataset.turn")?.as_str().unwrap_or("").to_string();
    s.ok("while the owner's turn runs, the send circle is Stop", stoppable && document_hidden(&mut chrome, &page, "send")?, &turn);
    chrome.click(&page, "#stop")?;
    let notice = shows(&mut chrome, &page, &format!("document.querySelector('.msg.notice.stopped[data-turn={}]') && document.getElementById('stop').hidden", js(&turn)));
    let ended = work_of(&records(api, &owner, &chat_name, "work"), &turn).iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["outcome"] == "stopped");
    let full = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|r| r["body"]["turn"] == turn.as_str());
    s.ok("clicking Stop stops the turn: it ends stopped, its answer never whole", notice && ended && !full, chrome.eval(&page, "document.getElementById('messages').innerText")?);
    let stop = records(api, &owner, &chat_name, "chat").into_iter().find(|r| r["body"]["kind"] == "stop").unwrap_or_default();
    s.ok("the page's Stop is a record naming the turn", stop["body"]["turn"] == turn.as_str(), &stop);

    // a picture the owner attaches: in their message, as the chat's blob, and read by the agent
    let png = chrome.eval(
        &page,
        "(() => { const c = document.createElement('canvas'); c.width = 160; c.height = 100; const g = c.getContext('2d'); const l = g.createLinearGradient(0, 0, 160, 100); l.addColorStop(0, '#a88bea'); l.addColorStop(1, '#62c8af'); g.fillStyle = l; g.fillRect(0, 0, 160, 100); return c.toDataURL('image/png').split(',')[1]; })()",
    )?;
    use base64::Engine;
    let picture = base64::engine::general_purpose::STANDARD.decode(png.as_str().context("the canvas answered a data URL")?)?;
    let picture_sha = hex::encode(Sha256::digest(&picture));
    let path = shots.join("picture.png");
    std::fs::write(&path, &picture)?;
    chrome.choose_files(&page, "#attachment-picker", &[Path::new(&path)])?;
    s.ok("a chosen file shows as a removable chip", shows(&mut chrome, &page, "[...document.querySelectorAll('#attachments .attachment-chip')].some((c) => c.textContent.includes('picture.png') && c.querySelector('.attachment-remove'))"), "");
    say(&mut chrome, "here is a picture")?;
    let shown = format!("!!document.querySelector('.msg.user.mine .attachment-image img[src=\"./__blob/{picture_sha}\"]')");
    s.ok("the message carries it, shown inline", shows(&mut chrome, &page, &shown), chrome.eval(&page, "document.getElementById('messages').innerHTML.slice(-600)")?);
    let fetched = chrome.eval(&page, &format!("fetch('./__blob/{picture_sha}').then(async (r) => [r.status, r.headers.get('content-type'), (await r.arrayBuffer()).byteLength])"))?;
    s.ok("the page reads it back as the chat's blob, as uploaded", fetched == json!([200, "image/png", picture.len()]), &fetched);
    let blob = api.signed(&owner, "GET", &format!("/api/f/{chat_name}/blobs/{picture_sha}"), None)?;
    s.ok("the same bytes the API serves", blob.status == 200 && blob.bytes == picture, blob.status);
    let r = records(api, &owner, &chat_name, "chat");
    let carried = r.iter().find(|x| x["body"]["text"] == "here is a picture").map(|x| x["body"]["attachments"].clone()).unwrap_or_default();
    s.ok(
        "its record names the file as docs/chat-records.md does",
        carried == json!([{ "sha256": picture_sha, "size": picture.len(), "type": "image/png", "name": "picture.png" }]),
        &carried,
    );
    s.ok("the agent read it", shows(&mut chrome, &page, &replied("[got 1: picture.png]")), "");

    // a reply's file: a chip that downloads it
    say(&mut chrome, "draw me something")?;
    let chip = "[...document.querySelectorAll('.msg.agent a.attachment-chip')].find((a) => a.textContent.includes('drawing.txt'))";
    s.ok("a reply's file shows as a chip", shows(&mut chrome, &page, chip), "");
    // no chip is no text: a FAIL below, the checks after it still made
    let drawing = chrome.eval(&page, &format!("(() => {{ const a = {chip}; return a ? fetch(a.getAttribute('href')).then((r) => r.text()) : null; }})()"))?;
    s.ok("which reads as the chat's blob", drawing.as_str().is_some_and(|t| t.contains("a drawing for")), &drawing);

    // what it looks like: desktop light and dark, and a phone's width with nothing sideways
    chrome.color_scheme(&page, "light")?;
    chrome.screenshot(&page, &shots.join("desktop-light.png"))?;
    chrome.color_scheme(&page, "dark")?;
    chrome.screenshot(&page, &shots.join("desktop-dark.png"))?;
    // the cards, further up
    chrome.eval(&page, "(() => { document.querySelector('.prompt')?.scrollIntoView({ block: 'center' }); return true; })()")?;
    chrome.screenshot(&page, &shots.join("desktop-dark-cards.png"))?;
    chrome.color_scheme(&page, "light")?;
    chrome.screenshot(&page, &shots.join("desktop-light-cards.png"))?;
    chrome.color_scheme(&page, "dark")?;
    chrome.viewport(&page, 375, 812, true)?;
    chrome.eval(&page, "(() => { const s = document.getElementById('scroll'); s.scrollTop = s.scrollHeight; return true; })()")?;
    std::thread::sleep(Duration::from_millis(300));
    let widths = chrome.eval(
        &page,
        "(() => { const s = document.getElementById('scroll'); return { inner: innerWidth, page: document.documentElement.scrollWidth, scroll: s.scrollWidth, client: s.clientWidth, composer: document.getElementById('say').getBoundingClientRect().right }; })()",
    )?;
    // a width the page did not answer is NaN, which fits nothing
    let w = |k: &str| widths[k].as_f64().unwrap_or(f64::NAN);
    let fits = w("page") <= w("inner") && w("scroll") <= w("client") && w("composer") <= w("inner");
    s.ok("at 375×812, nothing scrolls sideways", w("inner") == 375.0 && fits, &widths);
    chrome.screenshot(&page, &shots.join("phone-dark.png"))?;
    chrome.color_scheme(&page, "light")?;
    chrome.screenshot(&page, &shots.join("phone-light.png"))?;
    println!("      (screenshots in {})", shots.display());

    // nothing answered twice, and the computer goes back to sleep
    let turns: Vec<String> = records(api, &owner, &chat_name, "work").iter().filter(|r| r["body"]["kind"] == "turn.start").filter_map(|r| r["body"]["turn"].as_str().map(str::to_string)).collect();
    let once: std::collections::BTreeSet<&String> = turns.iter().collect();
    s.ok("every message the page sent was one turn", turns.len() == 7 && once.len() == turns.len(), json!(turns));
    let first_turn = turn_of(&agent_name, &chat_name, "chat", sent.and_then(|x| x["seq"].as_i64()).unwrap_or(0));
    s.ok("the first of them the turn of the page's first message", turns.first() == Some(&first_turn), json!({ "first": first_turn, "turns": turns }));
    std::thread::sleep(super::computers::QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{computer}/sleep"), Some(&json!({})))?;
    Ok(())
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Whether the element `id` is hidden on the page.
fn document_hidden(chrome: &mut Browser, page: &Page, id: &str) -> Result<bool> {
    Ok(chrome.eval(page, &format!("document.getElementById({}).hidden", js(id)))? == Value::Bool(true))
}
