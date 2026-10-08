//! One-click fragments (docs/api.md, Control API): a create from one of the
//! platform's templates, the server-side commit and deploy routes (an
//! agent's tools use them too), and the shell's list and its "new" app.

use anyhow::Result;
use fragment_core::npub;
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use super::shell::shell;
use super::signin::site_cookie;
use crate::api::{Api, Call, Reply};
use crate::Suite;

/// A person with a CLI key and a platform session.
pub(super) fn person(api: &Api) -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let session = api.sign_in(&format!("t-{}@e2e.test", &keys.pubkey_hex()[..12]))?;
    api.approve(&session, &keys)?;
    Ok((keys, session))
}

pub fn templates(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("templates", &[]) {
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
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("blank, todo, inbox, calories, watch"), &r);
    let r = api.status(&owner, &api.qualified(&owner, &none)?)?;
    s.ok("and nothing is made", r.status == 404, &r);

    // the files and deploy routes
    let blank = s.named(api, &owner, "tblank")?;
    let r = api.create_with(&owner, json!({ "name": blank, "template": "blank" }))?;
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
    let same = files(&editor, json!({ "files": [{ "path": "site/extra.txt", "text": "extra" }], "message": "the bytes main holds" }))?;
    s.ok("a write of what main holds commits nothing: it answers main's tip", same.status == 200 && same.body["commit"] == wrote.body["commit"], &same);
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
    skills(s, api, &owner)
}

/// A call to code.storage from a preview takes about this long (a skills
/// create's, 2026-10-05): the local fake answers that create this slowly,
/// so its seed and the alarm's meet as they do there.
const CODE_STORAGE_LATENCY_MS: u64 = 100;

/// How many of the fragment's newest 200 events are of `kind`.
fn events(api: &Api, owner: &Keys, name: &str, kind: &str) -> usize {
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=200"), None);
    r.map_or(0, |r| r.body["events"].as_array().into_iter().flatten().filter(|e| e["kind"] == kind).count())
}

