//! One-click fragments (docs/phase-6.md, step 2): a create from one of the
//! platform's templates, the server-side commit and deploy routes (an
//! agent's tools use them too), and the shell's list and its "new" app.

use anyhow::Result;
use fragment_core::npub;
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use super::shell::shell;
use super::signin::site_cookie;
use crate::api::{Api, Reply};
use crate::Suite;

/// A person with a CLI key and a platform session.
pub(super) fn person(api: &Api) -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let session = api.sign_in(&format!("t-{}@e2e.test", &keys.pubkey_hex()[..12]))?;
    api.approve(&session, &keys)?;
    Ok((keys, session))
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
    let todo = s.named(api, &owner, "ttodo")?;
    let r = api.create_with(&owner, json!({ "name": todo, "template": "todo" }))?;
    s.ok(
        "a create from a template answers the fragment, open to whoever holds its link",
        r.status == 200 && r.body["name"] == todo.as_str() && r.body["visibility"] == "link",
        &r,
    );
    s.hook(api, &r.body);
    let todo_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    let st = api.status(&owner, &todo)?;
    s.ok("its template is main's first commit, and live", st.body["pins"]["live"].is_string() && st.body["pins"]["live"] == st.body["pins"]["main"], &st);
    let m = api.signed(&owner, "GET", &format!("/api/f/{todo}/manifest"), None)?;
    s.ok("its fragment.json carries the fragment's own name", m.body["name"] == todo.as_str(), &m);
    let page = api.page(&todo, "", Some(&todo_cookie))?;
    s.ok("its site serves the template's page", page.status == 200 && page.text.contains("<title>Todo"), &page);
    let owner_id = api.identity(&owner)?;
    let people = api.page(&todo, &format!("__people?id={owner_id}&id=anon:00"), Some(&todo_cookie))?;
    let username = api.username(&owner)?;
    let profiles = &people.body["profiles"];
    s.ok(
        "its page can name who is in it: a person by username, no one for an anonymous visitor",
        profiles[owner_id.as_str()]["username"] == username.as_str() && profiles.get("anon:00").is_none(),
        &people,
    );

    let none = s.name("tnone");
    let r = api.create_with(&owner, json!({ "name": none, "template": "nope" }))?;
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("blank, todo, inbox, calories"), &r);
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

    // the shell: the person's fragments, and making one (as its "new" app does)
    let shut = s.named(api, &owner, "tshut")?;
    let r = api.create_with(&owner, json!({ "name": shut, "template": "blank" }))?;
    anyhow::ensure!(r.status == 200, "making {shut}: {r}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{shut}/visibility"), Some(&json!({ "visibility": "members" })))?;
    anyhow::ensure!(r.status == 200, "{shut}'s visibility: {r}");
    let listed = |session: &str| shell(api, session, "GET", "/api/fragments", None, &[]);
    let row = |list: &Reply, name: &str| list.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == name).cloned()).unwrap_or_default();
    let home = listed(&owner_session)?;
    s.ok(
        "the shell lists the person's fragments: each one's name, and on their own who may open it (for its share sheet)",
        home.status == 200
            && row(&home, &todo)["role"] == "owner"
            && row(&home, &todo)["sharing"]["visibility"] == "link"
            && row(&home, &shut)["sharing"]["visibility"] == "members",
        &home,
    );
    let theirs = listed(&editor_session)?;
    s.ok("and says which are shared with them, and as what", row(&theirs, &blank)["role"] == "editor", &theirs);
    let label = s.name("tnew");
    let make = |origin: String| shell(api, &owner_session, "POST", "/api/fragments", Some(&json!({ "name": label, "template": "todo" })), &[("origin", origin)]);
    let r = make(api.site_origin(&todo))?;
    let st = api.status(&owner, &api.qualified(&owner, &label)?)?;
    s.ok("a create from another origin (a fragment's page) is no one's (401), and makes nothing", r.status == 401 && st.status == 404, format!("{r} / {st}"));
    let r = make(api.base.clone())?;
    let name = api.qualified(&owner, &label)?;
    s.ok("the shell makes it from a template", r.status == 200 && r.body["name"] == name.as_str(), &r);
    let r = api.page(&name, "", Some(&format!("fragment_site={}", site_cookie(api, &owner_session, &name)?)))?;
    s.ok("the new fragment serves its template to its owner", r.status == 200 && r.text.contains("<title>Todo"), &r);
    let r = make(api.base.clone())?;
    s.ok("a label already taken says so", r.status == 409 && r.code() == Some(ErrorCode::AlreadyExists), &r);
    let st = api.status(&owner, &name)?;
    s.ok("(a todo made there opens to anyone with its link, as before)", st.body["visibility"] == "link", &st);
    Ok(())
}
