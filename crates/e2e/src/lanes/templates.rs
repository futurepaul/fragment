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
    s.ok("a create from a template answers the fragment", r.status == 200 && r.body["name"] == chat.as_str(), &r);
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
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("blank, todo, inbox, calories, chat, desktop"), &r);
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
    let offered: Vec<usize> = ["blank", "todo", "inbox", "calories", "chat", "desktop"].iter().filter_map(|t| home.text.find(&format!("value=\"{t}\""))).collect();
    s.ok(
        "and offers the templates, the simplest first and the desktop last, as the demo it is, saying it will show their fragments inside it",
        home.text.contains("New fragment")
            && offered.len() == 6
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
    Ok(())
}
