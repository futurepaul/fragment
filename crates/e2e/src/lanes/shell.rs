//! The shell's platform (docs/cloudflare-v1.md, phase 5): what its page
//! asks of the API and the fragments it frames.
//!
//! The shell is the platform's own page, so it calls the API with the
//! person's platform session rather than a key: only a same-origin request
//! that carries the shell's header (and, writing, the platform's Origin)
//! is taken that way; a fragment's page on the same site, or any other,
//! is not. A chat or an agent is a fragment on a blessed template
//! (decision 40), which the platform's release serves: its repo names the
//! template and holds only its face and data. A person's list says what
//! each fragment is (its kind) and its title.

use anyhow::Result;
use serde_json::{json, Value};

use crate::api::{Api, Call, Reply};
use crate::browser::{Browser, Page};
use crate::Suite;

/// A request as the shell's page sends it: the session cookie, the
/// shell's header, `Sec-Fetch-Site: same-origin`, and its Origin.
pub(super) fn shell(api: &Api, session: &str, method: &'static str, path: &str, body: Option<&Value>, extra: &[(&'static str, String)]) -> Result<Reply> {
    let mut headers: Vec<(&str, String)> = vec![("x-fragment-shell", "1".into()), ("sec-fetch-site", "same-origin".into()), ("origin", api.base.clone())];
    for (k, v) in extra {
        headers.retain(|(h, _)| h != k);
        headers.push((k, v.clone()));
    }
    api.call(Call {
        method,
        url: format!("{}{path}", api.base),
        body: body.map(|b| b.to_string().into_bytes()),
        content_type: body.map(|_| "application/json"),
        cookie: Some(format!("fragment_session={session}")),
        extra: headers.into_iter().filter(|(_, v)| !v.is_empty()).collect(),
        ..Call::default()
    })
}