/// The managed skills (decision 17): a fragment on the blessed `skills`
/// template lists and reads the release's managed set as its files
/// (decision 40: no copy to drift), beneath files of its own at the same
/// path; its repo holds only its manifest.
fn skills(s: &mut Suite, api: &Api, owner: &Keys) -> Result<()> {
    use fragment_templates::blessed;
    let label = s.name("tskills");
    // code.storage as far as a preview has it: the create's calls take
    // long enough for the alarm it arms to run beside it, as they do there
    if !s.hosted() {
        s.fake.set_latency(std::time::Duration::from_millis(CODE_STORAGE_LATENCY_MS));
    }
    let r = api.create_with(owner, json!({ "name": label, "template": "skills" }));
    if !s.hosted() {
        s.fake.set_latency(std::time::Duration::ZERO);
    }
    let r = r?;
    let name = r.body["name"].as_str().unwrap_or("").to_string();
    s.ok("a skills fragment is made from the blessed skills template", r.status == 200 && !name.is_empty(), &r);
    let view = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    let m = api.signed(owner, "GET", &format!("/api/f/{name}/manifest"), None)?;
    s.ok("its repo names the template, nothing else", m.body == json!({ "template": "skills" }), &m);
    let listed = api.signed(owner, "GET", "/api/fragments", None)?;
    let kind = listed.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == name.as_str())).map(|f| f["kind"].clone());
    s.ok("its owner's list says it is their skills", kind == Some(json!("skills")), &listed);

    let files = |k: &Keys| api.signed(k, "GET", &format!("/api/f/{name}/files"), None);
    let r = files(owner)?;
    let rows: Vec<Value> = r.body["files"].as_array().cloned().unwrap_or_default();
    let release: Vec<(String, String)> = rows.iter().filter(|f| f["release"] == true).map(|f| (f["path"].as_str().unwrap_or("").to_string(), f["lastCommitSha"].as_str().unwrap_or("").to_string())).collect();
    let want: Vec<(String, String)> = blessed::data("skills").iter().map(|d| (d.path.to_string(), d.version.clone())).collect();
    s.ok(
        &format!("its files are the release's managed set ({} files), each at its version, beside its own manifest", want.len()),
        r.status == 200 && !want.is_empty() && release == want && rows.iter().any(|f| f["path"] == "fragment.json" && f["release"].is_null()),
        format!("{} release rows, {} wanted; {}", release.len(), want.len(), &r.text[..r.text.len().min(300)]),
    );
    // the create's seed and the alarm's (the create arms it) take turns:
    // side by side, the second commit changes nothing and is refused (412
    // on code.storage), and when it was the create's, the create answered
    // before its template was live, listing none of the release
    let (landed, failed) = (events(api, owner, &name, "template"), events(api, owner, &name, "template.failed"));
    s.ok(
        "its template lands once, in the create: no second seed beside it, none that failed",
        landed == 1 && failed == 0,
        format!("{landed} template, {failed} template.failed events: {}", api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=20"), None).map(|r| r.text).unwrap_or_default()),
    );
    let skill_names: Vec<&str> = want.iter().filter_map(|(p, _)| p.strip_suffix("/SKILL.md")).filter_map(|d| d.rsplit('/').next()).collect();
    s.ok(
        "the managed set is decision 17's: the rewritten skills there, shared-skills and what they replace gone",
        skill_names.len() == 41
            && ["apps-finite", "git-finite", "brain-finite", "google-workspace-finite", "image-generation-finite"].iter().all(|n| skill_names.contains(n))
            && !skill_names.iter().any(|n| ["shared-skills-finite", "finite-sites-publishing-finite", "website-building-finite", "finitebrain", "llm-wiki-finite", "fal-image-editing-finite", "powerpoint-finite"].contains(n)),
        format!("{skill_names:?}"),
    );
    let apps = "skills/software-development/apps-finite/SKILL.md";
    let read = |k: &Keys, path: &str| api.signed(k, "GET", &format!("/api/f/{name}/file?path={}", crate::api::url_enc(path)), None);
    let r = read(owner, apps)?;
    s.ok("a managed skill reads as the release's bytes", r.status == 200 && Some(r.bytes.as_slice()) == blessed::data_file("skills", apps).map(|d| d.bytes), r.status);
    let r = read(owner, "skills/nope/SKILL.md")?;
    s.ok("a skill the set has not is no file (404)", r.status == 404, &r);

    // a file of its own at a managed path wins; removed, the release's is back
    let own = "---\nname: apps-finite\ndescription: my own apps skill\n---\n";
    let w = api.signed(owner, "POST", &format!("/api/f/{name}/files"), Some(&json!({ "files": [{ "path": apps, "text": own }], "key": "own-apps" })))?;
    let r = read(owner, apps)?;
    let at: Vec<Value> = files(owner)?.body["files"].as_array().map(|l| l.iter().filter(|f| f["path"] == apps).cloned().collect()).unwrap_or_default();
    let row = at.first().cloned().unwrap_or_default();
    s.ok(
        "a file of its own at a managed path wins over the release's, listed once",
        w.status == 200 && r.text == own && at.len() == 1 && row["release"].is_null() && !row["lastCommitSha"].as_str().unwrap_or("").starts_with("release:"),
        json!({ "rows": at, "read": r.status }),
    );
    let w = api.signed(owner, "POST", &format!("/api/f/{name}/files"), Some(&json!({ "files": [{ "path": apps, "delete": true }], "key": "own-apps-gone" })))?;
    let r = read(owner, apps)?;
    s.ok("and removed, the release's is back", w.status == 200 && Some(r.bytes.as_slice()) == blessed::data_file("skills", apps).map(|d| d.bytes), r.status);

    // its page, and its site's file routes, read the same
    let page = api.page(&name, "", Some(&view))?;
    s.ok("its page is the release's", page.status == 200 && page.text.contains("<title>Skills"), page.status);
    let site = api.call(Call { method: "GET", url: api.site_url(&name, "__files"), cookie: Some(view.clone()), extra: vec![("accept", "application/json".into())], ..Call::default() })?;
    let site_paths: Vec<&str> = site.body["files"].as_array().map(|l| l.iter().filter_map(|f| f["path"].as_str()).collect()).unwrap_or_default();
    s.ok("its site lists the managed set too", want.iter().all(|(p, _)| site_paths.contains(&p.as_str())), site.status);
    let file = api.page(&name, &format!("__file?path={}", crate::api::url_enc(apps)), Some(&view))?;
    s.ok("and reads a managed skill", file.status == 200 && file.text.contains("name: apps-finite"), file.status);

    // a stranger reads none of it on a members fragment
    let stranger = api.person()?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let r = files(&stranger)?;
    s.ok("a stranger lists none of a members skills fragment (403)", r.status == 403, &r);
    let r = read(&stranger, apps)?;
    s.ok("nor reads one of its skills", r.status == 403, &r);
    // a fragment on a template without data lists only its own
    let chat = s.named(api, owner, "tskillschat")?;
    let r = api.create_with(owner, json!({ "name": chat, "template": "chat" }))?;
    anyhow::ensure!(r.status == 200, "making a chat: {r}");
    let r = api.signed(owner, "GET", &format!("/api/f/{}/files", r.body["name"].as_str().unwrap_or("")), None)?;
    s.ok("a chat's files are its own alone (its template carries no data)", r.body["files"].as_array().is_some_and(|l| l.iter().all(|f| f["release"].is_null())), &r);
    Ok(())
}
