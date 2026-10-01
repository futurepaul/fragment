//! The desktop template in a browser (docs/phase-6.md, step 3; ROADMAP
//! phase 6's acceptance): its owner's apps and files open into the viewer,
//! panes reorder and close, both sides collapse, it works at phone width,
//! and the layout survives a reload. Every frame is a fragment of the
//! owner's, signed in on its own origin through the desktop's `__frame`:
//! a desktop made with the platform's new-fragment form may, with no visit
//! to the share sheet (the form's submit is the grant; a P0 when it was
//! not), and is its owner's alone. A chat is what New chat names one. An
//! app pane's header shows its sharing and who else has it open, which the
//! desktop reads from `__presence`. Then the files viewer (`__files`) as a
//! page of its own.

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use fragment_fakes::openrouter::Reply;

use super::signin::site_cookie;
use super::templates::{person, post_form};
use crate::api::{url_enc, Api, Call, Socket};
use crate::browser::{Browser, Page};
use crate::Suite;

/// A picture (1×1 PNG) an answer shows.
pub(super) const DOT_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// In a frame of the files viewer (`__files`): opens notes/hello.txt from
/// its tree, in its reader.
pub(super) const OPEN_HELLO: &str = "(() => { const a = document.querySelector('.row.file[data-path=\"notes/hello.txt\"]'); if (!a) return false; a.click(); return true; })()";
/// Then its bar's button: the page around it opens the file as a pane.
pub(super) const POP_OUT: &str = "(() => { const b = document.getElementById('pop'); if (!b || b.hidden || !document.querySelector('#reader pre.code')) return false; b.click(); return true; })()";

/// A markdown file whose HTML must stay text in the files viewer.
const HOSTILE_MD: &str = r#"# Not a page
<script>window.pwned = "script"</script>
<img src=x onerror="window.pwned = 'img'">
<iframe src="javascript:parent.pwned = 'frame'"></iframe>
<svg onload="window.pwned = 'svg'"></svg>
[a link](javascript:window.pwned='link') and ![a picture](javascript:window.pwned='picture')
"#;

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

/// Runs on a node restarted with the platform and the fragments on two domains
/// (as fragment.club is), then restarts it as it was for the lanes after.
pub fn desktop(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("desktop") {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let asked = presence(s, &api);
    let updated = template(s, &api);
    let result = run(s, &api);
    drop(api);
    s.stop()?;
    s.start(false, true)?;
    asked.and(updated).and(result)
}

/// `__presence`: who has the owner's apps open, for the desktop's panes,
/// to its owner alone: every signed-in principal with a live socket there
/// once (none of them shares presence), an anonymous visitor as a count,
/// and only the owner's own fragments.
fn presence(s: &mut Suite, api: &Api) -> Result<()> {
    let (owner, session) = person(api)?;
    let (guest, guest_session) = person(api)?;
    let make = |keys: &fragment_nip98::Keys, label: &str, template: &str| -> Result<String> {
        let name = api.qualified(keys, label)?;
        let r = api.create_with(keys, json!({ "name": name, "template": template }))?;
        anyhow::ensure!(r.status == 200, "making {name}: {r}");
        Ok(name)
    };
    let desk = make(&owner, &s.name("pdesk"), "desktop")?;
    let app = make(&owner, &s.name("papp"), "blank")?;
    let theirs = make(&guest, &s.name("ptheirs"), "blank")?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{app}/visibility"), Some(&json!({ "visibility": "public" })))?;
    anyhow::ensure!(r.status == 200, "making the app public: {r}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{desk}/members/{}", api.identity(&guest)?), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "adding an editor to the desktop: {r}");
    let ask = |cookie: Option<&str>, on: &str, names: &[&str]| -> Result<crate::api::Reply> {
        let query: Vec<String> = names.iter().map(|n| format!("name={}", url_enc(n))).collect();
        let cookie = cookie.map(|c| format!("fragment_site={c}"));
        api.call(Call { method: "GET", url: api.site_url(on, &format!("__presence?{}", query.join("&"))), cookie, ..Call::default() })
    };
    let owner_site = site_cookie(api, &session, &desk)?;
    let r = ask(Some(&owner_site), &desk, &[&app])?;
    s.ok("__presence: an app no one has open has no one in it", r.status == 200 && r.body["presence"][&app] == json!({ "people": [], "anonymous": 0 }), &r);

    // the owner and a guest, the guest on two pages, and a visitor signed out
    let mut open = vec![];
    for keys in [Some(&owner), Some(&guest), Some(&guest), None] {
        let mut socket = Socket::open(api, &app, "__live", keys, None)?;
        socket.until("hello", 5)?;
        open.push(socket);
    }
    let r = ask(Some(&owner_site), &desk, &[&app, &theirs])?;
    let here = &r.body["presence"][&app];
    let mut people = vec![api.identity(&owner)?, api.identity(&guest)?];
    people.sort();
    s.ok("two people with sockets open to the owner's app are in it, each once, though neither shares presence", r.status == 200 && here["people"] == json!(people), &r);
    s.ok("a visitor signed out is counted, not named", here["anonymous"] == 1 && !here["people"].to_string().contains("anon:"), &r);
    s.ok("another person's fragment is left out", r.body["presence"].get(&theirs).is_none(), &r);
    let many: Vec<String> = (0..9).map(|i| format!("{app}{i}")).collect();
    let r = ask(Some(&owner_site), &desk, &many.iter().map(String::as_str).collect::<Vec<_>>())?;
    s.ok("more than 8 names are refused", r.status == 400, &r);
    let r = ask(Some(&site_cookie(api, &guest_session, &desk)?), &desk, &[&app])?;
    s.ok("an editor of the desktop, not its owner, is refused", r.status == 403, &r);
    let r = ask(None, &desk, &[&app])?;
    s.ok("so is someone signed out", r.status == 403 || r.status == 401, &r);
    let r = ask(Some(&site_cookie(api, &session, &app)?), &app, &[&app])?;
    s.ok("and a page that does not ask for the fragments capability, even to its owner", r.status == 403, &r);
    let r = api.signed(&guest, "GET", &format!("/api/f/{app}/presence"), None)?;
    s.ok("an app answers who has it open to its owner alone", r.status == 403, &r);

    for socket in open {
        socket.close();
    }
    let gone = s.eventually(Duration::from_secs(10), || ask(Some(&owner_site), &desk, &[&app]).is_ok_and(|r| r.body["presence"][&app] == json!({ "people": [], "anonymous": 0 })));
    s.ok("their pages closed, no one is in it", gone, ask(Some(&owner_site), &desk, &[&app])?);
    Ok(())
}

