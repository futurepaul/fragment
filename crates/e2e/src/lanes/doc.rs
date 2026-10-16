//! The doc template (templates/doc): a page its readers leave notes on,
//! paragraph by paragraph. Its notes are a mixin in `applib/notes.mjs`
//! whose four operations are inherited methods, so this proves the
//! platform finds an App's operations up its prototype chain. Made as an
//! agent makes it (`fragment create --template doc`), public. Valid: a
//! signed-in reader annotates, quoting, and replies; the thread's author
//! resolves it, a reply reopens it, the owner resolves and deletes; an
//! agent's path is the CLI; every change is a record on `notes`. Invalid:
//! an anonymous visitor's note, a stranger's resolve and delete, a note on
//! no paragraph, a reply to a note that is gone. Replay: the same id is
//! the same note, another input under it a conflict, a delete twice
//! removes nothing, and a deleted note's id is never another's. Restart:
//! the notes survive the node's. In Chrome: an anonymous reader's note
//! button asks them to sign in, the sign-in brings them back, their note
//! reaches a second page live, and cursors are no one's until the page
//! turns them on (`<html data-cursors>`).

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use crate::api::{Api, Reply, Socket};
use crate::Suite;

const WAIT: Duration = Duration::from_secs(20);
const VERBS: [&str; 4] = ["notes", "annotate", "resolve", "remove_note"];
const FROM_CHROME: &str = "A question from Chrome: which document?";

/// Every note on the page, as `keys` reads them (none when the read fails).
fn notes(api: &Api, keys: &Keys, name: &str) -> Vec<Value> {
    api.op(keys, name, "notes", "q", json!({})).ok().and_then(|r| r.body["result"]["notes"].as_array().cloned()).unwrap_or_default()
}

fn ids(notes: &[Value]) -> Vec<i64> {
    notes.iter().filter_map(|n| n["id"].as_i64()).collect()
}

/// The note with `id`, if the page holds it.
fn note(notes: &[Value], id: i64) -> Value {
    notes.iter().find(|n| n["id"] == id).cloned().unwrap_or_default()
}

fn id_of(r: &Reply) -> i64 {
    r.body["result"]["id"].as_i64().unwrap_or(-1)
}

/// An app's refusal: 422, saying `why`.
fn refused(r: &Reply, why: &str) -> bool {
    r.status == 422 && r.code() == Some(ErrorCode::AppFailed) && r.message().contains(why)
}

