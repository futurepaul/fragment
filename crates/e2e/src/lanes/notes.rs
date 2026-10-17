//! The notes template (slice E): a folder of markdown scaffolded, synced,
//! and deployed with the CLI; the viewer reads the notes at `main` through
//! the app's routes, and open pages follow new notes without a deploy.

use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use crate::api::{Api, Call};
use crate::Suite;

pub fn notes(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("notes", &[crate::Need::Chrome]) {
        return Ok(());
    }
    let home = s.dir("notes-home");
    s.login(api, &home);
    let keys = s.cli_keys(&home).expect("the CLI logged in");
    let c = s.cli_json(api, &home, &["create", &s.name("notes"), "--show-tokens", "--json"])?;
    let name = c["name"].as_str().unwrap_or("").to_string();
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
    let Some(mut chrome) = s.browser()? else {
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
    hostile(s, api, &home, &name, &dir, &mut chrome, &page)
}

/// What a note, an editor's or an agent's, holds is data (audit H2: the
/// viewer once put `marked`'s output in `innerHTML`, and a note's `<img
/// onerror>` ran as each of its viewers). Goal: the viewer renders a note's
/// markup as its text and runs none of it; the platform's renderer is
/// served and holds to that in a real browser; a note's SVG served by the
/// app is sandboxed. Method: a committed hostile note opened in the
/// viewer; the renderer imported on the page with hooks that allow any
/// URL; an SVG that scripts itself, read and opened directly.
fn hostile(s: &mut Suite, api: &Api, home: &std::path::Path, name: &str, dir: &std::path::Path, chrome: &mut crate::browser::Browser, page: &crate::browser::Page) -> Result<()> {
    let wait = Duration::from_secs(20);
    // as the page reads: with its share link
    let query = page_query(chrome, page)?;
    let get = |path: &str| api.call(Call { method: "GET", url: format!("{}{}{query}", api.site_url(name, path), if path.contains('?') { "&" } else { "?" }), ..Call::default() });
    let dir_s = dir.to_str().expect("utf-8 path");
    std::fs::write(dir.join("notes/hostile.md"), HOSTILE_NOTE)?;
    std::fs::write(dir.join("notes/pic.svg"), SCRIPTED_SVG)?;
    s.cli(api, home, &["sync", name, "--dir", dir_s]);
    chrome.eval(page, "location.hash = '#/notes/hostile.md'; true")?;
    let shown = chrome.until(page, "document.querySelector('#content .md h1')?.textContent === 'A hostile note'", wait);
    std::thread::sleep(Duration::from_millis(500));
    s.ok(
        "a note's <img onerror>, <script>, <svg onload> and javascript: links render as their text, and none of it runs",
        shown && chrome.eval(page, "(() => { const c = document.getElementById('content'); return window.__ran === undefined && !c.querySelector('img,script,svg,iframe,a[href^=\"javascript\" i]') && c.textContent.includes('<img src=x onerror=') && c.textContent.includes('<script>'); })()")? == true,
        chrome.eval(page, "JSON.stringify({ ran: window.__ran ?? null, text: document.getElementById('content')?.innerText.slice(0, 400) })")?,
    );
    let r = get("__markdown.js")?;
    s.ok("the platform's renderer is served to every fragment, tagged with its build-time hash", r.status == 200 && r.header("content-type").starts_with("text/javascript") && r.header("etag").len() == 18 && r.text.contains("export function markdown"), &r);
    let rendered = chrome.eval(page, "import('./__markdown.js').then((m) => { const box = document.createElement('div'); box.append(m.markdown('<img src=x onerror=\"window.__ran2 = 1\">\\n\\n[x](javascript:window.__ran2=2) [y](\\u0001javascript:window.__ran2=3) ![z](javascript:1) [[w]]', { resolve: (t) => ({ href: t }), image: (src) => ({ src, href: src }) })); document.body.append(box); return new Promise((done) => setTimeout(() => done(window.__ran2 === undefined && !box.querySelector('img,script,a[href^=\"javascript\" i]') && box.textContent.startsWith('<img')), 300)); })")?;
    s.ok("in Chrome the renderer makes no element of markup and no script link, even when its hooks allow every URL", rendered == true, &rendered);
    let r = get("api/file?path=notes/pic.svg")?;
    s.ok(
        "an SVG the app serves is sandboxed, and never sniffed",
        r.status == 200 && r.header("content-type").starts_with("image/svg+xml") && r.header("content-security-policy").contains("sandbox") && r.header("x-content-type-options") == "nosniff",
        &r,
    );
    let svg = chrome.open(&api.site_url(name, &format!("api/file?path=notes/pic.svg&{query}")))?;
    std::thread::sleep(Duration::from_millis(500));
    let ran = chrome.eval(&svg, "document.documentElement.getAttribute('data-ran')")?;
    s.ok("opened directly, the SVG's script does not run", ran.is_null(), &ran);
    chrome.close(svg)?;
    Ok(())
}

/// The page's own query (its `view=` token), for a URL beside it.
fn page_query(chrome: &mut crate::browser::Browser, page: &crate::browser::Page) -> Result<String> {
    Ok(chrome.eval(page, "location.search.slice(1)")?.as_str().unwrap_or("").to_string())
}

const HOSTILE_NOTE: &str = include_str!("../../fixtures/hostile-note.md");
const SCRIPTED_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"document.documentElement.setAttribute('data-ran', '1')\"><circle cx=\"4\" cy=\"4\" r=\"4\"/></svg>\n";