/// `__template`: whether a desktop's own files are its template's latest,
/// to its owner alone, and the update, which commits the template's files
/// over its own (the owner's other files stay) and deploys them, once. The
/// same at `/api/f/{name}/template`, for the CLI.
fn template(s: &mut Suite, api: &Api) -> Result<()> {
    let (owner, session) = person(api)?;
    let (guest, guest_session) = person(api)?;
    let desk = api.qualified(&owner, &s.name("udesk"))?;
    let r = api.create_with(&owner, json!({ "name": desk, "template": "desktop" }))?;
    anyhow::ensure!(r.status == 200, "making {desk}: {r}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{desk}/members/{}", api.identity(&guest)?), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "adding an editor to the desktop: {r}");
    let owner_site = site_cookie(api, &session, &desk)?;
    let ask = |method: &str, cookie: Option<&str>, content_type: Option<&'static str>| {
        let (cookie, body) = (cookie.map(|c| format!("fragment_site={c}")), content_type.map(|_| b"{}".to_vec()));
        api.call(Call { method, url: api.site_url(&desk, "__template"), cookie, content_type, body, ..Call::default() })
    };
    let r = ask("GET", Some(&owner_site), None)?;
    s.ok("__template: a new desktop holds its template's latest files", r.status == 200 && r.body == json!({ "template": "desktop", "upToDate": true, "changed": [] }), &r);
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": desk, "op": "forget-template" })))?;
    anyhow::ensure!(r.status == 200, "forgetting its template: {r}");
    let r = ask("GET", Some(&owner_site), None)?;
    s.ok("one made before the platform kept its template says the same, from its template event", r.status == 200 && r.body["template"] == "desktop" && r.body["upToDate"] == true, &r);

    // one of its template's files changed, and one of the owner's own added, deployed
    let (css, mine) = ("site/desktop.css", "notes/mine.md");
    let file = |path: &str| api.signed(&owner, "GET", &format!("/api/f/{desk}/file?path={path}"), None);
    let change = |text: &str| -> Result<()> {
        let r = api.signed(&owner, "POST", &format!("/api/f/{desk}/files"), Some(&json!({ "files": [{ "path": css, "text": text }, { "path": mine, "text": "# mine" }] })))?;
        anyhow::ensure!(r.status == 200, "writing to the desktop: {r}");
        let r = api.signed(&owner, "POST", &format!("/api/f/{desk}/deploy"), None)?;
        anyhow::ensure!(r.status == 200, "deploying the desktop: {r}");
        Ok(())
    };
    let original = file(css)?;
    change("/* the owner's */")?;
    let r = ask("GET", Some(&owner_site), None)?;
    s.ok("a template file changed on main and deployed: not up to date, naming that file (not the owner's own)", r.status == 200 && r.body["upToDate"] == false && r.body["changed"] == json!([css]), &r);

    let r = ask("GET", Some(&site_cookie(api, &guest_session, &desk)?), None)?;
    s.ok("an editor of the desktop, not its owner, is refused", r.status == 403, &r);
    let r = ask("GET", None, None)?;
    s.ok("so is someone signed out", r.status == 401 || r.status == 403, &r);
    let r = ask("POST", Some(&owner_site), Some("text/plain"))?;
    s.ok("an update not sent as JSON (a cross-site form's) is refused", r.status == 400, &r);
    let r = api.signed(&guest, "POST", &format!("/api/f/{desk}/template"), None)?;
    s.ok("and through the API, anyone but its owner", r.status == 403, &r);

    let before = api.status(&owner, &desk)?;
    let r = ask("POST", Some(&owner_site), Some("application/json"))?;
    let after = api.status(&owner, &desk)?;
    let pins = |st: &crate::api::Reply| st.body["pins"].clone();
    s.ok(
        "its owner's update brings it up to date: a commit to main, deployed",
        r.status == 200 && r.body["upToDate"] == true && pins(&after)["main"] != pins(&before)["main"] && pins(&after)["live"] == pins(&after)["main"],
        format!("{r} / {after}"),
    );
    let (restored, kept) = (file(css)?, file(mine)?);
    s.ok("the file is the template's again, and the owner's own file stays", restored.status == 200 && restored.bytes == original.bytes && kept.text == "# mine", format!("{restored} / {kept}"));
    let r = ask("POST", Some(&owner_site), Some("application/json"))?;
    let again = api.status(&owner, &desk)?;
    s.ok("the update again commits nothing", r.status == 200 && r.body["upToDate"] == true && pins(&again) == pins(&after), &again);
    let events = api.signed(&owner, "GET", &format!("/api/f/{desk}/events?tail=100"), None)?;
    let logged = events.body["events"].as_array().into_iter().flatten().filter(|e| e["kind"] == "template.update").count();
    s.ok("and one template.update event says so", logged == 1, &events);
    change("/* the owner's, again */")?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{desk}/template"), None)?;
    s.ok("changed again, the update (here through the API, its owner's) updates it again", r.status == 200 && r.body["upToDate"] == true && file(css)?.bytes == original.bytes, &r);

    let plain = api.qualified(&owner, &s.name("uplain"))?;
    let r = api.create_with(&owner, json!({ "name": plain }))?;
    anyhow::ensure!(r.status == 200, "making {plain}: {r}");
    let r = api.signed(&owner, "GET", &format!("/api/f/{plain}/template"), None)?;
    s.ok("a fragment made from no template has none", r.status == 200 && r.body == json!({ "template": null }), &r);
    Ok(())
}

