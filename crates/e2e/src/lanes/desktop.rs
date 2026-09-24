//! The desktop template in a browser (docs/phase-6.md, step 3; ROADMAP
//! phase 6's acceptance): its owner's apps and files open into the viewer,
//! panes reorder and close, both sides collapse, it works at phone width,
//! and the layout survives a reload. Every frame is a fragment of the
//! owner's, signed in on its own origin.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

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
pub fn desktop(s: &mut Suite, api: Api) -> Result<Api> {
    if !s.section("desktop") {
        return Ok(api);
    }
    drop(api);
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let result = run(s, &api);
    drop(api);
    s.stop()?;
    let api = s.start(false, true)?;
    result.map(|()| api)
}

fn run(s: &mut Suite, api: &Api) -> Result<()> {
    let Some(mut chrome) = Browser::launch(&s.scratch)? else {
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

    // a chat: a chat fragment of the owner's, in the middle column
    chrome.eval(&page, "document.getElementById('new-chat').click(); true")?;
    let chatted = chrome.until(&page, "document.querySelectorAll('#chats .row').length === 1 && !!document.querySelector('#frames iframe:not([hidden])')", wait);
    let mine = api.signed(&owner, "GET", "/api/fragments", None)?;
    let chat = mine.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).find(|n| n.starts_with("chat-")).unwrap_or("").to_string();
    s.ok("New chat makes a chat fragment and opens it in the middle", chatted && !chat.is_empty(), &mine);
    let chat_title = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}.", label(&chat)), "document.title").ok() == Some(json!("Chat")));
    s.ok("the chat's own page shows there, signed in on its origin", chat_title, "");
    let said = s.eventually(wait, || {
        chrome.eval_in_frame(&page, &format!("{}.", label(&chat)), "document.getElementById('text').value = 'hi from the desktop'; document.getElementById('say').requestSubmit(); true").is_ok()
    });
    let me = api.identity(&owner)?;
    let landed = s.eventually(wait, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)
            .is_ok_and(|r| r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["body"]["text"] == "hi from the desktop" && x["principal"] == me.as_str())))
    });
    s.ok("a message sent in it is the owner's", said && landed, api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)?);

    // apps and files open into the viewer, newest on top
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{todo}")))?;
    s.ok("an app opens as a pane", chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={:?}]')", format!("app:{todo}")), wait), "");
    let todo_title = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}.", label(&todo)), "document.title").ok() == Some(json!("Todo")));
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
