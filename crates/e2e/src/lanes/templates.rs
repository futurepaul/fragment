//! One-click fragments (docs/phase-6.md, step 2): a create from one of the
//! platform's templates, the server-side commit and deploy routes (an
//! agent's tools use them too), the platform's "new" page, and the
//! `fragments` capability, which only the fragment's owner is granted.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_core::npub;
use fragment_fakes::openrouter::Reply as Say;
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use super::builder::{stand_in, CUA_VERSION, PNG};
use super::jobs::{settle, started};
use super::signin::{site_cookie, with_session};
use crate::api::{url_enc, Api, Call, Reply, Socket};
use crate::Suite;

/// A person with a CLI key and a platform session.
pub(super) fn person(api: &Api) -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let session = api.sign_in(&format!("t-{}@e2e.test", &keys.pubkey_hex()[..12]))?;
    api.approve(&session, &keys)?;
    Ok((keys, session))
}

pub(super) fn post_form(api: &Api, path: &str, form: &str, session: &str, origin: &str) -> Result<Reply> {
    api.call(Call {
        method: "POST",
        url: format!("{}{path}", api.base),
        body: Some(form.as_bytes().to_vec()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("origin", origin.to_string())],
        ..Call::default()
    })
}

pub fn templates(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("templates") {
        return Ok(());
    }
    let (owner, owner_session) = person(api)?;
    let (editor, editor_session) = person(api)?;
    let viewer = api.person()?;
    let npub_of = |k: &Keys| npub::encode(k.pubkey_hex());

    // a create from a template is a working fragment at once
    let chat = s.named(api, &owner, "tchat")?;
    let r = api.create_with(&owner, json!({ "name": chat, "template": "chat" }))?;
    s.ok(
        "a create from a template answers the fragment, open to whoever holds its link (the template declares no computer)",
        r.status == 200 && r.body["name"] == chat.as_str() && r.body["visibility"] == "link",
        &r,
    );
    s.hook(api, &r.body);
    let chat_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    let st = api.status(&owner, &chat)?;
    s.ok("its template is main's first commit, and live", st.body["pins"]["live"].is_string() && st.body["pins"]["live"] == st.body["pins"]["main"], &st);
    let channels = api.signed(&owner, "GET", &format!("/api/f/{chat}/channels"), None)?;
    let declared = |n: &str| channels.body["channels"].as_array().into_iter().flatten().find(|c| c["name"] == n).map(|c| (c["read"].clone(), c["post"].clone()));
    s.ok(
        "a chat is its channels: viewers post to chat, and read the agent's work, which editors post",
        declared("chat") == Some((json!("public"), json!("viewer"))) && declared("work") == Some((json!("viewer"), json!("editor"))),
        &channels,
    );
    let m = api.signed(&owner, "GET", &format!("/api/f/{chat}/manifest"), None)?;
    s.ok("its fragment.json carries the fragment's own name", m.body["name"] == chat.as_str(), &m);
    let page = api.page(&chat, "", Some(&chat_cookie))?;
    s.ok("its site serves the template's page", page.status == 200 && page.text.contains("<title>Chat"), &page);
    // a chat made from the template has its owner's own agent in it
    let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?;
    let agent = members.body["members"].as_array().into_iter().flatten().find(|m| m["kind"] == "agent").cloned().unwrap_or_default();
    let owner_id = api.identity(&owner)?;
    s.ok("a chat from the template has its owner's agent in it, as an editor", agent["role"] == "editor" && agent["owner"] == owner_id.as_str(), &members);
    let listening = |subs: &Reply| subs.body["subscriptions"].as_array().map(|a| a.iter().filter(|x| x["principal"] == agent["principal"] && x["channel"] == "chat").count());
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("listening to the chat, once", listening(&subs) == Some(1), &subs);
    let agent_name = api.qualified(&owner, "agent")?;
    let mine = api.signed(&owner, "GET", "/api/a/agent", None)?;
    s.ok("it is agent.<username>, made on first need", mine.status == 200 && mine.body["name"] == agent_name.as_str(), &mine);
    // a join that did not finish is retried by the chat's alarm: the same
    // listen, sent again as its owner, leaves the one subscription
    let again = api.signed(&owner, "POST", &format!("/api/a/{agent_name}/listen"), Some(&json!({ "fragment": chat })))?;
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("and a join sent again leaves the one subscription", again.status == 200 && listening(&subs) == Some(1), json!({ "listen": again.body, "subscriptions": subs.body }));
    // a message is a post; the model calls a `say` that is not there (as a
    // chat made before this had), then answers: one answer lands
    s.openrouter.clear_script();
    let say = fragment_core::tools::tool_name(&chat, "say").expect("a tool name");
    s.openrouter.script(&[Say::Tools(vec![(say, json!({ "text": "Hello from the tool." }))]), Say::Text("Hello! I'm here.".into())]);
    let said = api.signed(&owner, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "t1", "body": { "text": "hello from a template" } })))?;
    s.ok("a message is posted to its chat channel", said.status == 200 && said.body["record"]["principal"] == owner_id.as_str(), &said);
    let agents_records = || -> Vec<Value> {
        let records = api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
        records.into_iter().filter(|x| x["principal"] == agent["principal"]).collect()
    };
    let answered = s.eventually(Duration::from_secs(30), || agents_records().iter().any(|x| x["body"]["text"] == "Hello! I'm here."));
    // the answer comes after anything the turn did, so it is all there now
    let records = agents_records();
    s.ok(
        "and the agent answers in the chat, once, naming its turn",
        answered && records.len() == 1 && records[0]["body"]["turn"].as_str().is_some_and(|t| t.len() == 24),
        json!(records),
    );
    // a chat has no app code: no worker, however it is used
    let st = api.status(&owner, &chat)?;
    let builds = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": chat, "op": "code-builds" })))?;
    s.ok(
        "a new chat has no app code, and after a message and an answer it has loaded no worker",
        st.body["code"]["sha"].is_null() && builds.status == 200 && builds.body["builds"] == 0,
        format!("{} {}", st.body["code"], builds.body),
    );
    let people = api.page(&chat, &format!("__people?id={}&id={}&id=anon:00", agent["principal"].as_str().unwrap_or(""), owner_id), Some(&chat_cookie))?;
    let username = api.username(&owner)?;
    let profiles = &people.body["profiles"];
    s.ok(
        "its page can name who is in it: a person by username, an agent as its owner's",
        profiles[owner_id.as_str()]["username"] == username.as_str()
            && profiles[agent["principal"].as_str().unwrap_or("")]["kind"] == "agent"
            && profiles[agent["principal"].as_str().unwrap_or("")]["username"] == username.as_str()
            && profiles.get("anon:00").is_none(),
        &people,
    );
    let r = api.create_with(&owner, json!({ "name": s.name("tchat2"), "template": "chat" }))?;
    let second = r.body["name"].as_str().unwrap_or("").to_string();
    let members = api.signed(&owner, "GET", &format!("/api/f/{second}/members"), None)?;
    s.ok(
        "a second chat has the same agent",
        members.body["members"].as_array().is_some_and(|a| a.iter().any(|m| m["principal"] == agent["principal"])),
        &members,
    );

    let none = s.name("tnone");
    let r = api.create_with(&owner, json!({ "name": none, "template": "nope" }))?;
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("blank, todo, inbox, calories, pet, builder, chat, desktop"), &r);
    let r = api.status(&owner, &api.qualified(&owner, &none)?)?;
    s.ok("and nothing is made", r.status == 404, &r);

    // the files and deploy routes
    let blank = s.named(api, &owner, "tblank")?;
    let r = api.create_with(&owner, json!({ "name": blank, "template": "blank" }))?;
    s.hook(api, &r.body);
    let blank_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    for (k, role) in [(&editor, "editor"), (&viewer, "viewer")] {
        let r = api.signed(&owner, "PUT", &format!("/api/f/{blank}/members/{}", npub_of(k)), Some(&json!({ "role": role })))?;
        anyhow::ensure!(r.status == 200, "adding a {role}: {r}");
    }
    let page = |api: &Api| api.page(&blank, "", Some(&blank_cookie)).map(|r| r.text).unwrap_or_default();
    let files = |k: &Keys, body: Value| api.signed(k, "POST", &format!("/api/f/{blank}/files"), Some(&body));
    let write = json!({ "files": [
        { "path": "site/index.html", "text": "<h1>made through the api</h1>" },
        { "path": "site/extra.txt", "text": "extra" },
    ], "message": "from the files route", "key": "w1" });
    let r = files(&viewer, write.clone())?;
    s.ok("a viewer cannot write files", r.status == 403, &r);
    let wrote = files(&editor, write.clone())?;
    s.ok("an editor commits files to main", wrote.status == 200 && wrote.body["commit"].is_string(), &wrote);
    let again = files(&editor, write)?;
    s.ok("the same key commits nothing twice", again.status == 200 && again.body["commit"] == wrote.body["commit"], &again);
    let st = api.status(&owner, &blank)?;
    s.ok("main moves at once, live stays", st.body["pins"]["main"] == wrote.body["commit"] && st.body["pins"]["live"] != st.body["pins"]["main"], &st);
    s.ok("the site still serves live", !page(api).contains("made through the api"), page(api));
    let r = files(&editor, json!({ "files": [{ "path": "../escape", "text": "x" }] }))?;
    s.ok("a path outside the repo is refused", r.status == 400, &r);
    let many: Vec<Value> = (0..17).map(|i| json!({ "path": format!("f/{i}.txt"), "text": "x" })).collect();
    let r = files(&editor, json!({ "files": many }))?;
    s.ok("more files than one write takes are refused", r.status == 400, &r);

    let deploy = |k: &Keys| api.signed(k, "POST", &format!("/api/f/{blank}/deploy"), Some(&json!({ "note": "from the e2e" })));
    let r = deploy(&viewer)?;
    s.ok("a viewer cannot deploy", r.status == 403, &r);
    let r = deploy(&editor)?;
    s.ok("an editor deploys: live is main's tip", r.status == 200 && r.body["live"] == st.body["pins"]["main"], &r);
    s.ok("the site serves the deploy", page(api).contains("made through the api"), page(api));
    let r = files(&editor, json!({ "files": [{ "path": "site/extra.txt", "delete": true }] }))?;
    let gone = api.signed(&owner, "GET", &format!("/api/f/{blank}/file?path=site/extra.txt"), None)?;
    s.ok("a write can remove a file", r.status == 200 && gone.status == 404, &gone);

    // the fragments capability: the owner's own fragments, to the owner
    let dash = s.named(api, &owner, "tdash")?;
    let r = api.create_with(&owner, json!({ "name": dash, "template": "blank" }))?;
    s.hook(api, &r.body);
    let manifest = json!({ "name": dash, "capabilities": ["fragments"] }).to_string();
    let r = api.signed(&owner, "POST", &format!("/api/f/{dash}/files"), Some(&json!({ "files": [{ "path": "fragment.json", "text": manifest }] })))?;
    anyhow::ensure!(r.status == 200, "asking for the capability: {r}");
    let r = api.signed(&owner, "POST", &format!("/api/f/{dash}/deploy"), None)?;
    anyhow::ensure!(r.status == 200, "deploying it: {r}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{dash}/members/{}", npub_of(&editor)), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "adding an editor: {r}");
    let listed = |cookie: Option<String>, name: &str| {
        api.call(Call { method: "GET", url: api.site_url(name, "__fragments"), cookie: cookie.map(|c| format!("fragment_site={c}")), ..Call::default() })
    };
    let owner_site = site_cookie(api, &owner_session, &dash)?;
    let r = listed(Some(owner_site), &dash)?;
    let names: Vec<&str> = r.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).collect();
    s.ok("the owner, on a page that asks, lists their fragments", r.status == 200 && [&chat, &blank, &dash].iter().all(|n| names.contains(&n.as_str())), &r);
    let url = r.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == chat.as_str()).map(|f| f["url"].clone()).unwrap_or_default();
    s.ok("each with its URL", url == api.site_url(&chat, "").as_str(), &url);
    let r = listed(Some(site_cookie(api, &editor_session, &dash)?), &dash)?;
    s.ok("an editor of that page is refused", r.status == 403, &r);
    let r = listed(None, &dash)?;
    s.ok("so is someone signed out", r.status == 403 || r.status == 401, &r);
    let owner_on_blank = site_cookie(api, &owner_session, &blank)?;
    let r = listed(Some(owner_on_blank), &blank)?;
    s.ok("a page that does not ask is refused, even to its owner", r.status == 403, &r);
    let r = listed(Some(site_cookie(api, &owner_session, &dash)?), &dash)?;
    s.ok("and the page whether it may frame them (null: it does not ask)", r.status == 200 && r.body["frame"].is_null(), &r);
    // a desktop made through a page's __fragments: its owner's alone, and no
    // grant (a page's code never gives one)
    let desk_label = s.name("tdesk");
    let made = api.call(Call {
        method: "POST",
        url: api.site_url(&dash, "__fragments"),
        body: Some(json!({ "label": desk_label, "template": "desktop" }).to_string().into_bytes()),
        content_type: Some("application/json"),
        cookie: Some(format!("fragment_site={}", site_cookie(api, &owner_session, &dash)?)),
        ..Call::default()
    })?;
    let desk = api.qualified(&owner, &desk_label)?;
    let st = api.status(&owner, &desk)?;
    s.ok(
        "a desktop made through __fragments is its owner's alone (members only), and may not frame their fragments until they allow it",
        made.status == 200 && made.body["name"] == desk.as_str() && st.body["visibility"] == "members" && st.body["frame"] == json!(false),
        format!("{made} / {st}"),
    );

    // the platform's home: the person's fragments, and making one
    let home = with_session(api, "GET", "/", &owner_session)?;
    let row = |page: &Reply, name: &str| page.text.split("<li>").find(|li| li.contains(&format!("/share/{name}\""))).unwrap_or_default().to_string();
    s.ok(
        "the platform's home lists the person's fragments: each one's link, who may open it, and its share sheet",
        home.text.contains("Your fragments")
            && row(&home, &chat).contains(&format!("href=\"{}\"", api.site_url(&chat, "")))
            && row(&home, &chat).contains("yours · anyone with the link")
            && row(&home, &desk).contains("yours · only the people in it"),
        &home,
    );
    let theirs = with_session(api, "GET", "/", &editor_session)?;
    s.ok("and says which are shared with them, and as what", row(&theirs, &blank).contains("shared with you · editor"), &theirs);
    let offered: Vec<usize> = ["blank", "todo", "inbox", "calories", "pet", "builder", "chat", "desktop"].iter().filter_map(|t| home.text.find(&format!("value=\"{t}\""))).collect();
    s.ok(
        "and offers the templates, the simplest first and the desktop last, as the demo it is, saying it will show their fragments inside it",
        home.text.contains("New fragment")
            && offered.len() == 8
            && offered.is_sorted()
            && home.text.contains("A demo of what fragments can do")
            && home.text.contains("It will show your fragments inside it, signed in as you"),
        &home,
    );
    let label = s.name("tnew");
    let r = post_form(api, "/auth/new", &format!("label={}&template=desktop", url_enc(&label)), &owner_session, &api.site_origin(&dash))?;
    let st = api.status(&owner, &api.qualified(&owner, &label)?)?;
    s.ok("a form from another origin (a fragment's page) is refused, and makes nothing", r.status == 403 && st.status == 404, format!("{r} / {st}"));
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, &api.base)?;
    let name = api.qualified(&owner, &label)?;
    s.ok("the form makes it and walks to its sign-in", r.status == 302 && r.header("location") == format!("/auth/fragment?name={}&return=/", url_enc(&name)), &r);
    let r = api.page(&name, "", Some(&format!("fragment_site={}", site_cookie(api, &owner_session, &name)?)))?;
    s.ok("the new fragment serves its template to its owner", r.status == 200 && r.text.contains("<title>Todo"), &r);
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, &api.base)?;
    s.ok("a label already taken says so", r.status == 400 && r.text.contains("already exists"), &r);
    let st = api.status(&owner, &name)?;
    s.ok("(a todo made there opens to anyone with its link, as before)", st.body["visibility"] == "link", &st);
    let desk_label = s.name("tnewdesk");
    let r = post_form(api, "/auth/new", &format!("label={}&template=desktop", url_enc(&desk_label)), &owner_session, &api.base)?;
    let st = api.status(&owner, &api.qualified(&owner, &desk_label)?)?;
    s.ok(
        "a desktop made there is its owner's alone (members only), and may frame their fragments: the form's submit is their grant",
        r.status == 302 && st.body["visibility"] == "members" && st.body["frame"] == json!(true),
        format!("{r} / {st}"),
    );
    pet(s, api, &owner, &chat)
}