fn run(s: &mut Suite, api: &Api) -> Result<()> {
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the desktop lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    if api.suffix.is_none() {
        s.ok("the desktop lane runs with fragment hosts", false, "no suffix");
        return Ok(());
    }
    let (owner, session) = person(api)?;
    let make = |label: &str, template: &str| -> Result<String> {
        let name = api.qualified(&owner, label)?;
        let r = api.create_with(&owner, json!({ "name": name, "template": template }))?;
        anyhow::ensure!(r.status == 200, "making {name}: {r}");
        Ok(name)
    };
    // the desktop, made as a person makes one: the platform's form
    let desk_label = s.name("desk");
    let r = post_form(api, "/auth/new", &format!("label={}&template=desktop", url_enc(&desk_label)), &session, &api.base)?;
    anyhow::ensure!(r.status == 302, "making the desktop with the form: {r}");
    let desk = api.qualified(&owner, &desk_label)?;
    let todo = make(&s.name("dtodo"), "todo")?;
    let notes = make(&s.name("dnotes"), "blank")?;
    let files = json!([
        { "path": "notes/hello.txt", "text": "hello from a file" },
        { "path": "notes/start.md", "text": "---\ntitle: front matter\n---\n# Start\n\nSee [[linked]], or [the same](linked.md).\n" },
        { "path": "notes/linked.md", "text": "# Linked\n\n- one\n  - nested\n" },
        { "path": "notes/hostile.md", "text": HOSTILE_MD },
        { "path": "code/app.js", "text": "const answer = 42;\n\nconsole.log(answer);\n" },
        { "path": "pics/dot.png", "base64": DOT_PNG },
    ]);
    let r = api.signed(&owner, "POST", &format!("/api/f/{notes}/files"), Some(&json!({ "files": files })))?;
    anyhow::ensure!(r.status == 200, "writing files: {r}");
    let label = |name: &str| name.split('.').next().unwrap_or("").to_string();
    let wait = Duration::from_secs(20);
    let st = api.status(&owner, &desk)?;
    s.ok(
        "a desktop made with the platform's form may show its owner's fragments inside it: the form's submit was their grant (no share sheet)",
        st.body["frame"] == json!(true),
        &st,
    );
    s.ok("and a new desktop is its owner's alone (members only)", st.body["visibility"] == "members", &st);
    let todo_st = api.status(&owner, &todo)?;
    s.ok("(other templates keep theirs: a todo opens to anyone with its link)", todo_st.body["visibility"] == "link", &todo_st);

    // a chat made elsewhere (the API, another desktop), under any name, is a
    // chat here too: its manifest says so (its kind, in its owner's list)
    let elsewhere = make(&s.name("talk"), "chat")?;

    // signed in on the platform, the browser walks to the desktop's origin
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = chrome.open(&api.site_url(&desk, "__signin?return=/"))?;
    chrome.viewport(&page, 1440, 900, false)?;
    let listed = format!("[...document.querySelectorAll('#apps .row .label')].map(l => l.textContent).includes({:?})", label(&todo));
    s.ok("the owner's desktop lists their apps", chrome.until(&page, &listed, wait), chrome.eval(&page, "document.body.innerText.slice(0, 300)").unwrap_or_default());
    s.ok("but not itself", chrome.eval(&page, &format!("![...document.querySelectorAll('#apps .row .label')].map(l => l.textContent).includes({:?})", label(&desk)))? == json!(true), "");
    let sidebar = |chrome: &mut Browser| chrome.eval(&page, "[...document.querySelectorAll('#chats .row, #apps .row')].map(r => r.dataset.key)").unwrap_or_default();
    let sorted = format!("!!document.querySelector('#chats .row[data-key=\"chat:{elsewhere}\"]') && !document.querySelector('#apps .row[data-key=\"app:{elsewhere}\"]')");
    s.ok("a chat made elsewhere, whatever its name, is listed under Chats, not Apps: its manifest says it is one", chrome.until(&page, &sorted, wait), sidebar(&mut chrome));
    let mine = api.signed(&owner, "GET", "/api/fragments", None)?;
    let kind = |name: &str| mine.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == name).map(|f| f["kind"].clone()).unwrap_or_default();
    s.ok(
        "and its owner's list says what each is: a chat and who answers it, an app",
        kind(&elsewhere) == json!({ "chat": { "answers": "agent" } }) && kind(&todo) == json!({}),
        format!("{} / {}", kind(&elsewhere), kind(&todo)),
    );

    // Clickjacking: a fragment's page (its author's code, or an agent's)
    // frames the platform's approval of a key of its own. The two are one
    // site, so the owner's platform session rides into the frame; the
    // platform refuses to be framed, so nothing shows there to lay under a
    // click. Its sign-in redirects still run in a frame, but what they mint
    // is a top-level page's, which a frame's `__signin` refuses (and spends):
    // a frame signs in only through the page that frames it (`__frame`).
    let attacker = chrome.open(&api.site_url(&todo, "__signin?return=/"))?;
    let on_todo = format!("location.host.startsWith({:?}) && location.pathname === '/' && document.readyState === 'complete'", format!("{}--", label(&todo)));
    anyhow::ensure!(chrome.until(&attacker, &on_todo, wait), "the todo's page did not open");
    let frame = |chrome: &mut Browser, src: &str| chrome.eval(&attacker, &format!("(() => {{ const f = document.createElement('iframe'); f.src = {src:?}; document.body.append(f); return true; }})()"));
    frame(&mut chrome, &format!("{}/auth/fragment?name={notes}&return=/", api.base))?;
    let signed_in = s.eventually(Duration::from_secs(5), || {
        chrome.eval_in_frame(&attacker, &format!("{}--", label(&notes)), "location.pathname === '/' && document.readyState === 'complete'").ok() == Some(json!(true))
    });
    s.ok("a fragment's page that frames the platform's sign-in for another fragment gets no session there: the frame's __signin refuses a top-level page's redemption", !signed_in, "");
    let key = fragment_nip98::Keys::generate();
    frame(&mut chrome, &api.approval_link(&key, 0))?;
    let approval = |chrome: &mut Browser| chrome.eval_in_frame(&attacker, "/cli?key=", "document.body?.innerText ?? ''").ok().and_then(|v| v.as_str().map(str::to_string));
    let shown = s.eventually(Duration::from_secs(5), || approval(&mut chrome).is_some_and(|t| t.contains("Add this key")));
    s.ok("but a fragment's page that frames the platform's key approval gets a frame without it", !shown, format!("{:?}", approval(&mut chrome)));
    chrome.close(attacker)?;

    // a chat: a chat fragment of the owner's, in the middle column, with
    // the owner's agent in it (its model is the OpenRouter fake, scripted:
    // a greeting, then an app from a template when asked for one)
    let app_label = s.name("dlist");
    let app = api.qualified(&owner, &app_label)?;
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Text("Hi! I'm your agent.".into()),
        Reply::Tools(vec![("platform__create_fragment".into(), json!({ "label": app_label, "template": "todo" }))]),
        Reply::Text("Your list is in your apps.".into()),
        Reply::Text("Here it is.\n\n![a picture](__file?path=pics/dot.png)".into()),
    ]);
    chrome.eval(&page, "document.getElementById('new-chat').click(); true")?;
    let chatted = chrome.until(
        &page,
        &format!("document.querySelectorAll('#chats .row').length === 2 && document.querySelector('#chats .row')?.dataset.key !== 'chat:{elsewhere}' && !!document.querySelector('#frames iframe:not([hidden])')?.dataset.fragment?.startsWith('chat-')"),
        wait,
    );
    let mine = api.signed(&owner, "GET", "/api/fragments", None)?;
    let chat = mine.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).find(|n| n.starts_with("chat-") && *n != elsewhere).unwrap_or("").to_string();
    s.ok("New chat makes a chat fragment and opens it in the middle, first among the chats", chatted && !chat.is_empty(), format!("{mine} {}", sidebar(&mut chrome)));
    let chat_title = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.title").ok() == Some(json!("Chat")));
    s.ok("the chat's own page shows there, signed in on its origin", chat_title, "");
    // the page is ready once it knows who it is (its socket said hello)
    let ready = s.eventually(wait, || {
        chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('say')?.dataset.ready === '1'").ok() == Some(json!(true))
    });
    let said = ready
        && chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('text').value = 'hi from the desktop'; document.getElementById('say').requestSubmit(); true").is_ok();
    let me = api.identity(&owner)?;
    let landed = s.eventually(wait, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)
            .is_ok_and(|r| r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["body"]["text"] == "hi from the desktop" && x["principal"] == me.as_str())))
    });
    s.ok("a message sent in it is the owner's: its frame is signed in as them, and the share sheet was never opened", said && landed, api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)?);
    let in_chat = |chrome: &mut Browser, text: &str| {
        chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('messages').textContent").ok().and_then(|v| v.as_str().map(|t| t.contains(text))) == Some(true)
    };
    s.ok("the owner's agent answers in it", s.eventually(wait, || in_chat(&mut chrome, "Hi! I'm your agent.")), "");
    let mine = |chrome: &mut Browser| {
        chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "[...document.querySelectorAll('.msg.agent > .who')].some(w => w.textContent === 'Your agent' && !!w.querySelector('.face.agent'))").ok() == Some(json!(true))
    };
    s.ok("named, to its owner, as their own agent, with its face", s.eventually(wait, || mine(&mut chrome)), "");

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
    chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('text').value = 'make me a todo list'; document.getElementById('say').requestSubmit(); true")?;
    s.ok("asked for an app in the chat, the agent says it made one", s.eventually(Duration::from_secs(40), || in_chat(&mut chrome, "Your list is in your apps.")), "");
    let shown = format!("[...document.querySelectorAll('#apps .row .label')].map(l => l.textContent).includes({app_label:?})");
    s.ok("and it appears in the desktop's sidebar, with no reload", chrome.until(&page, &shown, wait), chrome.eval(&page, "document.getElementById('apps').innerText").unwrap_or_default());
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{app}")))?;
    let made = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{app_label}--"), "document.title").ok() == Some(json!("Todo")));
    s.ok("opened, it is the list the agent made", made, "");
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Close]').click(); true", format!("app:{app}")))?;

    // a picture in an answer (a screenshot, as a computer's land) opens in
    // the viewer: the chat's page asks the desktop around it to open it
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat}/files"), Some(&json!({ "files": [{ "path": "pics/dot.png", "base64": DOT_PNG }] })))?;
    anyhow::ensure!(r.status == 200, "writing a picture to the chat: {r}");
    chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "document.getElementById('text').value = 'show me a picture'; document.getElementById('say').requestSubmit(); true")?;
    let pictured = s.eventually(wait, || {
        chrome.eval_in_frame(&page, &format!("{}--", label(&chat)), "(() => { const i = document.querySelector('img.shot'); if (!i || !i.complete) return false; i.click(); return true; })()").ok() == Some(json!(true))
    });
    let viewed = pictured && chrome.until(&page, "[...document.querySelectorAll('.pane')].some(p => p.dataset.key.startsWith('file:') && p.dataset.key.includes('pics/dot.png'))", wait);
    s.ok("a picture in an answer opens in the viewer when clicked", viewed, format!("{:?}", panes(&mut chrome, &page)));
    chrome.eval(&page, "document.querySelector('.pane[data-key^=\"file:\"] .pane-action[title=Close]')?.click(); true")?;

    // New computer: a pet fragment, listed under Computers and open as a
    // pane, its own page, which is what keeps a computer awake (the sprites
    // lane checks that on a computer that paired; the templates lane, your
    // agent running a command on one). A computer here does not pair: its
    // CLI would reach the platform at fragment.localhost, which a runner's
    // resolver need not know (the harness and Chrome never ask it).
    chrome.eval(&page, "document.getElementById('new-computer').click(); true")?;
    let opened = chrome.until(&page, "!!document.querySelector('.pane[data-key^=\"app:computer-\"]')", wait);
    let mine = api.signed(&owner, "GET", "/api/fragments", None)?;
    let computer = mine.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).find(|n| n.starts_with("computer-")).unwrap_or("").to_string();
    let listed = format!("!!document.querySelector('#computers .row[data-key=\"app:{computer}\"]') && !document.querySelector('#apps .row[data-key=\"app:{computer}\"]')");
    s.ok(
        "New computer makes a pet fragment, lists it under Computers (not Apps), and opens it as a pane",
        opened && !computer.is_empty() && chrome.until(&page, &listed, wait),
        format!("{mine} {}", chrome.eval(&page, "document.getElementById('computers').innerText").unwrap_or_default()),
    );
    let pet = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}--", label(&computer)), "document.title").ok() == Some(json!("Pet")));
    let st = api.status(&owner, &computer)?;
    s.ok("the pane is the computer's own page, signed in on its origin; the computer is its owner's alone", pet && st.body["visibility"] == "members", &st);
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Close]').click(); true", format!("app:{computer}")))?;
    let gone = chrome.until(&page, &format!("![...document.querySelectorAll('iframe')].some(f => f.src.includes({:?}))", format!("name={}", url_enc(&computer))), wait);
    s.ok("its pane closed, its page is gone: nothing here keeps it awake", gone, "");

    // apps and files open into the viewer, newest on top; a guest of the
    // todo has it open meanwhile
    let (guest, _) = person(api)?;
    let r = api.signed(&owner, "PUT", &format!("/api/f/{todo}/members/{}", api.identity(&guest)?), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a guest to the todo: {r}");
    let mut guest_page = Socket::open(api, &todo, "__live", Some(&guest), None)?;
    guest_page.until("hello", 5)?;
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{todo}")))?;
    s.ok("an app opens as a pane", chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={:?}]')", format!("app:{todo}")), wait), "");
    let todo_title = s.eventually(wait, || chrome.eval_in_frame(&page, &format!("{}--", label(&todo)), "document.title").ok() == Some(json!("Todo")));
    s.ok("the app's own page is in it", todo_title, "");
    let head = format!(".pane[data-key=\"app:{todo}\"] .pane-head");
    let headed = format!(
        "(() => {{ const h = document.querySelector({head:?}); const p = h?.querySelector('.presence'); \
         return !!h?.querySelector('.pane-action[title=\"Share…\"]') && h.querySelector('.pane-sharing').textContent.includes('Anyone with the link') \
         && !!p && !p.hidden && p.querySelectorAll('.av').length === 1 && p.title.includes({:?}); }})()",
        api.username(&guest)?
    );
    s.ok(
        "its header says who may open it, has Share, and shows who else has it open: the guest, not the owner, whose page it is",
        chrome.until(&page, &headed, wait),
        chrome.eval(&page, &format!("document.querySelector({head:?})?.outerHTML")).unwrap_or_default(),
    );
    share_in_a_dialog(s, api, &mut chrome, &page, &owner, &desk, &todo)?;
    chrome.eval(&page, &format!("[...document.querySelectorAll('#apps .row')].find(r => r.dataset.key === {:?}).click(); true", format!("app:{notes}")))?;
    chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={:?}]')", format!("app:{notes}")), wait);
    chrome.eval(&page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=Files]').click(); true", format!("app:{notes}")))?;
    let tree_key = format!("tree:{notes}");
    s.ok("an app's files open as a pane", chrome.until(&page, &format!("!!document.querySelector('.pane[data-key={tree_key:?}]')"), wait), "");
    let clicked = s.eventually(wait, || chrome.eval_in_frame(&page, "/__files", OPEN_HELLO).ok() == Some(json!(true)));
    let read = clicked && s.eventually(wait, || chrome.eval_in_frame(&page, "/__files", "document.querySelector('#reader pre.code')?.textContent === 'hello from a file'").ok() == Some(json!(true)));
    s.ok("the pane is the files viewer: a file in its tree opens in its reader", read, "");
    let popped = s.eventually(wait, || chrome.eval_in_frame(&page, "/__files", POP_OUT).ok() == Some(json!(true)));
    let file_open = popped && chrome.until(&page, "[...document.querySelectorAll('.pane')].some(p => p.dataset.key.startsWith('file:'))", wait);
    s.ok("and its bar opens the file as a pane of its own", file_open, format!("{:?}", panes(&mut chrome, &page)));
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
    update_in_browser(s, api, &mut chrome, &page, &owner, &desk)?;
    chrome.close(page)?;
    files_viewer(s, api, &mut chrome, &owner, &notes)
}

