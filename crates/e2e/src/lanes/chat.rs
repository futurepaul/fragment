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
//! and as the chat's blob; a reply's file; a voice memo recorded from
//! Chrome's fake microphone, sent as an audio attachment and shown as a
//! player, which the agent receives. At a phone's width nothing scrolls
//! sideways. Screenshots (desktop light and dark, phone) stay in the run's
//! scratch (`chat/`).
//!
//! The chat runs the template's code from the platform's release
//! (decision 40), which its own repo cannot override, and that code's push
//! (docs/chat-records.md, Push): every agent reply starts its job, no
//! person's record does; while the owner's page is on screen nothing is
//! pushed to them, and once it is closed a reply reaches each of the
//! chat's people's subscriptions once (the push fake decrypts it), never
//! a tag that names no one; no one subscribes for another identity.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::computers::{agent_replies, turn_of, work_of, AGENT_JSON};
use super::jobs::{records, settle, started};
use super::signin::site_cookie;
use crate::api::{Api, Call, Reply};
use fragment_nip98::Keys;
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

/// A JS string literal.
fn js(s: &str) -> String {
    serde_json::to_string(s).expect("a string encodes")
}

pub fn chat(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("chat", &[crate::Need::Fakes, crate::Need::LocalDocker, crate::Need::Chrome, crate::Need::Levers]) {
        return Ok(());
    }
    let owner = api.person()?;
    let member = api.person()?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && computer.starts_with("computer:"), "making the computer: {r}");

    // an agent fragment on the computer, and a chat on the blessed template
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{computer}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && fragment_core::npub::is_identity(&identity), "assigning the agent: {r}");
    let chat_name = s.named(api, &owner, "talk")?;
    let r = api.create_with(&owner, json!({ "name": chat_name, "template": "chat" }))?;
    anyhow::ensure!(r.status == 200, "making the chat on the template: {r}");
    s.owned(&r.body, &owner);
    let made = r.body.clone();
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("the agent joins the chat on the template", r.status == 200, &r);
    // the platform tells the agent it joined (Paul, 2026-10-03), and wakes its computer
    let member_id = api.identity(&member)?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{member_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a viewer: {r}");

    // ---- the template's code runs from the platform's release (decision 40): its push job
    let code = |api: &Api| api.status(&owner, &chat_name).map(|r| r.body["code"].clone()).unwrap_or_default();
    let installed = s.eventually(Duration::from_secs(20), || code(api)["operations"]["notify_reply"]["kind"] == "job");
    let files = api.signed(&owner, "GET", &format!("/api/f/{chat_name}/files"), None)?;
    let own: Vec<String> = files.body["files"].as_array().into_iter().flatten().filter_map(|f| f["path"].as_str().map(str::to_string)).collect();
    s.ok(
        "the chat runs the template's code from the release, its push job, with none in its own repo",
        installed && code(api)["error"].is_null() && !own.iter().any(|p| p == "app.mjs"),
        json!({ "code": code(api), "files": own }),
    );
    let triggers = api.signed(&owner, "GET", &format!("/api/f/{chat_name}/triggers"), None)?;
    s.ok(
        "its trigger starts the job for its agents' records alone",
        triggers.body["triggers"] == json!([{ "channel": "chat", "from": "agent", "run": "notify_reply", "paused": false }]),
        &triggers,
    );
    s.commit(&made, &[("app.mjs", Some(b"export class App { notify_reply() { return { pushed: 99 }; } }\n"))]);
    s.deploy(&made);
    let refused = s.eventually(Duration::from_secs(30), || code(api)["error"].as_str().is_some_and(|e| e.contains("carries no code of its own (app.mjs): fork it")));
    s.ok(
        "an app.mjs of the chat's own does not override the release's: it is refused, saying to fork, and the release's code stays",
        refused && code(api)["operations"]["notify_reply"]["kind"] == "job",
        code(api),
    );
    s.commit(&made, &[("app.mjs", None)]);
    s.deploy(&made);
    let back = s.eventually(Duration::from_secs(30), || code(api)["error"].is_null());
    s.ok("without it, the chat installs again from the release", back && code(api)["operations"]["notify_reply"].is_object(), code(api));

    // ---- the page's own routes, as a browser signed in there calls them
    let owner_session = api.sign_in(&Api::email_of(&owner))?;
    let member_session = api.sign_in(&Api::email_of(&member))?;
    let owner_site = format!("fragment_site={}", site_cookie(api, &owner_session, &chat_name)?);
    let member_site = format!("fragment_site={}", site_cookie(api, &member_session, &chat_name)?);
    let served = s.eventually(Duration::from_secs(20), || api.page(&chat_name, "", Some(&owner_site)).is_ok_and(|r| r.status == 200 && r.text.contains("./chat.js")));
    s.ok("the chat serves the template's page", served, "");
    let label = fragment_proto::split_fragment_name(&agent_name).map_or("", |(l, _)| l).to_string();
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

    // ---- push: each of the chat's people subscribes their own browser
    let owner_id = api.identity(&owner)?;
    let owner_push = s.name("chat-owner-push");
    let r = subscribe(s, api, &chat_name, &owner_site, &owner_id, &owner_push, 17)?;
    s.ok("the owner's page subscribes their browser, tagged with their identity", r.status == 200 && r.body["who"] == owner_id.as_str(), &r);
    let another = subscribe(s, api, &chat_name, &owner_site, &member_id, &s.name("chat-not-theirs"), 19)?;
    let agents = subscribe(s, api, &chat_name, &owner_site, &identity, &s.name("chat-agents-push"), 23)?;
    s.ok("no one subscribes for another identity, a person's or an agent's (403)", another.status == 403 && agents.status == 403, format!("{another} | {agents}"));

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
    s.ok("nor record a voice memo", chrome.eval(&theirs, "document.getElementById('record').disabled")? == true, "");
    chrome.click(&page, ".prompt:not(.closed) button[data-option=\"once\"]")?;
    let closed = shows(&mut chrome, &page, "document.querySelector('.prompt.closed .outcome.answered')?.textContent.startsWith('Allow once')");
    s.ok("its owner answers by clicking, and the card says how it closed", closed, chrome.eval(&page, "document.querySelector('.prompt')?.innerText")?);
    s.ok("the turn goes on approved", shows(&mut chrome, &page, &replied("(approved)")), "");
    let r = records(api, &owner, &chat_name, "chat");
    let answer = r.iter().find(|x| x["body"]["kind"] == "prompt_response").cloned().unwrap_or_default();
    s.ok("the page answered with a prompt_response for the card's option", answer["body"]["option"] == "once" && answer["body"]["prompt"].is_string(), &answer);
    s.ok("the member's card closes too", chrome.until(&theirs, "!!document.querySelector('.prompt.closed .outcome.answered')", TURN), "");
    chrome.close(theirs)?;

    // choices the agent asks (as Hermes' clarify asks them), the last
    // answered in words: typed on the card, sent with Enter (Paul on p5,
    // 2026-10-09: "'other' type your answer for questions doesn't work very well")
    say(&mut chrome, "choose a plant")?;
    let field = ".prompt:not(.closed) input[data-option=\"other\"]";
    let offered = shows(&mut chrome, &page, &format!("!!document.querySelector({0}) && !document.querySelector({0}).disabled", js(field)));
    let _ = chrome.screenshot(&page, &shots.join("desktop-choices.png"));
    s.ok("a card of choices offers the option answered in words as a field on the card", offered, chrome.eval(&page, "document.querySelector('.prompt:not(.closed)')?.innerText ?? null")?);
    if offered {
        chrome.click(&page, field)?;
        chrome.type_text(&page, "rosemary")?;
        chrome.press(&page, "Enter", false)?;
    }
    let closed = shows(&mut chrome, &page, "[...document.querySelectorAll('.prompt.closed .outcome.answered')].some((o) => o.textContent.startsWith('rosemary'))");
    s.ok("typed there and sent with Enter, the card closes saying what was answered", closed, chrome.eval(&page, "[...document.querySelectorAll('.prompt')].map((p) => p.innerText)")?);
    s.ok("and the agent has the words as the answer", shows(&mut chrome, &page, &replied("(chose: rosemary)")), chrome.eval(&page, "document.getElementById('messages').innerText")?);
    let r = records(api, &owner, &chat_name, "chat");
    let answer = r.iter().find(|x| x["body"]["kind"] == "prompt_response" && x["body"]["option"] == "other").cloned().unwrap_or_default();
    s.ok("the page answered with the option and its words, `{option, text}`", answer["body"]["text"] == "rosemary", &answer);
    let _ = chrome.screenshot(&page, &shots.join("desktop-choices-answered.png"));

    // Stop: the send circle becomes Stop while the owner's turn runs. The
    // turn is the message's own (docs/chat-records.md): the page may still
    // show Stop for the turn before, whose end it has not heard yet, and a
    // click then stops nothing
    let slow = "slow, then stopped";
    say(&mut chrome, slow)?;
    let sent_at = |r: &Value| (r["body"]["text"] == slow).then(|| r["seq"].as_i64()).flatten();
    let mut seq = None;
    s.eventually(TURN, || {
        seq = records(api, &owner, &chat_name, "chat").iter().find_map(sent_at);
        seq.is_some()
    });
    let turn = turn_of(&agent_name, &chat_name, "chat", seq.unwrap_or(0));
    let stoppable = chrome.until(&page, &format!("(() => {{ const b = document.getElementById('stop'); return !b.hidden && !b.disabled && b.dataset.turn === {}; }})()", js(&turn)), TURN);
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

    // a voice memo: the mic records (Chrome's fake microphone) and the message carries the clip
    let mic = "document.getElementById('record')";
    s.ok("an editor's composer has a mic", chrome.eval(&page, &format!("!{mic}.hidden && !{mic}.disabled"))? == true, "");
    let bubbles = |chrome: &mut Browser| chrome.eval(&page, "document.querySelectorAll('.msg.user.mine').length").ok().and_then(|v| v.as_u64()).unwrap_or(0);
    let before = bubbles(&mut chrome);
    chrome.click(&page, "#record")?;
    let recording = format!("{mic}.classList.contains('on') && !document.getElementById('recording').hidden && !document.getElementById('discard').hidden");
    let on = shows(&mut chrome, &page, &recording);
    std::thread::sleep(Duration::from_millis(800));
    chrome.click(&page, "#discard")?;
    let off = shows(&mut chrome, &page, &format!("!{mic}.classList.contains('on') && document.getElementById('recording').hidden"));
    std::thread::sleep(Duration::from_millis(500));
    let why = if on && off {
        Value::Null
    } else {
        // what the page's microphone and composer said, for the failure
        chrome.eval(&page, "Promise.race([navigator.mediaDevices.getUserMedia({ audio: true }).then((s) => { s.getTracks().forEach((t) => t.stop()); return 'granted'; }, (e) => e.name), new Promise((r) => setTimeout(() => r('no answer in 5 s'), 5000))]).then((mic) => ({ mic, focused: document.hasFocus(), visible: document.visibilityState, banner: document.getElementById('banner').hidden ? null : document.getElementById('banner-text').textContent, record: document.getElementById('record').outerHTML.slice(0, 200) }))")?
    };
    s.ok("pressed, it records, quietly (the mic is its Stop, beside a clock); the x lets the memo go, sending nothing", on && off && bubbles(&mut chrome) == before, json!({ "on": on, "off": off, "why": why }));
    chrome.click(&page, "#record")?;
    let ticking = shows(&mut chrome, &page, &format!("{recording} && document.getElementById('recording-time').textContent !== '0:00'"));
    chrome.click(&page, "#text")?;
    chrome.type_text(&page, "a voice memo")?;
    chrome.click(&page, "#record")?;
    let memo_shown = "[...document.querySelectorAll('.msg.user.mine')].some((m) => m.querySelector('.attachment-audio audio[src^=\"./__blob/\"]') && m.textContent.includes('a voice memo'))";
    s.ok("pressed again, it sends the clip with what was typed, shown as a player", ticking && shows(&mut chrome, &page, memo_shown), chrome.eval(&page, "document.getElementById('messages').innerHTML.slice(-600)")?);
    let r = records(api, &owner, &chat_name, "chat");
    let memo = r.iter().find(|x| x["body"]["text"] == "a voice memo").map(|x| x["body"]["attachments"].clone()).unwrap_or_default();
    let memo_sha = memo[0]["sha256"].as_str().unwrap_or("").to_string();
    s.ok(
        "its record names the clip as an audio attachment, as the recorder made it",
        memo.as_array().is_some_and(|a| a.len() == 1) && memo[0]["type"] == "audio/webm" && memo[0]["name"] == "voice-memo.webm" && memo[0]["size"].as_u64().is_some_and(|n| n > 0),
        &memo,
    );
    let played = chrome.eval(&page, &format!("fetch('./__blob/{memo_sha}').then(async (r) => [r.status, r.headers.get('content-type'), (await r.arrayBuffer()).byteLength])"))?;
    s.ok("the page plays it from the chat's blob, served as audio", played == json!([200, "audio/webm", memo[0]["size"]]), &played);
    s.ok("the agent received it", shows(&mut chrome, &page, &replied("[got 1: voice-memo.webm]")), "");

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
    s.ok("every message the page sent was one turn", turns.len() == 8 && once.len() == turns.len(), json!(turns));
    let first_turn = turn_of(&agent_name, &chat_name, "chat", sent.and_then(|x| x["seq"].as_i64()).unwrap_or(0));
    s.ok("the first of them the turn of the page's first message", turns.first() == Some(&first_turn), json!({ "first": first_turn, "turns": turns }));

    // files a record names stay past the grace period; an upload none names goes
    let blob = |sha: &str| api.signed(&owner, "HEAD", &format!("/api/f/{chat_name}/blobs/{sha}"), None).map(|r| r.status).unwrap_or(0);
    // a quiet fragment's poll pass, which collects, is a day off: a test lever brings it in
    let poll_now = || api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": chat_name, "op": "poll-now" })));
    let collected = s.eventually(Duration::from_secs(30), || poll_now().is_ok_and(|r| r.status == 200) && blob(&sha) == 404);
    s.ok("an upload no record names is collected after the grace period", collected, blob(&sha));
    let named: Vec<String> = records(api, &owner, &chat_name, "chat")
        .iter()
        .flat_map(|r| r["body"]["attachments"].as_array().cloned().unwrap_or_default())
        .filter_map(|a| a["sha256"].as_str().map(str::to_string))
        .collect();
    let kept: Vec<(String, u16)> = named.iter().map(|sha| (sha.clone(), blob(sha))).collect();
    s.ok(
        "while the files the chat's records name stay (the picture, the agent's drawing, the voice memo)",
        collected && kept.len() >= 3 && kept.iter().all(|(_, status)| *status == 200),
        json!(kept),
    );

    // ---- push (docs/chat-records.md, Push): while the owner's page was on
    // screen, the agent's replies were not pushed to them
    let runs_ended = |s: &Suite| {
        let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len();
        s.eventually(TURN, || {
            let runs = notify_runs(api, &owner, &chat_name);
            runs.len() == replies && runs.iter().all(|r| r["status"] == "succeeded")
        })
    };
    let ended = runs_ended(s);
    let runs = notify_runs(api, &owner, &chat_name);
    let replies = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity);
    s.ok(
        "each agent reply started the chat's push job once, as the chat itself; no person's record started one",
        ended && runs.len() == replies.len() && runs.iter().all(|r| r["via"] == "channel" && r["trigger"] == "chat"),
        json!({ "runs": runs.len(), "replies": replies.len() }),
    );
    let looked = chrome.eval(&page, "document.visibilityState")?;
    let last = runs.first().and_then(|r| r["id"].as_i64()).unwrap_or(0);
    let last = settle(api, &owner, &chat_name, last, &["succeeded"], TURN);
    s.ok(
        "while the owner's page was on screen (`looking`), none was pushed to them: the job left them out",
        looked == "visible" && s.push.received(&owner_push).is_empty() && last["output"]["to"] == 1 && last["output"]["pushed"] == 0,
        json!({ "visibility": looked, "received": s.push.received(&owner_push), "last": last["output"] }),
    );

    // away from the chat (its page closed), a reply reaches each of its people once
    chrome.close(page)?;
    let member_push = s.name("chat-member-push");
    let anyone = s.name("chat-anyone-push");
    let mine = subscribe(s, api, &chat_name, &member_site, &member_id, &member_push, 29)?;
    let untagged = subscribe(s, api, &chat_name, &member_site, "everyone", &anyone, 31)?;
    s.ok("a viewer subscribes for themselves, and under a tag that names no one", mine.status == 200 && untagged.status == 200, format!("{mine} | {untagged}"));
    let said = "while I am away";
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "away-1", "body": { "text": said } })))?;
    anyhow::ensure!(r.status == 200, "posting while away: {r}");
    let answered = s.eventually(TURN, || agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).iter().any(|x| x["body"]["text"].as_str().is_some_and(|t| t.contains(said))));
    let reply = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().find(|x| x["body"]["text"].as_str().is_some_and(|t| t.contains(said))).unwrap_or_default();
    let pushed = s.eventually(TURN, || !s.push.received(&owner_push).is_empty() && !s.push.received(&member_push).is_empty());
    let ended = runs_ended(s);
    std::thread::sleep(super::computers::QUEUE_DRAIN);
    let text = reply["body"]["text"].as_str().unwrap_or("").to_string();
    let want = json!({ "title": capital(&label), "body": text, "tag": chat_name, "url": "./" });
    s.ok(
        "away, the agent's reply is pushed once to each of the chat's people: its name, its words, the chat",
        answered && pushed && ended && text.chars().count() <= 120 && s.push.received(&owner_push) == [want.clone()] && s.push.received(&member_push) == [want.clone()],
        json!({ "want": want, "owner": s.push.received(&owner_push), "member": s.push.received(&member_push) }),
    );
    s.ok(
        "nothing for the person's own message or the reply's drafts, nor to a tag that names no person",
        s.push.received(&anyone).is_empty() && notify_runs(api, &owner, &chat_name).len() == agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len(),
        json!({ "anyone": s.push.received(&anyone) }),
    );
    let r = api.op(&owner, &chat_name, "notify_reply", "by-hand", json!({ "channel": "chat", "record": reply }))?;
    let by_hand = settle(api, &owner, &chat_name, started(&r), &["succeeded", "held"], TURN);
    std::thread::sleep(super::computers::QUEUE_DRAIN);
    s.ok(
        "a member who runs the push job by hand pushes nothing: only its trigger's runs push",
        by_hand["status"] == "succeeded" && by_hand["output"]["pushed"] == 0 && s.push.received(&owner_push).len() == 1,
        &by_hand,
    );

    std::thread::sleep(super::computers::QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{computer}/sleep"), Some(&json!({})))?;
    Ok(())
}