/// An 8×5 JPEG, as the pet's computer sends its screen.
const JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAYEBQYFBAYGBQYHBwYIChAKCgkJChQODwwQFxQYGBcUFhYaHSUfGhsjHBYWICwgIyYnKSopGR8tMC0oMCUoKSj/2wBDAQcHBwoIChMKChMoGhYaKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCj/wAARCAAFAAgDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAb/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFAEBAAAAAAAAAAAAAAAAAAAABf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AIcA2Kf/2Q==";

/// The pet (templates/pet): a computer, a `control` channel people drive it
/// through, and its screen, one row its computer stores through `frame` and
/// `screen` answers; and its agent, `do`, goose with Cua Driver as its hands
/// (both stand-ins here, as in the builder section: this proves the
/// plumbing). Its display loop and the real agent need a real Sprite
/// (docs/computers.md).
fn pet(s: &mut Suite, api: &Api, owner: &Keys, chat: &str) -> Result<()> {
    let viewer = api.person()?;
    let viewer_id = api.identity(&viewer)?;
    let name = s.named(api, owner, "tpet")?;
    let made = api.create_with(owner, json!({ "name": name, "template": "pet" }))?;
    s.hook(api, &made.body);
    let st = api.status(owner, &name)?;
    let m = api.signed(owner, "GET", &format!("/api/f/{name}/manifest"), None)?;
    let ops = &st.body["code"]["operations"];
    s.ok(
        "a pet from its template declares its computer, so it starts its owner's alone (members), and its code installs: frame (ephemeral) for editors, screen for viewers, the run job for editors",
        made.status == 200
            && made.body["visibility"] == "members"
            && m.body["computer"]["start"] == "node computer/pet.mjs"
            && st.body["code"]["error"].is_null()
            && ops["frame"]["role"] == "editor"
            && ops["frame"]["ephemeral"] == true
            && ops["screen"]["kind"] == "query"
            && ops["run"]["kind"] == "job"
            && ops["run"]["role"] == "editor"
            && ops["do"]["kind"] == "job"
            && ops["do"]["role"] == "editor",
        format!("{made} / {st}"),
    );
    let channels = api.signed(owner, "GET", &format!("/api/f/{name}/channels"), None)?;
    let channel = |n: &str| channels.body["channels"].as_array().into_iter().flatten().find(|c| c["name"] == n).cloned().unwrap_or_default();
    let (control, work) = (channel("control"), channel("work"));
    s.ok(
        "and a control channel viewers signed in post to, and a work channel its editors post to",
        control["read"] == "viewer" && control["post"] == "viewer" && control["signedIn"] == true && work["read"] == "viewer" && work["post"] == "editor",
        &channels,
    );
    // shared by its link: who holds it is a viewer, signed in or not
    let r = api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "link" })))?;
    anyhow::ensure!(r.status == 200, "sharing the pet by its link: {r}");
    let link = format!("fragview={}", made.body["viewToken"].as_str().unwrap_or(""));
    let page = api.page(&name, "", Some(&link))?;
    s.ok("its page is the template's", page.status == 200 && page.text.contains("<title>Pet"), &page);

    // driving: someone signed in posts; someone who is not, does not
    let r = api.signed(owner, "PUT", &format!("/api/f/{name}/members/{}", npub::encode(viewer.pubkey_hex())), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a viewer: {r}");
    let click = json!({ "kind": "click", "x": 100, "y": 60 });
    let r = api.signed(&viewer, "POST", &format!("/api/f/{name}/channels/control"), Some(&json!({ "id": "p1", "body": click })))?;
    s.ok("a signed-in viewer posts a control record", r.status == 200 && r.body["record"]["principal"] == viewer_id.as_str(), &r);
    let anonymous = |cookie: Option<&str>| {
        let body = json!({ "id": "p2", "input": click }).to_string().into_bytes();
        let url = api.site_url(&name, "__op/channels/control");
        api.call(Call { method: "POST", url, body: Some(body), content_type: Some("application/json"), cookie: cookie.map(str::to_string), ..Call::default() })
    };
    let r = anonymous(None)?;
    s.ok("an anonymous visitor cannot", r.status == 401 || r.status == 403, &r);
    let r = anonymous(Some(&link))?;
    let held = api.signed(owner, "GET", &format!("/api/f/{name}/channels/control"), None)?;
    s.ok(
        "nor can one holding the link, a viewer but anonymous: control is for people signed in (401), and nothing is appended",
        r.code() == Some(ErrorCode::Unauthenticated) && held.body["records"].as_array().map(Vec::len) == Some(1),
        format!("{r} / {held}"),
    );

    // frames: its computer's (an editor here), one row that screen answers
    let mut sprite = String::new();
    s.eventually(Duration::from_secs(30), || {
        let events = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
        let ready = events["events"].as_array().into_iter().flatten().find(|e| e["kind"] == "computer.ready");
        sprite = ready.and_then(|e| e["data"]["sprite"].as_str()).unwrap_or_default().to_string();
        !sprite.is_empty()
    });
    let computer = s.cli_keys(&s.scratch.join("sprites/sprites").join(&sprite)).context("the pet's computer, paired on its Sprite")?;
    let frame = |keys: &Keys, id: &str, jpeg: &str, title: &str, driver: Option<&str>| {
        let mut input = json!({ "jpeg": jpeg, "width": 8, "height": 5, "title": title });
        if let Some(driver) = driver {
            input["driver"] = json!(driver);
        }
        api.signed(keys, "POST", &format!("/api/f/{name}/ops/frame"), Some(&json!({ "id": id, "input": input })))
    };
    let r = frame(&viewer, "f1", JPEG, "a viewer's", None)?;
    s.ok("a viewer cannot store a frame", r.status == 403, &r);
    let r = frame(&computer, "f2", "iVBORw0KGgo", "a PNG", None)?;
    s.ok("nor can anyone store what is not a JPEG", r.status == 422, &r);
    let stored = [frame(&computer, "f3", JPEG, "first", Some(&viewer_id))?, frame(&computer, "f4", JPEG, "Hello from your pet", None)?];
    let screen = api.signed(&viewer, "POST", &format!("/api/f/{name}/ops/screen"), Some(&json!({ "id": "s1", "input": {} })))?;
    let got = &screen.body["result"];
    s.ok(
        "its computer stores frames, and the live query answers the latest: the JPEG, what is on screen, and who drove it (kept by a frame that names no one)",
        stored.iter().all(|r| r.status == 200) && got["jpeg"] == JPEG && got["title"] == "Hello from your pet" && got["driver"] == viewer_id.as_str() && got["width"] == 8,
        format!("{} / {screen}", stored[1]),
    );

    // its computer runs start from the live files: shown a fixed screen (as
    // off a Sprite), it stores that frame, and follows control
    let mut manifest = m.body.clone();
    manifest["computer"]["start"] = json!("PET_FAKE_SCREEN=computer/screen.jpg node computer/pet.mjs");
    let files = json!({ "files": [{ "path": "fragment.json", "text": manifest.to_string() }, { "path": "computer/screen.jpg", "base64": JPEG }] });
    let page = Socket::open(api, &name, "__live", Some(owner), None)?;
    let wrote = api.signed(owner, "POST", &format!("/api/f/{name}/files"), Some(&files))?;
    let deployed = api.signed(owner, "POST", &format!("/api/f/{name}/deploy"), None)?;
    let screen = || api.op(&viewer, &name, "screen", "s2", json!({})).map(|r| r.body["result"].clone()).unwrap_or_default();
    let shown = s.eventually(Duration::from_secs(60), || screen()["width"] == 1024 && screen()["jpeg"] == JPEG);
    s.ok("its computer runs start from the live files: shown a fixed screen, it stores that frame", wrote.status == 200 && shown, format!("{deployed} / {}", screen()));
    let owner_id = api.identity(owner)?;
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/control"), Some(&json!({ "id": "p3", "body": { "kind": "key", "key": "Enter" } })))?;
    let drove = s.eventually(Duration::from_secs(20), || screen()["driver"] == owner_id.as_str());
    let log = std::fs::read_to_string(s.scratch.join("sprites/sprites").join(&sprite).join("fragment.log")).unwrap_or_default();
    let seq = r.body["record"]["seq"].clone();
    s.ok(
        "and follows control: a signed-in poster's record, taken once, makes them its driver",
        drove && log.matches(&format!("#{seq} {{")).count() == 1,
        &log,
    );

    // run: a command on its computer, answered with what it printed; editors only
    let r = api.op(&viewer, &name, "run", "r1", json!({ "command": "echo nope" }))?;
    s.ok("a viewer cannot run a command on it", r.status == 403, &r);
    let r = api.op(owner, &name, "run", "r2", json!({ "command": "echo ran $((6 * 7)); echo oops >&2; exit 3" }))?;
    let ran = settle(api, owner, &name, started(&r), &["succeeded", "held"], Duration::from_secs(60));
    let out = &ran["output"];
    s.ok(
        "its owner runs a command on it: the run answers the command's code and output",
        ran["status"] == "succeeded" && out["code"] == 3 && out["stdout"] == "ran 42\n" && out["stderr"] == "oops\n" && out["truncated"] == false,
        &ran,
    );

    // its owner's agent runs a command there, asked in their chat: the
    // platform's verbs reach it (the agent is no member of the pet, so no
    // tool of its own names it), as an editor, and a job's call answers
    // what the job answered
    let r = api.signed(owner, "GET", "/api/a/agent/tools", None)?;
    let tools: Vec<&str> = r.body["tools"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
    s.ok("its owner's agent has the platform's verbs that reach it", tools.contains(&"platform__operations") && tools.contains(&"platform__call"), &r);
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Say::Tools(vec![("platform__operations".into(), json!({ "fragment": name }))]),
        Say::Tools(vec![("platform__call".into(), json!({ "fragment": name, "operation": "run", "input": { "command": "echo ran $((6 * 7))" } }))]),
        Say::Text("Your computer says 42.".into()),
    ]);
    let asked = api.signed(owner, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "t-run", "body": { "text": "what is 6 times 7 on my computer?" } })))?;
    let answer = || {
        let records = api.signed(owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
        records.iter().any(|x| x["body"]["text"] == "Your computer says 42.")
    };
    let answered = asked.status == 200 && s.eventually(Duration::from_secs(60), answer);
    let read = s.openrouter.chats().last().map(|c| c["messages"].to_string()).unwrap_or_default();
    s.ok(
        "asked in its owner's chat, their agent finds run on the pet, as an editor, runs a command there, and reads what it printed",
        answered && read.contains(r#"\"role\":\"editor\""#) && read.contains(r#"\"run\":{"#) && read.contains(r#"\"stdout\":\"ran 42\\n\""#),
        &read,
    );

    // do: its agent drives its screen, for editors only
    let home = s.scratch.join("sprites/sprites").join(&sprite);
    let computer_id = api.identity(&computer)?;
    stand_in(&home, &format!(".local/share/cua-driver-{CUA_VERSION}/cua-driver"), "cua-driver")?;
    let r = api.op(&viewer, &name, "do", "d1", json!({ "task": "click it" }))?;
    s.ok("a viewer cannot ask its agent", r.status == 403, &r);
    let (look, said) = (Say::Tools(vec![("cua__get_desktop_state".into(), json!({}))]), "I looked and clicked.");
    s.openrouter.clear_script();
    s.openrouter.script(&[look.clone(), Say::Tools(vec![("cua__click".into(), json!({ "x": 10, "y": 20 }))]), look.clone(), look.clone(), look, Say::Text(said.into())]);
    let before = s.openrouter.chats().len();
    let r = api.op(owner, &name, "do", "d2", json!({ "task": "click it" }))?;
    let run = started(&r);
    let done = settle(api, owner, &name, run, &["succeeded", "held"], Duration::from_secs(90));
    s.ok(
        "its owner asks its agent: Cua Driver is installed, the hands do the task, and the run answers goose's last words",
        done["status"] == "succeeded" && done["output"]["code"] == 0 && done["output"]["message"] == said,
        &done,
    );
    let chats: Vec<Value> = s.openrouter.chats().into_iter().skip(before).collect();
    let offered: Vec<&str> = chats.first().and_then(|c| c["tools"].as_array()).into_iter().flatten().filter_map(|t| t["function"]["name"].as_str()).collect();
    s.ok(
        "each model call went through the platform, on the hands' model (flashx, which reads images), offered goose's shell and the Cua Driver tools its config names",
        chats.len() == 6 && chats.iter().all(|c| c["model"] == "z-ai/glm-5.3-flashx" && c["stream"] == true) && offered == ["shell", "cua__get_desktop_state", "cua__click"],
        json!(offered),
    );
    let images = |c: &Value| c["messages"].as_array().into_iter().flatten().filter_map(|m| m["content"].as_array()).flatten().filter(|p| p["type"] == "image_url").count();
    let seen: Vec<usize> = chats.iter().map(images).collect();
    let text = |i: usize| chats.get(i).map(Value::to_string).unwrap_or_default();
    s.ok(
        "each screenshot reaches the model and stays, the requests as goose made them (no proxy trims them), and Cua Driver drives the pet's display",
        seen == [0, 1, 1, 2, 3, 4] && text(1).contains(&format!("data:image/png;base64,{PNG}")) && !text(5).contains("left out") && text(2).contains("clicked at 10, 20 on :99"),
        json!(seen),
    );
    let work = api.signed(owner, "GET", &format!("/api/f/{name}/channels/work"), None)?;
    let records: Vec<Value> = work.body["records"].as_array().into_iter().flatten().filter(|x| x["body"]["run"] == run).cloned().collect();
    let steps: Vec<String> = records.iter().map(|x| format!("{} {}", x["body"]["kind"].as_str().unwrap_or(""), x["body"]["tool"].as_str().unwrap_or(""))).collect();
    let by_computer = records.iter().filter(|x| x["body"]["kind"] == "turn.step").all(|x| x["principal"] == computer_id.as_str());
    let step = "turn.step get_desktop_state";
    s.ok(
        "asked on its page, work has the run's start, each step as its computer posted it, and its end",
        steps == ["start ", step, "turn.step click", step, step, step, "end "]
            && by_computer
            && records.first().is_some_and(|x| x["body"]["asker"] == owner_id.as_str())
            && records.get(6).is_some_and(|x| x["body"]["message"] == said),
        &work,
    );
    let agent = format!("agent:{owner_id}");
    let drove = s.eventually(Duration::from_secs(20), || screen()["driver"] == agent.as_str());
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/channels/control"), Some(&json!({ "id": "p4", "body": { "kind": "key", "key": "Escape" } })))?;
    let back = r.status == 200 && s.eventually(Duration::from_secs(20), || screen()["driver"] == owner_id.as_str());
    s.ok("its agent is the driver the screen names, for who asked, until a person drives it again", drove && back, screen());

    // its owner's agent hands work to the pet by name: its `do` runs there,
    // and the pet's computer answers in the chat that asked (the agent's
    // turn ends on the hand-off, so goose's answer is the fake's echo)
    let dos = || api.signed(owner, "GET", &format!("/api/f/{name}/runs?op=do"), None).map_or(0, |r| r.body["runs"].as_array().map_or(0, Vec::len));
    let (before, sprites) = (dos(), s.sprites.sprites().len());
    s.openrouter.clear_script();
    s.openrouter.script(&[Say::Tools(vec![("platform__hand_off".into(), json!({ "task": "click the pet's button", "computer": name }))])]);
    api.signed(owner, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "t-hand-off", "body": { "text": "have my pet click its button" } })))?;
    let result = || {
        let records = api.signed(owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default();
        let turn = format!("hand-off:{name}:");
        records.into_iter().find(|x| x["principal"] == computer_id.as_str() && x["body"]["turn"].as_str().is_some_and(|t| t.starts_with(&turn)))
    };
    let landed = s.eventually(Duration::from_secs(90), || result().is_some());
    let said = result().map(|x| x["body"]["text"].to_string()).unwrap_or_default();
    s.ok(
        "handed work by name, the owner's agent picks the pet's do: it runs there, the pet stays, and its computer answers in the chat",
        landed && dos() == before + 1 && said.contains("click the pet") && s.sprites.sprites().len() == sprites,
        json!({ "said": said, "do runs": dos() }),
    );
    page.close();
    Ok(())
}