/// Share on an app pane: the platform's sheet in a dialog on the desktop,
/// signed in as its owner through `__share` (the platform's session never
/// reaches a frame of the desktop's site), its cookie partitioned under
/// the desktop; a change made in it takes effect, and Done closes it, the
/// pane's header following.
fn share_in_a_dialog(s: &mut Suite, api: &Api, chrome: &mut Browser, page: &Page, owner: &Keys, desk: &str, app: &str) -> Result<()> {
    let wait = Duration::from_secs(20);
    let sheet = format!("/share/{app}");
    let in_sheet = |chrome: &mut Browser, js: &str| chrome.eval_in_frame(page, &sheet, js).ok() == Some(json!(true));
    chrome.eval(page, &format!("document.querySelector('.pane[data-key={:?}] .pane-action[title=\"Share…\"]').click(); true", format!("app:{app}")))?;
    let opened = chrome.until(page, "document.getElementById('sheet').open", wait);
    let signed_in = opened && s.eventually(wait, || in_sheet(chrome, "document.body.innerText.includes('General access') && !!document.querySelector('input[name=username]')"));
    // its height came as a message to this origin, from the sheet's frame alone
    let sized = signed_in && chrome.until(page, "parseFloat(document.querySelector('#sheet iframe').style.height) > 0", wait);
    let desk_host = api.site_url(desk, "").split("//").nth(1).and_then(|h| h.split(':').next()).unwrap_or("").to_string();
    let cookie = chrome.cookies()?.into_iter().find(|c| c["name"] == "fragment_share");
    let partitioned = cookie.as_ref().is_some_and(|c| {
        c["path"] == sheet.as_str() && c["partitionKey"]["topLevelSite"].as_str().is_some_and(|t| desk_host.ends_with(t.trim_start_matches("http://")))
    });
    chrome.screenshot(page, &s.scratch.join("desktop-share.png"))?;
    s.ok(
        "Share on an app pane opens the platform's sheet in a dialog on the desktop, signed in as its owner (their controls; the sheet told this origin its height), its cookie on the sheet's path, partitioned under the desktop",
        signed_in && sized && partitioned,
        json!({ "cookie": cookie, "sheet": chrome.eval_in_frame(page, &sheet, "document.body.innerText.slice(0, 300)").unwrap_or_default() }),
    );
    let select = "document.querySelector('select[name=visibility]')";
    let armed = s.eventually(wait, || in_sheet(chrome, &format!("(s => !!s && !s.disabled)({select})")));
    let chose = armed && in_sheet(chrome, &format!("(s => {{ s.value = 'public'; s.dispatchEvent(new Event('change')); return true; }})({select})"));
    let public = chose && s.eventually(wait, || api.status(owner, app).is_ok_and(|r| r.body["visibility"] == "public"));
    let again = public && s.eventually(wait, || in_sheet(chrome, &format!("{select}?.value === 'public'")));
    s.ok("changing its General access to Public takes effect, and the sheet shows it again, still signed in", public && again, api.status(owner, app)?);
    let done = in_sheet(chrome, "(document.querySelector('button[data-done]').click(), true)");
    let closed = done && chrome.until(page, "!document.getElementById('sheet').open", wait);
    let header = format!("document.querySelector('.pane[data-key={:?}] .pane-sharing')?.textContent.includes('Public')", format!("app:{app}"));
    s.ok(
        "its Done closes the dialog, and the pane's header follows (the list read again): Public",
        closed && chrome.until(page, &header, wait),
        chrome.eval(page, &format!("document.querySelector('.pane[data-key={:?}] .pane-head')?.outerHTML", format!("app:{app}"))).unwrap_or_default(),
    );
    Ok(())
}

