//! One-click fragments (docs/phase-6.md, step 2): a create from one of the
//! platform's templates, the server-side commit and deploy routes (an
//! agent's tools use them too), the platform's "new" page, and the
//! `fragments` capability, which only the fragment's owner is granted.

use std::time::Duration;

use anyhow::Result;
use fragment_core::npub;
use fragment_fakes::openrouter::Reply as Say;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::signin::{site_cookie, with_session};
use crate::api::{url_enc, Api, Call, Reply};
use crate::Suite;

/// A person with a CLI key and a platform session.
pub(super) fn person(api: &Api) -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let session = api.sign_in(&format!("t-{}@e2e.test", &keys.pubkey_hex()[..12]))?;
    api.approve(&session, &keys)?;
    Ok((keys, session))
}

fn post_form(api: &Api, path: &str, form: &str, session: &str, origin: &str) -> Result<Reply> {
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
    s.ok("a create from a template answers the fragment", r.status == 200 && r.body["name"] == chat.as_str(), &r);
    s.hook(api, &r.body);
    let chat_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    let st = api.status(&owner, &chat)?;
    s.ok("its template is main's first commit, and live", st.body["pins"]["live"].is_string() && st.body["pins"]["live"] == st.body["pins"]["main"], &st);
    s.ok("its app is installed from live", st.body["code"]["operations"]["say"]["kind"] == "mutation", &st.body["code"]);
    let m = api.signed(&owner, "GET", &format!("/api/f/{chat}/manifest"), None)?;
    s.ok("its fragment.json carries the fragment's own name", m.body["name"] == chat.as_str(), &m);
    let page = api.page(&chat, "", Some(&chat_cookie))?;
    s.ok("its site serves the template's page", page.status == 200 && page.text.contains("<title>Chat"), &page);
    // a chat made from the template has its owner's own agent in it
    let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?;
    let agent = members.body["members"].as_array().into_iter().flatten().find(|m| m["kind"] == "agent").cloned().unwrap_or_default();
    let owner_id = api.identity(&owner)?;
    s.ok("a chat from the template has its owner's agent in it, as an editor", agent["role"] == "editor" && agent["owner"] == owner_id.as_str(), &members);
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("listening to the chat", subs.body["subscriptions"].as_array().is_some_and(|a| a.iter().any(|x| x["principal"] == agent["principal"] && x["channel"] == "chat")), &subs);
    let mine = api.signed(&owner, "GET", "/api/a/agent", None)?;
    s.ok("it is agent.<username>, made on first need", mine.status == 200 && mine.body["name"] == api.qualified(&owner, "agent")?.as_str(), &mine);
    s.openrouter.clear_script();
    s.openrouter.script(&[Say::Text("Hello! I'm here.".into())]);
    let said = api.op(&owner, &chat, "say", "t1", json!({ "text": "hello from a template" }))?;
    s.ok("its operations answer", said.status == 200, &said);
    let answered = s.eventually(Duration::from_secs(30), || {
        api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)
            .is_ok_and(|r| r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["principal"] == agent["principal"] && x["body"]["text"] == "Hello! I'm here.")))
    });
    s.ok("and the agent answers in the chat", answered, "");
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
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("desktop, chat, todo, inbox, blank"), &r);
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

    // the platform's "new" page
    let home = with_session(api, "GET", "/", &owner_session)?;
    s.ok("the platform's home lists the person's fragments", home.text.contains("Your fragments") && home.text.contains(&chat), &home);
    s.ok("and offers the templates", home.text.contains("New fragment") && home.text.contains("value=\"todo\""), &home);
    let label = s.name("tnew");
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, "https://elsewhere.example")?;
    s.ok("a form from another origin is refused", r.status == 403, &r);
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, &api.base)?;
    let name = api.qualified(&owner, &label)?;
    s.ok("the form makes it and walks to its sign-in", r.status == 302 && r.header("location") == format!("/auth/fragment?name={}&return=/", url_enc(&name)), &r);
    let r = api.page(&name, "", Some(&format!("fragment_site={}", site_cookie(api, &owner_session, &name)?)))?;
    s.ok("the new fragment serves its template to its owner", r.status == 200 && r.text.contains("<title>Todo"), &r);
    let r = post_form(api, "/auth/new", &format!("label={}&template=todo", url_enc(&label)), &owner_session, &api.base)?;
    s.ok("a label already taken says so", r.status == 400 && r.text.contains("already exists"), &r);
    Ok(())
}