pub fn doc(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("doc", &[crate::Need::Chrome]) {
        return Ok(());
    }
    // the owner works from the CLI, as an agent does
    let home = s.dir("doc-home");
    s.login(api, &home);
    let owner = s.cli_keys(&home).context("the CLI logged in")?;
    let label = s.name("doc");
    let made = s.cli_json(api, &home, &["create", &label, "--template", "doc", "--visibility", "public", "--json"]).unwrap_or_else(|e| json!({ "error": format!("{e:#}") }));
    let name = made["name"].as_str().unwrap_or("").to_string();
    s.ok("the CLI makes a doc from its template, public", made["visibility"] == "public" && !name.is_empty(), &made);
    anyhow::ensure!(!name.is_empty(), "no doc to test: {made}");
    let st = api.status(&owner, &name)?;
    if st.body["code"]["sha"].is_null() {
        // the create answers once its template is live: say why it is not
        let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=20"), None)?;
        anyhow::bail!("the doc's template did not land: {} / {events}", st.body["code"]);
    }
    let ops = &st.body["code"]["operations"];
    let names: BTreeSet<&str> = ops.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    s.ok(
        "its code installs the notes' four operations, inherited from the mixin, each described and public",
        st.body["code"]["error"].is_null()
            && names == BTreeSet::from(VERBS)
            && VERBS.iter().all(|v| ops[v]["description"].as_str().is_some_and(|d| !d.is_empty()) && ops[v]["role"] == "public"),
        &st.body["code"],
    );
    let page = api.page(&name, "", None)?;
    s.ok("its page is open to anyone, with blocks that take notes", page.status == 200 && page.text.contains("data-a=\"lede\"") && page.text.contains("./notes.js"), &page);

    // valid: a signed-in reader (no member: a public doc's `public` role) annotates and replies
    let reader = api.person()?;
    let reader_id = api.identity(&reader)?;
    let stranger = api.person()?;
    let owner_id = api.identity(&owner)?;
    let r = api.op(&reader, &name, "annotate", "r1", json!({ "anchor": "lede", "quote": "the way a shared document", "text": "Which document?" }))?;
    let first = id_of(&r);
    s.ok("a signed-in reader leaves a note on a paragraph, quoting it: its first note is its thread", r.status == 200 && first > 0 && r.body["result"]["thread"] == first, &r);
    let r = api.op(&reader, &name, "annotate", "r2", json!({ "reply_to": first, "text": "Any you share." }))?;
    let reply = id_of(&r);
    s.ok("and replies in the thread", r.status == 200 && reply > first && r.body["result"]["thread"] == first, &r);
    let all = notes(api, &owner, &name);
    let (a, b) = (note(&all, first), note(&all, reply));
    s.ok(
        "the notes say where, what and whose: the paragraph, the quote, its author and their role",
        all.len() == 2
            && a["anchor"] == "lede"
            && a["quote"] == "the way a shared document"
            && a["author"] == reader_id.as_str()
            && a["agent"].is_null()
            && a["role"] == "public"
            && a["reply_to"].is_null()
            && a["resolved"] == false
            && b["reply_to"] == first
            && b["anchor"] == "lede"
            && b["quote"] == "",
        json!(all),
    );
    let channel = |api: &Api| api.signed(&owner, "GET", &format!("/api/f/{name}/channels/notes"), None).map(|r| r.body["records"].as_array().cloned().unwrap_or_default());
    let records = channel(api)?;
    s.ok(
        "each is a record on the notes channel",
        records.len() == 2 && records.iter().all(|r| r["kind"] == "annotated") && records[1]["body"]["id"] == reply && records[1]["body"]["thread"] == first,
        json!(records),
    );

    // replay: the same id is the same note
    let r = api.op(&reader, &name, "annotate", "r1", json!({ "anchor": "lede", "quote": "the way a shared document", "text": "Which document?" }))?;
    s.ok("a retry with the same id answers the same note, and adds none", r.status == 200 && r.body["replayed"] == true && id_of(&r) == first && notes(api, &owner, &name).len() == 2, &r);
    let r = api.op(&reader, &name, "annotate", "r1", json!({ "anchor": "lede", "text": "Something else" }))?;
    s.ok("another note under that id is a conflict", r.status == 409 && r.code() == Some(ErrorCode::ConflictingBody), &r);

    // invalid: who and where
    let r = api.browser_op(&name, "annotate", "a1", json!({ "anchor": "lede", "text": "anonymous" }), None)?;
    s.ok("an anonymous visitor's note is refused", refused(&r, "Sign in"), &r);
    let r = api.browser_op(&name, "notes", "q", json!({}), None)?;
    s.ok("though they read the notes", r.status == 200 && ids(r.body["result"]["notes"].as_array().map(Vec::as_slice).unwrap_or_default()) == vec![first, reply], &r);
    let r = api.op(&reader, &name, "annotate", "r3", json!({ "anchor": "not an id!", "text": "x" }))?;
    let r2 = api.op(&reader, &name, "annotate", "r4", json!({ "text": "on nothing" }))?;
    s.ok("a note on no paragraph's id is refused", refused(&r, "data-a") && refused(&r2, "data-a"), format!("{r} / {r2}"));
    let r = api.op(&reader, &name, "annotate", "r5", json!({ "reply_to": first, "anchor": "notes-1", "text": "elsewhere" }))?;
    s.ok("a reply names no other paragraph than its thread's", refused(&r, "thread's paragraph"), &r);

    // the rules: a thread's author or the page's owner
    let r = api.op(&stranger, &name, "resolve", "s1", json!({ "id": first, "resolved": true }))?;
    let r2 = api.op(&stranger, &name, "remove_note", "s2", json!({ "id": reply }))?;
    s.ok(
        "someone else's resolve and delete are refused, and change nothing",
        refused(&r, "Only the thread's author") && refused(&r2, "Only a note's author") && ids(&notes(api, &owner, &name)) == vec![first, reply] && !note(&notes(api, &owner, &name), first)["resolved"].as_bool().unwrap_or(true),
        format!("{r} / {r2}"),
    );
    let r = api.op(&reader, &name, "resolve", "r6", json!({ "id": reply, "resolved": true }))?;
    s.ok("the thread's author resolves it, by any of its notes", r.status == 200 && r.body["result"] == json!({ "id": first, "resolved": true }) && note(&notes(api, &owner, &name), first)["resolved"] == true, &r);
    let r = api.op(&stranger, &name, "annotate", "s3", json!({ "reply_to": first, "text": "Not done yet." }))?;
    let theirs = id_of(&r);
    s.ok("anyone signed in replies, and a reply reopens a resolved thread", r.status == 200 && note(&notes(api, &owner, &name), first)["resolved"] == false, &r);

    // an agent's path: the CLI, as the page's owner
    let cli = |s: &Suite, args: &[&str]| -> Value {
        let mut all = vec!["call", name.as_str()];
        all.extend_from_slice(args);
        all.push("--json");
        s.cli_json(api, &home, &all).unwrap_or_else(|e| json!({ "error": format!("{e:#}") }))
    };
    let listed = cli(s, &["notes"]);
    s.ok("an agent reads the notes with fragment call", ids(listed["result"]["notes"].as_array().map(Vec::as_slice).unwrap_or_default()) == vec![first, reply, theirs], &listed);
    let answered = cli(s, &["annotate", "--input", &json!({ "reply_to": first, "text": "Any document you share: a spec, a draft." }).to_string(), "--id", "c1"]);
    let resolved = cli(s, &["resolve", "--input", &json!({ "id": first, "resolved": true }).to_string()]);
    let all = notes(api, &owner, &name);
    let by_owner = note(&all, answered["result"]["id"].as_i64().unwrap_or(-1));
    s.ok(
        "it answers in the thread and resolves it, as the owner",
        by_owner["author"] == owner_id.as_str() && by_owner["role"] == "owner" && resolved["result"] == json!({ "id": first, "resolved": true }),
        json!({ "answered": answered, "resolved": resolved }),
    );
    let removed = cli(s, &["remove_note", "--input", &json!({ "id": theirs }).to_string()]);
    let again = cli(s, &["remove_note", "--input", &json!({ "id": theirs }).to_string()]);
    s.ok(
        "the owner deletes anyone's note, for good; deleting it again removes nothing",
        removed["result"]["removed"] == 1 && again["result"]["removed"] == 0 && !ids(&notes(api, &owner, &name)).contains(&theirs),
        json!({ "removed": removed, "again": again }),
    );
    // the newest note goes too (its author's own delete): SQLite would hand
    // its id to the next note, but for AUTOINCREMENT
    let own = answered["result"]["id"].as_i64().unwrap_or(-1);
    let gone = cli(s, &["remove_note", "--input", &json!({ "id": own }).to_string()]);
    let r = api.op(&reader, &name, "annotate", "r7", json!({ "reply_to": theirs, "text": "to a note that is gone" }))?;
    let r2 = api.op(&reader, &name, "annotate", "r8", json!({ "anchor": "notes-2", "text": "Who resolves?" }))?;
    let newest = id_of(&r2);
    s.ok(
        "a reply to a deleted note is refused, and no new note takes a deleted one's id",
        gone["result"]["removed"] == 1 && refused(&r, "gone") && r2.status == 200 && newest > own && ids(&notes(api, &owner, &name)) == vec![first, reply, newest],
        format!("{gone} / {r} / {r2}"),
    );
    let kinds: Vec<String> = channel(api)?.iter().filter_map(|r| r["kind"].as_str().map(str::to_string)).collect();
    s.ok(
        "the channel tells each change in order",
        kinds == ["annotated", "annotated", "resolved", "annotated", "annotated", "resolved", "removed", "removed", "annotated"],
        json!(kinds),
    );

    // restart: the notes are the app's own SQLite
    let before = notes(api, &owner, &name);
    let restarted;
    let api = match s.hosted() {
        true => {
            s.skip("the notes survive a restart", "the hosted lane restarts no node");
            api
        }
        false => {
            s.stop()?;
            restarted = s.start(false)?;
            &restarted
        }
    };
    if !s.hosted() {
        let after = notes(api, &owner, &name);
        s.ok("the notes survive a restart, as they were", !before.is_empty() && after == before, json!({ "before": before, "after": after }));
    }

    // Chrome: a reader's page, and a second, anonymous one
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the doc's page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let session = api.sign_in(&Api::email_of(&reader))?;
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let url = api.site_url(&name, "");
    let a = chrome.open(&url)?;
    chrome.viewport(&a, 1400, 900, false)?;
    let other = chrome.another_context()?;
    let b = chrome.open_in(&other, &url)?;
    chrome.viewport(&b, 1400, 900, false)?;
    let shown = |text: &str| format!("getComputedStyle(document.querySelector('.notes-gutter')).display === 'block' && document.querySelector('.notes-gutter').innerText.includes({text:?})");
    let loaded = chrome.until(&a, &shown("Who resolves?"), WAIT) && chrome.until(&b, &shown("Who resolves?"), WAIT);
    s.ok("on a wide window the notes sit in the margin, for a reader and an anonymous visitor", loaded, chrome.eval(&a, "document.body.innerText.slice(0, 300)")?);
    let resolved_shown = chrome.eval(&b, &format!("document.querySelector('[data-thread=\"{first}\"]')?.classList.contains('resolved') ?? false"))?;
    s.ok("a resolved thread folds to a line", resolved_shown == true, &resolved_shown);

    // not signed in on the doc yet: the note button asks them to sign in, and brings them back
    chrome.hover(&a, "[data-a=\"notes-1\"]")?;
    let offered = chrome.until(&a, "!document.querySelector('.notes-btn').hidden", WAIT);
    chrome.click(&a, ".notes-btn")?;
    let asked = chrome.until(&a, "document.querySelector('.notes-composer a.notes-signin')?.getAttribute('href') === './__signin?return=%2F'", WAIT);
    s.ok("a reader not signed in on the doc gets the note button, which asks them to sign in", offered && asked, chrome.eval(&a, "document.querySelector('.notes-composer')?.outerHTML ?? ''")?);
    chrome.click(&a, ".notes-composer a.notes-signin")?;
    let consent = chrome.until(&a, "location.pathname === '/auth/fragment' && document.body.innerText.includes('Continue to')", WAIT);
    let armed = consent && chrome.until(&a, "document.querySelector('button[data-arm]')?.disabled === false", Duration::from_secs(5));
    if armed {
        chrome.click(&a, "button[data-arm]")?;
    }
    let back = chrome.until(&a, &format!("location.host === new URL({url:?}).host && location.pathname === '/' && {}", shown("Who resolves?")), WAIT);
    s.ok("the platform asks once, then brings them back to the doc", consent && armed && back, json!({ "consent": consent, "armed": armed, "back": back, "at": chrome.eval(&a, "location.href")? }));

    // signed in: a note through the page, and the second page sees it live
    chrome.hover(&a, "[data-a=\"notes-1\"]")?;
    chrome.until(&a, "!document.querySelector('.notes-btn').hidden", WAIT);
    chrome.click(&a, ".notes-btn")?;
    let writing = chrome.until(&a, "!!document.querySelector('.notes-composer textarea')", WAIT);
    chrome.click(&a, ".notes-composer textarea")?;
    chrome.type_text(&a, FROM_CHROME)?;
    chrome.click(&a, ".notes-composer button[type=submit]")?;
    let posted = writing && chrome.until(&a, &format!("!document.querySelector('.notes-composer textarea') && {}", shown(FROM_CHROME)), WAIT);
    let theirs = notes(api, &owner, &name).into_iter().find(|n| n["text"] == FROM_CHROME).unwrap_or_default();
    s.ok("signed in, they leave a note from the page, as themselves", posted && theirs["anchor"] == "notes-1" && theirs["author"] == reader_id.as_str(), &theirs);
    s.ok("a second page sees the new note live, without a reload", chrome.until(&b, &shown(FROM_CHROME), WAIT), chrome.eval(&b, "document.querySelector('.notes-gutter')?.innerText ?? ''")?);

    // cursors: no one's, until the page turns them on
    for block in ["lede", "notes-1", "notes-2", "agents-1"] {
        chrome.hover(&a, &format!("[data-a=\"{block}\"]"))?;
    }
    let sharing = |api: &Api| -> Result<Vec<Value>> {
        let mut probe = Socket::open(api, &name, "__live", Some(&owner), None)?;
        let hello = probe.until("hello", 5)?;
        probe.close();
        Ok(hello["presence"].as_array().cloned().unwrap_or_default())
    };
    let none = sharing(api)?;
    let drawn = chrome.eval(&a, "!!document.querySelector('.notes-cursors')")? == true || chrome.eval(&b, "!!document.querySelector('.notes-cursors')")? == true;
    s.ok("cursors are off by default: the pages share no presence, and draw none", none.is_empty() && !drawn, json!(none));
    let html = fragment_templates::DOC.file("site/index.html").context("the doc's page")?.bytes;
    let html = std::str::from_utf8(html)?.replacen("<html lang=\"en\">", "<html lang=\"en\" data-cursors>", 1);
    anyhow::ensure!(html.contains("data-cursors>"), "the template's page opens with <html lang=\"en\">");
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/files"), Some(&json!({ "files": [{ "path": "site/index.html", "text": html }], "message": "cursors on" })))?;
    let r2 = api.signed(&owner, "POST", &format!("/api/f/{name}/deploy"), Some(&json!({ "note": "cursors on" })))?;
    anyhow::ensure!(r.status == 200 && r2.status == 200, "turning cursors on: {r} / {r2}");
    chrome.reload(&a)?;
    chrome.reload(&b)?;
    let layer = format!("!!document.querySelector('.notes-cursors') && {}", shown(FROM_CHROME));
    let on = chrome.until(&a, &layer, WAIT) && chrome.until(&b, &layer, WAIT);
    // the latest of a page's pointer is sent, once its socket is open
    chrome.hover(&a, "[data-a=\"notes-2\"]")?;
    let seen = chrome.until(&b, "!!document.querySelector('.notes-cursors .notes-cursor')", WAIT);
    let shared = sharing(api)?;
    s.ok(
        "with <html data-cursors>, a reader's cursor reaches the other page, anchored to a paragraph",
        on && seen && shared.iter().any(|p| p["principal"] == reader_id.as_str() && p["data"]["a"].is_string()),
        json!(shared),
    );
    Ok(())
}
