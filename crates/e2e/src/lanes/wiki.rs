//! The wiki template, as a team uses it: its owner reads and edits pages
//! in a browser while someone with the link watches them change, and a
//! teammate (an agent would do the same) edits through a synced folder,
//! whose commits the file trigger brings to every open page; who is here
//! and who is editing shows on the page, an edit made under someone else's
//! says so, and Recent changes says who changed what, in which commit.

use std::time::Duration;

use anyhow::Result;
use fragment_core::npub;
use serde_json::json;

use super::signin::site_cookie;
use super::templates::person;
use crate::api::Api;
use crate::Suite;

pub fn wiki(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("wiki", &[crate::Need::Chrome]) {
        return Ok(());
    }
    let (owner, session) = person(api)?;
    let username = api.username(&owner)?;
    let name = s.named(api, &owner, "wiki")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "wiki" }))?;
    anyhow::ensure!(made.status == 200, "wiki from its template: {made}");
    let view = made.body["viewToken"].as_str().unwrap_or("").to_string();
    let wait = Duration::from_secs(30);

    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the wiki template (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&api.site_url(&name, ""), "fragment_site", &site_cookie(api, &session, &name)?)?;
    let page = chrome.open(&api.site_url(&name, "#/home"))?;
    let has = |text: &str| format!("document.getElementById('view').textContent.includes({text:?})");
    s.ok("its home page renders from wiki/home.md", chrome.until(&page, "document.querySelector('#view h1')?.textContent === 'Home'", wait), "");
    s.ok(
        "a [[wikilink]] to a page there is a link, one to a page not yet written is red",
        chrome.eval(&page, "[...document.querySelectorAll('#view a.wiki')].some((a) => a.textContent === 'How this wiki works') && document.querySelector('#view a.missing')?.textContent === 'Meeting notes'")? == true,
        "",
    );
    chrome.eval(&page, "document.querySelector('#view a.wiki').click(); true")?;
    s.ok("following it opens that page", chrome.until(&page, "location.hash === '#/how-this-wiki-works' && document.querySelector('#view h1')?.textContent === 'How this wiki works'", wait), "");
    chrome.eval(&page, "location.hash = '#/home'; true")?;

    // someone with the link reads along; the owner edits
    let other = chrome.another_context()?;
    let theirs = chrome.open_in(&other, &api.site_url(&name, &format!("?view={view}#/home")))?;
    s.ok("someone with the link reads it, and cannot edit", chrome.until(&theirs, &has("This is your team's wiki"), wait) && chrome.eval(&theirs, "!document.getElementById('edit') && document.getElementById('new').hidden")? == true, "");
    s.ok("the owner's page has an Edit", chrome.until(&page, "!!document.getElementById('edit')", wait), "");
    chrome.click(&page, "#edit")?;
    s.ok("whoever has the page open sees who is editing it", chrome.until(&theirs, &format!("document.getElementById('who').textContent.includes('{username} is editing')"), wait), "");
    chrome.eval(&page, "const t = document.getElementById('text'); t.value = t.value.replace('## Start here', 'We ship on Fridays.\\n\\n## Start here'); document.getElementById('editor').requestSubmit(); true")?;
    s.ok("a save is live on every open page", chrome.until(&theirs, &has("We ship on Fridays."), wait), "");
    s.ok(
        "and Recent changes says who saved it, in which commit",
        chrome.until(&page, "document.getElementById('meta').textContent.includes('by you')", wait)
            && chrome.until(&theirs, &format!("document.getElementById('meta').textContent.includes('by {username}')"), wait),
        "",
    );

    // a teammate edits through the folder, as an agent would
    let home = s.dir("wiki-home");
    s.login(api, &home);
    let mate = s.cli_keys(&home).expect("the CLI logged in");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", npub::encode(mate.pubkey_hex())), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "adding the teammate: {r}");
    let dir = s.dir("wiki-folder");
    let dir_s = dir.to_str().expect("a UTF-8 path").to_string();
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_s]);
    let pulled = std::fs::read_to_string(dir.join("wiki/home.md")).unwrap_or_default();
    s.ok("a teammate's folder sync pulls the pages, the owner's edit in them", out.status.success() && pulled.contains("We ship on Fridays."), String::from_utf8_lossy(&out.stderr));
    std::fs::write(dir.join("wiki/meeting-notes.md"), "# Meeting notes\n\n- Monday: the new board, from the folder.\n")?;
    std::fs::write(dir.join("wiki/home.md"), pulled.replace("We ship on Fridays.", "We ship on Thursdays now."))?;
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_s]);
    s.ok("its sync commits the edits", out.status.success(), String::from_utf8_lossy(&out.stderr));
    s.ok("an open page follows the folder's edit without a reload", chrome.until(&theirs, &has("We ship on Thursdays now."), wait), "");
    s.ok(
        "the red link is a link, now that its page exists, and the page list has it",
        chrome.until(&page, "[...document.querySelectorAll('#view a.wiki')].some((a) => a.textContent === 'Meeting notes') && document.getElementById('list').textContent.includes('Meeting notes')", wait),
        "",
    );
    let recent = api.op(&owner, &name, "recent", "q", json!({}))?;
    let changes = recent.body["result"]["changes"].as_array().cloned().unwrap_or_default();
    let owner_id = api.identity(&owner)?;
    s.ok(
        "Recent changes keeps each: the folder's (no one named) and the owner's, each with its commit",
        changes.iter().any(|c| c["path"] == "wiki/meeting-notes.md" && c["by"].is_null() && c["commit_sha"].is_string())
            && changes.iter().any(|c| c["path"] == "wiki/home.md" && c["by"].is_null() && c["commit_sha"].is_string())
            && changes.iter().any(|c| c["path"] == "wiki/home.md" && c["by"] == owner_id.as_str() && c["commit_sha"].is_string()),
        &recent,
    );
    chrome.eval(&page, "location.hash = '#/_recent'; true")?;
    s.ok("and the page lists them", chrome.until(&page, "document.getElementById('changes')?.textContent.includes('from the folder')", wait), "");

    // an edit made while the page changed under it says so before it saves
    chrome.eval(&page, "location.hash = '#/home'; true")?;
    chrome.until(&page, "!!document.getElementById('edit')", wait);
    chrome.click(&page, "#edit")?;
    let now = std::fs::read_to_string(dir.join("wiki/home.md")).unwrap_or_default();
    std::fs::write(dir.join("wiki/home.md"), format!("{now}\nOne more line, from the folder.\n"))?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_s]);
    s.ok(
        "an editor whose page changed under it is told, and its save says it replaces that change",
        chrome.until(&page, "document.getElementById('edit-note').className === 'warn' && document.getElementById('save').textContent === 'Save anyway'", wait),
        chrome.eval(&page, "document.getElementById('edit-note').textContent")?,
    );
    chrome.click(&page, "#cancel")?;

    // a new page from the sidebar
    chrome.eval(&page, "document.getElementById('new-title').value = 'Release checklist'; document.getElementById('new').requestSubmit(); true")?;
    s.ok("a new page opens its editor", chrome.until(&page, "location.hash === '#/release-checklist' && !document.getElementById('editor').hidden && document.getElementById('text').value.startsWith('# Release checklist')", wait), "");
    chrome.eval(&page, "document.getElementById('text').value += '- [ ] Tag it\\n- [x] Write the notes\\n'; document.getElementById('editor').requestSubmit(); true")?;
    s.ok("saved, it is a page, its tasks rendered", chrome.until(&page, "document.querySelectorAll('#view li.task input').length === 2", wait), "");

    // as a person sees it, on a phone and on a laptop
    chrome.eval(&page, "location.hash = '#/home'; true")?;
    chrome.until(&page, &has("One more line"), wait);
    let shots = s.dir("wiki");
    for (file, w, h, mobile, scheme) in [("phone.png", 390, 844, true, "light"), ("laptop.png", 1280, 800, false, "light"), ("laptop-dark.png", 1280, 800, false, "dark")] {
        chrome.viewport(&page, w, h, mobile)?;
        chrome.color_scheme(&page, scheme)?;
        std::thread::sleep(Duration::from_millis(300));
        let _ = chrome.screenshot(&page, &shots.join(file));
    }
    println!("      (screenshots in {})", shots.display());

    // only pages: nothing outside wiki/, nothing that climbs out of it
    for (i, path) in ["site/index.html", "wiki/../app.mjs", "wiki/.hidden.md", "app.md"].into_iter().enumerate() {
        let r = api.op(&owner, &name, "save", &format!("bad-{i}"), json!({ "path": path, "text": "x" }))?;
        s.ok(&format!("a save of {path} is refused"), r.status == 422 && r.message().contains("a page is a .md file under wiki/"), &r);
    }
    Ok(())
}