/// The files viewer as a page of its own (`__files`): the tree and a reader
/// for markdown, text, and pictures; a file's HTML is only ever text; it
/// follows writes to main; and it works at phone width.
fn files_viewer(s: &mut Suite, api: &Api, chrome: &mut Browser, owner: &Keys, notes: &str) -> Result<()> {
    let wait = Duration::from_secs(20);
    let page = chrome.open(&api.site_url(notes, "__signin?return=/__files"))?;
    chrome.viewport(&page, 1280, 800, false)?;
    let row = |path: &str| format!("!!document.querySelector('.row.file[data-path={path:?}]')");
    let landed = chrome.until(&page, "document.querySelector('#reader h1')?.textContent === 'blank'", wait);
    let listed = chrome.eval(&page, &format!("{} && {} && !document.querySelector('.row.file[data-path=\"fragment.json\"]')", row("notes/hello.txt"), row("code/app.js")))? == json!(true);
    s.ok("__files lists the fragment's files (not its machinery) and opens its README", landed && listed, chrome.eval(&page, "document.body.innerText.slice(0, 400)").unwrap_or_default());

    let open = |chrome: &mut Browser, path: &str| chrome.eval(&page, &format!("location.hash = '#/{path}'; true"));
    open(chrome, "notes/start.md")?;
    let rendered = chrome.until(&page, "!!document.querySelector('#reader a.wikilink[data-path=\"notes/linked.md\"]') && !document.getElementById('reader').textContent.includes('front matter')", wait);
    chrome.eval(&page, "document.querySelector('#reader a.wikilink').click(); true")?;
    let followed = chrome.until(&page, "location.hash === '#/notes/linked.md' && document.querySelector('#reader h1')?.textContent === 'Linked' && !!document.querySelector('#reader li ul li')", wait);
    s.ok("markdown renders without its front matter, and a [[wikilink]] opens the file it names", rendered && followed, chrome.eval(&page, "location.hash + ' ' + document.getElementById('reader').innerHTML.slice(0, 400)").unwrap_or_default());

    let write = json!({ "files": [{ "path": "notes/linked.md", "text": "# Linked, again\n" }, { "path": "notes/later.md", "text": "# Later\n" }] });
    let r = api.signed(owner, "POST", &format!("/api/f/{notes}/files"), Some(&write))?;
    let live = r.status == 200 && chrome.until(&page, &format!("document.querySelector('#reader h1')?.textContent === 'Linked, again' && {}", row("notes/later.md")), wait);
    s.ok("while it is open, a write to main shows: the open file re-read, a new file in the tree (__watch)", live, &r);

    open(chrome, "code/app.js")?;
    let lines = chrome.until(&page, "[...document.querySelectorAll('#reader pre.code .line')].map(l => l.textContent).join('|') === 'const answer = 42;||console.log(answer);'", wait);
    let numbered = chrome.eval(&page, "getComputedStyle(document.querySelector('#reader pre.code .line'), '::before').content")?;
    s.ok("a .js file shows as text, a numbered row to each line", lines && numbered.as_str().is_some_and(|c| c.contains("counter")), &numbered);

    open(chrome, "pics/dot.png")?;
    s.ok("a picture shows inline", chrome.until(&page, "document.querySelector('#reader .media img')?.naturalWidth === 1", wait), "");

    open(chrome, "notes/hostile.md")?;
    let shown = chrome.until(&page, "(document.querySelector('#reader .doc')?.textContent ?? '').includes('<script>window.pwned')", wait);
    std::thread::sleep(Duration::from_millis(500));
    let inert = "window.pwned === undefined && !document.querySelector('#reader .doc :is(script, iframe, svg, [onerror], [onload], a[href^=\"javascript\"], img[src^=\"javascript\"])')";
    let safe = chrome.eval(&page, inert)? == json!(true);
    s.ok("a file's HTML shows as text and never runs: no script, handler, frame, or javascript: link", shown && safe, chrome.eval(&page, "document.querySelector('#reader .doc')?.innerHTML").unwrap_or_default());
    chrome.screenshot(&page, &s.scratch.join("files.png"))?;

    chrome.viewport(&page, 390, 844, true)?;
    let tucked = chrome.until(&page, "document.querySelector('.side').getBoundingClientRect().right <= 0", wait);
    chrome.eval(&page, "document.getElementById('menu').click(); true")?;
    let shown = chrome.until(&page, "document.querySelector('.side').getBoundingClientRect().left >= 0", wait);
    chrome.eval(&page, "document.querySelector('.row.file[data-path=\"notes/hello.txt\"]').click(); true")?;
    let read = chrome.until(&page, "document.querySelector('#reader pre.code')?.textContent === 'hello from a file' && !document.getElementById('files').classList.contains('nav')", wait);
    let fits = chrome.eval(&page, "document.documentElement.scrollWidth <= innerWidth")? == json!(true);
    chrome.screenshot(&page, &s.scratch.join("files-phone.png"))?;
    s.ok("at phone width the tree waits behind a button, a file opens from it, and nothing scrolls sideways", tucked && shown && read && fits, "");
    chrome.close(page)?;
    Ok(())
}

