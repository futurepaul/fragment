//! The desktop template in a browser (docs/phase-6.md, step 3; ROADMAP
//! phase 6's acceptance): its owner's apps and files open into the viewer,
//! panes reorder and close, both sides collapse, it works at phone width,
//! and the layout survives a reload. Every frame is a fragment of the
//! owner's, signed in on its own origin.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use fragment_fakes::openrouter::Reply;

use super::templates::person;
use crate::api::Api;
use crate::browser::{Browser, Page};
use crate::Suite;

/// The viewer's panes, top to bottom.
const PANES: &str = "[...document.querySelectorAll('.pane')].filter(p => !p.hidden).sort((a, b) => a.style.gridRow - b.style.gridRow).map(p => p.dataset.key)";

fn panes(chrome: &mut Browser, page: &Page) -> Vec<String> {
    chrome.eval(page, PANES).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// The middle of an element's box, in the page's pixels.
fn centre(chrome: &mut Browser, page: &Page, selector: &str) -> Option<(f64, f64)> {
    let v = chrome.eval(page, &format!("(() => {{ const r = document.querySelector({selector:?})?.getBoundingClientRect(); return r ? [r.x + r.width / 2, r.y + r.height / 2] : null; }})()")).ok()?;
    Some((v[0].as_f64()?, v[1].as_f64()?))
}

/// Runs on a node restarted with its platform on the fragments' domain
/// (as fragment.club is), then restarts it as it was for the lanes after.
pub fn desktop(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("desktop") {
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

fn run(s: &mut Suite, api: &Api) -> Result<()> {
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the desktop lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let Some(suffix) = api.suffix.clone() else {
        s.ok("the desktop lane runs with fragment hosts", false, "no suffix");
        return Ok(());
    };
    let (owner, session) = person(api)?;
    let make = |label: &str, template: &str| -> Result<String> {
        let name = api.qualified(&owner, label)?;
        let r = api.create_with(&owner, json!({ "name": name, "template": template }))?;
        anyhow::ensure!(r.status == 200, "making {name}: {r}");
        Ok(name)
    };
    let desk = make(&s.name("desk"), "desktop")?;
    let todo = make(&s.name("dtodo"), "todo")?;
    let notes = make(&s.name("dnotes"), "blank")?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{notes}/files"), Some(&json!({ "files": [{ "path": "notes/hello.txt", "text": "hello from a file" }] })))?;
    anyhow::ensure!(r.status == 200, "writing a file: {r}");
    let label = |name: &str| name.split('.').next().unwrap_or("").to_string();
    let wait = Duration::from_secs(20);

    // signed in on the platform, the browser walks to the desktop's origin
    chrome.set_cookie(&format!("http://{suffix}:{}/", api.port), "fragment_session", &session)?;
    let page = chrome.open(&api.site_url(&desk, "__signin?return=/"))?;
    chrome.viewport(&page, 1440, 900, false)?;
    let listed = format!("[...document.querySelectorAll('#apps .row .label')].map(l => l.textContent).includes({:?})", label(&todo));
    s.ok("the owner's desktop lists their apps", chrome.until(&page, &listed, wait), chrome.eval(&page, "document.body.innerText.slice(0, 300)").unwrap_or_default());
    s.ok("but not itself", chrome.eval(&page, &format!("![...document.querySelectorAll('#apps .row .label')].map(l => l.textContent).includes({:?})", label(&desk)))? == json!(true), "");

    // Clickjacking: a fragment's page (its author's code, or an agent's)
    // frames the platform's approval of a key of its own. The two are one
    // site, so the owner's platform session rides into the frame; the
    // platform refuses to be framed, so nothing shows there to lay under a
    // click. Its redirects still work in a frame: that is how every frame
    // on the desktop signs in.
    let attacker = chrome.open(&api.site_url(&todo, "__signin?return=/"))?;
    let on_todo = format!("location.host.startsWith({:?}) && location.pathname === '/' && document.readyState === 'complete'", format!("{}--", label(&todo)));
    anyhow::ensure!(chrome.until(&attacker, &on_todo, wait), "the todo's page did not open");
    let frame = |chrome: &mut Browser, src: &str| chrome.eval(&attacker, &format!("(() => {{ const f = document.createElement('iframe'); f.src = {src:?}; document.body.append(f); return true; }})()"));
    frame(&mut chrome, &format!("{}/auth/fragment?name={notes}&return=/", api.base))?;
    let signed_in = s.eventually(wait, || {
        chrome.eval_in_frame(&attacker, &format!("{}--", label(&notes)), "location.pathname === '/' && document.readyState === 'complete'").ok() == Some(json!(true))
    });
    s.ok("(the owner's platform session rides into a frame on a fragment's page: a redirect through the platform signs it in on the fragment)", signed_in, "");
    let key = fragment_nip98::Keys::generate();
    frame(&mut chrome, &api.approval_link(&key, 0))?;
    let approval = |chrome: &mut Browser| chrome.eval_in_frame(&attacker, "/cli?key=", "document.body?.innerText ?? ''").ok().and_then(|v| v.as_str().map(str::to_string));
    let shown = s.eventually(Duration::from_secs(5), || approval(&mut chrome).is_some_and(|t| t.contains("Add this key")));
    s.ok("but a fragment's page that frames the platform's key approval gets a frame without it", !shown, format!("{:?}", approval(&mut chrome)));
    chrome.close(attacker)?;

    // a chat: a chat fragment of the owner's, in the middle column, with
    // the owner's agent in it (its model is the OpenRouter fake, scripted:
    // a greeting, then an app when asked for one)
    let app_label = s.name("dcounter");
    let app = api.qualified(&owner, &app_label)?;
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Text("Hi! I'm your agent.".into()),
        Reply::Tools(vec![("platform__create_fragment".into(), json!({ "label": app_label, "template": "blank" }))]),
        Reply::Tools(vec![("platform__write_files".into(), json!({ "fragment": app, "files": [{ "path": "site/index.html", "text": "<h1>A counter your agent made</h1>" }] }))]),
        Reply::Tools(vec![("platform__deploy".into(), json!({ "fragment": app }))]),
        Reply::Text("Your counter is in your apps.".into()),
    ]);
    chrome.eval(&page, "document.getElementById('new-chat').click(); true")?;
    let chatted = chrome.until(&page, "document.querySelectorAll('#chats .row').length === 1 && !!document.querySelector('#frames iframe:not([hidden])')", wait);
    let mine = api.signed(&owner, "GET", "/api/fragments", None)?;
    let chat = mine.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).find(|n| n.starts_with("chat-")).unwrap_or("").to_string();
    s.ok("New chat makes a chat fragment and opens it in the middle", chatted && !chat.is_empty(), &mine);
    let chat_title = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.title").ok() == Some(json!("Chat")));
    s.ok("the chat's own page shows there, signed in on its origin", chat_title, "");
    // the page is ready once it is connected (its module has run)
    let ready = s.eventually(wait, || {
        chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('here').textContent !== 'connecting…'").ok() == Some(json!(true))
    });
    let said = ready
        && chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('text').value = 'hi from the desktop'; document.getElementById('say').requestSubmit(); true").is_ok();
    let me = api.identity(&owner)?;
    let landed = s.eventually(wait, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)
            .is_ok_and(|r| r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["body"]["text"] == "hi from the desktop" && x["principal"] == me.as_str())))
    });
    s.ok("a message sent in it is the owner's", said && landed, api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)?);
    let in_chat = |chrome: &mut Browser, text: &str| {
        chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('messages').textContent").ok().and_then(|v| v.as_str().map(|t| t.contains(text))) == Some(true)
    };
    s.ok("the owner's agent answers in it", s.eventually(wait, || in_chat(&mut chrome, "Hi! I'm your agent.")), "");
    let named = format!("{}'s agent", api.username(&owner)?);
    s.ok("named as its owner's agent", s.eventually(wait, || in_chat(&mut chrome, &named)), &named);

    // A socket has no CORS, and every fragment is one site with the others:
    // a page on the todo's origin (its author's code, or an agent's) opens
    // the chat's live socket, and the owner's session on the chat's origin
    // rides along. The chat takes a socket only from its own page.
    let read_chat = format!(
        "new Promise((done) => {{ const got = {{ opened: false, read: false, frames: [] }}; const ws = new WebSocket({url:?}); \
         const t = setTimeout(() => {{ ws.close(); done(got); }}, 5000); \
         ws.onopen = () => {{ got.opened = true; ws.send(JSON.stringify({{ type: 'subscribe', channel: 'chat', last: 50 }})); }}; \
         ws.onmessage = (e) => {{ got.frames.push(JSON.parse(e.data).type); if (e.data.includes('hi from the desktop')) {{ got.read = true; clearTimeout(t); ws.close(); done(got); }} }}; \
         ws.onclose = () => {{ clearTimeout(t); done(got); }}; }})",
        url = api.site_url(&chat, "__live?v=2").replacen("http", "ws", 1)
    );
    let attacker = chrome.open(&api.site_url(&todo, "__signin?return=/"))?;
    anyhow::ensure!(chrome.until(&attacker, &on_todo, wait), "the todo's page did not open");
    let foreign = chrome.eval(&attacker, &read_chat).unwrap_or_else(|e| json!({ "error": e.to_string() }));
    chrome.close(attacker)?;
    s.ok(
        "a page on another fragment's origin cannot open the chat's live socket with the owner's session, nor read the chat through it",
        foreign["opened"] == json!(false) && foreign["read"] == json!(false),
        &foreign,
    );
    let own = chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), &read_chat).unwrap_or_else(|e| json!({ "error": e.to_string() }));
    s.ok("(the chat's own page reads it through the same socket)", own["read"] == json!(true), &own);

    // asked for an app, the agent makes it, and the desktop shows it
    chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('text').value = 'make me a counter app'; document.getElementById('say').requestSubmit(); true")?;
    s.ok("asked for an app in the chat, the agent says it made one", s.eventually(Duration::from_secs(40), || in_chat(&mut chrome, "Your counter is in your apps.")), "");
    let shown = format!("[...document.querySelectorAll('#apps .row .label')].map(l => l.textContent).includes({app_label:?})");
    s.ok("and it appears in the desktop's sidebar, with no reload", chrome.until(&page, &shown, wait), chrome.eval(&page, "document.getElementById('apps').innerText").unwrap_or_default());
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{app}")))?;
    let made = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{app_label}--"), "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.contains("A counter your agent made"))) == Some(true));
    s.ok("opened, it is the page the agent wrote", made, "");
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Close]').click(); true", format!("app:{app}")))?;

    // apps and files open into the viewer, newest on top
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{todo}")))?;
    s.ok("an app opens as a pane", chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={:?}]')", format!("app:{todo}")), wait), "");
    let todo_title = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}--", label(&todo)), "document.title").ok() == Some(json!("Todo")));
    s.ok("the app's own page is in it", todo_title, "");
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{notes}")))?;
    chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={:?}]')", format!("app:{notes}")), wait);
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Files]').click(); true", format!("app:{notes}")))?;
    let tree_key = format!("tree:{notes}");
    s.ok("an app's files open as a pane", chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={tree_key:?}]')"), wait), "");
    let clicked = s.eventually(wait, || {
        chrome.eval_in_frame(&page, "/__files", "(() => { const a = [...document.querySelectorAll('a')].find(a => a.textContent === 'notes/hello.txt'); if (!a) return false; a.click(); return true; })()").ok() == Some(json!(true))
    });
    let file_open = clicked && chrome.until(&page, "[...document.querySelectorAll('.pane')].some(p => p.dataset.key.startsWith('file:'))", wait);
    s.ok("a file in that list opens as its own pane", file_open, format!("{:?}", panes(&mut chrome, &page)));
    let text = s.eventually(wait, || chrome.eval_in_frame(&page, "__file?path=", "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.contains("hello from a file"))) == Some(true));
    let seen = chrome.eval_in_frame(&page, "__file?path=", "location.href + ' ' + document.body?.innerText").map_err(|e| e.to_string());
    s.ok("read through its own fragment's __file", text, format!("{seen:?}"));
    let order = panes(&mut chrome, &page);
    chrome.screenshot(&page, &s.scratch.join("desktop.png"))?;
    s.ok("the newest pane is on top", order.len() == 4 && order[0].starts_with("file:") && order[3] == format!("app:{todo}"), format!("{order:?}"));

    // reorder: the top pane's header dragged below the bottom pane
    let from = centre(&mut chrome, &page, &format!(".pane[data-key=\"{}\"] .pane-title", order[0]));
    let to = centre(&mut chrome, &page, &format!(".pane[data-key=\"{}\"] .pane-body", order[3]));
    if let (Some(from), Some(to)) = (from, to) {
        chrome.drag(&page, from, (to.0, to.1 + 40.0))?;
    }
    let moved = panes(&mut chrome, &page);
    s.ok("dragging a pane's header reorders it", moved.len() == 4 && moved[3] == order[0] && moved[0] == order[1], format!("{order:?} → {moved:?}"));
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Close]').click(); true", tree_key))?;
    let closed = panes(&mut chrome, &page);
    s.ok("a pane closes", closed.len() == 3 && !closed.contains(&tree_key), format!("{closed:?}"));

    // both sides collapse, and the layout is kept across a reload
    let layout = |chrome: &mut Browser| -> Value { chrome.eval(&page, "[...document.getElementById('layout').classList]").unwrap_or_default() };
    chrome.eval(&page, "document.getElementById('collapse-left').click(); true")?;
    chrome.eval(&page, "document.getElementById('toggle-right').click(); true")?;
    let both = layout(&mut chrome);
    s.ok("both sides collapse", !both.to_string().contains("left-open") && !both.to_string().contains("right-open"), &both);
    chrome.eval(&page, "document.getElementById('toggle-right').click(); true")?;
    let before = panes(&mut chrome, &page);
    chrome.reload(&page)?;
    std::thread::sleep(Duration::from_millis(500));
    let restored = chrome.until(&page, &format!("JSON.stringify({PANES}) === {:?}", serde_json::to_string(&before)?), wait);
    let after = layout(&mut chrome);
    s.ok(
        "a reload keeps the panes, their order, and what is collapsed",
        restored && !after.to_string().contains("left-open") && after.to_string().contains("right-open"),
        format!("{before:?} → {:?}, {after}", panes(&mut chrome, &page)),
    );
    s.ok("and the open chat", chrome.until(&page, &format!("document.getElementById('chat-title').textContent === {:?}", label(&chat)), wait), "");

    // a phone: the sides are overlays over the chat
    chrome.viewport(&page, 390, 844, true)?;
    let narrow = chrome.until(&page, "document.getElementById('layout').classList.contains('narrow')", wait);
    chrome.eval(&page, "document.getElementById('toggle-left').click(); true")?;
    let overlay = chrome.until(&page, "document.getElementById('layout').classList.contains('left-open') && document.getElementById('scrim').classList.contains('on')", wait);
    chrome.eval(&page, "document.getElementById('scrim').click(); true")?;
    let back = chrome.until(&page, "!document.getElementById('layout').classList.contains('left-open')", wait);
    let fits = chrome.eval(&page, "document.documentElement.scrollWidth <= innerWidth")? == json!(true);
    chrome.screenshot(&page, &s.scratch.join("desktop-phone.png"))?;
    s.ok("at phone width the sidebar is an overlay, and nothing scrolls sideways", narrow && overlay && back && fits, layout(&mut chrome));
    Ok(())
}