pub fn shell_platform(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("shell") {
        return Ok(());
    }
    let email = format!("shell-{}@e2e.test", crate::api::now_s());
    let session = api.sign_in(&email)?;

    // the shell's own requests act as the signed-in person
    let r = shell(api, &session, "GET", "/api/identities/me", None, &[])?;
    let id = r.body["id"].as_str().unwrap_or("").to_string();
    s.ok("the shell's page reads the API as its signed-in person, with no key", r.status == 200 && r.body["kind"] == "person", &r);
    let username = format!("sh{}", &id.trim_start_matches("id:")[..8]);
    let r = shell(api, &session, "PUT", "/api/identities/me/username", Some(&json!({ "username": username })), &[])?;
    s.ok("and chooses its username", r.status == 200 && r.body["username"] == username.as_str(), &r);
    let r = shell(api, &session, "GET", "/api/identities/me", None, &[("x-fragment-shell", String::new())])?;
    s.ok("a request without the shell's header is no one's (401)", r.status == 401, &r);
    let r = shell(api, &session, "GET", "/api/identities/me", None, &[("sec-fetch-site", "same-site".into())])?;
    s.ok("nor one from another origin of the site, a fragment's page (401)", r.status == 401, &r);
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "name": "nope" })), &[("origin", String::new())])?;
    s.ok("nor a write without the platform's Origin (401)", r.status == 401, &r);
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "name": "nope" })), &[("origin", api.site_origin(&format!("x.{username}")))])?;
    s.ok("nor a write from a fragment's origin (401)", r.status == 401, &r);
    let r = shell(api, "f".repeat(64).as_str(), "GET", "/api/identities/me", None, &[])?;
    s.ok("a session that is not one is refused (401)", r.status == 401, &r);
    let r = shell(api, &session, "POST", &format!("/api/identities/{id}/keys"), Some(&json!({ "proof": "x" })), &[])?;
    s.ok("a key is added only by a key you hold, never the shell's session", r.status == 401, &r);

    // a chat and an agent, on blessed templates
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "name": "juniper", "template": "agent", "title": "Juniper" })), &[])?;
    let agent = r.body["name"].as_str().unwrap_or("").to_string();
    s.ok("the shell makes an agent fragment on the agent template", r.status == 200 && agent == format!("juniper.{username}"), &r);
    let landed = s.eventually(std::time::Duration::from_secs(30), || {
        shell(api, &session, "GET", "/api/fragments", None, &[]).is_ok_and(|r| {
            r.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == agent.as_str() && f["kind"] == "agent" && f["title"] == "Juniper"))
        })
    });
    let list = shell(api, &session, "GET", "/api/fragments", None, &[])?;
    s.ok("the person's list says it is an agent, and its title", landed, &list);
    let manifest = shell(api, &session, "GET", &format!("/api/f/{agent}/manifest"), None, &[])?;
    s.ok("its repo names the template and its face, nothing else", manifest.body == json!({ "template": "agent", "meta": { "title": "Juniper" } }), &manifest);
    let page = api.call(Call { method: "GET", url: api.site_url(&agent, ""), cookie: None, ..Call::default() })?;
    let owner_page = shell_site(s, api, &session, &agent, "")?;
    s.ok(
        "its page is the release's template, with its own title",
        owner_page.status == 200 && owner_page.text.contains("agent.js") && owner_page.text.contains(r#"og:title" content="Juniper""#),
        format!("{} / anonymous {}", owner_page.status, page.status),
    );
    let script = shell_site(s, api, &session, &agent, "agent.js")?;
    s.ok("and its script, from the release", script.status == 200 && script.text.contains("SOUL.md"), script.status);
    let channels = shell(api, &session, "GET", &format!("/api/f/{agent}/channels"), None, &[])?;
    s.ok(
        "the template's channels run on it",
        channels.body["channels"].as_array().is_some_and(|c| c.iter().any(|c| c["name"] == "tasks")),
        &channels,
    );
    // its data is its own: a job, read from its repo by the release's page
    let r = shell(
        api,
        &session,
        "POST",
        &format!("/api/f/{agent}/files"),
        Some(&json!({ "files": [{ "path": "SOUL.md", "text": "Water the tomatoes.\n" }, { "path": "agent.json", "text": "{\"tier\":\"cheap\",\"color\":\"#62c8af\"}" }] })),
        &[],
    )?;
    let deploy = shell(api, &session, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})), &[])?;
    s.ok("its job and settings are files of its own, deployed", r.status == 200 && deploy.status == 200, format!("{r} {deploy}"));
    let soul = shell_site(s, api, &session, &agent, "__file?path=SOUL.md")?;
    s.ok("which its page reads", soul.status == 200 && soul.text.contains("tomatoes"), soul.status);

    // code of its own is a fork's, refused while it names the template
    let r = shell(
        api,
        &session,
        "POST",
        &format!("/api/f/{agent}/files"),
        Some(&json!({ "files": [{ "path": "fragment.json", "text": "{\"template\":\"agent\",\"channels\":{\"x\":{\"read\":\"viewer\",\"post\":\"editor\"}}}" }] })),
        &[],
    )?;
    shell(api, &session, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})), &[])?;
    let refused = s.eventually(std::time::Duration::from_secs(30), || {
        shell(api, &session, "GET", &format!("/api/f/{agent}/status"), None, &[]).is_ok_and(|r| r.body["code"]["error"].as_str().is_some_and(|e| e.contains("fork it")))
    });
    s.ok("a fragment on a template that declares code of its own is refused, saying to fork", r.status == 200 && refused, "");

    // connections (decision 22): the deployment's offers, and the person's account at each
    let status = |r: &Reply| r.body["connections"].as_array().and_then(|l| l.iter().find(|c| c["provider"] == crate::SWAP_CONNECTION)).map(|c| c["status"].clone());
    let r = shell(api, &session, "GET", "/api/connections", None, &[])?;
    s.ok("the shell lists the connections the deployment offers, none connected yet", status(&r) == Some(json!("none")), &r);
    let r = shell(api, &session, "POST", &format!("/api/connections/{}/authorize", crate::SWAP_CONNECTION), Some(&json!({})), &[])?;
    let consent = r.body["url"].as_str().unwrap_or("").to_string();
    s.ok("and starts one: a consent URL for the person's browser", r.status == 200 && consent.starts_with(&s.workos.url), &r);
    let done = api.external(&consent)?;
    let r = shell(api, &session, "GET", "/api/connections", None, &[])?;
    s.ok("followed, the account is connected", done.status == 200 && status(&r) == Some(json!("connected")), &r);
    let again = api.external(&consent)?;
    s.ok("(a consent is followed once)", again.status == 400, &again);
    let r = shell(api, &session, "POST", "/api/connections/notion/authorize", Some(&json!({})), &[])?;
    s.ok("a provider the deployment does not offer is none to connect (404)", r.status == 404, &r);

    // a template that is not blessed is copied, and its kind is what it says
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "name": "garden", "template": "todo", "title": "x" })), &[])?;
    s.ok("a title is a blessed template's alone", r.status == 400, &r);
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "name": "lab", "template": "nope" })), &[])?;
    s.ok("a template that is none is refused, naming the blessed ones", r.status == 400 && r.text.contains("agent"), &r);
    search_and_archive(s, api, &session, &username)
}

/// How long a message takes to reach a person's search: its fragment's
/// alarm sends it, and a backoff waits on a list that holds no role yet.
const SEARCH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A search's message hits in `fragment`.
fn hits_in(r: &Reply, fragment: &str) -> Vec<Value> {
    r.body["messages"].as_array().map(|l| l.iter().filter(|m| m["fragment"] == fragment).cloned().collect()).unwrap_or_default()
}

/// `q`, as a URL's query value.
fn query(q: &str) -> String {
    crate::api::url_enc(q)
}

