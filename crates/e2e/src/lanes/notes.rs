//! The notes template (slice E): a folder of markdown scaffolded, synced,
//! and deployed with the CLI; the viewer reads the notes at `main` through
//! the app's routes, and open pages follow new notes without a deploy.

use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use crate::api::{Api, Call};
use crate::browser::Browser;
use crate::Suite;

pub fn notes(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("notes") {
        return Ok(());
    }
    let home = s.dir("notes-home");
    s.cli(api, &home, &["login"]);
    let keys = s.cli_keys(&home).expect("the CLI logged in");
    let name = s.name("notes");
    let c = s.cli_json(api, &home, &["create", &name, "--json"])?;
    s.hook(api, &c);
    let root = s.dir("notes");
    let dir = root.join("vault");
    let dir_s = dir.to_str().expect("utf-8 path").to_string();
    let out = s.cli(api, &home, &["new", &dir_s, "--template", "notes"]);
    s.ok("fragment new scaffolds the notes template", out.status.success() && dir.join("site/assets/viewer.js").is_file(), String::from_utf8_lossy(&out.stderr));
    let out = s.cli(api, &home, &["deploy", &name, "--dir", &dir_s]);
    s.ok("it deploys", out.status.success(), String::from_utf8_lossy(&out.stderr));

    let view = c["viewToken"].as_str().unwrap_or("").to_string();
    let get = |path: &str| {
        api.call(Call { method: "GET", url: format!("{}{}view={view}", api.site_url(&name, path), if path.contains('?') { "&" } else { "?" }), ..Call::default() })
    };
    let r = get("api/tree")?;
    let paths: Vec<&str> = r.body["files"].as_array().map(|a| a.iter().filter_map(|f| f["path"].as_str()).collect()).unwrap_or_default();
    s.ok(
        "api/tree lists the notes, not the machinery (the app's route on its own host)",
        r.status == 200 && paths.contains(&"notes/welcome.md") && paths.contains(&"README.md") && !paths.iter().any(|p| p.starts_with("site/") || *p == "app.mjs"),
        &r,
    );
    let r = get("api/file?path=notes/welcome.md")?;
    s.ok("api/file serves a note", r.status == 200 && r.text.contains("# welcome to your notes"), &r);

    // a new note needs no deploy: the viewer reads main
    std::fs::write(dir.join("notes/second.md"), "# second\n")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_s]);
    let r = get("api/file?path=notes/second.md")?;
    s.ok("a synced note is served without a deploy", r.status == 200 && r.text == "# second\n", &r);
    let changed = s.eventually(Duration::from_secs(20), || {
        api.op(&keys, &name, "last_change", "q", json!({}))
            .is_ok_and(|r| r.body["result"]["paths"].as_array().is_some_and(|p| p.iter().any(|x| x == "notes/second.md")))
    });
    s.ok("the file trigger recorded the change", changed, "");

    // the viewer, in a browser: it renders the tree and follows new notes
    let Some(mut chrome) = Browser::launch(&s.scratch)? else {
        s.ok("Chrome is installed for the notes viewer (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let page = chrome.open(&api.site_url(&name, &format!("?view={view}")))?;
    let wait = Duration::from_secs(20);
    let has = |n: &str| format!("document.getElementById('tree')?.textContent.includes('{n}')");
    s.ok("the viewer renders the notes tree", chrome.until(&page, &has("second"), wait), "");
    std::fs::write(dir.join("notes/third.md"), "# third\n")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_s]);
    s.ok("an open page shows a new note without a reload", chrome.until(&page, &has("third"), wait), "");
    chrome.eval(&page, "location.hash = '#/notes/third.md'; true")?;
    s.ok("and opens it", chrome.until(&page, "document.getElementById('content')?.textContent.includes('third')", wait), "");
    Ok(())
}
