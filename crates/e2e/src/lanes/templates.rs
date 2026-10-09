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
    let session = api.sign_in(&email_of(&keys))?;
    api.approve(&session, &keys)?;
    Ok((keys, session))
}

/// The email `person` signs in with.
pub(super) fn email_of(keys: &Keys) -> String {
    format!("t-{}@e2e.test", &keys.pubkey_hex()[..12])
}

pub fn templates(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("templates", &[crate::Need::Chrome]) {
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
    let profiles = &people.body["profiles"];
    s.ok(
        "its page can name who is in it: a person, whose email only a member is shown (a link holder is none), and no one for an anonymous visitor",
        profiles[owner_id.as_str()]["kind"] == "person" && profiles[owner_id.as_str()].get("email").is_none() && profiles.get("anon:00").is_none(),
        &people,
    );

    let none = s.name("tnone");
    let r = api.create_with(&owner, json!({ "label": none, "template": "nope" }))?;
    s.ok("an unknown template is refused, naming the templates", r.status == 400 && r.message().contains("blank, todo, inbox, calories"), &r);
    let r = api.status(&owner, &api.qualified(&owner, &none)?)?;
    s.ok("and nothing is made", r.status == 404, &r);

    // the files and deploy routes
    let blank = s.named(api, &owner, "tblank")?;
    let r = api.create_with(&owner, json!({ "name": blank, "template": "blank" }))?;
    let blank_cookie = format!("fragview={}", r.body["viewToken"].as_str().unwrap_or(""));
    template_pages(s, api, &todo, &todo_cookie, &blank, &blank_cookie)?;
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
    let name = s.named(api, &owner, "tnew")?;
    let make = |origin: String| shell(api, &owner_session, "POST", "/api/fragments", Some(&json!({ "name": name, "template": "todo" })), &[("origin", origin)]);
    let r = make(api.site_origin(&todo))?;
    let st = api.status(&owner, &name)?;
    s.ok("a create from another origin (a fragment's page) is no one's (401), and makes nothing", r.status == 401 && st.status == 404, format!("{r} / {st}"));
    let r = make(api.base.clone())?;
    s.ok("the shell makes it from a template", r.status == 200 && r.body["name"] == name.as_str(), &r);
    let r = api.page(&name, "", Some(&format!("fragment_site={}", site_cookie(api, &owner_session, &name)?)))?;
    s.ok("the new fragment serves its template to its owner", r.status == 200 && r.text.contains("<title>Todo"), &r);
    let r = make(api.base.clone())?;
    s.ok("a name already taken says so", r.status == 409 && r.code() == Some(ErrorCode::AlreadyExists), &r);
    let st = api.status(&owner, &name)?;
    s.ok("(a todo made there opens to anyone with its link, as before)", st.body["visibility"] == "link", &st);
    skills(s, api, &owner)
}

/// A call to code.storage from a preview takes about this long (a skills
/// create's, 2026-10-05): the local fake answers that create this slowly,
/// so its seed and the alarm's meet as they do there.
const CODE_STORAGE_LATENCY_MS: u64 = 100;

/// The starter pages as a person sees them, in a narrow pane and on a
/// desktop, with the system's light and dark schemes. Keep the shots in
/// the run's scratch for visual review.
fn template_pages(s: &mut Suite, api: &Api, todo: &str, todo_cookie: &str, blank: &str, blank_cookie: &str) -> Result<()> {
    let Some(mut b) = s.browser()? else {
        s.ok("Chrome is installed for the starter pages (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let shots = s.dir("template-pages");
    for (label, name, cookie) in [("blank", blank, blank_cookie), ("todo", todo, todo_cookie)] {
        let origin = api.site_origin(name);
        b.set_cookie(&origin, "fragview", cookie.strip_prefix("fragview=").unwrap_or(cookie))?;
        let page = b.open(&api.site_url(name, ""))?;
        anyhow::ensure!(b.until(&page, "document.querySelector('main')", std::time::Duration::from_secs(15)), "{label} did not load");
        let headers = b.eval(&page, "document.querySelectorAll('header, h1').length")?;
        s.ok(&format!("{label} starts with its content, without a page-title header"), headers == 0, &headers);
        if label == "todo" {
            anyhow::ensure!(b.until(&page, "document.getElementById('here').textContent === 'just you here'", std::time::Duration::from_secs(15)), "todo did not connect");
            b.eval(&page, "(async () => { const f = await import('./__fragment.js'); await f.call('add', {text: 'Review the first draft'}); await f.call('add', {text: 'Share it with Bea'}); })()")?;
            anyhow::ensure!(b.until(&page, "document.querySelectorAll('#todos li').length === 2", std::time::Duration::from_secs(15)), "todo did not update live");
        }
        for (width, view) in [(380, "pane"), (1280, "desktop")] {
            b.viewport(&page, width, 800, false)?;
            for scheme in ["light", "dark"] {
                b.color_scheme(&page, scheme)?;
                b.eval(&page, "document.fonts.ready.then(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))")?;
                let fits = b.eval(&page, "document.documentElement.scrollWidth <= innerWidth")?;
                s.ok(&format!("{label} fits the {view} in {scheme}"), fits == true, &fits);
                b.screenshot(&page, &shots.join(format!("{label}-{view}-{scheme}.png")))?;
            }
        }
        if label == "blank" {
            stylesheet(s, &mut b, &page)?;
        }
        b.close(page)?;
    }
    println!("      (starter screenshots: {})", shots.display());
    Ok(())
}

/// Author CSS keeps control even when declared before the platform link;
/// the page's explicit theme wins over the system, and native controls
/// and hidden content keep their behavior.
fn stylesheet(s: &mut Suite, b: &mut crate::browser::Browser, page: &crate::browser::Page) -> Result<()> {
    b.eval(page, r#"(() => {
      const style = document.createElement('style');
      style.textContent = 'button { border-radius: 3px; padding: 7px; }';
      document.head.prepend(style);
      const box = document.createElement('div');
      box.innerHTML = '<button>Action</button><input type="checkbox"><input type="radio"><p hidden>Hidden</p>';
      document.querySelector('main').append(box);
      window.controls = box;
    })()"#)?;
    let controls = b.eval(page, r#"(() => {
      const css = e => getComputedStyle(e);
      return {
        radius: css(controls.querySelector('button')).borderRadius,
        padding: css(controls.querySelector('button')).padding,
        checkbox: css(controls.querySelector('[type=checkbox]')).appearance,
        radio: css(controls.querySelector('[type=radio]')).appearance,
        hidden: css(controls.querySelector('[hidden]')).display
      };
    })()"#)?;
    s.ok("author rules before the link override button defaults", controls["radius"] == "3px" && controls["padding"] == "7px", &controls);
    s.ok("checkboxes and radios keep native appearance; hidden content stays hidden", controls["checkbox"] == "auto" && controls["radio"] == "auto" && controls["hidden"] == "none", &controls);
    b.color_scheme(page, "light")?;
    let light = b.eval(page, "getComputedStyle(document.body).backgroundColor")?;
    let dark = b.eval(page, "document.documentElement.dataset.theme = 'dark'; getComputedStyle(document.body).backgroundColor")?;
    b.color_scheme(page, "dark")?;
    let chosen_light = b.eval(page, "document.documentElement.dataset.theme = 'light'; getComputedStyle(document.body).backgroundColor")?;
    s.ok("an explicit page theme wins over the system in both directions", light != dark && chosen_light == light, format!("light {light}, dark {dark}, chosen light {chosen_light}"));
    b.eval(page, "delete document.documentElement.dataset.theme; document.documentElement.style.colorScheme = 'light'; getComputedStyle(document.body).backgroundColor")?;
    let css_light = b.eval(page, "getComputedStyle(document.body).backgroundColor")?;
    s.ok("a page can also choose its scheme with CSS", css_light == light, &css_light);
    Ok(())
}

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
    let r = api.create_with(owner, json!({ "label": label, "template": "skills" }));
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
        "the managed set contains the twelve retained skills; generic skills come from Hermes",
        skill_names.len() == 12
            && ["apps-finite", "git-finite", "brain-finite", "google-workspace-finite", "image-generation-finite", "model-council-finite", "cocod-finite", "nostr-agent-interface-cli-finite", "x-api-finite", "music-generation-finite", "trading-agent-finite", "polymarket-finite"].iter().all(|n| skill_names.contains(n)),
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