/// Search (decision 9) and archiving, the person's own view (docs/api.md,
/// Search): a message's words find it, for the chat's people only, in
/// fragments they are in now; a query is words, never FTS5 syntax; a
/// person archives a fragment for themselves alone, twice the same as
/// once, and only one of theirs. Goal: the person's list is a fenced
/// projection (lesson 12). Method: three people (the shell's person, a
/// member who is removed and comes back, and an outsider with a chat of
/// their own), each asking their own list through the API.
fn search_and_archive(s: &mut Suite, api: &Api, session: &str, username: &str) -> Result<()> {
    let r = shell(api, session, "POST", "/api/fragments", Some(&json!({ "name": "garden-talk", "template": "chat", "title": "Garden talk" })), &[])?;
    let chat = r.body["name"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && chat == format!("garden-talk.{username}"), "making the chat: {r}");
    let said = "Our tomatoes need water every Tuesday";
    let post = |id: &str, channel: &str, body: Value| shell(api, session, "POST", &format!("/api/f/{chat}/channels/{channel}"), Some(&json!({ "id": id, "body": body })), &[]);
    // the template's channels are the chat's as soon as it installs
    let took = s.eventually(SEARCH_WAIT, || post("m1", "chat", json!({ "text": said })).is_ok_and(|r| r.status == 200));
    let posted = post("m1", "chat", json!({ "text": said }))?;
    anyhow::ensure!(took && posted.body["replayed"] == true, "posting to the chat: {posted}");
    let seq = posted.body["record"]["seq"].as_i64().unwrap_or(0);
    // a record that is no message (a page's own kind, an agent's step) is never searched
    let step = post("w1", "work", json!({ "kind": "turn.step", "turn": "t1", "step": 1, "text": "zucchini plans" }))?;
    anyhow::ensure!(step.status == 200, "posting a step: {step}");
    let search = |q: &str| shell(api, session, "GET", &format!("/api/search?q={}", query(q)), None, &[]);

    // a message's words find it, newest first, with where it is and a snippet
    let found = s.eventually(SEARCH_WAIT, || search("tomatoes").is_ok_and(|r| !hits_in(&r, &chat).is_empty()));
    let r = search("tomatoes")?;
    let hit = hits_in(&r, &chat).first().cloned().unwrap_or_default();
    s.ok(
        "search finds a message by its words: its chat, channel, record and a snippet",
        found && r.status == 200 && hit["channel"] == "chat" && hit["seq"] == seq && hit["at"].is_i64() && hit["snippet"].as_str().is_some_and(|t| t.contains("tomatoes")),
        &r,
    );
    let r = search("TOMAT tues")?;
    s.ok("every word, ignoring case, each a prefix", hits_in(&r, &chat).len() == 1, &r);
    let basil = post("m2", "chat", json!({ "text": "Basil wants water too" }))?;
    anyhow::ensure!(basil.status == 200, "posting again: {basil}");
    let both = s.eventually(SEARCH_WAIT, || search("water").is_ok_and(|r| hits_in(&r, &chat).len() == 2));
    let r = search("water")?;
    let snippets: Vec<Value> = hits_in(&r, &chat).iter().map(|h| h["snippet"].clone()).collect();
    s.ok("two messages are two hits, the newest first, each with its own snippet", both && snippets == [json!("Basil wants water too"), json!(said)], &r);
    let r = search("tomatoes zucchini")?;
    s.ok("a word the message lacks finds nothing (an agent's step is not searched)", r.status == 200 && hits_in(&r, &chat).is_empty(), &r);
    let r = search("zucchini")?;
    s.ok("nor is a record that is no message", r.status == 200 && hits_in(&r, &chat).is_empty(), &r);
    let r = search("garden")?;
    let titled = r.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == chat.as_str() && f["kind"] == "chat" && f["title"] == "Garden talk"));
    s.ok("a fragment's title finds it, before any message", titled, &r);

    // FTS5's syntax in a query is words, never operators
    let r = search("tomatoes OR zucchini")?;
    s.ok("OR in a query is a word: no message has it, so nothing matches", r.status == 200 && hits_in(&r, &chat).is_empty(), &r);
    let r = search("\"tomat")?;
    s.ok("an unbalanced quote is text", r.status == 200 && hits_in(&r, &chat).len() == 1, &r);
    for q in ["NOT tomatoes", "text:tomatoes", "NEAR(tomatoes water)", "tomatoes AND", "*", "^water", "(", "{text}: water"] {
        let r = search(q)?;
        s.ok(&format!("{q:?} is a query like any other (200, no FTS5 error)"), r.status == 200 && r.body["messages"].is_array(), &r);
    }
    let r = shell(api, session, "GET", &format!("/api/search?q={}", "x".repeat(fragment_proto::limits::SEARCH_QUERY_MAX_BYTES + 1)), None, &[])?;
    s.ok("a query past its length is refused (400)", r.status == 400, &r);
    let r = shell(api, session, "GET", "/api/search", None, &[])?;
    s.ok("a search names its query (400)", r.status == 400, &r);
    let r = shell(api, session, "GET", "/api/search?q=a&q=b", None, &[])?;
    s.ok("once (400)", r.status == 400, &r);
    let r = api.call(Call { method: "GET", url: format!("{}/api/search?q=tomatoes", api.base), ..Call::default() })?;
    s.ok("and no one unsigned searches (401)", r.status == 401, &r);

    // a member finds what was said before they joined; an outsider never does
    let member = api.person()?;
    let outsider = api.person()?;
    let member_id = api.identity(&member)?;
    let r = shell(api, session, "PUT", &format!("/api/f/{chat}/members/{member_id}"), Some(&json!({ "role": "viewer" })), &[])?;
    anyhow::ensure!(r.status == 200, "adding the member: {r}");
    let theirs = |keys: &fragment_nip98::Keys, q: &str| api.signed(keys, "GET", &format!("/api/search?q={}", query(q)), None);
    let found = s.eventually(SEARCH_WAIT, || theirs(&member, "tomatoes").is_ok_and(|r| !hits_in(&r, &chat).is_empty()));
    s.ok("a new member's search finds what the chat said before they joined (signed, with their key)", found, theirs(&member, "tomatoes")?);
    let own = s.named(api, &outsider, "plot")?;
    let r = api.create_with(&outsider, json!({ "name": own, "template": "chat" }))?;
    anyhow::ensure!(r.status == 200, "the outsider's chat: {r}");
    let mine = api.signed(&outsider, "POST", &format!("/api/f/{own}/channels/chat"), Some(&json!({ "id": "o1", "body": { "text": "my tomatoes are fine" } })));
    let theirs_found = s.eventually(SEARCH_WAIT, || theirs(&outsider, "tomatoes").is_ok_and(|r| !hits_in(&r, &own).is_empty()));
    let r = theirs(&outsider, "tomatoes")?;
    s.ok(
        "someone not in the chat finds their own message, never the chat's",
        mine.is_ok_and(|m| m.status == 200) && theirs_found && hits_in(&r, &chat).is_empty(),
        &r,
    );
    let r = search("tomatoes")?;
    s.ok("nor does the chat's person find the outsider's", hits_in(&r, &own).is_empty() && hits_in(&r, &chat).len() == 1, &r);

    // archiving: the person's own view, and only of what they hold a role on
    let archived = |r: &Reply, name: &str| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == name).map(|f| f["archived"] == true));
    let list = |keys: Option<&fragment_nip98::Keys>| match keys {
        Some(k) => api.signed(k, "GET", "/api/fragments", None),
        None => shell(api, session, "GET", "/api/fragments", None, &[]),
    };
    let archive = |name: &str, body: Value| shell(api, session, "PUT", &format!("/api/fragments/{name}/archived"), Some(&body), &[]);
    let r = archive(&chat, json!({ "archived": true }))?;
    s.ok("the person archives a chat of theirs", r.status == 200 && r.body == json!({ "name": chat, "archived": true }), &r);
    let again = archive(&chat, json!({ "archived": true }))?;
    s.ok("archiving it again is the same", again.status == 200 && again.body == r.body, &again);
    let mine = list(None)?;
    s.ok("their list says it is archived", archived(&mine, &chat) == Some(true), &mine);
    let member_list = list(Some(&member))?;
    s.ok("a member's list does not", archived(&member_list, &chat) == Some(false), &member_list);
    let r = search("tomatoes")?;
    let r2 = search("garden")?;
    let titled = r2.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == chat.as_str() && f["archived"] == true));
    s.ok("search still finds it, and its messages, saying it is archived", hits_in(&r, &chat).len() == 1 && titled, &r2);
    let r = api.signed(&member, "PUT", &format!("/api/fragments/{chat}/archived"), Some(&json!({ "archived": true })))?;
    let unarchived = archive(&chat, json!({ "archived": false }))?;
    let (mine, member_list) = (list(None)?, list(Some(&member))?);
    s.ok(
        "the member archives it for themselves: the person unarchives it, and each list keeps its own",
        r.status == 200 && unarchived.status == 200 && archived(&mine, &chat) == Some(false) && archived(&member_list, &chat) == Some(true),
        json!({ "mine": archived(&mine, &chat), "member's": archived(&member_list, &chat) }),
    );
    let r = archive("garden-talk", json!({ "archived": true }))?;
    let undone = archive("garden-talk", json!({ "archived": false }))?;
    s.ok("a bare label names the person's own", r.status == 200 && r.body["name"] == chat.as_str() && undone.status == 200, &r);
    let r = archive(&own, json!({ "archived": true }))?;
    s.ok("a fragment they are not in is none of theirs to archive (404)", r.status == 404, &r);
    let r = archive(&format!("nothing-here.{username}"), json!({ "archived": true }))?;
    s.ok("nor one that does not exist (404)", r.status == 404, &r);
    let r = archive("Not A Name", json!({ "archived": true }))?;
    s.ok("a name that is none is refused (400)", r.status == 400, &r);
    let r = archive(&chat, json!({}))?;
    let r2 = archive(&chat, json!({ "archived": "yes" }))?;
    s.ok("a body without `archived`, or not a boolean, is refused (400)", r.status == 400 && r2.status == 400, format!("{r} {r2}"));

    // removed, a member's results go, and so does their archiving
    let r = shell(api, session, "DELETE", &format!("/api/f/{chat}/members/{member_id}"), None, &[])?;
    anyhow::ensure!(r.status == 200, "removing the member: {r}");
    let gone = s.eventually(SEARCH_WAIT, || theirs(&member, "tomatoes").is_ok_and(|r| r.status == 200 && hits_in(&r, &chat).is_empty()));
    let r = theirs(&member, "garden")?;
    let titled = r.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == chat.as_str()));
    s.ok("a removed member's search finds nothing of the chat, by its words or its title", gone && !titled, &r);
    let r = api.signed(&member, "PUT", &format!("/api/fragments/{chat}/archived"), Some(&json!({ "archived": false })))?;
    s.ok("nor may they archive it now (404)", r.status == 404, &r);
    let r = shell(api, session, "PUT", &format!("/api/f/{chat}/members/{member_id}"), Some(&json!({ "role": "viewer" })), &[])?;
    anyhow::ensure!(r.status == 200, "adding the member again: {r}");
    let back = s.eventually(SEARCH_WAIT, || theirs(&member, "tomatoes").is_ok_and(|r| hits_in(&r, &chat).len() == 1));
    let member_list = list(Some(&member))?;
    s.ok("back in, they find it again, once, and it is not archived for them", back && archived(&member_list, &chat) == Some(false), &member_list);

    // a deleted fragment's messages go from its people's search
    let r = api.signed(&outsider, "DELETE", &format!("/api/f/{own}"), None)?;
    let gone = s.eventually(SEARCH_WAIT, || theirs(&outsider, "tomatoes").is_ok_and(|r| r.status == 200 && hits_in(&r, &own).is_empty()));
    s.ok("a deleted fragment's messages leave its people's search", r.status == 200 && gone, theirs(&outsider, "tomatoes")?);
    Ok(())
}