/// A browser's push subscription at the push fake (`id`), stored by the
/// chat's page as the person whose site cookie it carries, tagged `who`.
fn subscribe(s: &Suite, api: &Api, chat: &str, cookie: &str, who: &str, id: &str, seed: u8) -> Result<Reply> {
    let sub = s.push.subscribe(id, seed);
    let body = json!({ "who": who, "endpoint": sub.endpoint, "p256dh": sub.p256dh, "auth": sub.auth });
    api.call(Call {
        method: "POST",
        url: api.site_url(chat, "__push-sub"),
        body: Some(body.to_string().into_bytes()),
        content_type: Some("application/json"),
        cookie: Some(cookie.to_string()),
        ..Call::default()
    })
}

/// The chat's push job's runs, the newest first.
fn notify_runs(api: &Api, owner: &Keys, chat: &str) -> Vec<Value> {
    api.signed(owner, "GET", &format!("/api/f/{chat}/runs?op=notify_reply&limit=200"), None).ok().and_then(|r| r.body["runs"].as_array().cloned()).unwrap_or_default()
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Whether the element `id` is hidden on the page.
fn document_hidden(chrome: &mut Browser, page: &Page, id: &str) -> Result<bool> {
    Ok(chrome.eval(page, &format!("document.getElementById({}).hidden", js(id)))? == Value::Bool(true))
}