/// "Update available" at the foot of the desktop's sidebar, while one of
/// its template's files is not the template's; its Update, confirmed in
/// place, brings the desktop up to date and reloads the page.
fn update_in_browser(s: &mut Suite, api: &Api, chrome: &mut Browser, page: &Page, owner: &Keys, desk: &str) -> Result<()> {
    let wait = Duration::from_secs(20);
    let r = api.signed(owner, "POST", &format!("/api/f/{desk}/files"), Some(&json!({ "files": [{ "path": "README.md", "text": "# the owner's words\n" }] })))?;
    anyhow::ensure!(r.status == 200, "writing to the desktop: {r}");
    let r = api.signed(owner, "POST", &format!("/api/f/{desk}/deploy"), None)?;
    anyhow::ensure!(r.status == 200, "deploying the desktop: {r}");
    chrome.viewport(page, 1440, 900, false)?;
    chrome.reload(page)?;
    let asked = chrome.until(page, "!document.getElementById('update').hidden", wait);
    chrome.eval(page, "document.getElementById('layout').classList.contains('left-open') || document.getElementById('toggle-left').click(); true")?;
    let shown = asked && chrome.until(page, "document.getElementById('update-pill').offsetParent !== null", wait);
    chrome.screenshot(page, &s.scratch.join("desktop-update.png"))?;
    s.ok("with one of its template's files not the template's, the desktop's sidebar says an update is available", shown, chrome.eval(page, "document.getElementById('sidebar').innerText").unwrap_or_default());
    chrome.click(page, "#update-pill")?;
    let confirm = "!document.getElementById('update-confirm').hidden && document.getElementById('update-text').textContent.startsWith('Update this desktop to the latest version?')";
    let confirming = chrome.until(page, confirm, wait);
    chrome.screenshot(page, &s.scratch.join("desktop-update-confirm.png"))?;
    chrome.eval(page, "window.beforeUpdate = true; true")?;
    chrome.click(page, "#update-go")?;
    let reloaded = chrome.until(page, "window.beforeUpdate === undefined && document.querySelectorAll('#apps .row').length > 0", wait);
    let current = api.signed(owner, "GET", &format!("/api/f/{desk}/template"), None)?;
    // what the reloaded page asked has had time to answer
    std::thread::sleep(Duration::from_millis(1000));
    let gone = chrome.eval(page, "document.getElementById('update').hidden")? == json!(true);
    s.ok("clicked, it asks first, in place; its Update brings the desktop up to date and reloads it, the pill gone", confirming && reloaded && current.body["upToDate"] == true && gone, &current);
    Ok(())
}