/// A fragment's page, as the shell's person sees it once signed in there.
fn shell_site(_s: &Suite, api: &Api, session: &str, name: &str, path: &str) -> Result<Reply> {
    let token = super::signin::site_cookie(api, session, name)?;
    api.call(Call { method: "GET", url: api.site_url(name, path), cookie: Some(format!("fragment_site={token}")), ..Call::default() })
}

/// Sets a field of the page's and fires its input, as typing would.
fn fill(selector: &str, value: &str) -> String {
    format!(
        "(() => {{ const e = document.querySelector({selector:?}); if (!e) return false; e.value = {value:?}; e.dispatchEvent(new Event('input', {{ bubbles: true }})); return true; }})()"
    )
}

/// The shell in a browser (phase 5's exit, at desktop and phone sizes):
/// first run (a username, the first agent), the agent's chat framed and
/// signed in on its own origin with the agent's answer in it, a second
/// agent, an app's window, settings (at `/settings`, which the address
/// keeps), and the phone's layout. The agents run on the stub image
/// (Docker), as the computers section's do.
pub fn shell_ui(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("shell-ui") {
        return Ok(());
    }
    let Some(mut b) = s.browser()? else {
        s.ok("Chrome is installed for the shell's lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let shots = s.dir("shell-ui");
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    // signed out, settings is the shell asking them to sign in, and back to settings
    let page = b.open(&format!("{}/settings", api.base))?;
    let asked = b.until(&page, "document.querySelector('#first-run-card a.primary')?.getAttribute('href') === '/auth/login?return=%2Fsettings'", wait);
    s.ok("signed out, /settings is the shell asking them to sign in, and to come back to settings", asked, b.eval(&page, "document.body.innerText.slice(0, 200)")?);
    b.close(page)?;

    let email = format!("shell-ui-{}@e2e.test", crate::api::now_s());
    let session = api.sign_in(&email)?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = b.open(&format!("{}/", api.base))?;
    b.viewport(&page, 1280, 800, false)?;

    // first run: a username, then the default agent, made while the shell
    // waits with no question asked (Paul, 2026-10-03)
    let asked = b.until(&page, "document.querySelector('#first-run-card input[name=username]')", wait);
    s.ok("signed in with no username, the shell asks for one", asked, "");
    let username = format!("ui{}", &crate::api::now_s().to_string()[4..]);
    b.eval(&page, &fill("#first-run-card input[name=username]", &username))?;
    b.eval(&page, "document.querySelector('#first-run-card form').requestSubmit()")?;
    let creating = b.until(&page, "document.querySelector('#first-run-card .creating-steps')", wait);
    s.ok("then the shell makes their default agent, asking nothing, and says so", creating, b.eval(&page, "document.getElementById('first-run-card').innerText.slice(0, 200)")?);
    let _ = b.screenshot(&page, &shots.join("creating.png"));
    let opened = b.until(&page, "!document.getElementById('layout').hidden && document.querySelectorAll('#chats .agent-row').length === 1 && document.querySelector('#frames iframe')", agent_wait);
    let row = b.eval(&page, "document.querySelector('#chats .agent-row .label')?.textContent")?;
    if !opened {
        let _ = b.screenshot(&page, &shots.join("creating-failed.png"));
    }
    let said = b.eval(&page, "({ row: document.querySelector('#chats .agent-row .label')?.textContent ?? null, card: document.getElementById('first-run-card').innerText.slice(0, 300), apps: [...document.querySelectorAll('#apps .row')].map((r) => r.textContent) })")?;
    s.ok("the agent is made, named, and its chat opens once it is ready", opened && row.as_str().is_some_and(|t| !t.is_empty()), &said);
    // the chat's name as the sidebar holds it: a title like "Starfire 40K"
    // is labelled starfire-40k, so it is read, never guessed
    let key = b.eval(&page, "document.querySelector('#chats .agent-row')?.dataset.key ?? ''")?;
    let chat = key.as_str().and_then(|k| k.strip_prefix("chat:")).unwrap_or("").to_string();
    let first_label = chat.split('.').next().unwrap_or("").trim_end_matches("-chat").to_string();
    let host = fragment_proto::flat_name(&chat).unwrap_or_default();
    // ready is ready: its computer awake, and the agent following its chat
    let computers = shell(api, &session, "GET", "/api/computers", None, &[])?;
    let subs = shell(api, &session, "GET", &format!("/api/f/{chat}/subscriptions"), None, &[])?;
    let awake = computers.body["computers"][0]["phase"] == "awake";
    let follows = subs.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat"));
    s.ok("as it opens, its computer is awake and the agent follows the chat", awake && follows, json!({ "computer": computers.body["computers"][0]["phase"], "subscriptions": subs.body["subscriptions"] }));
    let signed = b.until(&page, "[...document.querySelectorAll('#frames iframe')].some(f => !f.dataset.blocked)", wait);
    // the person's first message, answered by an agent already up
    let t0 = std::time::Instant::now();
    let mut typed = false;
    while t0.elapsed() < wait && !typed {
        let sent = b.eval_in_frame(&page, &host, "(() => { const t = document.getElementById('text'); if (!t || t.disabled) return false; t.value = 'hello there, please help me water the garden'; t.dispatchEvent(new Event('input', { bubbles: true })); document.getElementById('say').requestSubmit(); return true; })()");
        typed = sent.ok() == Some(Value::Bool(true));
        if !typed {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    let t1 = std::time::Instant::now();
    let mut answered = false;
    while typed && t1.elapsed() < wait && !answered {
        answered = b.eval_in_frame(&page, &host, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.contains("water the garden") && t.contains("echo:"))).unwrap_or(false);
        if !answered {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    s.ok(
        "its chat is framed, signed in on the chat's own origin, and the agent answers the first message at once",
        signed && typed && answered,
        json!({ "host": host, "typed": typed, "answeredInMs": t1.elapsed().as_millis() as u64 }),
    );
    let sidebar = b.until(&page, "document.getElementById('layout').classList.contains('left-open') && document.getElementById('sidebar').getBoundingClientRect().width > 0", wait);
    s.ok("at a desktop's width the sidebar shows beside the chat", sidebar, "");
    let _ = b.screenshot(&page, &shots.join("desktop-chat.png"));

    // a second agent, from the sidebar
    b.click(&page, "#new-agent")?;
    b.eval(&page, &fill("#new-agent-job", "keep my reading list"))?;
    b.eval(&page, &fill("#new-agent-name", "Reader"))?;
    b.eval(&page, "document.getElementById('new-agent-form').requestSubmit()")?;
    // its row may show before the dialog closes (the list is read again as it is made)
    let two = b.until(&page, "document.querySelectorAll('#chats .agent-row').length === 2 && !document.getElementById('new-agent-dialog').open", agent_wait);
    s.ok("a second agent, named, gets a chat of its own in the sidebar", two, "");

    // both agents in one chat, search, and archiving, as the person uses them
    let first_title = row.as_str().unwrap_or("").to_string();
    groups_ui(s, api, &mut b, &page, &Person { session: &session, username: &username, first: &first_label, first_title: &first_title }, &shots)?;

    // an app's window
    b.click(&page, "#add-app")?;
    let catalog = b.until(&page, "document.querySelector('.catalog form')", wait);
    s.ok("Add an app opens the catalog in the viewer", catalog, "");
    b.eval(&page, "document.querySelector('.catalog form')?.requestSubmit()")?;
    let window = b.until(&page, "document.querySelectorAll('#apps .row[data-key]').length === 1 && document.querySelector('.viewer iframe')", wait);
    s.ok("an app from the catalog opens in a window beside the chat", window, "");
    let _ = b.screenshot(&page, &shots.join("desktop-app.png"));

    // settings, at /settings: what a person needs of their account
    let id = super::signin::who(api, &session)?["id"].as_str().unwrap_or("").to_string();
    b.click(&page, "#settings")?;
    let settings = b.until(&page, "location.pathname === '/settings' && ['Account', 'Credit', 'Computer', 'Connections', 'The command line', 'Wallpaper'].every(h => document.getElementById('settings-page').innerText.toUpperCase().includes(h.toUpperCase()))", wait);
    let text = b.eval(&page, "document.getElementById('settings-page').innerText")?;
    let text = text.as_str().unwrap_or("");
    let mut has = |q: &str| b.eval(&page, &format!("!!document.querySelector({q:?})")).ok() == Some(serde_json::json!(true));
    s.ok(
        "settings, at /settings: the account (who they are, their identity, another sign-in, signing out), credit, computer, connections, and the CLI",
        settings
            && text.contains(&format!("@{username}"))
            && text.contains(&email)
            && text.contains(&id)
            && has("#settings-page a[href='/auth/link?return=%2Fsettings']")
            && has("#settings-page form[method=post][action='/auth/logout'] button")
            && text.contains("releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz")
            && text.contains("fragment login")
            && text.contains("fragment skill > ~/.claude/skills/fragment/SKILL.md"),
        &text[..text.len().min(600)],
    );
    let credit = "document.querySelector(\"#settings-page a[href='https://www.pexels.com/@teobadini/'][target=_blank][rel=noopener]\")";
    let credited = b.eval(&page, &format!("{credit}?.textContent === 'Teo Badini' && {credit}.parentElement.textContent === 'Photo by Teo Badini on Pexels'"))?;
    s.ok("and the wallpaper's photographer, credited with a link", credited == true, &credited);
    b.color_scheme(&page, "dark")?;
    let _ = b.screenshot(&page, &shots.join("desktop-settings-dark.png"));
    b.color_scheme(&page, "light")?;
    b.eval(&page, "(document.getElementById('settings-page').scrollTop = 1e6, true)")?;
    let _ = b.screenshot(&page, &shots.join("desktop-settings-end.png"));
    b.reload(&page)?;
    let kept = b.until(&page, "location.pathname === '/settings' && !document.getElementById('settings-page').hidden && document.getElementById('frames').hidden && document.getElementById('settings-page').innerText.includes('Account'.toUpperCase())", wait);
    s.ok("a reload stays on settings", kept, b.eval(&page, "location.pathname")?);
    b.click(&page, "#chats .agent-row")?;
    let home = b.until(&page, "location.pathname === '/' && document.getElementById('settings-page').hidden && !document.getElementById('frames').hidden", wait);
    b.eval(&page, "history.back(), true")?;
    let back = b.until(&page, "location.pathname === '/settings' && !document.getElementById('settings-page').hidden", wait);
    b.eval(&page, "history.forward(), true")?;
    let forward = b.until(&page, "location.pathname === '/' && document.getElementById('settings-page').hidden", wait);
    s.ok("a chat opened from settings is at /, and back and forward walk between the two", home && back && forward, b.eval(&page, "location.pathname")?);

    // the phone: the list, then a chat
    b.color_scheme(&page, "light")?;
    b.viewport(&page, 375, 812, true)?;
    b.reload(&page)?;
    let phone = b.until(&page, "!document.getElementById('layout').hidden && document.querySelector('#frames iframe')", wait);
    let fits = b.eval(&page, "document.documentElement.scrollWidth <= innerWidth + 1")?;
    s.ok("on a phone it fits its width, a chat open", phone && fits == true, &fits);
    // the chat names its agent once `__people` answers: the picture waits for it
    let reader = fragment_proto::flat_name(&format!("reader-chat.{username}")).unwrap_or_default();
    let t0 = std::time::Instant::now();
    while t0.elapsed() < wait {
        let named = b.eval_in_frame(&page, &reader, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.contains("Message Reader")));
        if named == Some(true) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let _ = b.screenshot(&page, &shots.join("phone-chat.png"));
    println!("      (screenshots: {})", shots.display());
    Ok(())
}

/// The shell's person in its browser lane: their platform session, their
/// username, and their first agent's label (its fragment's, as the sidebar holds it).
struct Person<'a> {
    session: &'a str,
    username: &'a str,
    first: &'a str,
    /// Its title, as the shell shows it ("Novatron DX").
    first_title: &'a str,
}

/// A JS string literal.
fn js(s: &str) -> String {
    serde_json::to_string(s).expect("a string encodes")
}

/// The replies on a chat's `chat` from `agent` whose text holds `said`.
fn replies(api: &Api, session: &str, chat: &str, agent: &str, said: &str) -> usize {
    let r = shell(api, session, "GET", &format!("/api/f/{chat}/channels/chat?after=0&limit=1000"), None, &[]);
    r.ok()
        .and_then(|r| r.body["records"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|x| x["principal"] == agent && x["body"]["text"].as_str().is_some_and(|t| t.starts_with("echo:") && t.contains(said)))
        .count()
}

/// Clicks the open menu's item named `text` (a script's click: a menu's
/// button needs no gesture). Answers whether there was one.
fn menu_item(b: &mut Browser, page: &Page, text: &str) -> Result<bool> {
    let clicked = b.eval(page, &format!("(() => {{ const i = [...document.querySelectorAll('#menu button')].find((b) => b.textContent === {}); i?.click(); return !!i; }})()", js(text)))?;
    Ok(clicked == true)
}

/// A group chat of the person's two agents, made from the sidebar (the
/// first picked its lead): the sidebar shows it as a group, its agents'
/// colours stacked; each answers when @mentioned, and the lead when no one
/// is. Then search: a message's words find it, and clicking it opens its
/// chat. Then archiving from the chat's menu: the chat leaves the sidebar,
/// search still finds it, and Unarchive brings it back.
fn groups_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    let r = shell(api, me.session, "GET", "/api/computers", None, &[])?;
    let agents = r.body["computers"][0]["agents"].as_array().cloned().unwrap_or_default();
    let id_of = |label: &str| agents.iter().find(|a| a["fragment"] == format!("{label}.{}", me.username).as_str()).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    let first_label = me.first;
    let (lead, other) = (id_of("reader"), id_of(first_label));
    anyhow::ensure!(lead.starts_with("id:") && other.starts_with("id:"), "the two agents' identities: {r}");

    b.click(page, "#new-group")?;
    let offered = b.until(page, "document.getElementById('new-group-dialog').open && document.querySelectorAll('#new-group-agents .pick').length === 2", wait);
    s.ok("New group chat offers the person's agents", offered, "");
    let pick = |id: &str| format!("#new-group-agents .pick[data-identity={}]", js(id));
    b.click(page, &pick(&lead))?;
    let one = b.eval(page, "document.getElementById('new-group-go').disabled")?;
    b.click(page, &pick(&other))?;
    let order = b.eval(
        page,
        &format!(
            "[document.querySelector({})?.querySelector('.meta')?.textContent, document.querySelector({})?.querySelector('.meta')?.textContent, document.getElementById('new-group-go').disabled]",
            js(&pick(&lead)),
            js(&pick(&other))
        ),
    )?;
    s.ok("it takes two agents or more, in the order picked: the first leads", one == true && order == json!(["Lead", "2", false]), &order);
    let _ = b.screenshot(page, &shots.join("desktop-new-group.png"));
    b.eval(page, "document.getElementById('new-group-form').requestSubmit()")?;
    let made = b.until(page, "document.querySelector('#chats .agent-row[data-group=\"2\"]') && !document.getElementById('new-group-dialog').open", wait);
    let row = b.eval(
        page,
        "(() => { const g = document.querySelector('#chats .agent-row[data-group=\"2\"]'); if (!g) return null; const color = (a) => a.style.getPropertyValue('--agent-color'); \
         const direct = Object.fromEntries([...document.querySelectorAll('#chats .agent-row:not([data-group])')].map((r) => [r.querySelector('.label').textContent, color(r.querySelector('.agent-avatar'))])); \
         return { key: g.dataset.key, label: g.querySelector('.label').textContent, stack: [...g.querySelectorAll('.avatar-stack .agent-avatar')].map(color), direct, heading: document.querySelectorAll('#agent-mark .avatar-stack .agent-avatar').length }; })()",
    )?;
    let colors = json!([row["direct"]["Reader"], row["direct"][me.first_title]]);
    s.ok(
        "the sidebar shows it as a group: its agents' names, their colours stacked (the lead's first, each its identity's)",
        made && row["label"] == format!("Reader, {}", me.first_title).as_str() && row["stack"] == colors && colors[0].is_string() && row["heading"] == 2,
        &row,
    );
    let group = row["key"].as_str().unwrap_or("").trim_start_matches("chat:").to_string();
    let r = shell(api, me.session, "GET", &format!("/api/f/{group}/members"), None, &[])?;
    let listed: Vec<&str> = r.body["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").filter_map(|m| m["principal"].as_str()).collect();
    s.ok("its members are the two agents, the lead added first", listed == [lead.as_str(), other.as_str()], &r);

    // each answers its @mention; with none, the lead answers
    let say = |id: &str, text: &str| shell(api, me.session, "POST", &format!("/api/f/{group}/channels/chat"), Some(&json!({ "id": id, "body": { "text": text } })), &[]);
    let to_other = format!("@{first_label} are you there");
    say("g1", &to_other)?;
    let answered = s.eventually(agent_wait, || replies(api, me.session, &group, &other, &to_other) == 1);
    s.ok(&format!("@{first_label} answers its @mention in the group"), answered, "");
    say("g2", "@reader and you")?;
    let answered = s.eventually(agent_wait, || replies(api, me.session, &group, &lead, "@reader and you") == 1);
    s.ok("and @reader its own", answered, "");
    say("g3", "hello both of you")?;
    let answered = s.eventually(agent_wait, || replies(api, me.session, &group, &lead, "hello both of you") == 1);
    s.ok("a message that @mentions no one, the lead answers", answered && replies(api, me.session, &group, &other, "hello both of you") == 0, "");
    let host = fragment_proto::flat_name(&group).unwrap_or_default();
    let shown = s.eventually(wait, || {
        b.eval_in_frame(page, &host, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.matches("echo:").count() >= 3 && t.contains("are you there") && t.contains("and you"))).unwrap_or(false)
    });
    s.ok("the group's chat, framed in the shell, shows both agents' answers", shown, &host);
    let _ = b.screenshot(page, &shots.join("desktop-group.png"));

    // search: a message's words, and clicking one opens its chat
    let first_chat = format!("{first_label}-chat.{}", me.username);
    b.click(page, "#search-agents")?;
    b.eval(page, &fill("#workspace-search", "water garden"))?;
    let hit = format!("#search-results .search-message[data-fragment={}]", js(&first_chat));
    let found = b.until(page, &format!("document.querySelector({})", js(&hit)), wait);
    s.ok("searching a message's words lists it under Messages, in its chat", found, b.eval(page, "document.getElementById('search-results').innerText")?);
    let _ = b.screenshot(page, &shots.join("desktop-search.png"));
    b.click(page, &hit)?;
    let opened = b.until(page, &format!("!document.getElementById('search-dialog').open && document.getElementById('chat-title').textContent === {}", js(me.first_title)), wait);
    s.ok("clicking it opens its chat", opened, b.eval(page, "document.getElementById('chat-title').textContent")?);

    // archiving, from the chat's menu: the person's own view
    let row_of = |name: &str| format!("document.querySelector({})", js(&format!("#chats [data-key={}]", js(&format!("chat:{name}")))));
    b.click(page, "#agent-heading")?;
    let archived = menu_item(b, page, "Archive")?;
    let left = b.until(page, &format!("!{} && {} && document.getElementById('chat-title').textContent !== {}", row_of(&first_chat), row_of(&group), js(me.first_title)), wait);
    s.ok("Archive, from its menu: the chat leaves the sidebar, and another opens", archived && left, b.eval(page, "document.getElementById('chats').innerText")?);
    b.click(page, "#search-agents")?;
    b.eval(page, &fill("#workspace-search", "water garden"))?;
    let rows = js(&format!("#search-results [data-fragment={}]", js(&first_chat)));
    let marked = b.until(page, &format!("[...document.querySelectorAll({rows})].some((r) => r.querySelector('.meta')?.textContent === 'Archived')"), wait);
    s.ok("search still finds it, marked archived", marked, b.eval(page, "document.getElementById('search-results').innerText")?);
    b.click(page, &hit)?;
    b.until(page, &format!("document.getElementById('chat-title').textContent === {}", js(me.first_title)), wait);
    b.click(page, "#agent-heading")?;
    let unarchived = menu_item(b, page, "Unarchive")?;
    let back = b.until(page, &row_of(&first_chat), wait);
    s.ok("Unarchive, from its menu, brings it back", unarchived && back, "");

    // the phone's picture below is of the Reader's chat
    b.click(page, &format!("#chats [data-key={}]", js(&format!("chat:reader-chat.{}", me.username))))?;
    Ok(())
}
