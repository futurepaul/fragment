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

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::api::{Api, Call, Reply, Socket};
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

/// A fragment's label: its name before its random suffix.
fn label_of(name: &str) -> &str {
    fragment_proto::split_fragment_name(name).map_or("", |(label, _)| label)
}

/// The person's own fragment labelled `label` (the shell names what it
/// makes from a label, with a random suffix), from their list.
fn own_named(api: &Api, session: &str, label: &str) -> Result<String> {
    let r = shell(api, session, "GET", "/api/fragments", None, &[])?;
    let list = r.body["fragments"].as_array().cloned().unwrap_or_default();
    let found = list.iter().find_map(|f| f["name"].as_str().filter(|n| f["role"] == "owner" && label_of(n) == label));
    found.map(str::to_string).with_context(|| format!("no fragment of theirs labelled {label}: {r}"))
}

pub fn shell_platform(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("shell", &[crate::Need::Fakes]) {
        return Ok(());
    }
    let email = format!("shell-{}@e2e.test", crate::api::now_s());
    let session = api.sign_in(&email)?;

    // the shell's own requests act as the signed-in person
    let r = shell(api, &session, "GET", "/api/identities/me", None, &[])?;
    let id = r.body["id"].as_str().unwrap_or("").to_string();
    s.ok("the shell's page reads the API as its signed-in person, with no key", r.status == 200 && r.body["kind"] == "person", &r);
    s.ok("someone new is someone at once: their own npub, and their email", fragment_core::npub::is_identity(&id) && r.body["email"] == email.as_str(), &r);
    let r = shell(api, &session, "GET", "/api/identities/me", None, &[("x-fragment-shell", String::new())])?;
    s.ok("a request without the shell's header is no one's (401)", r.status == 401, &r);
    let r = shell(api, &session, "GET", "/api/identities/me", None, &[("sec-fetch-site", "same-site".into())])?;
    s.ok("nor one from another origin of the site, a fragment's page (401)", r.status == 401, &r);
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "nope" })), &[("origin", String::new())])?;
    s.ok("nor a write without the platform's Origin (401)", r.status == 401, &r);
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "nope" })), &[("origin", api.site_origin("x--k3x9"))])?;
    s.ok("nor a write from a fragment's origin (401)", r.status == 401, &r);
    let r = shell(api, "f".repeat(64).as_str(), "GET", "/api/identities/me", None, &[])?;
    s.ok("a session that is not one is refused (401)", r.status == 401, &r);
    let r = shell(api, &session, "POST", &format!("/api/identities/{id}/keys"), Some(&json!({ "proof": "x" })), &[])?;
    s.ok("a key is added only by a key you hold, never the shell's session", r.status == 401, &r);

    // a chat and an agent, on blessed templates
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "juniper", "template": "agent", "title": "Juniper" })), &[])?;
    let agent = r.body["name"].as_str().unwrap_or("").to_string();
    s.ok("the shell makes an agent fragment on the agent template, named from its label", r.status == 200 && label_of(&agent) == "juniper", &r);
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
    release_assets(s, api, &session, &agent)?;
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
        Some(&json!({ "files": [{ "path": "SOUL.md", "text": "Water the tomatoes.\n" }, { "path": "agent.json", "text": "{\"tier\":\"cheap\"}" }] })),
        &[],
    )?;
    let deploy = shell(api, &session, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})), &[])?;
    s.ok("its job and settings are files of its own, deployed", r.status == 200 && deploy.status == 200, format!("{r} {deploy}"));
    let soul = shell_site(s, api, &session, &agent, "__file?path=SOUL.md")?;
    s.ok("which its page reads", soul.status == 200 && soul.text.contains("tomatoes"), soul.status);
    // an agent is no app: its deploys get no preview card (decision 31)
    let events = || shell(api, &session, "GET", &format!("/api/f/{agent}/events?tail=200"), None, &[]).map(|r| r.body).unwrap_or(Value::Null);
    let skipped = s.eventually(super::site::CARD_WAIT, || events()["events"].as_array().is_some_and(|l| l.iter().any(|e| e["kind"] == "card.skipped" && e["data"]["why"] == "not_an_app")));
    let card = shell(api, &session, "GET", &format!("/api/f/{agent}/card"), None, &[])?;
    s.ok("an agent fragment is not shot for a card (it is no app): 404", skipped && card.status == 404, format!("{card} | {}", events()));

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

    // connections (decisions 22 and 37): every provider the deployment
    // offers, its kind and the person's state there
    let status = |r: &Reply| r.body["providers"].as_array().and_then(|l| l.iter().find(|c| c["provider"] == crate::SWAP_CONNECTION)).map(|c| c["state"].clone());
    let r = shell(api, &session, "GET", "/api/connections", None, &[])?;
    let rows = r.body["providers"].as_array().cloned().unwrap_or_default();
    let row = |p: &str| rows.iter().find(|x| x["provider"] == p).cloned().unwrap_or(Value::Null);
    let keys = crate::SWAP_KEYS.iter().all(|(name, _, env)| {
        let k = row(name);
        k["kind"] == "operator" && k["state"] == "offered" && k["env"] == json!([env]) && k["price"]["micros"].as_i64().is_some_and(|m| m > 0)
    });
    s.ok(
        "the shell lists every provider the deployment offers: the connection not connected yet, the operator's keys offered at their prices, own keys not set (one signed in to, with its models)",
        status(&r) == Some(json!("not_connected"))
            && row(crate::SWAP_CONNECTION)["kind"] == "connection"
            && keys
            && row(crate::SWAP_OWN)["state"] == "not_set"
            && row(crate::SWAP_ROUTER)["state"] == "not_set"
            && row(crate::SWAP_ROUTER)["signIn"]["manage"].is_string()
            && row(crate::SWAP_ROUTER)["models"].as_array().is_some_and(|m| !m.is_empty())
            && rows.len() == crate::SWAP_KEYS.len() + 3,
        &r,
    );
    let r = shell(api, &session, "POST", "/api/connections/perplexity/authorize", Some(&json!({})), &[])?;
    s.ok("an operator key is no connection to authorize (400)", r.status == 400, &r);
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

    // a template that is not blessed is copied, and the title its maker
    // gives it is its fragment.json's, over the template's: the face every
    // member's list shows
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "orchard", "template": "todo", "title": "Orchard rota" })), &[])?;
    let todo = r.body["name"].as_str().unwrap_or("").to_string();
    let titled = s.eventually(std::time::Duration::from_secs(30), || {
        shell(api, &session, "GET", "/api/fragments", None, &[]).is_ok_and(|r| r.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == todo.as_str() && f["title"] == "Orchard rota")))
    });
    let manifest = shell(api, &session, "GET", &format!("/api/f/{todo}/manifest"), None, &[])?;
    s.ok(
        "a copied template's fragment takes the title its maker gives it, over the template's, and its list says so",
        r.status == 200 && titled && manifest.body["meta"]["title"] == "Orchard rota" && manifest.body["meta"]["description"].as_str().is_some_and(|d| d.contains("todo")) && manifest.body["operations"]["add"].is_object(),
        &manifest,
    );
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "bare", "title": "Bare" })), &[])?;
    s.ok("a title needs a template: a fragment made bare says its own in fragment.json (400)", r.status == 400, &r);
    let r = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "lab", "template": "nope" })), &[])?;
    s.ok("a template that is none is refused, naming the blessed ones", r.status == 400 && r.text.contains("agent"), &r);
    search_and_archive(s, api, &session)?;
    search_follows_reading(s, api)?;
    unread_and_seen(s, api, &session)?;
    list_watch(s, api, &session, &id)
}

/// A chat's `unread` in a person's list (docs/api.md, Control API): the
/// messages their search holds after the last they marked seen (`PUT
/// /api/fragments/{name}/seen`), each person's own. Valid: a chat counts
/// every message before any mark (the person's own from elsewhere too),
/// a mark clears it and tells their open shell, a new message counts one;
/// another member's count is theirs alone. Replay: a mark again answers
/// the same. Invalid: a body, a name that is none, a bare label, a
/// fragment not theirs, no one. Leaving the chat drops the mark: back in,
/// everything counts again.
fn unread_and_seen(s: &mut Suite, api: &Api, session: &str) -> Result<()> {
    let r = shell(api, session, "POST", "/api/fragments", Some(&json!({ "label": "unread-talk", "template": "chat", "title": "Unread talk" })), &[])?;
    let chat = r.body["name"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200, "making the chat: {r}");
    let post = |id: &str, text: &str| shell(api, session, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": id, "body": { "text": text } })), &[]);
    let took = s.eventually(SEARCH_WAIT, || post("u1", "first of three").is_ok_and(|r| r.status == 200));
    anyhow::ensure!(took && post("u2", "second of three")?.status == 200, "posting to the chat");
    let row_in = |r: &Reply| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == chat.as_str()).cloned()).unwrap_or(Value::Null);
    let mine = || shell(api, session, "GET", "/api/fragments", None, &[]).map(|r| row_in(&r)).unwrap_or(Value::Null);
    let counted = s.eventually(SEARCH_WAIT, || mine()["unread"] == 2);
    s.ok("a chat counts the messages its person's search holds, before any mark: two (their own, from elsewhere, too)", counted, mine());

    // marked seen: nothing unread, and their open shell is told
    let url = format!("{}/api/fragments/watch", api.base);
    let mut tab = watching(Socket::connect(api, &url, None, Some(&format!("fragment_session={session}")), Some(&api.base)).map(|(socket, _)| socket));
    let seen = |extra: Option<&Value>| shell(api, session, "PUT", &format!("/api/fragments/{chat}/seen"), extra, &[]);
    let r = seen(None)?;
    let told_it = told(&mut tab);
    s.ok(
        "PUT …/seen marks it: {name, unread: 0}, its row counts nothing (left out), and their open shell is told",
        r.status == 200 && r.body == json!({ "name": chat, "unread": 0 }) && mine()["unread"].is_null() && told_it == json!({ "type": "changed" }),
        json!([r.body, mine(), told_it]),
    );
    let again = seen(None)?;
    s.ok("marking it again answers the same", again.status == 200 && again.body == r.body, &again);
    if let Ok(w) = tab {
        w.close();
    }
    anyhow::ensure!(post("u3", "third of three")?.status == 200, "posting again");
    let one = s.eventually(SEARCH_WAIT, || mine()["unread"] == 1);
    s.ok("a message after the mark counts one", one, mine());

    // each person's own: a member never marked counts all three; theirs moves alone
    let member = api.person()?;
    let member_id = api.identity(&member)?;
    let r = shell(api, session, "PUT", &format!("/api/f/{chat}/members/{member_id}"), Some(&json!({ "role": "viewer" })), &[])?;
    anyhow::ensure!(r.status == 200, "adding the member: {r}");
    let theirs = || api.signed(&member, "GET", "/api/fragments", None).map(|r| row_in(&r)).unwrap_or(Value::Null);
    let three = s.eventually(SEARCH_WAIT, || theirs()["unread"] == 3);
    let r = api.signed(&member, "PUT", &format!("/api/fragments/{chat}/seen"), None)?;
    s.ok(
        "a member's count is theirs (all three, never marked); their mark (signed, with their key) clears theirs, and the person's stays one",
        three && r.status == 200 && theirs()["unread"].is_null() && mine()["unread"] == 1,
        json!({ "theirs": theirs(), "mine": mine(), "mark": r.body }),
    );
    // leaving drops their mark: back in, all three count again
    let out = shell(api, session, "DELETE", &format!("/api/f/{chat}/members/{member_id}"), None, &[])?;
    let back = shell(api, session, "PUT", &format!("/api/f/{chat}/members/{member_id}"), Some(&json!({ "role": "viewer" })), &[])?;
    let again = s.eventually(SEARCH_WAIT, || theirs()["unread"] == 3);
    s.ok("removed and added again, the member counts all three again (their mark went with them)", out.status == 200 && back.status == 200 && again, theirs());

    // refused: a body, no name, a bare label, not theirs, no one
    let body = seen(Some(&json!({ "n": 1 })))?;
    s.ok("a mark takes no body (400)", body.status == 400, &body);
    let r = shell(api, session, "PUT", "/api/fragments/Not%20A%20Name/seen", None, &[])?;
    s.ok("a name that is none is refused (400)", r.status == 400, &r);
    let r = shell(api, session, "PUT", "/api/fragments/unread-talk/seen", None, &[])?;
    s.ok("a bare label names nothing here (404)", r.status == 404 && r.message().contains("in full"), &r);
    let outsider = api.person()?;
    let r = api.signed(&outsider, "PUT", &format!("/api/fragments/{chat}/seen"), None)?;
    s.ok("a fragment they are not in is none of theirs to mark (404)", r.status == 404, &r);
    let r = api.call(Call { method: "PUT", url: format!("{}/api/fragments/{chat}/seen", api.base), ..Call::default() })?;
    s.ok("and no one unsigned marks anything (401)", r.status == 401, &r);
    Ok(())
}

/// A watch socket of a person's list, past its `hello`; or why not (the
/// refusal's status is in it).
fn watching(socket: Result<Socket>) -> Result<Socket, String> {
    socket.and_then(|mut w| w.expect("hello").map(|_| w)).map_err(|e| format!("{e:#}"))
}

/// The next frame a watch socket was sent, or why none came (its read
/// waits 5 s: a change reaches it in the request that made it).
fn told(w: &mut Result<Socket, String>) -> Value {
    match w {
        Ok(w) => w.next().unwrap_or_else(|e| json!(format!("{e:#}"))),
        Err(why) => json!(why.clone()),
    }
}

/// The person's list, watched (`GET /api/fragments/watch`; Paul on p5,
/// 2026-10-05: an app his agent made did not show in his sidebar until he
/// reloaded). Their Principal tells each socket only that the list
/// changed, and the page reads it again. Valid: the shell's socket (its
/// session, from the platform's own page) is told of a fragment made, one
/// shared with them by someone else, an archive, a removal and a delete;
/// a second tab's is told too, and a key's (the CLI's) of its own list.
/// Invalid: the session from any other page (no Origin, a fragment's), no
/// one, or no upgrade. Its frame names nothing. Past the most sockets a
/// list holds, one more is refused (429).
fn list_watch(s: &mut Suite, api: &Api, session: &str, id: &str) -> Result<()> {
    let url = format!("{}/api/fragments/watch", api.base);
    let cookie = format!("fragment_session={session}");
    let page = |origin: Option<&str>| Socket::connect(api, &url, None, Some(&cookie), origin).map(|(socket, _)| socket);
    let changed = json!({ "type": "changed" });
    let mut tab = watching(page(Some(&api.base)));
    let mut second = watching(page(Some(&api.base)));
    s.ok("the shell's page watches the person's list: a socket with its session, from the platform's own page", tab.is_ok() && second.is_ok(), json!([tab.as_ref().err(), second.as_ref().err()]));
    let r = shell(api, session, "POST", "/api/fragments", Some(&json!({ "label": "watched" })), &[])?;
    let made = r.body["name"].as_str().unwrap_or("").to_string();
    let (one, two) = (told(&mut tab), told(&mut second));
    s.ok("a fragment made is told to it at once, and to a second tab's, naming nothing", r.status == 200 && one == changed && two == changed, json!([r.status, one, two]));

    // someone else's fragment, shared with them; the CLI's socket is a key's
    let other = api.person()?;
    let mut cli = watching(Socket::connect(api, &url, Some(&other), None, None).map(|(socket, _)| socket));
    let theirs = s.named(api, &other, "lent")?;
    let r = api.create(&other, &theirs)?;
    let own = told(&mut cli);
    s.ok("a key's socket (the CLI's, naming no page) watches its own list: told of a fragment it made", r.status == 200 && own == changed, json!([r.status, own]));
    let r = api.signed(&other, "PUT", &format!("/api/f/{theirs}/members/{id}"), Some(&json!({ "role": "viewer" })))?;
    let shared = told(&mut tab);
    let list = shell(api, session, "GET", "/api/fragments", None, &[])?;
    let listed = list.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == theirs.as_str() && f["role"] == "viewer"));
    s.ok("one someone else shares with them is told, and their list then has it", r.status == 200 && shared == changed && listed, json!([r.status, shared, list.body]));
    let r = shell(api, session, "PUT", &format!("/api/fragments/{made}/archived"), Some(&json!({ "archived": true })), &[])?;
    let (one, two) = (told(&mut tab), told(&mut second));
    s.ok("an archive in one tab is told to every tab", r.status == 200 && one == changed && two == changed, json!([r.status, one, two]));
    let r = api.signed(&other, "DELETE", &format!("/api/f/{theirs}/members/{id}"), None)?;
    let removed = told(&mut tab);
    s.ok("their removal from someone else's is told", r.status == 200 && removed == changed, json!([r.status, removed]));
    let r = shell(api, session, "DELETE", &format!("/api/f/{made}"), None, &[])?;
    let deleted = told(&mut tab);
    s.ok("and a delete of their own", r.status == 200 && deleted == changed, json!([r.status, deleted]));

    // a page that is not the platform's own, or no one, watches nothing
    let refused = |socket: Result<Socket>| watching(socket).err().unwrap_or_else(|| "opened".into());
    let unnamed = refused(page(None));
    s.ok("the session from an upgrade that names no page is no one's (401)", unnamed.contains("401"), &unnamed);
    let fragment = refused(page(Some(&api.site_origin("x--k3x9"))));
    s.ok("nor from a fragment's page, one site with the platform (401)", fragment.contains("401"), &fragment);
    let nobody = refused(Socket::connect(api, &url, None, None, Some(&api.base)).map(|(socket, _)| socket));
    s.ok("and no one watches no list (401)", nobody.contains("401"), &nobody);
    let r = shell(api, session, "GET", "/api/fragments/watch", None, &[])?;
    s.ok("it is a socket: a request without the upgrade is refused (400)", r.status == 400, &r);

    // a list holds at most LIST_WATCHERS_MAX sockets (two are open)
    let mut more = vec![];
    for _ in 2..fragment_proto::limits::LIST_WATCHERS_MAX {
        more.push(watching(page(Some(&api.base))));
    }
    let past = refused(page(Some(&api.base)));
    s.ok(
        &format!("a list holds {} sockets at most: one more is refused (429)", fragment_proto::limits::LIST_WATCHERS_MAX),
        more.iter().all(Result::is_ok) && past.contains("429"),
        json!({ "opened": more.iter().filter(|w| w.is_ok()).count(), "past": past }),
    );
    for w in more.into_iter().chain([tab, second, cli]).flatten() {
        w.close();
    }
    Ok(())
}

/// How long a message takes to reach a person's search: its fragment's
/// alarm sends it, and a backoff waits on a list that holds no role yet.
const SEARCH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// The first chat's title (cell/shell/shell.js, `FIRST_CHAT`): its own,
/// never its agent's name.
const FIRST_CHAT: &str = "Your first chat";

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
/// once, and only one of theirs; a chat's row names its agents and shows
/// its newest message from each person's own search, so the shell's
/// sidebar is one read, told when a new message comes. Goal: the person's list is a fenced
/// projection (lesson 12). Method: three people (the shell's person, a
/// member who is removed and comes back, and an outsider with a chat of
/// their own), each asking their own list through the API.
fn search_and_archive(s: &mut Suite, api: &Api, session: &str) -> Result<()> {
    let r = shell(api, session, "POST", "/api/fragments", Some(&json!({ "label": "garden-talk", "template": "chat", "title": "Garden talk" })), &[])?;
    let chat = r.body["name"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && label_of(&chat) == "garden-talk", "making the chat: {r}");
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

    // the chat's row in the person's list shows its newest message, from
    // their search, and their open shell is told when a new one arrives
    let row = |r: &Reply| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == chat.as_str()).cloned()).unwrap_or(Value::Null);
    let mine = || shell(api, session, "GET", "/api/fragments", None, &[]);
    let r = mine()?;
    s.ok(
        "the chat's row shows its newest message (the agent's step after it is none), and when it was said (previewAt: its record's time)",
        row(&r)["preview"] == said && row(&r)["previewAt"] == posted.body["record"]["at"] && posted.body["record"]["at"].is_i64(),
        &r,
    );
    let url = format!("{}/api/fragments/watch", api.base);
    let mut tab = watching(Socket::connect(api, &url, None, Some(&format!("fragment_session={session}")), Some(&api.base)).map(|(socket, _)| socket));
    if let Ok(w) = tab.as_mut() {
        w.patience(SEARCH_WAIT)?;
    }
    let basil = post("m2", "chat", json!({ "text": "Basil wants water too" }))?;
    anyhow::ensure!(basil.status == 200, "posting again: {basil}");
    let told_new = told(&mut tab);
    let r = mine()?;
    s.ok(
        "a new message is told to the person's open shell, and their list's row then shows it",
        told_new == json!({ "type": "changed" }) && row(&r)["preview"] == "Basil wants water too" && row(&r)["previewAt"] == basil.body["record"]["at"],
        json!([told_new, row(&r)]),
    );
    if let Ok(w) = tab {
        w.close();
    }
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

    // the chat's agents, the first added first, are named in every member's row
    let rows = || -> Result<(Value, Value)> { Ok((row(&mine()?), row(&api.signed(&member, "GET", "/api/fragments", None)?))) };
    let (_, lead) = super::delegation::agent_of(api, &member)?;
    let (_, second) = super::delegation::agent_of(api, &member)?;
    let added = [&lead, &second].map(|a| shell(api, session, "PUT", &format!("/api/f/{chat}/members/{a}"), Some(&json!({ "role": "viewer" })), &[]));
    let (my_row, their_row) = rows()?;
    s.ok(
        "agents added are named in every member's row, the first added first; the member's row shows the newest message from their own search",
        added.iter().all(|r| r.as_ref().is_ok_and(|r| r.status == 200))
            && my_row["agents"] == json!([lead, second])
            && their_row["agents"] == json!([lead, second])
            && their_row["preview"] == "Basil wants water too",
        json!([my_row, their_row]),
    );
    let removed = shell(api, session, "DELETE", &format!("/api/f/{chat}/members/{second}"), None, &[])?;
    let (my_row, their_row) = rows()?;
    s.ok("the second removed, every row names the lead left", removed.status == 200 && my_row["agents"] == json!([lead]) && their_row["agents"] == json!([lead]), json!([my_row, their_row]));
    // the lead is the chat's own agent: its owner keeps it (members.rs `chat_agent`)
    let refused = shell(api, session, "DELETE", &format!("/api/f/{chat}/members/{lead}"), None, &[])?;
    let (my_row, their_row) = rows()?;
    s.ok(
        "the lead, the chat's own agent, is not removed (400, saying why), and every row still names it",
        refused.status == 400 && refused.message().contains("this chat's own agent") && my_row["agents"] == json!([lead]) && their_row["agents"] == json!([lead]),
        json!([refused.body, my_row, their_row]),
    );
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
    s.ok("a bare label names nothing here (404): a fragment is named in full", r.status == 404 && r.message().contains("in full"), &r);
    let r = archive(&own, json!({ "archived": true }))?;
    s.ok("a fragment they are not in is none of theirs to archive (404)", r.status == 404, &r);
    let r = archive("nothing-here--k3x9", json!({ "archived": true }))?;
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

/// Search follows who may read (#156, problem 5): a deploy that makes a
/// channel the editors' takes its messages from every list (only channels
/// every member reads are searched), and a member who joins after never
/// gets them. Method: an app with two postable channels, `talk` tightened
/// by a second deploy and `notes` left as it was; each wait ends on what
/// does arrive, never on an absence.
fn search_follows_reading(s: &mut Suite, api: &Api) -> Result<()> {
    const OPEN: &[u8] = br#"{ "channels": { "talk": { "read": "viewer", "post": "viewer" }, "notes": { "read": "viewer", "post": "viewer" } } }"#;
    const CLOSED: &[u8] = br#"{ "channels": { "talk": { "read": "editor", "post": "editor" }, "notes": { "read": "viewer", "post": "viewer" } } }"#;
    let (owner, viewer, late) = (api.person()?, api.person()?, api.person()?);
    let name = s.named(api, &owner, "readers")?;
    let c = s.create(api, &owner, &name)?;
    s.commit(&c, &[("fragment.json", Some(OPEN))]);
    s.deploy(&c);
    let add = |keys: &fragment_nip98::Keys| api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", keys.pubkey_hex()), Some(&json!({ "role": "viewer" })));
    anyhow::ensure!(add(&viewer)?.status == 200, "adding the viewer");
    let post = |channel: &str, id: &str, text: &str| api.signed(&owner, "POST", &format!("/api/f/{name}/channels/{channel}"), Some(&json!({ "id": id, "body": { "text": text } })));
    let search = |keys: &fragment_nip98::Keys| api.signed(keys, "GET", "/api/search?q=rhubarb", None);
    let took = s.eventually(SEARCH_WAIT, || post("talk", "t1", "the rhubarb is ready").is_ok_and(|r| r.status == 200));
    let found = s.eventually(SEARCH_WAIT, || search(&viewer).is_ok_and(|r| hits_in(&r, &name).len() == 1));
    s.ok("a viewer finds a message on a channel every member reads", took && found, search(&viewer)?);

    s.commit(&c, &[("fragment.json", Some(CLOSED))]);
    s.deploy(&c);
    let gone = s.eventually(SEARCH_WAIT, || search(&viewer).is_ok_and(|r| r.status == 200 && hits_in(&r, &name).is_empty()));
    let read = api.signed(&viewer, "GET", &format!("/api/f/{name}/channels/talk"), None)?;
    s.ok("a deploy that makes the channel the editors' takes its message from the viewer's search, as from their reading (403)", gone && read.status == 403, &read);
    let gone = s.eventually(SEARCH_WAIT, || search(&owner).is_ok_and(|r| r.status == 200 && hits_in(&r, &name).is_empty()));
    s.ok("and from the owner's: a channel only some members read is never searched", gone, search(&owner)?);

    anyhow::ensure!(add(&late)?.status == 200, "adding the late viewer");
    let posted = post("notes", "n1", "the rhubarb went to market")?;
    let caught_up = s.eventually(SEARCH_WAIT, || search(&late).is_ok_and(|r| !hits_in(&r, &name).is_empty()));
    let r = search(&late)?;
    s.ok(
        "a member who joins after finds the channels searched now, never the tightened one's old message",
        posted.status == 200 && caught_up && hits_in(&r, &name).iter().all(|h| h["channel"] == "notes"),
        &r,
    );
    Ok(())
}

/// A fragment's page, as the shell's person sees it once signed in there.
/// The release's files (cell/src/assets.rs), read from the cell's Static
/// Assets and served by the router alone, each as its route answered it
/// when the cell held its bytes: the shell's with their type, validator and
/// policy, a blessed template's site file as the release's bytes, and none
/// of the assets at a path of its own (`run_worker_first`).
fn release_assets(s: &mut Suite, api: &Api, session: &str, agent: &str) -> Result<()> {
    let js = fragment_templates::shell("shell.js").context("the release's shell.js")?;
    let r = api.unsigned("GET", "/__shell/shell.js", None)?;
    let tag = r.header("etag");
    s.ok(
        "the shell's script is the release's bytes, typed, revalidated by their hash, never sniffed",
        r.status == 200
            && r.bytes == js.bytes
            && r.header("content-type") == "text/javascript; charset=utf-8"
            && tag == format!("\"s-{}\"", &js.sha256[..20])
            && r.header("cache-control") == "no-cache"
            && r.header("x-content-type-options") == "nosniff",
        format!("{} {} bytes, {} {tag}", r.status, r.bytes.len(), r.header("content-type")),
    );
    let again = api.call(Call { method: "GET", url: format!("{}/__shell/shell.js", api.base), extra: vec![("if-none-match", tag)], ..Call::default() })?;
    let wallpaper = fragment_templates::shell("wallpaper.jpg").context("the release's wallpaper")?;
    let head = api.call(Call { method: "HEAD", url: format!("{}/__shell/wallpaper.jpg", api.base), ..Call::default() })?;
    s.ok(
        "asked again it is 304, and a HEAD names a file's size and type alone",
        again.status == 304 && again.bytes.is_empty() && head.status == 200 && head.header("content-length") == wallpaper.size.to_string() && head.header("content-type") == "image/jpeg" && head.bytes.is_empty(),
        format!("{} / HEAD {} {} {}", again.status, head.status, head.header("content-length"), head.header("content-type")),
    );
    let index = fragment_templates::shell("index.html").context("the release's shell page")?;
    let page = api.unsigned("GET", "/", None)?;
    s.ok(
        "the shell's page is the release's, under the platform's policy",
        page.status == 200 && page.bytes == index.bytes && page.header("content-security-policy").contains("frame-ancestors 'none'") && page.header("x-frame-options") == "DENY" && page.header("cache-control") == "no-store",
        format!("{} {} bytes; {}", page.status, page.bytes.len(), page.header("content-security-policy")),
    );
    let paths = [
        "/__shell/CREDITS.md".to_string(),
        "/__shell/index.html".into(),
        "/__shell/vendor/lucide-LICENSE".into(),
        format!("/{}", js.sha256),
        format!("/__shell/{}", js.sha256),
        format!("/assets/{}", js.sha256),
        format!("/build/assets/{}", js.sha256),
    ];
    let mut served = vec![];
    for path in &paths {
        let r = api.unsigned("GET", path, None)?;
        if r.status != 404 || r.bytes == js.bytes {
            served.push(format!("{path}: {}", r.status));
        }
    }
    s.ok("nothing else of cell/shell/, and no asset by its name, is served (404)", served.is_empty(), served.join(", "));
    let f = fragment_templates::blessed::site_file("agent", "site/agent.js").context("the release's agent.js")?;
    let r = shell_site(s, api, session, agent, "agent.js")?;
    let by_name = shell_site(s, api, session, agent, f.sha256)?;
    s.ok(
        "a blessed template's file is the release's bytes, and its fragment's host serves no asset by its name",
        r.status == 200 && r.bytes == f.bytes && by_name.status == 404 && by_name.bytes != f.bytes,
        format!("{} {} bytes / by its name {}", r.status, r.bytes.len(), by_name.status),
    );
    Ok(())
}

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
/// first run (the first agent), the agent's chat framed and
/// signed in on its own origin with the agent's answer in it, a second
/// agent, an app's window, settings (at `/settings`, which the address
/// keeps), and the phone's layout. The agents run on the stub image
/// (Docker), as the computers section's do.
pub fn shell_ui(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("shell-ui", &[crate::Need::Chrome, crate::Need::LocalDocker]) {
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
    let other = b.eval(&page, "(() => { const f = document.querySelector('#first-run-card form[action]'); return { method: f?.getAttribute('method'), action: f?.getAttribute('action'), says: f?.textContent }; })()")?;
    s.ok(
        "and offers another account: a form posting to /auth/switch, back to settings too",
        other["method"] == "post" && other["action"] == "/auth/switch?return=%2Fsettings" && other["says"] == "Use another account",
        &other,
    );
    b.close(page)?;

    let email = format!("shell-ui-{}@e2e.test", crate::api::now_s());
    let session = api.sign_in(&email)?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    // The first run, held twice in its first page: its computer's making
    // (then failed once), and its agent's joining the chat (never let go:
    // the page is reloaded there). The chat is there to write in from the
    // start; what it shows of the making is what the platform said.
    let page = b.open_with_script(&format!("{}/", api.base), r#"
      if (window === window.top && !sessionStorage.getItem('e2e-reloaded')) {
        const fetch = window.fetch.bind(window);
        let heldComputer = false, heldJoin = false;
        window.fetch = async (url, options) => {
          if (url === '/api/computers' && options?.method === 'POST' && !heldComputer) {
            heldComputer = true;
            await new Promise(resolve => { window.releaseSetup = resolve; });
            return new Response(JSON.stringify({message: 'Setup temporarily unavailable'}), {status: 503});
          }
          if (/^\/api\/f\/[^/]+\/members\/npub1/.test(url) && options?.method === 'PUT' && !heldJoin) {
            heldJoin = true;
            window.joinHeld = true;
            await new Promise(() => {});
          }
          return fetch(url, options);
        };
      }
    "#)?;
    b.viewport(&page, 1280, 800, false)?;

    // first run: the default agent, made with no question asked (Paul,
    // 2026-10-03), its chat first and in the open (Skyler, 2026-10-12),
    // and nothing to choose before it: a seat (the e2e's people are the
    // deployment's plan's) skips the welcome
    let creating = b.until(&page, "!document.getElementById('layout').hidden && document.getElementById('first-run').hidden", wait);
    let first = b.eval(&page, "({ path: location.pathname, seats: document.querySelectorAll('#first-run-card [data-plan]').length, steps: [...document.querySelectorAll('#first-run-card .creating-steps li')].map((li) => li.textContent) })")?;
    s.ok(
        "signed in, a seat's shell makes their default agent at once, asking nothing, in the chat's column: no welcome, no seat to choose or wait for, at home's address",
        creating && first["path"] == "/" && first["seats"] == 0 && first["steps"] == json!([]),
        &first,
    );
    let opened = b.until(&page, "document.querySelectorAll('#chats .chat-row').length === 1 && !!document.querySelector('#frames iframe')", agent_wait);
    if !opened {
        let _ = b.screenshot(&page, &shots.join("creating-failed.png"));
    }
    let said = b.eval(&page, "({ rows: [...document.querySelectorAll('#chats .chat-row')].map((r) => r.textContent), notice: document.getElementById('notice').innerText.slice(0, 200) })")?;
    s.ok("its chat is made first and opens at once, the one row of their chats", opened, &said);
    // the chat's name as the sidebar holds it: a title like "Starfire 40K"
    // is labelled starfire-40k, so it is read, never guessed
    let key = b.eval(&page, "document.querySelector('#chats .chat-row')?.dataset.key ?? ''")?;
    let chat = key.as_str().and_then(|k| k.strip_prefix("chat:")).unwrap_or("").to_string();
    let first_label = label_of(&chat).trim_end_matches("-chat").to_string();
    let host = chat.clone();
    // while its computer is held: a card of what is being made, typing, no Send, no message
    let mut held = Value::Null;
    let drafting = s.eventually(wait, || {
        held = b
            .eval_in_frame(
                &page,
                &host,
                "(() => { const t = document.getElementById('text'); const c = document.querySelector('.setup-card'); if (!t || t.disabled || !c || !c.querySelector('.setup-steps li')) return null; \
                 t.value = 'my first draft'; t.dispatchEvent(new Event('input', { bubbles: true })); \
                 return { send: document.getElementById('send').disabled, steps: [...c.querySelectorAll('.setup-steps li')].map((l) => [l.className, l.textContent]), messages: document.querySelectorAll('#messages .msg').length }; })()",
            )
            .unwrap_or(Value::Null);
        held["steps"].as_array().is_some_and(|l| l.len() == 3)
    });
    let making = held["steps"][0][0] == "doing" && held["steps"][0][1].as_str().is_some_and(|t| t.starts_with("Making ")) && held["steps"][1][0] == "todo" && held["steps"][2][0] == "todo";
    s.ok(
        "while its making waits, the chat shows a card of it (no one's message, nothing in the agent's name), takes typing, and sends nothing",
        drafting && making && held["send"] == true && held["messages"] == 0,
        &held,
    );
    let _ = b.screenshot(&page, &shots.join("creating.png"));
    b.until(&page, "typeof window.releaseSetup === 'function'", wait);
    b.eval(&page, "(window.releaseSetup(), true)")?;
    let failed = s.eventually(wait, || {
        b.eval_in_frame(&page, &host, "document.querySelector('.setup-error')?.textContent.includes('Setup temporarily unavailable') && !!document.querySelector('.setup-steps li.failed') && !!document.querySelector('.setup-error button')").ok() == Some(Value::Bool(true))
    });
    s.ok("a step that fails says so in the chat, the platform's words, with Try again", failed, "");
    b.eval_in_frame(&page, &host, "(document.querySelector('.setup-error button').click(), true)")?;
    // tried again, it is made, and its joining held: still no Send
    let joining = b.until(&page, "window.joinHeld === true", agent_wait);
    let mut steps = Value::Null;
    let made = s.eventually(wait, || {
        steps = b
            .eval_in_frame(&page, &host, "({ steps: [...document.querySelectorAll('.setup-steps li')].map((l) => [l.className, l.textContent]), send: document.getElementById('send').disabled, draft: document.getElementById('text').value, error: !!document.querySelector('.setup-error') })")
            .unwrap_or(Value::Null);
        steps["steps"][0][0] == "done" && steps["steps"][1][0] == "doing"
    });
    let agents_in = |api: &Api| -> Vec<Value> {
        shell(api, &session, "GET", &format!("/api/f/{chat}/members"), None, &[])
            .map(|r| r.body["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").map(|m| m["principal"].clone()).collect())
            .unwrap_or_default()
    };
    s.ok(
        "tried again, it says its agent is made (and is: theirs, on their computer) while it joins the chat, keeps what was typed, and Send stays off: the agent is not in the chat",
        joining && made && steps["send"] == true && steps["draft"] == "my first draft" && steps["error"] == false && agents_in(api).is_empty() && agent_ids(api, &session, &[&first_label]).is_ok(),
        &steps,
    );
    let _ = b.screenshot(&page, &shots.join("creating-joining.png"));
    // a reload there goes on from what was made: nothing is kept in the browser
    b.eval(&page, "(sessionStorage.setItem('e2e-reloaded', '1'), true)")?;
    b.reload(&page)?;
    let resumed = b.until(&page, &format!("document.querySelectorAll('#chats .chat-row').length === 1 && document.querySelector('#chats .chat-row')?.dataset.key === {}", js(&format!("chat:{chat}"))), agent_wait);
    let sendable = s.eventually(agent_wait, || {
        b.eval_in_frame(&page, &host, "(() => { const t = document.getElementById('text'); if (!t) return false; t.value = 'my first draft'; t.dispatchEvent(new Event('input', { bubbles: true })); return !document.getElementById('send').disabled; })()").ok() == Some(Value::Bool(true))
    });
    let joined = agents_in(api);
    s.ok("reloaded mid-run, the shell goes on from what was made (the same chat, no other), and Send turns on once the platform has the agent in it", resumed && sendable && joined.len() == 1, json!({ "agents": joined }));
    let ready = s.eventually(agent_wait, || b.eval_in_frame(&page, &host, "!document.querySelector('.setup-card') && document.getElementById('note').textContent === ''").ok() == Some(Value::Bool(true)));
    s.ok("once the agent follows its chat, the card goes", ready, "");
    let row = b.eval(&page, "document.querySelector('#sidebar-agent .title')?.textContent ?? null")?;
    let list = shell(api, &session, "GET", "/api/fragments", None, &[])?;
    let owned = |kind: &str| list.body["fragments"].as_array().into_iter().flatten().filter(|f| f["kind"] == kind && f["role"] == "owner").count();
    s.ok(
        "the sidebar shows their one agent, named, and the run made one agent and one chat, though it was tried twice and reloaded",
        row.as_str().is_some_and(|t| !t.is_empty()) && owned("agent") == 1 && owned("chat") == 1,
        json!({ "agent": row, "agents": owned("agent"), "chats": owned("chat") }),
    );

    // its chat's title is its own (Paul, 2026-10-16), and its job never
    // names it: its name is its agent fragment's title alone
    let first_name = row.as_str().unwrap_or("").to_string();
    let chat_row = js(&format!("#chats [data-key={}] .label", js(&format!("chat:{chat}"))));
    let shown = b.eval(&page, &format!("({{ row: document.querySelector({chat_row})?.textContent ?? null, header: document.getElementById('chat-title').textContent }})"))?;
    let listed = own_row_of(api, &session, &chat);
    s.ok(
        "their first chat is titled \"Your first chat\", never its agent's name: its row, the shell's header and its own title",
        listed["title"] == FIRST_CHAT && shown["row"] == FIRST_CHAT && shown["header"] == FIRST_CHAT && first_name != FIRST_CHAT,
        json!({ "listed": listed, "shown": shown, "agent": first_name }),
    );
    let soul = shell(api, &session, "GET", &format!("/api/f/{}/file?path=SOUL.md", own_named(api, &session, &first_label)?), None, &[])?;
    s.ok(
        "its first agent's job (its SOUL.md) says what it does and never its name",
        soul.status == 200 && soul.text.contains("default agent") && !first_name.is_empty() && !soul.text.contains(&first_name),
        json!({ "name": first_name, "soul": soul.text }),
    );
    let settings = shell(api, &session, "GET", &format!("/api/f/{}/file?path=agent.json", own_named(api, &session, &first_label)?), None, &[])?;
    s.ok(
        "the shell's first agent names the cheap tier (GLM-5.3 Flash), and no colour: its colour is its identity's",
        settings.status == 200 && settings.body["tier"] == "cheap" && settings.body.get("color").is_none(),
        &settings,
    );
    // ready is ready: its computer awake, and the agent following its chat
    let computers = shell(api, &session, "GET", "/api/computers", None, &[])?;
    let subs = shell(api, &session, "GET", &format!("/api/f/{chat}/subscriptions"), None, &[])?;
    let awake = computers.body["computers"][0]["phase"] == "awake";
    let follows = subs.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat"));
    s.ok("as the card goes, its computer is awake and the agent follows the chat", awake && follows, json!({ "computer": computers.body["computers"][0]["phase"], "subscriptions": subs.body["subscriptions"] }));
    let first_id = agent_ids(api, &session, &[&first_label])?.remove(0);
    let (hello, seen) = said_hello(s, api, &session, &chat, &first_id, agent_wait);
    s.ok("the agent speaks first: its hello once, and no message of the person's before it", hello && seen.as_array().is_some_and(|l| l.len() == 1), &seen);
    // a reload once it is made shows no card and makes nothing
    b.reload(&page)?;
    let again = b.until(&page, "!document.getElementById('layout').hidden && document.querySelectorAll('#chats .chat-row').length === 1 && !!document.querySelector('#frames iframe')", wait);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let quiet = b.eval_in_frame(&page, &host, "!document.querySelector('.setup-card')").ok() == Some(Value::Bool(true));
    let list = shell(api, &session, "GET", "/api/fragments", None, &[])?;
    let chats_now = list.body["fragments"].as_array().into_iter().flatten().filter(|f| f["kind"] == "chat").count();
    s.ok("reloaded once it is made, the chat shows no card, and nothing is made again", again && quiet && chats_now == 1, chats_now);
    let signed = b.until(&page, "[...document.querySelectorAll('#frames iframe')].some(f => !f.dataset.blocked)", wait);
    let _ = s.eventually(wait, || b.eval_in_frame(&page, &host, "!!document.querySelector('.msg.agent:not(.streaming)')").ok() == Some(Value::Bool(true)));
    let _ = b.screenshot(&page, &shots.join("first-hello.png"));
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
    s.ok("the chat frame's accessible name cannot show a tooltip over its contents; the chat's title carries it", b.eval(&page, "(() => { const f = [...document.querySelectorAll('#frames iframe')].find(f => !f.hidden); const n = document.getElementById('chat-title'); return !!f && !f.hasAttribute('title') && f.getAttribute('aria-label') === n.textContent && n.title === n.textContent; })()")? == true, "");
    let _ = b.screenshot(&page, &shots.join("desktop-chat.png"));

    // another agent of theirs, made elsewhere (the CLI, or an agent): the
    // sidebar keeps one agent (Paul, 2026-10-13), and the others are its
    // team, a row each under it (decision 62, proposed), their own chats
    // out of the person's list
    let first_title = row.as_str().unwrap_or("").to_string();
    let (_, reader_chat, reader_id) = extra_agent(api, &session, "reader", "Reader", "keep my reading list")?;
    let reader_row = format!("document.querySelector({})", js(&format!("#chats [data-key={}]", js(&format!("chat:{reader_chat}")))));
    let reader_team = team_row(&reader_id);
    let one = b.until(
        &page,
        &format!("document.querySelectorAll('#primary-agent .sidebar-agent').length === 1 && document.querySelector('#sidebar-agent .title')?.textContent === {} && !!document.querySelector({}) && !{reader_row}", js(&first_title), js(&reader_team)),
        agent_wait,
    );
    s.ok(
        "another agent of theirs is their one agent's team: the sidebar shows the one agent (the first), the other a row under it, its own chat out of their chats",
        one,
        b.eval(&page, "({ agent: document.getElementById('primary-agent').innerText, team: document.getElementById('team').innerText, chats: document.getElementById('chats').innerText })")?,
    );
    let (hello, seen) = said_hello(s, api, &session, &reader_chat, &reader_id, agent_wait);
    anyhow::ensure!(hello, "the Reader's hello: {seen:?}");

    // what came to a chat not open, and another chat with the Reader (its
    // hello shown once it is open: no frame of it before)
    let first_title = row.as_str().unwrap_or("").to_string();
    let me = Person { session: &session, first: &first_label, first_title: &first_title };
    let opened = open_team_chat(&mut b, &page, &reader_id)?;
    let titled = b.until(&page, &format!("document.getElementById('chat-title').textContent === 'Reader' && document.querySelector({})?.getAttribute('aria-current') === 'page'", js(&reader_team)), wait);
    s.ok("its row opens its own chat with the person, and is marked while it is open", opened && titled, b.eval(&page, "({ title: document.getElementById('chat-title').textContent, team: document.getElementById('team').innerText })")?);
    // "Your team" folds the rows away, the open chat's teammate kept, and
    // unfolds them
    b.click(&page, "#team .team-head")?;
    let folded = b.until(&page, &format!("document.querySelector('#team .team-head').getAttribute('aria-expanded') === 'false' && document.querySelectorAll('#team .team-row').length === 1 && !!document.querySelector({})", js(&reader_team)), wait);
    b.click(&page, "#team .team-head")?;
    let unfolded = b.until(&page, "document.querySelector('#team .team-head').getAttribute('aria-expanded') === 'true'", wait);
    s.ok("\"Your team\" folds the team away but the open chat's teammate, and unfolds it", folded && unfolded, b.eval(&page, "document.getElementById('team').innerText")?);
    // another chat of theirs with the Reader alone (as New chat with it makes
    // one) is the Reader's too: under its row, never among their chats
    let made = shell(api, &session, "POST", "/api/fragments", Some(&json!({ "label": "reading-notes", "template": "chat", "title": "Reading notes" })), &[])?;
    anyhow::ensure!(made.status == 200, "a chat with the Reader: {made}");
    let notes = made.body["name"].as_str().unwrap_or("").to_string();
    let r = shell(api, &session, "PUT", &format!("/api/f/{notes}/members/{reader_id}"), Some(&json!({ "role": "editor" })), &[])?;
    anyhow::ensure!(r.status == 200, "the Reader in it: {r}");
    let under = format!("!!document.querySelector({}) && !document.querySelector({})", js(&format!("#team [data-key={}]", js(&format!("team-chat:{notes}")))), js(&format!("#chats [data-key={}]", js(&format!("chat:{notes}")))));
    let listed = b.until(&page, &under, wait);
    s.ok(
        "another chat of theirs with the Reader alone lists under its row (its own chat open), never among their chats",
        listed,
        b.eval(&page, "({ team: document.getElementById('team').innerText, chats: document.getElementById('chats').innerText })")?,
    );
    let _ = s.eventually(wait, || b.eval_in_frame(&page, &reader_chat, "!!document.querySelector('.msg.agent:not(.streaming)')").ok() == Some(Value::Bool(true)));
    unread_ui(s, api, &mut b, &page, &me, &reader_chat, &shots)?;
    new_chat_ui(s, api, &mut b, &page, &me, &shots)?;

    // both agents in one chat, search, and archiving, as the person uses them
    groups_ui(s, api, &mut b, &page, &me, &shots)?;
    // the person's agents in any chat's @, and one asking another
    roster_ui(s, api, &mut b, &page, &me, &shots)?;
    ask_cli(s, api, &me)?;

    // an app's window: the desktop's (its dock and its windows)
    let desktop = b.until(&page, "document.getElementById('layout').classList.contains('right-open') && document.getElementById('viewer').getBoundingClientRect().width > 0 && !!document.querySelector('#apps #add-app')", wait);
    // its windows (the agents' screens) closed: the desktop itself
    b.eval(&page, "([...document.querySelectorAll('#stack .pane button[aria-label=Close]')].forEach((b) => b.click()), true)")?;
    let bare = b.until(&page, "document.getElementById('viewer').classList.contains('is-empty') && document.getElementById('desktop-empty').getBoundingClientRect().height > 0", wait);
    let empty = b.eval(&page, "({ title: document.querySelector('#desktop-empty .desktop-empty-title')?.textContent ?? null, asks: [...document.querySelectorAll('#desktop-empty .desktop-suggestion')].map((b) => b.textContent), die: document.querySelector('#desktop-empty .prompt-dice')?.getAttribute('aria-label') ?? null, apps: document.querySelectorAll('#apps [data-key^=\"app:\"]').length })")?;
    s.ok(
        "at a desktop's width the desktop shows beside the chat: no apps yet, it says where they land and offers three ways to ask their agent for one, the first three of the twelve, with a die for three more, and its dock offers Add",
        desktop && bare && empty["apps"] == 0 && empty["title"] == "Your apps land here" && strings(&empty["asks"]) == APP_PROMPTS[..3] && empty["die"] == DIE_LABEL,
        &empty,
    );
    let _ = b.screenshot(&page, &shots.join("desktop-empty.png"));
    let asked_in = app_prompts_ui(s, api, &mut b, &page, &session, &empty["asks"], &shots)?;
    // an app made elsewhere, as the CLI makes one from a template (the
    // shell offers none): in the dock with no reload, its window on a click
    let home = cli_home(s, api, &session, "desktop-cli-home")?;
    let made = s.cli_json(api, &home, &["create", "groceries", "--template", "todo", "--title", "groceries", "--json"])?;
    let groceries = made["name"].as_str().unwrap_or("").to_string();
    let docked = b.until(&page, &format!("!!document.querySelector({})", js(&format!("#apps [data-key=\"app:{groceries}\"]"))), wait);
    b.eval(&page, &format!("(document.querySelector({})?.click(), true)", js(&format!("#apps [data-key=\"app:{groceries}\"]"))))?;
    let window = b.until(&page, "document.querySelectorAll('#apps [data-key^=\"app:\"]').length === 1 && document.querySelector('.viewer iframe') && document.getElementById('desktop-empty').hidden", wait);
    s.ok("an app the CLI makes from a template (`fragment create --template todo`) lands in the dock with no reload, and opens in a window on the desktop", label_of(&groceries) == "groceries" && docked && window, &made);
    // its card, once the platform has shot it: read with the shell's session,
    // shown as a blob URL (the shell's CSP allows `img-src blob:`), larger
    // over its icon while a pointer is on it
    let icon = "document.querySelector('#apps [data-key^=\"app:\"]')";
    let carded = b.until(
        &page,
        &format!("(() => {{ {icon}?.dispatchEvent(new PointerEvent('pointerenter', {{ pointerType: 'mouse' }})); const p = document.querySelector('.card-peek'); const i = p?.querySelector('img'); return !!p && !p.hidden && !!i && i.src.startsWith('blob:') && i.complete && i.naturalWidth === 1280 && i.naturalHeight === 800 && p.getBoundingClientRect().width === 320; }})()"),
        super::site::CARD_WAIT,
    );
    s.ok("a pointer over its dock icon shows its card, a blob URL of the 1280×800 shot, larger above it", carded, b.eval(&page, "document.querySelector('.card-peek')?.outerHTML.slice(0, 200) ?? null")?);
    // deployed (its card is shot after), its face is the title it was given, not its template's
    let named = b.eval(&page, &format!("{icon}?.querySelector('.label')?.textContent ?? null"))?;
    s.ok("and its icon is titled with the title its maker gave it, not its template's", named == "groceries", &named);
    let _ = b.screenshot(&page, &shots.join("desktop-app-card.png"));
    b.eval(&page, &format!("({icon}?.dispatchEvent(new PointerEvent('pointerleave')), true)"))?;
    let _ = b.screenshot(&page, &shots.join("desktop-app.png"));
    compose_elsewhere_ui(s, api, &mut b, &page, &session, &asked_in, &groceries)?;
    open_beside_ui(s, api, &mut b, &page, &me, &groceries, &shots)?;
    sidebar_live(s, api, &mut b, &page, &session, &shots)?;

    // settings, at /settings: what a person needs of their account
    let id = super::signin::who(api, &session)?["id"].as_str().unwrap_or("").to_string();
    b.click(&page, "#settings")?;
    let settings = b.until(&page, "location.pathname === '/settings' && ['Account', 'Credit', 'Computer', 'Sharing', 'Connections', 'Build with Claude Code or Codex'].every(h => document.getElementById('settings-page').innerText.toUpperCase().includes(h.toUpperCase()))", wait);
    let text = b.eval(&page, "document.getElementById('settings-page').innerText")?;
    let text = text.as_str().unwrap_or("");
    let mut has = |q: &str| b.eval(&page, &format!("!!document.querySelector({q:?})")).ok() == Some(serde_json::json!(true));
    s.ok(
        "settings, at /settings: the account (who they are, their identity, another sign-in, signing out), credit, computer, connections, and building with a coding agent",
        settings
            && text.contains(&email)
            && text.contains(&id)
            && has("#settings-page a[href='/auth/link?return=%2Fsettings']")
            && has("#settings-page form[method=post][action='/auth/logout'] button"),
        &text[..text.len().min(600)],
    );
    // a prompt for Claude Code or Codex naming this platform's own llms.txt,
    // copied whole; the CLI's install folded below it
    let prompt = format!("Read {}/llms.txt and help me build: ", api.base);
    let shown = b.eval(&page, "(() => { const p = document.getElementById('build-prompt'); const d = document.querySelector('#settings-page details.settings-install'); return { prompt: p?.value, size: p && getComputedStyle(p).fontSize, folded: d && !d.open, install: d?.querySelector('pre.command')?.textContent }; })()")?;
    s.ok(
        "settings' Build with Claude Code or Codex: a prompt naming this platform's llms.txt, its text 16px, and the install folded behind Install it yourself",
        shown["prompt"] == prompt.as_str()
            && shown["size"] == "16px"
            && shown["folded"] == true
            && shown["install"].as_str().is_some_and(|i| {
                i.contains("releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz") && i.contains(&format!("fragment login --host {}", api.base))
            }),
        &shown,
    );
    b.eval(&page, "(() => { window.__copied = null; navigator.clipboard.writeText = async (text) => { window.__copied = text; }; const p = document.getElementById('build-prompt'); p.value += 'a chore wheel'; document.querySelector('#settings-page [data-action=copy-prompt]').click(); return true; })()")?;
    let copied = b.until(&page, &format!("window.__copied === {:?} && document.querySelector('#settings-page [data-action=copy-prompt]').textContent === 'Copied'", format!("{prompt}a chore wheel")), wait);
    s.ok("its Copy copies the prompt as the person finished it, and says so", copied, b.eval(&page, "window.__copied")?);
    // Sharing: what they own, each with who can open it (GET /api/access)
    let theirs = format!("#settings-access .access-row[data-fragment={groceries:?}]");
    let listed = b.until(&page, &format!("!!document.querySelector({theirs:?}) && document.querySelector({theirs:?}).textContent.includes('Anyone with the link')"), wait);
    s.ok(
        "settings' Sharing lists what they own, with who can open each",
        listed,
        b.eval(&page, "document.getElementById('settings-access')?.innerText ?? 'no Sharing section'")?,
    );
    add_skills_ui(s, api, &mut b, &page, &session)?;
    skills_ui(s, api, &mut b, &page, &session)?;
    connections_ui(s, api, &mut b, &page, &session, &email, &chat)?;
    models_ui(s, api, &mut b, &page, &session, &shots)?;
    computer_ui(s, api, &mut b, &page, &session, &shots)?;
    update_ui(s, api, &mut b, &page, &session, &shots)?;
    b.color_scheme(&page, "dark")?;
    let _ = b.screenshot(&page, &shots.join("desktop-settings-dark.png"));
    b.color_scheme(&page, "light")?;
    b.eval(&page, "(document.getElementById('settings-page').scrollTop = 1e6, true)")?;
    let _ = b.screenshot(&page, &shots.join("desktop-settings-end.png"));
    b.reload(&page)?;
    let kept = b.until(&page, "location.pathname === '/settings' && !document.getElementById('settings-page').hidden && document.getElementById('frames').hidden && [...document.querySelectorAll('#settings-page h2')].some((h) => h.textContent === 'Account')", wait);
    s.ok("a reload stays on settings", kept, b.eval(&page, "location.pathname")?);
    b.click(&page, "#chats .chat-row")?;
    let home = b.until(&page, "location.pathname === '/' && document.getElementById('settings-page').hidden && !document.getElementById('frames').hidden", wait);
    b.eval(&page, "history.back(), true")?;
    let back = b.until(&page, "location.pathname === '/settings' && !document.getElementById('settings-page').hidden", wait);
    b.eval(&page, "history.forward(), true")?;
    let forward = b.until(&page, "location.pathname === '/' && document.getElementById('settings-page').hidden", wait);
    s.ok("a chat opened from settings is at /, and back and forward walk between the two", home && back && forward, b.eval(&page, "location.pathname")?);
    // settings fill the work area: a window opened there (an agent's Open)
    // goes back to the open chat, the window beside it, seen
    b.click(&page, "#settings")?;
    let listed = b.until(&page, "location.pathname === '/settings' && [...document.querySelectorAll('#settings-page .settings-section button.row')].some((b) => b.textContent === 'Open')", wait);
    b.eval(&page, "([...document.querySelectorAll('#settings-page .settings-section button.row')].find((b) => b.textContent === 'Open')?.click(), true)")?;
    let seen = b.until(
        &page,
        "location.pathname === '/' && !document.getElementById('layout').classList.contains('settings-view') && document.getElementById('viewer').getBoundingClientRect().width > 0 && !!document.querySelector('#stack .pane iframe')",
        wait,
    );
    s.ok("an agent's Open in settings goes back to the chat, its window beside it", listed && seen, b.eval(&page, "({ path: location.pathname, view: document.getElementById('layout').className, panes: [...document.querySelectorAll('#stack .pane')].map((p) => p.dataset.key) })")?);

    // the phone: the list, then a chat
    b.color_scheme(&page, "light")?;
    b.viewport(&page, 375, 812, true)?;
    b.reload(&page)?;
    let phone = b.until(&page, "!document.getElementById('layout').hidden && document.querySelector('#frames iframe')", wait);
    let fits = b.eval(&page, "document.documentElement.scrollWidth <= innerWidth + 1")?;
    s.ok("on a phone it fits its width, a chat open", phone && fits == true, &fits);
    // the open chat names its agent once `__people` answers, in its
    // composer's placeholder ("Message Reader", which no innerText holds),
    // and the picture waits for it; the frame the shell shows names its
    // fragment (`data-fragment`), whose flat host its page is on
    let open = b.eval(&page, "[...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden)?.dataset.fragment ?? null")?;
    let host = open.as_str().unwrap_or_default().to_string();
    let names = |p: &Value| p.as_str().is_some_and(|p| p.len() > "Message ".len() && p.starts_with("Message "));
    let t0 = std::time::Instant::now();
    let mut placeholder = Value::Null;
    // bounded: the wait, a look every 250 ms
    while !host.is_empty() && t0.elapsed() < wait {
        placeholder = b.eval_in_frame(&page, &host, "document.getElementById('text')?.placeholder ?? null").unwrap_or(Value::Null);
        if names(&placeholder) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    s.ok("and the open chat names its agent", names(&placeholder), json!({ "fragment": open, "placeholder": placeholder }));
    let _ = b.screenshot(&page, &shots.join("phone-chat.png"));
    // the list: the sidebar over the chat, its + the launcher, bottom right
    b.click(&page, "#toggle-left")?;
    // (once the list has slid in: its + is fixed within it while it moves)
    let fab_expr = "(() => { const r = document.getElementById('sidebar-new-chat').getBoundingClientRect(); return r.width >= 44 && r.right > innerWidth - 40 && r.bottom > innerHeight - 60; })()";
    let list = b.until(&page, &format!("document.getElementById('layout').classList.contains('left-open') && document.querySelector('.sidebar-shell').getBoundingClientRect().width >= innerWidth - 2 && {fab_expr}"), wait);
    let fab = b.eval(&page, &format!("({{ fab: {fab_expr}, at: (() => {{ const r = document.getElementById('sidebar-new-chat').getBoundingClientRect(); return [r.left, r.top, r.width, innerWidth, innerHeight]; }})() }})"))?;
    let _ = b.screenshot(&page, &shots.join("phone-list.png"));
    s.ok("on a phone the list covers the chat, its + the launcher at its bottom right, a thumb's size", list && fab["fab"] == true, &fab);
    b.eval(&page, "(document.getElementById('scrim').click(), true)")?;
    b.until(&page, "!document.getElementById('layout').classList.contains('left-open')", wait);
    // an app's window on a phone: the desktop is a sheet from the bottom, the
    // chat's top above it; its dock in it (a script's click: under the
    // phone's emulation a second pointer press right after the first
    // reaches no element of the page)
    b.eval(&page, "(document.getElementById('toggle-right').click(), true)")?;
    b.until(&page, "document.getElementById('layout').classList.contains('right-open') && !!document.querySelector('#apps [data-key^=\"app:\"]')", wait);
    b.eval(&page, "(document.querySelector('#apps [data-key^=\"app:\"]')?.click(), true)")?;
    let sheet = b.until(
        &page,
        "(() => { const v = document.getElementById('viewer').getBoundingClientRect(); return document.getElementById('layout').classList.contains('right-open') \
         && Math.abs(v.bottom - innerHeight) < 2 && Math.abs(v.width - innerWidth) < 2 && v.top > 24 && !!document.querySelector('#stack .pane iframe'); })()",
        wait,
    );
    let shown = b.eval(&page, "document.querySelector('#stack .pane')?.dataset.key?.replace(/^app:/, '') ?? ''")?;
    let shown = shown.as_str().unwrap_or_default().to_string();
    let _ = s.eventually(wait, || !shown.is_empty() && b.eval_in_frame(&page, &shown, "document.readyState").ok() == Some(json!("complete")));
    let _ = b.screenshot(&page, &shots.join("phone-sheet.png"));
    s.ok(
        "on a phone an app's window opens as a sheet from the bottom, the chat's top showing above it",
        sheet,
        b.eval(&page, "(() => { const v = document.getElementById('viewer').getBoundingClientRect(); return { top: v.top, bottom: v.bottom, width: v.width, innerWidth, innerHeight }; })()")?,
    );
    phone_ui(s, api, &mut b, &page, &session, &shots)?;
    // last: it renames the person's first agent, which the checks above name
    faces_ui(s, api, &mut b, &page, &me, &chat, &shots)?;
    println!("      (screenshots: {})", shots.display());
    Ok(())
}

/// An agent's name and picture in the shell (docs/api.md, "An agent's name
/// and picture"): Settings' Agents renames the person's agent and gives it
/// a picture from the row's own controls, and both reach the sidebar, its
/// settings row, the chat's page (its header, and its replies' byline and
/// face) and the share sheet, with no reload, while its first chat keeps
/// its own title. Renamed elsewhere (as `fragment agent` does), the shell
/// and its page follow, and nothing on its page says an old name (its job
/// names none). Rename asks for the chat's name in the chat's ⋯, the
/// agent's in its menu. Remove takes the picture away everywhere. On a
/// phone the name's field is 16px (no zoom on focus) and the row's
/// controls stay on screen.
fn faces_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, chat: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    b.viewport(page, 1280, 900, false)?;
    let r = shell(api, me.session, "GET", "/api/computers", None, &[])?;
    let first = r.body["computers"][0]["agents"].as_array().into_iter().flatten().find(|a| a["fragment"].as_str().map(label_of) == Some(me.first)).cloned().unwrap_or_default();
    let (agent, id) = (first["fragment"].as_str().unwrap_or("").to_string(), first["identity"].as_str().unwrap_or("").to_string());
    anyhow::ensure!(!agent.is_empty() && !id.is_empty(), "the first agent: {r}");
    let face = js(&format!("[data-agent-face={}]", js(&agent)));
    let own_row = js(&format!("#chats [data-key={}] .label", js(&format!("chat:{chat}"))));
    // Rename, in Settings' Agents
    b.click(page, "#settings")?;
    let rename = js(&format!("[data-rename-agent={}]", js(&agent)));
    b.until(page, &format!("!!document.querySelector({rename})"), wait);
    b.eval(page, &format!("(document.querySelector({rename}).click(), true)"))?;
    let asked = b.until(page, "document.getElementById('rename-agent').open && document.getElementById('rename-title').textContent === 'Name your agent'", wait);
    b.eval(page, &fill("#agent-name", "Frank"))?;
    b.eval(page, "(document.getElementById('rename-form').requestSubmit(), true)")?;
    let screen = js(&format!("[data-screen-for={}]", js(&agent)));
    let renamed = b.until(
        page,
        &format!(
            "document.querySelector('#sidebar-agent .title')?.textContent === 'Frank' && document.querySelector({face})?.textContent === 'Frank' && document.querySelector({own_row})?.textContent === {first_chat} && document.querySelector({screen})?.textContent === \"Open Frank's screen\"",
            first_chat = js(FIRST_CHAT)
        ),
        wait,
    );
    s.ok(
        "Settings' Agents renames the person's agent: its name shows in the sidebar, its settings row and its screen's button, with no reload, and its first chat's row keeps the chat's own title",
        asked && renamed,
        b.eval(page, &format!("({{ pill: document.querySelector('#sidebar-agent .title')?.textContent, row: document.querySelector({face})?.textContent, chat: document.querySelector({own_row})?.textContent, screen: document.querySelector({screen})?.textContent }})"))?,
    );
    // a picture, from a file
    let orange = super::faces::orange();
    let file = shots.join("agent-picture.png");
    std::fs::write(&file, &orange)?;
    b.choose_files(page, &format!("[data-picture-for={}]", js(&agent)), &[&file])?;
    let path = super::faces::picture_path(&id, &orange);
    let loaded = |sel: &str| format!("(() => {{ const i = document.querySelector({}); return !!i && i.getAttribute('src') === {} && i.complete && i.naturalWidth === 16; }})()", js(sel), js(&path));
    let pictured = b.until(page, &format!("{} && {}", loaded("#sidebar-agent img"), loaded(&format!("[data-agent-face={}] img", js(&agent)))), wait);
    let offers = b.eval(page, &format!("({{ label: document.querySelector('[data-picture-label={a}]')?.textContent, remove: !document.querySelector('[data-picture-remove={a}]')?.hidden }})", a = js(&agent)))?;
    s.ok(
        "and gives it a picture from a file: the sidebar and its row show it in its image's place, loaded, and the row offers Change and Remove",
        pictured && offers["label"] == "Change picture" && offers["remove"] == true,
        json!({ "offers": offers, "row": own_row_of(api, me.session, &agent) }),
    );
    b.eval(page, &format!("(document.querySelector({face})?.scrollIntoView({{ block: 'center' }}), true)"))?;
    let _ = b.screenshot(page, &shots.join("settings-agent-face.png"));
    // the chat: its header in the shell (the chat's own title), and its page
    open_row(b, page, chat, wait)?;
    let headed = b.until(page, &format!("document.getElementById('chat-title').textContent === {}", js(FIRST_CHAT)), wait);
    let in_page = format!(
        "(() => {{ const p = {}; const m = document.querySelector('#head-marks img'); const by = [...document.querySelectorAll('.msg.agent .who')].pop(); \
         return document.getElementById('head-label')?.textContent === 'Frank' && !!m && m.src.endsWith(p) && m.naturalWidth === 16 && !!by && by.textContent.includes('Frank') && !!by.querySelector('img') && by.querySelector('img').src.endsWith(p); }})()",
        js(&path)
    );
    let framed = s.eventually(wait, || b.eval_in_frame(page, chat, &in_page).ok() == Some(Value::Bool(true)));
    s.ok(
        "its first chat's header in the shell is still the chat's own title, and the chat's page names the agent anew and shows its picture in its header and its replies' byline",
        headed && framed,
        b.eval_in_frame(page, chat, "({ head: document.getElementById('head-label')?.textContent, marks: document.getElementById('head-marks')?.innerHTML, by: [...document.querySelectorAll('.msg.agent .who')].pop()?.innerHTML })")?,
    );
    let _ = b.screenshot(page, &shots.join("chat-agent-face.png"));
    let sheet = super::signin::with_session(api, "GET", &format!("/share/{chat}"), me.session)?;
    s.ok("its chat's share sheet names it so, with its picture", sheet.status == 200 && sheet.text.contains("Frank") && sheet.text.contains(&format!("src=\"{path}\"")), &sheet);
    // renamed elsewhere, as `fragment agent --name` does (its files, then a
    // deploy): the sidebar and the chat's page follow; the chat's own title
    // is the chat's, and stays
    let mut manifest = shell(api, me.session, "GET", &format!("/api/f/{agent}/manifest"), None, &[])?.body;
    manifest["meta"]["title"] = json!("Frankie");
    let files = json!({ "message": "rename", "files": [{ "path": "fragment.json", "text": format!("{}\n", serde_json::to_string_pretty(&manifest)?) }] });
    let wrote = shell(api, me.session, "POST", &format!("/api/f/{agent}/files"), Some(&files), &[])?;
    let deployed = shell(api, me.session, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})), &[])?;
    let followed = b.until(page, "document.querySelector('#sidebar-agent .title')?.textContent === 'Frankie'", wait);
    let in_frame = s.eventually(wait, || b.eval_in_frame(page, chat, "document.getElementById('head-label')?.textContent === 'Frankie'").ok() == Some(Value::Bool(true)));
    let kept = b.eval(page, &format!("({{ row: document.querySelector({own_row})?.textContent ?? null, header: document.getElementById('chat-title').textContent }})"))?;
    let listed = own_row_of(api, me.session, chat);
    s.ok(
        "renamed elsewhere (its files and a deploy, as the CLI does), the sidebar and the chat's page follow with no reload, and its first chat keeps its own title: its row, its header and its fragment's",
        wrote.status == 200 && deployed.status == 200 && followed && in_frame && kept["row"] == FIRST_CHAT && kept["header"] == FIRST_CHAT && listed["title"] == FIRST_CHAT,
        json!({ "shown": kept, "chat": listed }),
    );
    // its page (its menu's Its profile): its new name, and no old one
    // anywhere on it (its job names none)
    b.click(page, "#sidebar-agent")?;
    let profile = menu_item(b, page, "Its profile")?;
    let mut seen = Value::Null;
    // its own origin's frame (a screen's names it in its address too)
    let its_page = api.site_origin(&agent);
    let named = profile
        && s.eventually(wait, || {
            seen = b.eval_in_frame(page, &its_page, "({ name: document.getElementById('name')?.textContent ?? null, title: document.title, soul: document.getElementById('soul')?.textContent ?? '', text: document.body.innerText })").unwrap_or(Value::Null);
            seen["name"] == "Frankie" && seen["soul"].as_str().is_some_and(|t| t.contains("default agent"))
        });
    let text = seen["text"].as_str().unwrap_or("");
    let _ = b.screenshot(page, &shots.join("agent-page-renamed.png"));
    s.ok(
        "its page shows its new name, and none of the names it had (its job says what it does, never who it is)",
        named && seen["title"] == "Frankie" && !text.contains(me.first_title) && !seen["soul"].as_str().unwrap_or("").contains("Frank"),
        json!({ "was": me.first_title, "seen": seen }),
    );
    // Rename asks for what it renames: the chat's name in the chat's ⋯, the
    // agent's in its menu, each alone
    let asked_for = |b: &mut Browser, open: &str| -> Result<Value> {
        b.click(page, open)?;
        menu_item(b, page, "Rename…")?;
        b.until(page, "document.getElementById('rename-agent').open", wait);
        let said = b.eval(page, "({ title: document.getElementById('rename-title').textContent, value: document.getElementById('agent-name').value })")?;
        b.eval(page, "(document.getElementById('rename-cancel').click(), true)")?;
        b.until(page, "!document.getElementById('rename-agent').open", wait);
        Ok(said)
    };
    let chats_own = asked_for(b, "#chat-options")?;
    let agents_own = asked_for(b, "#sidebar-agent")?;
    s.ok(
        "Rename… in the first chat's ⋯ names the chat (\"Name this chat\", its own title), and in the agent's menu the agent (\"Name your agent\", its name)",
        chats_own == json!({ "title": "Name this chat", "value": FIRST_CHAT }) && agents_own == json!({ "title": "Name your agent", "value": "Frankie" }),
        json!({ "chat": chats_own, "agent": agents_own }),
    );
    // Remove: gone everywhere
    b.click(page, "#settings")?;
    let remove = js(&format!("[data-picture-remove={}]", js(&agent)));
    b.until(page, &format!("!!document.querySelector({remove}) && !document.querySelector({remove}).hidden"), wait);
    b.eval(page, &format!("(document.querySelector({remove}).click(), true)"))?;
    let gone = b.until(page, &format!("!document.querySelector('#sidebar-agent img') && !document.querySelector('[data-agent-face={a}] img') && document.querySelector('[data-picture-label={a}]')?.textContent === 'Add a picture'", a = js(&agent)), wait);
    let none = super::faces::fetched(api, &format!("/api/identities/{id}/picture"))?;
    s.ok("Remove takes its picture away: no picture in the sidebar or its row, which offers Add again, and none at its address", gone && none.status == 404, none.status);
    // its colour, by its identity (the agent template's agent-color.js, which
    // the shell and the chat link to), is one colour on its page, in the
    // sidebar and on its chat's page; agent.json names none
    let tint = "(e) => e?.style.getPropertyValue('--agent-color') ?? null";
    let want = b.eval(page, &format!("import('/__shell/agent-color.js').then((m) => m.colorOf({}))", js(&id)))?;
    let sidebar = b.eval(page, &format!("({tint})(document.querySelector('#sidebar-agent .agent-avatar'))"))?;
    let in_chat = s.eventually(wait, || b.eval_in_frame(page, chat, &format!("({tint})(document.querySelector('#head-marks .agent-avatar'))")).ok() == Some(want.clone()));
    b.click(page, "#sidebar-agent")?;
    let profile = menu_item(b, page, "Its profile")?;
    let on_page = profile && s.eventually(wait, || b.eval_in_frame(page, &agent, &format!("({tint})(document.getElementById('avatar'))")).ok() == Some(want.clone()));
    let settings = shell(api, me.session, "GET", &format!("/api/f/{agent}/file?path=agent.json"), None, &[])?;
    s.ok(
        "its colour is its identity's, one on its own page, in the sidebar and on its chat's page; agent.json names none",
        want.as_str().is_some_and(|c| c.starts_with('#')) && sidebar == want && in_chat && on_page && settings.status == 200 && settings.body.get("color").is_none(),
        json!({ "identity's": want, "sidebar": sidebar, "agentPage": b.eval_in_frame(page, &agent, &format!("({tint})(document.getElementById('avatar'))")).ok(), "agentJson": settings.body }),
    );
    // back to Settings' Agents, which its profile's window left
    b.click(page, "#settings")?;
    b.until(page, &format!("location.pathname === '/settings' && !!document.querySelector({rename})"), wait);
    // on a phone: its name's field is 16px, and its row's controls stay on screen
    b.viewport(page, 390, 844, true)?;
    b.reload(page)?;
    b.until(page, &format!("!!document.querySelector({rename})"), wait);
    let fits = b.eval(page, &format!("(() => {{ const r = document.querySelector({rename}); r.scrollIntoView({{ block: 'center' }}); return [...r.parentElement.querySelectorAll('button, label')].every((e) => e.getBoundingClientRect().right <= innerWidth + 1); }})()"))?;
    b.eval(page, &format!("(document.querySelector({rename}).click(), true)"))?;
    b.until(page, "document.getElementById('rename-agent').open", wait);
    let size = b.eval(page, "parseFloat(getComputedStyle(document.getElementById('agent-name')).fontSize)")?;
    let _ = b.screenshot(page, &shots.join("phone-rename-agent.png"));
    b.eval(page, "(document.getElementById('rename-cancel').click(), true)")?;
    s.ok("on a phone the agent's controls stay on screen, and its name's field is 16px (no zoom as it takes focus)", fits == true && size.as_f64().is_some_and(|px| px >= 16.0), json!({ "fits": fits, "px": size }));
    b.viewport(page, 1280, 900, false)?;
    Ok(())
}

/// `name`'s row in the person's list, as the shell reads it.
fn own_row_of(api: &Api, session: &str, name: &str) -> Value {
    let r = shell(api, session, "GET", "/api/fragments", None, &[]).ok();
    r.and_then(|r| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == name).cloned())).unwrap_or(Value::Null)
}

/// Whether a page stays in its viewport: nothing wider than it, and the
/// page itself does not scroll (asked to, it stays put), with its own
/// overscroll held (no bounce).
const PAGE_FITS: &str = "(() => { const d = document.documentElement; const wide = d.scrollWidth; scrollTo(0, 400); document.body.scrollTop = 400; \
    const moved = scrollY + d.scrollTop + document.body.scrollTop; scrollTo(0, 0); document.body.scrollTop = 0; \
    return { ok: wide <= innerWidth && moved === 0 && getComputedStyle(d).overscrollBehaviorY === 'none', wide, width: innerWidth, moved }; })()";

/// Opens `chat` from its row in the sidebar, as a person picks it (a
/// reload, or /, opens the newest chat: maybe another).
fn open_row(b: &mut Browser, page: &Page, chat: &str, wait: std::time::Duration) -> Result<()> {
    let row = format!("document.querySelector({})", js(&format!("#chats [data-key={}]", js(&format!("chat:{chat}")))));
    let open = format!("location.pathname === '/' && [...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden)?.dataset.fragment === {}", js(chat));
    if b.eval(page, &format!("!!({open})"))? != true {
        b.eval(page, &format!("({row}?.click(), true)"))?;
    }
    anyhow::ensure!(b.until(page, &open, wait), "{chat} opened from its row");
    Ok(())
}

/// Whether settings' Your data stays in a phone's width: none of its rows
/// past the screen's edge, nor the page or the section wider than it shows.
const YOUR_DATA_FITS: &str = "(() => { const d = document.getElementById('settings-data'); const p = document.getElementById('settings-page'); if (!d || !p) return { ok: false, missing: true }; \
    d.scrollIntoView({ block: 'center' }); const out = [...d.querySelectorAll('*')].filter((e) => e.getBoundingClientRect().right > innerWidth + 1).map((e) => e.dataset.action || e.className || e.tagName); \
    return { ok: out.length === 0 && p.scrollWidth <= p.clientWidth && d.scrollWidth <= d.clientWidth, out, page: [p.scrollWidth, p.clientWidth], section: [d.scrollWidth, d.clientWidth] }; })()";

/// The fields a phone would zoom into on focus (text under 16px): none.
const SMALL_FIELDS: &str = "[...document.querySelectorAll('input:not([type=checkbox]):not([type=radio]):not([type=hidden]):not([type=file]):not([type=range]), textarea, select')] \
    .map((e) => [e.id || e.name || e.getAttribute('aria-label') || e.placeholder || e.tagName, parseFloat(getComputedStyle(e).fontSize)]).filter(([, px]) => px < 16)";

/// A message the page must hold without widening: a word that never
/// breaks, a long line of code, and a wide table.
const WIDE_MESSAGE: &str = "A word that never breaks: supercalifragilisticexpialidociousandthensomemoreletterswithoutanyspaceatallforthephone\n\n```js\nconst line = 'a very long line of code that must scroll inside its own block and never widen the page at all';\n```\n\n| column one | column two | column three | column four | column five | column six |\n|---|---|---|---|---|---|\n| a wide table cell | another wide cell | and another one | four four four | five five five | six six six |";

/// The shell on a phone, as an app (Paul, 2026-10-13: "staying strictly in
/// the viewport (shouldn't feel overflow scrolly, shouldn't zoom on text
/// input, should feel like a native app)"), at 320, 375, 390 and 430 px in
/// Chrome's phone emulation: the page never wider than the screen nor
/// scrolling, with its sidebar open and closed, a chat holding a long
/// word, code and a wide table (the table scrolls in its own box), the
/// launcher, settings (its Your data with an export ready, its widest
/// rows), the share sheet, an app's window and the ways to ask for one
/// (three, then three more from its die, a target of 44 px or more that a
/// press reaches); no field a phone zooms into on focus (16px or more), in
/// the shell, the chat, the launcher, settings and the sheet; and, at 375,
/// a way to ask the die dealt, pressed, goes into the chat's composer, the
/// desktop's sheet closed.
/// Safari's keyboard, bounce and zoom are not Chrome's: a phone tests those.
fn phone_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    b.color_scheme(page, "light")?;
    let home = "!document.getElementById('layout').hidden && !!document.querySelector('#frames iframe:not([hidden])')";
    b.viewport(page, 375, 812, true)?;
    b.reload(page)?;
    b.until(page, home, wait);
    // a chat of theirs (theirs to invite to), their newest
    let listed = shell(api, session, "GET", "/api/fragments", None, &[])?;
    let chat = listed.body["fragments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["kind"] == "chat" && f["role"] == "owner" && f["archived"] != true)
        .max_by_key(|f| f["previewAt"].as_i64().unwrap_or(0))
        .and_then(|f| f["name"].as_str())
        .unwrap_or("")
        .to_string();
    anyhow::ensure!(!chat.is_empty(), "a chat of theirs: {listed}");
    open_row(b, page, &chat, wait)?;
    // the wide message, and its echo, in that chat
    let r = shell(api, session, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "phone-wide-1", "body": { "text": WIDE_MESSAGE } })), &[])?;
    anyhow::ensure!(r.status == 200, "the wide message: {r}");
    let echoed = s.eventually(agent_wait, || b.eval_in_frame(page, &chat, "!!document.querySelector('.msg.agent .md-table table')").ok() == Some(Value::Bool(true)));
    // an export of theirs, built while the first width's chat is looked at
    let r = shell(api, session, "POST", "/api/export", None, &[])?;
    anyhow::ensure!(r.status == 200, "the export: {r}");
    let mut small: Vec<Value> = Vec::new();
    let mut phone_prompt: Option<(bool, Value)> = None;
    for w in [320u32, 375, 390, 430] {
        b.viewport(page, w, 812, true)?;
        b.reload(page)?;
        b.until(page, home, wait);
        open_row(b, page, &chat, wait)?;
        let mut places: Vec<(&str, Value)> = Vec::new();
        // the chat open, the sidebar closed; its page in its frame, the table in its own box
        places.push(("chat", b.eval(page, PAGE_FITS)?));
        let _ = s.eventually(wait, || b.eval_in_frame(page, &chat, "!!document.querySelector('.msg.agent .md-table table')").ok() == Some(Value::Bool(true)));
        b.eval_in_frame(page, &chat, "(() => { const s = document.getElementById('scroll'); s.scrollTop = s.scrollHeight; return true; })()")?;
        let framed = b.eval_in_frame(
            page,
            &chat,
            &format!("(() => {{ const p = {PAGE_FITS}; const t = [...document.querySelectorAll('.msg.agent .md-table')].pop(); const c = [...document.querySelectorAll('.msg.agent pre')].pop(); \
              return {{ ...p, ok: p.ok && !!t && t.getBoundingClientRect().right <= innerWidth + 1 && (!c || c.getBoundingClientRect().right <= innerWidth + 1), table: t ? [t.scrollWidth, t.clientWidth] : null }}; }})()"),
        )?;
        places.push(("its chat's page, a long word, code and a wide table", framed));
        if let Ok(Value::Array(l)) = b.eval_in_frame(page, &chat, SMALL_FIELDS) { small.extend(l.into_iter().map(|f| json!({ "width": w, "in": "chat", "field": f }))); }
        if w == 320 || w == 430 {
            let _ = b.screenshot(page, &shots.join(format!("phone-{w}-chat.png")));
        }
        // the list, over the chat
        b.eval(page, "(document.getElementById('toggle-left').click(), true)")?;
        b.until(page, "document.getElementById('layout').classList.contains('left-open') && document.getAnimations().every((a) => a.playState !== 'running' || a.effect?.getComputedTiming().iterations === Infinity)", wait);
        places.push(("the list", b.eval(page, PAGE_FITS)?));
        // the launcher, words typed
        b.eval(page, "(document.getElementById('sidebar-new-chat').click(), true)")?;
        b.until(page, "document.getElementById('search-dialog').open", wait);
        b.eval(page, &fill("#workspace-search", "garden"))?;
        b.until(page, "!!document.querySelector('#search-results .search-result')", wait);
        places.push(("the launcher", b.eval(page, PAGE_FITS)?));
        if let Value::Array(l) = b.eval(page, SMALL_FIELDS)? { small.extend(l.into_iter().map(|f| json!({ "width": w, "in": "launcher", "field": f }))); }
        b.eval(page, "(document.getElementById('search-dialog').dispatchEvent(new Event('cancel')), true)")?;
        b.until(page, "!document.getElementById('search-dialog').open", wait);
        // settings
        b.eval(page, "(document.getElementById('scrim').click(), document.getElementById('settings').click(), true)")?;
        b.until(page, "location.pathname === '/settings' && !!document.querySelector('#settings-access')", wait);
        places.push(("settings", b.eval(page, PAGE_FITS)?));
        if let Value::Array(l) = b.eval(page, SMALL_FIELDS)? { small.extend(l.into_iter().map(|f| json!({ "width": w, "in": "settings", "field": f }))); }
        if w == 320 {
            let _ = b.screenshot(page, &shots.join(format!("phone-{w}-settings.png")));
        }
        // its Your data, the export ready: its download, size, until when, and again
        let ready = b.until(page, "document.querySelector('#settings-data .settings-export')?.dataset.state === 'ready' && !!document.querySelector('#settings-data [data-action=export-download]')", agent_wait);
        let data = b.eval(page, YOUR_DATA_FITS)?;
        places.push(("settings' Your data, an export ready", json!({ "ok": ready && data["ok"] == true, "seen": data })));
        if w == 320 {
            let _ = b.screenshot(page, &shots.join(format!("phone-{w}-your-data.png")));
        }
        // the share sheet, over the chat
        // (the chat of theirs, from its row: at / the newest chat opens, maybe one shared with them)
        open_row(b, page, &chat, wait)?;
        let invite = "(() => { const i = [...document.querySelectorAll('#menu button')].find((b) => b.textContent === 'Invite…'); if (i) { i.click(); return true; } \
            if (!document.querySelector('#menu button')) document.getElementById('chat-options')?.click(); return false; })()";
        let sheet = b.until(page, invite, wait) && b.until(page, "document.getElementById('sheet').open", wait) && s.eventually(wait, || b.eval_in_frame(page, "/share/", "!!document.querySelector('input[type=email]')").ok() == Some(Value::Bool(true)));
        places.push(("the share sheet", b.eval(page, PAGE_FITS)?));
        let inside = b.eval_in_frame(page, "/share/", "({ ok: document.documentElement.scrollWidth <= innerWidth, wide: document.documentElement.scrollWidth, width: innerWidth })").unwrap_or(Value::Null);
        places.push(("the share sheet's page", json!({ "ok": sheet && inside["ok"] == true, "inside": inside })));
        if let Ok(Value::Array(l)) = b.eval_in_frame(page, "/share/", SMALL_FIELDS) { small.extend(l.into_iter().map(|f| json!({ "width": w, "in": "share sheet", "field": f }))); }
        b.eval(page, "(document.getElementById('sheet').close(), true)")?;
        // an app's window: the desktop's sheet
        b.eval(page, "(document.getElementById('toggle-right').click(), true)")?;
        b.until(page, "document.getElementById('layout').classList.contains('right-open') && !!document.querySelector('#apps [data-key^=\"app:\"]')", wait);
        b.eval(page, "(document.querySelector('#apps [data-key^=\"app:\"]')?.click(), true)")?;
        b.until(page, "!!document.querySelector('#stack .pane iframe')", wait);
        places.push(("an app's window", b.eval(page, PAGE_FITS)?));
        // ways to ask for an app: the dock's Add, in the desktop's sheet, three
        let prompts = "#stack .pane[data-key=prompts] .app-prompt";
        let die = "#stack .pane[data-key=prompts] .prompt-dice";
        let settled = "document.getAnimations().every((a) => a.playState !== 'running' || a.effect?.getComputedTiming().iterations === Infinity)";
        b.eval(page, "(document.getElementById('add-app')?.click(), true)")?;
        b.until(page, &format!("document.querySelectorAll('{prompts}').length === 3"), wait);
        places.push(("ways to ask for an app", b.eval(page, PAGE_FITS)?));
        // and three more from its die: a thumb's target in the sheet, the
        // one a press there reaches once its window scrolls to it, every
        // prompt inside the screen's width
        b.until(page, settled, wait);
        let first = b.eval(page, &format!("[...document.querySelectorAll('{prompts}')].map((b) => b.textContent)"))?;
        b.eval(page, &format!("(document.querySelector('{die}')?.click(), true)"))?;
        let dealt = b.until(page, &format!("(() => {{ const h = [...document.querySelectorAll('{prompts}')].map((b) => b.textContent); return h.length === 3 && h.every((p) => !{first}.includes(p)); }})()"), wait);
        b.until(page, settled, wait);
        let target = b.eval(
            page,
            &format!("(() => {{ const d = document.querySelector('{die}'); d?.scrollIntoView({{ block: 'nearest' }}); const r = d?.getBoundingClientRect(); const hit = !!r && document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2) === d; \
              const inside = [...document.querySelectorAll('{prompts}')].every((b) => b.getBoundingClientRect().right <= innerWidth + 1); \
              return {{ ok: !!r && r.width >= 44 && r.height >= 44 && r.left >= 0 && r.right <= innerWidth && hit && inside, die: r && [r.left, r.top, r.width, r.height], hit, inside, width: innerWidth, height: innerHeight }}; }})()"),
        )?;
        let fits = b.eval(page, PAGE_FITS)?;
        places.push(("ways to ask, dealt by its die", json!({ "ok": dealt && target["ok"] == true && fits["ok"] == true, "first": first, "target": target, "page": fits })));
        if w == 375 {
            let _ = b.screenshot(page, &shots.join(format!("phone-{w}-prompts.png")));
            let text = b.eval(page, &format!("document.querySelector('{prompts}')?.textContent ?? ''"))?.as_str().unwrap_or("").to_string();
            let before = chat_count(api, session, &chat);
            b.eval(page, &format!("(document.querySelector('{prompts}')?.click(), true)"))?;
            let filled = s.eventually(wait, || b.eval_in_frame(page, &chat, COMPOSER).ok().is_some_and(|v| v["value"] == text.as_str() && v["focused"] == true));
            let uncovered = b.until(page, "!document.getElementById('layout').classList.contains('right-open') && !document.getElementById('layout').classList.contains('left-open') && document.getAnimations().every((a) => a.playState !== 'running' || a.effect?.getComputedTiming().iterations === Infinity)", wait);
            let seen = b.eval_in_frame(page, &chat, "(() => { const r = document.getElementById('text').getBoundingClientRect(); return { top: r.top, bottom: r.bottom, innerHeight }; })()")?;
            let shows = seen["top"].as_f64().is_some_and(|t| t >= 0.0) && seen["bottom"].as_f64().zip(seen["innerHeight"].as_f64()).is_some_and(|(b, h)| b <= h);
            std::thread::sleep(std::time::Duration::from_secs(2));
            let after = chat_count(api, session, &chat);
            let _ = b.screenshot(page, &shots.join(format!("phone-{w}-prompt-composed.png")));
            phone_prompt = Some((
                !text.is_empty() && filled && uncovered && shows && after == before,
                json!({ "composer": b.eval_in_frame(page, &chat, COMPOSER).unwrap_or(Value::Null), "box": seen, "records": [before, after], "uncovered": uncovered }),
            ));
            b.eval_in_frame(page, &chat, "(() => { const t = document.getElementById('text'); t.value = ''; t.dispatchEvent(new Event('input', { bubbles: true })); t.blur(); return true; })()")?;
        }
        let bad: Vec<Value> = places.iter().filter(|(_, v)| v["ok"] != true).map(|(p, v)| json!({ "place": p, "seen": v })).collect();
        s.ok(
            &format!("at {w} px on a phone, the page stays in its viewport, never scrolling or wider: the chat (its sidebar closed and open), a long word, code and a wide table, the launcher, settings and its Your data, the share sheet, an app's window, and the ways to ask for one, three and three more from its die (a 44 px target)"),
            echoed && bad.is_empty(),
            json!({ "bad": bad, "places": places.iter().map(|(p, _)| p).collect::<Vec<_>>() }),
        );
    }
    s.ok("on a phone no field is under 16px (Safari zooms in on one as it takes focus): the chat's composer, the launcher, settings and the share sheet", small.is_empty(), json!(small));
    let (composed, seen) = phone_prompt.unwrap_or((false, Value::Null));
    s.ok("on a phone, a way to ask from the dock's Add goes into the chat's composer: the desktop's sheet closes, the composer shows, focused, and nothing is sent", composed, seen);
    Ok(())
}

/// What the person is told of their computer, and the way back to working
/// (docs/computers.md, "What its owner is told"), as they see it on
/// settings: a sleep whose save fails (the lever's) shows at once, with no
/// reload (the computer tells the page's list socket); its Restart restarts
/// it once, the restart's own save failing too, and the notice then says
/// what it went back to; OK tells it no more, here or after a reload; and
/// settings' Restart computer asks once more, then restarts it, saved.
fn computer_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let computers = shell(api, session, "GET", "/api/computers", None, &[])?;
    let id = computers.body["computers"][0]["computer"].as_str().unwrap_or("").to_string();
    let view = || shell(api, session, "GET", &format!("/api/computers/{id}"), None, &[]).map(|r| r.body).unwrap_or(Value::Null);
    let shown = |kind: &str| format!("document.querySelector('#computer-notices:not([hidden]) .computer-notice[data-kind={kind}]')");
    // its saves fail; its owner's sleep keeps it awake, and tells them
    std::thread::sleep(super::computers::QUEUE_DRAIN);
    let failing = super::computers::lever_with(api, &id, json!({ "op": "fail-saves", "times": 50 }))?;
    let r = shell(api, session, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})), &[])?;
    let generation = r.body["generation"].as_u64().unwrap_or(0);
    let warned = b.until(page, &format!("{n}?.innerText.includes(\"can't save\") && !!{n}.querySelector('button[data-action=restart]')", n = shown("unsaved")), wait);
    let said = b.eval(page, "document.getElementById('computer-notices').innerText")?;
    let _ = b.screenshot(page, &shots.join("computer-unsaved.png"));
    s.ok(
        "a sleep whose save fails is shown at once, with no reload: since when its work is unsaved, when it stops, and Restart",
        failing.status == 200 && r.body["phase"] == "awake" && warned && said.as_str().is_some_and(|t| t.contains("hasn't saved since") && t.contains("If it still can't by")),
        &said,
    );
    // Restart, in the notice: its save fails too, so it goes back, and says so
    b.eval(page, &format!("({}.querySelector('button[data-action=restart]').click(), true)", shown("unsaved")))?;
    let back = b.until(page, &format!("{n}?.innerText.includes(\"Your restart couldn't save first\") && !!{n}.querySelector('button[data-action=seen]')", n = shown("went_back")), std::time::Duration::from_secs(90));
    let v = view();
    let said = b.eval(page, "document.getElementById('computer-notices').innerText")?;
    let _ = b.screenshot(page, &shots.join("computer-went-back.png"));
    s.ok(
        "Restart in the notice restarts it once, back to its last save, and the notice says which save, with an OK",
        back && v["generation"].as_u64() == Some(generation + 1) && v["restored"]["rollback"] == true && said.as_str().is_some_and(|t| t.contains("It went back to its save of")),
        json!({ "said": said, "view": v }),
    );
    super::computers::lever_with(api, &id, json!({ "op": "fail-saves", "times": 0 }))?;
    b.eval(page, &format!("({}.querySelector('button[data-action=seen]').click(), true)", shown("went_back")))?;
    let gone = b.until(page, "document.getElementById('computer-notices').hidden", wait);
    b.reload(page)?;
    let loaded = b.until(page, "!document.getElementById('settings-page').hidden && document.getElementById('settings-page').innerText.toUpperCase().includes('COMPUTER')", wait);
    let still = b.eval(page, "document.getElementById('computer-notices').hidden")?;
    s.ok("OK: it is told no more, here or after a reload", gone && loaded && still == true && view()["notices"].as_array().is_none_or(|l| l.is_empty()), view());
    // settings' Restart computer asks once more, then restarts it, saved
    let asked = b.until(page, "!!document.querySelector('#settings-page [data-action=restart-ask]')", wait);
    b.click(page, "#settings-page [data-action=restart-ask]")?;
    let confirm = b.until(page, "!!document.querySelector('#settings-page button[data-action=restart]')", wait);
    let before = view()["generation"].as_u64().unwrap_or(0);
    b.click(page, "#settings-page button[data-action=restart]")?;
    let restarted = s.eventually(std::time::Duration::from_secs(90), || view()["generation"].as_u64() == Some(before + 1) && view()["phase"] == "awake");
    let settled = b.until(page, "!!document.querySelector('#settings-page [data-action=restart-ask]') && document.getElementById('computer-notices').hidden", wait);
    let v = view();
    s.ok(
        "settings' Restart computer asks once more, then restarts it, saved first: nothing to tell",
        asked && confirm && restarted && settled && v["restored"]["rollback"] == false && v["notices"].as_array().is_none_or(|l| l.is_empty()),
        &v,
    );
    Ok(())
}

/// An update, as the person sees it (docs/computers.md, "What its owner is
/// told"; Paul, 2026-10-09: "new update, click to restart"): their computer
/// awake, between turns, on another image than its next start runs (a pin
/// here, which is what a deploy that changed its image is to it) is shown
/// at once, with no reload, over settings, with Restart and Later; settings
/// says the version it runs and the latest. Restart takes it, and nothing
/// more is told; Later puts a version off, here and after a reload, while
/// settings still says the latest.
fn update_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let computers = shell(api, session, "GET", "/api/computers", None, &[])?;
    let id = computers.body["computers"][0]["computer"].as_str().unwrap_or("").to_string();
    let view = || shell(api, session, "GET", &format!("/api/computers/{id}"), None, &[]).map(|r| r.body).unwrap_or(Value::Null);
    let shown = "document.querySelector('#computer-notices:not([hidden]) .computer-notice[data-kind=update]')";
    let settings_text = |b: &mut Browser| b.eval(page, "document.getElementById('settings-page').innerText").ok().and_then(|t| t.as_str().map(str::to_string)).unwrap_or_default();
    let idle = |s: &Suite| s.eventually(std::time::Duration::from_secs(15), || super::computers::lever(api, &id, "saves").is_ok_and(|r| r.body["keepalives"] == 0));
    // settings' Computer section, in view for its screenshot
    let to_computer = "([...document.querySelectorAll('#settings-page h2')].find((h) => h.textContent === 'Computer')?.scrollIntoView({ block: 'start' }), true)";
    let first = view();
    let running = first["version"].as_str().unwrap_or("").to_string();
    idle(s);
    let r = shell(api, session, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub-next" })), &[])?;
    let latest = r.body["latest"].as_str().unwrap_or("").to_string();
    let told = b.until(page, &format!("{shown}?.innerText.includes('An update is ready for your computer') && !!{shown}.querySelector('button[data-action=restart]') && !!{shown}.querySelector('button[data-action=later]')"), wait);
    let said = b.eval(page, "document.getElementById('computer-notices').innerText")?;
    let _ = b.screenshot(page, &shots.join("computer-update.png"));
    s.ok(
        "an update is shown at once, with no reload: an update is ready, Restart (about a minute, chats and work kept; or by itself at its next sleep), and Later",
        r.status == 200 && told && !running.is_empty() && latest != running && said.as_str().is_some_and(|t| t.contains("about a minute; your chats and work are kept") && t.contains("the next time your computer sleeps")),
        json!({ "said": said, "view": r.body }),
    );
    // settings, read again, says the version it runs and the latest
    b.reload(page)?;
    let versions = b.until(page, "document.getElementById('settings-page').innerText.includes('Latest version')", wait);
    let text = settings_text(b);
    b.eval(page, to_computer)?;
    let _ = b.screenshot(page, &shots.join("settings-update.png"));
    s.ok("settings says the version it runs, and the latest, ready", versions && text.contains(&running) && text.contains(&latest), &text[..text.len().min(800)]);
    // Restart, in the notice: it takes the update, and nothing more is told
    b.eval(page, &format!("({shown}.querySelector('button[data-action=restart]').click(), true)"))?;
    let restarted = s.eventually(std::time::Duration::from_secs(90), || {
        let v = view();
        v["phase"] == "awake" && v["version"] == latest.as_str()
    });
    let settled = b.until(page, "document.getElementById('computer-notices').hidden && !!document.querySelector('#settings-page [data-action=restart-ask]') && !document.getElementById('settings-page').innerText.includes('Latest version')", wait);
    let v = view();
    s.ok("Restart in the notice takes it: it runs the latest, and nothing more is told", restarted && settled && v["latest"] == latest.as_str() && v["notices"].as_array().is_none_or(|l| l.is_empty()), &v);
    // pinned back, another version is ready; Later puts it off, here and after a reload
    idle(s);
    shell(api, session, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "stub" })), &[])?;
    let again = b.until(page, &format!("!!{shown}?.querySelector('button[data-action=later]')"), wait);
    b.eval(page, &format!("({shown}.querySelector('button[data-action=later]').click(), true)"))?;
    let hidden = b.until(page, "document.getElementById('computer-notices').hidden", wait);
    b.reload(page)?;
    let loaded = b.until(page, "!document.getElementById('settings-page').hidden && document.getElementById('settings-page').innerText.includes('Latest version')", wait);
    let still = b.eval(page, "document.getElementById('computer-notices').hidden")?;
    b.eval(page, to_computer)?;
    let _ = b.screenshot(page, &shots.join("settings-update-later.png"));
    let v = view();
    s.ok(
        "Later: that version is told no more, here or after a reload, and settings still says the latest",
        again && hidden && loaded && still == true && v["notices"].as_array().is_none_or(|l| l.is_empty()) && v["latest"] == running.as_str() && v["version"] == latest.as_str(),
        &v,
    );
    Ok(())
}

/// The Connections page in settings (decisions 22, 37 and 44): every
/// provider the deployment offers, one row each, with its kind, the
/// person's state there, which agents may use it (and a press that narrows
/// one), and this month's calls by agent with an operator key's cost. The
/// person's first agent calls a connection and an operator key through its
/// computer (the stub's `fetch`), so the page has uses to show.
fn connections_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, email: &str, chat: &str) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let computers = shell(api, session, "GET", "/api/computers", None, &[])?;
    let computer = computers.body["computers"][0].clone();
    let agents: Vec<(String, String)> = computer["agents"].as_array().map(|l| l.iter().map(|a| (a["fragment"].as_str().unwrap_or("").to_string(), a["identity"].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
    let Some((lead, lead_id)) = agents.first().cloned() else {
        s.ok("the Connections page has agents to show", false, &computers);
        return Ok(());
    };
    // the person connects Google, and reading their connections tells their computer
    s.workos.connect(email, crate::SWAP_CONNECTION, true);
    shell(api, session, "GET", "/api/connections", None, &[])?;
    let replies = || {
        shell(api, session, "GET", &format!("/api/f/{chat}/channels/chat"), None, &[])
            .ok()
            .and_then(|r| r.body["records"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r["principal"] == lead_id.as_str() && r["body"]["text"].as_str().is_some_and(|t| t.starts_with("fetched")))
            .count()
    };
    let calls = [format!("fetch http://{}/gmail/v1/users/me/profile with ${}", crate::SWAP_CONNECTION_HOST, crate::SWAP_CONNECTION_ENV), "fetch http://api.perplexity.ai/search with $PERPLEXITY_API_KEY".to_string()];
    for (n, text) in calls.iter().enumerate() {
        shell(api, session, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": format!("conn-{n}"), "body": { "text": text } })), &[])?;
        s.eventually(std::time::Duration::from_secs(60), || replies() > n);
    }
    let id = computer["computer"].as_str().unwrap_or("").to_string();
    let counted = s.eventually(wait, || {
        shell(api, session, "GET", &format!("/api/computers/{id}/uses"), None, &[]).is_ok_and(|r| r.body["uses"].as_array().is_some_and(|l| l.len() >= 2))
    });
    b.reload(page)?;
    let row = |p: &str| format!("#settings-connections [data-provider={p:?}]");
    // each row: its provider, kind, state, agents allowed, and uses
    let rows = "[...document.querySelectorAll('#settings-connections [data-provider]')].map((r) => [r.dataset.provider, r.dataset.kind, r.dataset.state, \
                r.querySelectorAll('[data-agent][aria-pressed=true]').length, [...r.querySelectorAll('[data-use]')].map((u) => [u.dataset.use, Number(u.dataset.calls), Number(u.dataset.micros)])])";
    let shown = b.until(page, &format!("document.querySelectorAll('#settings-connections [data-provider]').length === {} && !!document.querySelector('{} [data-use]')", crate::SWAP_KEYS.len() + 3, row("perplexity")), wait);
    let got = b.eval(page, rows)?;
    let of = |p: &str| got.as_array().and_then(|l| l.iter().find(|r| r[0] == p)).cloned().unwrap_or(Value::Null);
    let perplexity_charge = 7_500; // $0.005 a call at list, and the margin
    s.ok(
        "settings' Connections lists every provider, one row each, its kind and the person's state: Google connected, the operator's keys offered, own keys not set",
        shown
            && of(crate::SWAP_CONNECTION)[1] == "connection"
            && of(crate::SWAP_CONNECTION)[2] == "connected"
            && crate::SWAP_KEYS.iter().all(|(k, _, _)| of(k)[1] == "operator" && of(k)[2] == "offered")
            && of(crate::SWAP_OWN)[1] == "own"
            && of(crate::SWAP_OWN)[2] == "not_set"
            && of(crate::SWAP_ROUTER)[1] == "own"
            && of(crate::SWAP_ROUTER)[2] == "not_set",
        &got,
    );
    s.ok("each row names which agents may use it: all of them, by default", got.as_array().is_some_and(|l| l.iter().all(|r| r[3] == agents.len())), &got);
    s.ok(
        "and this month's use by agent: a connection's calls counted, an operator key's with its cost",
        counted && of(crate::SWAP_CONNECTION)[4] == json!([[lead, 1, 0]]) && of("perplexity")[4] == json!([[lead, 1, perplexity_charge]]),
        &got,
    );
    b.eval(page, "(document.getElementById('settings-connections').scrollIntoView(), true)")?;
    // the lane's pictures so far are there: `dir` would start it afresh
    let _ = b.screenshot(page, &s.scratch.join("shell-ui/desktop-connections.png"));
    // a press narrows that agent from that provider, and the page says so
    b.eval(page, &format!("document.querySelector('{} [data-agent={lead:?}]').click(), true", row("perplexity")))?;
    let narrowed = b.until(page, &format!("document.querySelector('{} [data-agent={lead:?}]')?.getAttribute('aria-pressed') === 'false'", row("perplexity")), wait);
    let view = shell(api, session, "GET", &format!("/api/computers/{id}"), None, &[])?;
    let list = view.body["agents"].as_array().and_then(|l| l.iter().find(|a| a["fragment"] == lead.as_str())).map(|a| a["connections"].clone()).unwrap_or(Value::Null);
    let others: Vec<&str> = std::iter::once(crate::SWAP_CONNECTION).chain(crate::SWAP_KEYS.iter().map(|(k, _, _)| *k).filter(|k| *k != "perplexity")).chain([crate::SWAP_OWN, crate::SWAP_ROUTER]).collect();
    s.ok(
        "pressing an agent takes that provider from it (a narrowing of the rest), and the page says so",
        narrowed && list.as_array().is_some_and(|l| l.len() == others.len() && others.iter().all(|p| l.iter().any(|x| x == p))),
        &list,
    );
    b.eval(page, &format!("document.querySelector('{} [data-agent={lead:?}]').click(), true", row("perplexity")))?;
    let again = b.until(page, &format!("document.querySelector('{} [data-agent={lead:?}]')?.getAttribute('aria-pressed') === 'true'", row("perplexity")), wait);
    let view = shell(api, session, "GET", &format!("/api/computers/{id}"), None, &[])?;
    let back = view.body["agents"].as_array().and_then(|l| l.iter().find(|a| a["fragment"] == lead.as_str())).is_some_and(|a| a["connections"].is_null());
    s.ok("pressed again, it may use every provider again (null)", again && back, &view);
    Ok(())
}

/// Settings' own models (Paul, 2026-10-08: "they should be able to connect
/// them from settings ideally"): OpenRouter's row is connected with a
/// press, through its own page (the fake's, which approves at once), with
/// no key pasted and no CLI; then each agent offers the provider's models
/// beside its tier's (named by the tier, never a vendor), and the one
/// picked is its agent.json's `model`, its tier kept, which its computer
/// runs its next turn on. Its tier again takes the model away;
/// disconnecting takes the offer.
fn models_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let row = format!("#settings-connections [data-provider={:?}]", crate::SWAP_ROUTER);
    b.reload(page)?;
    let offered = b.until(page, &format!("document.querySelector('{row}')?.dataset.state === 'not_set' && !!document.querySelector('{row} [data-action=sign-in]')"), wait);
    let pasted = b.eval(page, &format!("!!document.querySelector('{row} input')"))?;
    s.ok(
        "an own key with a sign-in is connected from its row with a press, never pasted: no key field, and no agent offers its models yet",
        offered && pasted == false && b.eval(page, "document.querySelectorAll('[data-model-for]').length")? == 0,
        b.eval(page, &format!("document.querySelector('{row}')?.innerText"))?,
    );
    // the press opens the provider's page, which sends the browser back to the platform
    let keys_before = s.openrouter.keys().len();
    b.click(page, &format!("{row} [data-action=sign-in]"))?;
    let callback = format!("/api/connections/{}/callback", crate::SWAP_ROUTER);
    let mut popup = None;
    let came_back = s.eventually(wait, || {
        popup = b.pages().ok().and_then(|l| l.into_iter().find(|(_, url)| url.contains(&callback)).map(|(id, _)| id));
        popup.is_some()
    });
    let said = match &popup {
        Some(id) => {
            let p = b.attach(id)?;
            let connected = b.until(&p, "document.body?.innerText.includes('is connected')", wait);
            let text = b.eval(&p, "document.body?.innerText ?? ''")?;
            b.close(p)?;
            (connected, text)
        }
        None => (false, Value::Null),
    };
    b.reload(page)?;
    let set = b.until(page, &format!("document.querySelector('{row}')?.dataset.state === 'set' && !!document.querySelector('{row} [data-action=disconnect]')"), wait);
    let manage = b.eval(page, &format!("document.querySelector('{row} a[target=_blank][rel=noopener]')?.href ?? null"))?;
    s.ok(
        "pressed, its provider's page sends the browser back to the platform, which says it is connected; settings shows it connected, a Disconnect, and the provider's page to revoke the key",
        came_back && said.0 && set && s.openrouter.keys().len() == keys_before + 1 && manage == "https://openrouter.ai/settings/keys",
        json!({ "popup": said.1, "manage": manage }),
    );
    // each agent offers the provider's models beside its tier's, by the tier's name
    let lead = b.eval(page, "document.querySelector('[data-model-for]')?.dataset.modelFor ?? ''")?.as_str().unwrap_or("").to_string();
    let read = b.until(page, &format!("document.querySelector('[data-model-for={lead:?}]')?.dataset.read === '1'"), wait);
    let options = b.eval(page, &format!("[...(document.querySelector('[data-model-for={lead:?}]')?.options ?? [])].map((o) => [o.value, o.textContent])"))?;
    let options = options.as_array().cloned().unwrap_or_default();
    let agent_json = || shell(api, session, "GET", &format!("/api/f/{lead}/file?path=agent.json"), None, &[]).map(|r| r.body).unwrap_or(Value::Null);
    let before = agent_json();
    let vendor = b.eval(page, "document.getElementById('settings-page').innerText.includes('Workers AI') || [...document.querySelectorAll('[data-model-for] option')].some((o) => o.textContent.includes('Workers AI'))")?;
    s.ok(
        "then each agent offers the provider's models, after its tier's, which it runs on now, named by its tier (cheap: Cheap) and no vendor: no Workers AI anywhere in settings",
        read && !lead.is_empty()
            && before["tier"] == "cheap"
            && options.first().is_some_and(|o| o[0] == "" && o[1] == "Cheap")
            && options.iter().any(|o| o[0] == "openrouter anthropic/claude-sonnet-5.5" && o[1] == "Claude Sonnet 5.5")
            && vendor == false,
        json!({ "options": options, "agentJson": before, "workersAi": vendor }),
    );
    let pick = |b: &mut Browser, value: &str| -> Result<bool> {
        b.eval(page, &format!("(() => {{ const p = document.querySelector('[data-model-for={lead:?}]'); p.value = {value:?}; p.dispatchEvent(new Event('change')); return true; }})()"))?;
        Ok(b.until(page, &format!("document.querySelector('[data-model-for={lead:?}]')?.dataset.saved === {value:?}"), wait))
    };
    let saved = pick(b, "openrouter anthropic/claude-sonnet-5.5")?;
    let after = agent_json();
    b.eval(page, &format!("(document.querySelector('[data-model-for={lead:?}]')?.scrollIntoView({{ block: 'center' }}), true)"))?;
    let _ = b.screenshot(page, &shots.join("desktop-own-models.png"));
    b.eval(page, &format!("(document.querySelector('{row}')?.scrollIntoView({{ block: 'center' }}), true)"))?;
    let _ = b.screenshot(page, &shots.join("desktop-own-models-connected.png"));
    s.ok(
        "picked, it is the agent's agent.json model ({provider, id}), its tier kept, and no colour (its identity's)",
        saved && after["model"] == json!({ "provider": "openrouter", "id": "anthropic/claude-sonnet-5.5" }) && after["tier"] == before["tier"] && after.get("color").is_none(),
        json!({ "before": before, "after": after }),
    );
    b.reload(page)?;
    let shown = b.until(page, &format!("document.querySelector('[data-model-for={lead:?}]')?.dataset.read === '1' && document.querySelector('[data-model-for={lead:?}]').value === 'openrouter anthropic/claude-sonnet-5.5'"), wait);
    let back = pick(b, "")?;
    let gone = agent_json();
    s.ok("settings shows it again as picked; its tier again takes the model away, its tier kept", shown && back && gone.get("model").is_none() && gone["tier"] == before["tier"], &gone);
    // disconnected: the key goes, and no agent offers its models
    b.click(page, &format!("{row} [data-action=disconnect]"))?;
    let off = b.until(page, &format!("document.querySelector('{row}')?.dataset.state === 'not_set' && document.querySelectorAll('[data-model-for]').length === 0"), wait);
    s.ok("Disconnect takes the key away, and no agent offers its models", off, b.eval(page, &format!("document.querySelector('{row}')?.innerText"))?);
    Ok(())
}

/// How soon an open shell shows a change to its person's list, made
/// anywhere: "within a few seconds", with no reload.
const SIDEBAR_LIVE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The sidebar is live (Paul on p5, 2026-10-05: "Fred published a
/// calories app but it didn't show up in my apps list on the left until I
/// reloaded"). Method: the shell open in two tabs; an agent of the
/// person's, acting for them (`?for=<owner>`, as their computer's egress
/// signs a `fragment` CLI's requests in agent mode), makes an app and
/// deploys it through the API. Valid: both tabs list it within a few
/// seconds, with no reload, and its card once the platform shoots it; the
/// first tab's open chat is not reloaded, and the rows it showed are the
/// same elements (patched in place). Then the same for an app someone
/// else shares with them, one deleted elsewhere, and one archived in the
/// other tab.
fn sidebar_live(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let me = super::signin::who(api, session)?["id"].as_str().unwrap_or("").to_string();
    // their agent: a key of theirs (`fragment login`'s) registers it
    let keys = fragment_nip98::Keys::generate();
    api.approve(session, &keys)?;
    let (agent, _) = super::delegation::agent_of(api, &keys)?;
    let second = b.open(&format!("{}/", api.base))?;
    let ready = b.until(&second, "!document.getElementById('layout').hidden && document.querySelectorAll('#apps [data-key^=\"app:\"]').length === 1", wait);
    // what the first tab shows: its open chat (counting its frame's loads) and its rows, marked
    let marked = b.eval(
        page,
        "(() => { const f = [...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden); window.__loads = 0; f?.addEventListener('load', () => window.__loads++); \
         window.__open = f?.dataset.fragment ?? null; const rows = [...document.querySelectorAll('#chats .row[data-key], #apps [data-key]')]; rows.forEach((r) => { r.__kept = true; }); return rows.length; })()",
    )?;
    let row = |name: &str| format!("document.querySelector({})", js(&format!("#apps [data-key={}]", js(&format!("app:{name}")))));
    let both = |b: &mut Browser, expr: &str| b.until(page, expr, SIDEBAR_LIVE_WAIT) && b.until(&second, expr, SIDEBAR_LIVE_WAIT);

    // the agent makes an app for its owner, writes its page, and deploys it
    let as_agent = |method: &str, path: &str, body: Value| api.signed(&agent, method, &format!("{path}?for={me}"), Some(&body));
    // the first tab is the one the person looks at
    b.front(page)?;
    let made = as_agent("POST", "/api/fragments", json!({ "label": "meals", "template": "blank" }))?;
    let t0 = std::time::Instant::now();
    let name = made.body["name"].as_str().unwrap_or("").to_string();
    let first = b.until(page, &row(&name), SIDEBAR_LIVE_WAIT);
    let first_ms = t0.elapsed().as_millis() as u64;
    let other_tab = b.until(&second, &row(&name), SIDEBAR_LIVE_WAIT);
    let second_ms = t0.elapsed().as_millis() as u64;
    let shown = first && other_tab;
    println!("      (the agent's app showed in the open tab in {first_ms} ms, in the second by {second_ms} ms)");
    let page_html = "<!doctype html><title>Meals</title><h1>Meals</h1><p>What we ate today.</p>";
    let wrote = as_agent("POST", &format!("/api/f/{name}/files"), json!({ "message": "the page", "files": [{ "path": "site/index.html", "text": page_html }] }))?;
    let deployed = as_agent("POST", &format!("/api/f/{name}/deploy"), json!({}))?;
    s.ok(
        "an app the person's agent makes for them shows in the open shell's dock within a few seconds, with no reload, and in a second tab",
        ready && made.status == 200 && shown,
        json!({ "made": made.status, "firstInMs": first_ms, "secondInMs": second_ms, "first": b.eval(page, "[...document.querySelectorAll('#apps [data-key]')].map((r) => r.dataset.key)")?, "second": b.eval(&second, "[...document.querySelectorAll('#apps [data-key]')].map((r) => r.dataset.key)")? }),
    );
    let carded = b.until(
        page,
        &format!("(() => {{ {}?.dispatchEvent(new PointerEvent('pointerenter', {{ pointerType: 'mouse' }})); const i = document.querySelector('.card-peek:not([hidden]) img'); return !!i && i.src.startsWith('blob:'); }})()", row(&name)),
        super::site::CARD_WAIT,
    );
    b.eval(page, &format!("({}?.dispatchEvent(new PointerEvent('pointerleave')), true)", row(&name)))?;
    s.ok("its icon shows its card once the platform shoots its deploy", wrote.status == 200 && deployed.status == 200 && carded, json!([wrote.status, deployed.status]));
    let kept = b.eval(
        page,
        "(() => { const f = [...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden); return { loads: window.__loads, open: f?.dataset.fragment ?? null, was: window.__open, \
         kept: [...document.querySelectorAll('#chats .row[data-key], #apps [data-key]')].filter((r) => r.__kept).length }; })()",
    )?;
    s.ok(
        "the open chat stays open, its frame not reloaded, and the rows shown before are the same elements (patched in place)",
        kept["loads"] == 0 && kept["open"] == kept["was"] && !kept["open"].is_null() && kept["kept"] == marked && marked.as_i64().is_some_and(|n| n > 0),
        json!({ "kept": kept, "marked": marked }),
    );
    let _ = b.screenshot(page, &shots.join("desktop-sidebar-live.png"));

    // shared by someone else, deleted elsewhere, archived in the other tab
    let other = api.person()?;
    let theirs = api.qualified(&other, "potluck")?;
    let r = api.create(&other, &theirs)?;
    anyhow::ensure!(r.status == 200, "the other person's app: {r}");
    let r = api.signed(&other, "PUT", &format!("/api/f/{theirs}/members/{me}"), Some(&json!({ "role": "viewer" })))?;
    let lent = both(b, &format!("{}?.dataset.role === 'viewer' && {}.title.endsWith('(viewer)')", row(&theirs), row(&theirs)));
    s.ok("an app someone else shares with them shows in both tabs' docks, as a viewer's", r.status == 200 && lent, &r);
    let there = b.eval(page, &format!("!!{}", row(&name)))? == true;
    let r = shell(api, session, "DELETE", &format!("/api/f/{name}"), None, &[])?;
    let gone = both(b, &format!("!{}", row(&name)));
    s.ok("an app deleted elsewhere leaves both tabs' docks", there && r.status == 200 && gone, &r);
    // the other tab archives it as its page does (the app's menu's call), and says nothing to the first
    b.eval(&second, &format!("(fetch('/api/fragments/{theirs}/archived', {{ method: 'PUT', headers: {{ 'x-fragment-shell': '1', 'content-type': 'application/json' }}, body: '{{\"archived\":true}}' }}), true)"))?;
    let archived = b.until(page, &format!("!{}", row(&theirs)), SIDEBAR_LIVE_WAIT);
    shell(api, session, "PUT", &format!("/api/fragments/{theirs}/archived"), Some(&json!({ "archived": false })), &[])?;
    let back = b.until(page, &row(&theirs), SIDEBAR_LIVE_WAIT);
    s.ok("one archived in the other tab leaves the first's, and comes back unarchived", lent && archived && back, "");
    b.close(second)?;
    Ok(())
}

/// The person's own skills fragments (kind `skills`, theirs), as their list says.
fn own_skills(api: &Api, session: &str) -> Result<Vec<String>> {
    let list = shell(api, session, "GET", "/api/fragments", None, &[])?;
    Ok(list.body["fragments"].as_array().into_iter().flatten().filter(|f| f["kind"] == "skills" && f["role"] == "owner").filter_map(|f| f["name"].as_str().map(str::to_string)).collect())
}

/// The managed skills added from settings (decision 17): a person with an
/// agent and no skills fragment (theirs deleted here; people set up before
/// 2026-10-03 have none). Valid: their settings offer the managed skills,
/// and the button makes the fragment from the blessed template and lists
/// it. Replay: loaded again, there is no second. Loading makes none unasked.
fn add_skills_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let before = own_skills(api, session)?;
    let Some(old) = before.first().cloned() else {
        s.ok("the shell made the person's skills fragment at setup", false, json!(before));
        return Ok(());
    };
    let r = shell(api, session, "DELETE", &format!("/api/f/{old}"), None, &[])?;
    b.reload(page)?;
    let add = "[...document.querySelectorAll('#settings-skills button')].find((b) => b.textContent === 'Add the managed skills')";
    let offered = b.until(page, &format!("!!{add}"), wait);
    let none = own_skills(api, session)?;
    s.ok("with it deleted, their settings offer the managed skills, and loading made none", r.status == 200 && offered && none.is_empty(), json!({ "deleted": r.status, "skills": none }));
    b.eval(page, &format!("({add}.click(), true)"))?;
    let made = s.eventually(wait, || own_skills(api, session).is_ok_and(|l| l.len() == 1));
    let now = own_skills(api, session)?;
    let name = now.first().cloned().unwrap_or_default();
    // listed as it is made; its template's first commit lands a moment later
    let manifest_of = || shell(api, session, "GET", &format!("/api/f/{name}/manifest"), None, &[]);
    s.eventually(wait, || manifest_of().is_ok_and(|m| m.body["template"] == "skills"));
    let manifest = manifest_of()?;
    let listed = b.until(page, &format!("document.getElementById('settings-skills')?.dataset.fragment === {}", js(&name)), wait);
    s.ok(
        "the button makes them one from the blessed template, and their settings list it",
        made && manifest.body["template"] == "skills" && listed,
        json!({ "skills": now, "manifest": manifest.body, "settings": b.eval(page, "document.getElementById('settings-skills')?.innerText.slice(0, 200)")? }),
    );
    b.reload(page)?;
    b.until(page, "!!document.getElementById('settings-skills')", wait);
    let again = own_skills(api, session)?;
    s.ok("loaded again, there is no second", again == now, json!(again));
    Ok(())
}

/// The skills in settings (decision 17): the shell made the person's skills
/// fragment at setup, and its Skills section lists exactly that fragment's
/// managed set by category, then each agent's own skills (its fragment's
/// `skills/`), as the agents' computers read them.
fn skills_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let list = shell(api, session, "GET", "/api/fragments", None, &[])?;
    let mine = |kind: &str| list.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["kind"] == kind && f["role"] == "owner")).and_then(|f| f["name"].as_str()).map(str::to_string);
    let (Some(skills), Some(agent)) = (mine("skills"), mine("agent")) else {
        s.ok("the shell made the person's skills fragment at setup, beside their agent", false, &list);
        return Ok(());
    };
    s.ok("the shell made the person's skills fragment at setup, beside their agent", true, &skills);
    // what the fragment's files say: skills/<category>/<name>/SKILL.md, or skills/<name>/SKILL.md
    let skills_of = |paths: &[String]| -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = paths
            .iter()
            .filter_map(|p| {
                let parts: Vec<&str> = p.split('/').collect();
                match parts.as_slice() {
                    ["skills", c, n, "SKILL.md"] => Some((c.to_string(), n.to_string())),
                    ["skills", n, "SKILL.md"] => Some(("general".to_string(), n.to_string())),
                    _ => None,
                }
            })
            .collect();
        out.sort();
        out
    };
    let paths = |name: &str| -> Result<Vec<String>> {
        let r = shell(api, session, "GET", &format!("/api/f/{name}/files"), None, &[])?;
        Ok(r.body["files"].as_array().map(|l| l.iter().filter_map(|f| f["path"].as_str().map(str::to_string)).collect()).unwrap_or_default())
    };
    let want = skills_of(&paths(&skills)?);
    let shown = "JSON.stringify([...document.querySelectorAll('#settings-skills [data-category] [data-skill]')].map((e) => [e.closest('[data-category]').dataset.category, e.dataset.skill]).sort())";
    let listed = b.until(page, &format!("{shown} !== '[]'"), wait);
    let got: Vec<(String, String)> = serde_json::from_str(b.eval(page, shown)?.as_str().unwrap_or("[]")).unwrap_or_default();
    s.ok(
        &format!("settings' Skills lists the managed set by category ({} skills), exactly the skills fragment's files", want.len()),
        listed && want.len() == 13 && got == want && got.iter().any(|(c, n)| c == "software-development" && n == "apps-finite") && got.iter().any(|(c, n)| c == "autonomous-ai-agents" && n == "agents-finite"),
        json!({ "shown": got.len(), "files": want.len(), "missing": want.iter().filter(|w| !got.contains(w)).collect::<Vec<_>>(), "extra": got.iter().filter(|g| !want.contains(g)).collect::<Vec<_>>() }),
    );
    // an agent's own skill, from its fragment, shows beside it
    let own = "---\nname: garden-notes\ndescription: What this garden needs.\n---\n";
    let w = shell(api, session, "POST", &format!("/api/f/{agent}/files"), Some(&json!({ "files": [{ "path": "skills/garden-notes/SKILL.md", "text": own }], "key": "own-skill" })), &[])?;
    b.reload(page)?;
    let beside = format!("!!document.querySelector('#settings-skills [data-agent={}] [data-skill=\"garden-notes\"]')", js(&agent));
    let shows = b.until(page, &beside, wait);
    let own_files = skills_of(&paths(&agent)?);
    s.ok(
        "and an agent's own skill shows beside it, as its fragment's files say",
        w.status == 200 && shows && own_files.contains(&("general".to_string(), "garden-notes".to_string())),
        json!({ "write": w.status, "own": own_files }),
    );
    Ok(())
}

/// The shell's person in its browser lane: their platform session, and
/// their first agent's label (its fragment's, as the sidebar holds it).
struct Person<'a> {
    session: &'a str,
    first: &'a str,
    /// Its title, as the shell shows it ("Novatron DX").
    first_title: &'a str,
}

/// A JS string literal.
fn js(s: &str) -> String {
    serde_json::to_string(s).expect("a string encodes")
}

/// Whether `agent` speaks first in `chat` within `wait`: a reply of the
/// turn the shell's routine on its `tasks` starts (the stub echoes it as
/// `[your routine]`), and no one else's record there. With what the chat
/// holds, for the check's note.
fn said_hello(s: &Suite, api: &Api, session: &str, chat: &str, agent: &str, wait: std::time::Duration) -> (bool, Value) {
    let mut seen = Vec::new();
    let hello = s.eventually(wait, || {
        seen = shell(api, session, "GET", &format!("/api/f/{chat}/channels/chat?after=0&limit=1000"), None, &[])
            .ok()
            .and_then(|r| r.body["records"].as_array().cloned())
            .unwrap_or_default();
        !seen.is_empty() && seen.iter().all(|r| r["principal"] == agent) && seen.iter().any(|r| r["body"]["text"].as_str().is_some_and(|t| t.starts_with("echo: [your routine] ")))
    });
    (hello, seen.iter().map(|r| json!({ "principal": r["principal"], "body": r["body"] })).collect())
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

/// Whether a frame of the shell's landed on `agent`'s screen: the
/// computer's screen port at `?agent=<agent>`, its page there (the stub's,
/// which shows no screen: what it reads of the query is the image's).
fn screen_landed(s: &mut Suite, b: &mut Browser, page: &Page, agent: &str) -> bool {
    let landed = format!("/p/6080/?agent={agent}");
    s.eventually(std::time::Duration::from_secs(30), || b.eval_in_frame(page, &landed, "location.search").ok() == Some(json!(format!("?agent={agent}"))))
}

/// A teammate's row under the person's one agent (decision 62, proposed):
/// the button that opens its own chat.
fn team_row(identity: &str) -> String {
    format!("#team .team-row[data-key={}] .team-open", js(&format!("team:{identity}")))
}

/// Opens a teammate's own chat as its person does: its row.
fn open_team_chat(b: &mut Browser, page: &Page, identity: &str) -> Result<bool> {
    let wait = std::time::Duration::from_secs(30);
    if !b.until(page, &format!("!!document.querySelector({})", js(&team_row(identity))), wait) {
        return Ok(false);
    }
    b.click(page, &team_row(identity))?;
    Ok(true)
}

/// Clicks the open menu's item named `text` (a script's click: a menu's
/// button needs no gesture). Answers whether there was one.
fn menu_item(b: &mut Browser, page: &Page, text: &str) -> Result<bool> {
    let clicked = b.eval(page, &format!("(() => {{ const i = [...document.querySelectorAll('#menu button')].find((b) => b.textContent === {}); i?.click(); return !!i; }})()", js(text)))?;
    Ok(clicked == true)
}

/// A group chat of the person's two agents, made through the API (the
/// first added leads; the shell makes none: one agent) is a chat like any
/// other: listed, its agents' colours stacked where search shows it; each
/// answers when @mentioned, and the lead when no one is; its ⋯ offers each
/// one's screen. Then search: a message's words find it, and clicking it
/// opens its chat. Then archiving from the chat's ⋯: the chat leaves the
/// list, search still finds it, and Unarchive brings it back.
fn groups_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    let r = shell(api, me.session, "GET", "/api/computers", None, &[])?;
    let agents = r.body["computers"][0]["agents"].as_array().cloned().unwrap_or_default();
    let id_of = |label: &str| agents.iter().find(|a| a["fragment"].as_str().map(label_of) == Some(label)).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    let first_label = me.first;
    let (lead, other) = (id_of("reader"), id_of(first_label));
    anyhow::ensure!(fragment_core::npub::is_identity(&lead) && fragment_core::npub::is_identity(&other), "the two agents' identities: {r}");

    // made as the CLI or a page's add makes one: a chat, its agents added in order
    let made = shell(api, me.session, "POST", "/api/fragments", Some(&json!({ "label": "group", "template": "chat", "title": format!("Reader, {}", me.first_title) })), &[])?;
    anyhow::ensure!(made.status == 200, "the group: {made}");
    let group = made.body["name"].as_str().unwrap_or("").to_string();
    for id in [&lead, &other] {
        let r = shell(api, me.session, "PUT", &format!("/api/f/{group}/members/{id}"), Some(&json!({ "role": "editor" })), &[])?;
        anyhow::ensure!(r.status == 200, "a member of the group: {r}");
    }
    let row = format!("#chats [data-key={}]", js(&format!("chat:{group}")));
    let listed = b.until(page, &format!("document.querySelector({})?.textContent === {}", js(&row), js(&format!("Reader, {}", me.first_title))), wait);
    s.ok("a group chat made elsewhere lists among their chats, by its title", listed, b.eval(page, "document.getElementById('chats').innerText")?);
    let r = shell(api, me.session, "GET", &format!("/api/f/{group}/members"), None, &[])?;
    let members: Vec<&str> = r.body["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").filter_map(|m| m["principal"].as_str()).collect();
    s.ok("its members are the two agents, the lead added first", members == [lead.as_str(), other.as_str()], &r);
    b.click(page, &row)?;
    b.until(page, &format!("document.getElementById('chat-title').textContent === {}", js(&format!("Reader, {}", me.first_title))), wait);

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
    let host = group.clone();
    let shown = s.eventually(wait, || {
        b.eval_in_frame(page, &host, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.matches("echo:").count() >= 3 && t.contains("are you there") && t.contains("and you"))).unwrap_or(false)
    });
    s.ok("the group's chat, framed in the shell, shows both agents' answers", shown, &host);
    let _ = b.screenshot(page, &shots.join("desktop-group.png"));

    // each agent's own screen (its own desktop): from the group's ⋯ the
    // person picks whose, and its frame lands on the computer's screen port
    // at `?agent=<that agent>`, the image's to read (the platform carries it)
    let fragment_of = |label: &str| agents.iter().find_map(|a| a["fragment"].as_str().filter(|f| label_of(f) == label)).unwrap_or("").to_string();
    let (reader, first) = (fragment_of("reader"), fragment_of(first_label));
    b.click(page, "#chat-options")?;
    let items = b.eval(page, "[...document.querySelectorAll('#menu button')].map((b) => b.textContent)")?;
    let mine = format!("{}'s screen", me.first_title);
    let offered = items.as_array().is_some_and(|l| l.iter().any(|t| t == "Reader's screen") && l.iter().any(|t| t.as_str() == Some(mine.as_str())) && !l.iter().any(|t| t == "New chat"));
    s.ok("a group chat's ⋯ offers each of its agents' screens (and no New chat: new chats are with their one agent)", offered, &items);
    let picked = menu_item(b, page, "Reader's screen")?;
    let landed = picked && screen_landed(s, b, page, &reader);
    let frames = b.eval(page, "[...document.querySelectorAll('#stack iframe')].map((f) => f.dataset.src)")?;
    s.ok("picking one opens that agent's screen: its frame lands on the computer's screen port at ?agent=<that agent>", landed, &frames);

    // search: a message's words, and clicking one opens its chat; a group's agents stacked
    let first_chat = own_named(api, me.session, &format!("{first_label}-chat"))?;
    b.click(page, "#sidebar-new-chat")?;
    b.eval(page, &fill("#workspace-search", "water garden"))?;
    let hit = format!("#search-results .search-message[data-fragment={}]", js(&first_chat));
    let found = b.until(page, &format!("document.querySelector({})", js(&hit)), wait);
    s.ok("searching a message's words lists it under Messages, in its chat", found, b.eval(page, "document.getElementById('search-results').innerText")?);
    let _ = b.screenshot(page, &shots.join("desktop-search.png"));
    b.click(page, &hit)?;
    let opened = b.until(page, &format!("!document.getElementById('search-dialog').open && document.getElementById('chat-title').textContent === {}", js(FIRST_CHAT)), wait);
    s.ok("clicking it opens its chat", opened, b.eval(page, "document.getElementById('chat-title').textContent")?);
    b.click(page, "#sidebar-new-chat")?;
    b.eval(page, &fill("#workspace-search", "Reader"))?;
    let stacked = format!("#search-results .search-result:not(.search-message)[data-fragment={}] .avatar-stack .agent-avatar", js(&group));
    let colors = b.until(page, &format!("document.querySelectorAll({}).length === 2", js(&stacked)), wait);
    let found_row = b.eval(page, &format!("document.querySelector({})?.outerHTML ?? null", js(&format!("#search-results .search-result[data-fragment={}]", js(&group)))))?;
    let listed_row = shell(api, me.session, "GET", "/api/fragments", None, &[])?.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == group.as_str()).cloned()).unwrap_or(Value::Null);
    s.ok("found by its title, a group shows its agents' colours stacked, the lead's first", colors, json!({ "row": found_row, "listed": listed_row }));
    b.eval(page, "(document.getElementById('search-dialog').dispatchEvent(new Event('cancel')), true)")?;
    b.until(page, "!document.getElementById('search-dialog').open", wait);
    // a direct chat's ⋯ opens its own agent's screen
    b.click(page, "#chat-options")?;
    let picked = menu_item(b, page, "Its screen")?;
    let landed = picked && screen_landed(s, b, page, &first);
    let frames = b.eval(page, "[...document.querySelectorAll('#stack iframe')].map((f) => f.dataset.src)")?;
    s.ok("a direct chat's ⋯ opens its own agent's screen (Its screen: ?agent=<its agent>)", landed, &frames);

    // archiving, from the chat's ⋯: the person's own view
    let row_of = |name: &str| format!("document.querySelector({})", js(&format!("#chats [data-key={}]", js(&format!("chat:{name}")))));
    b.click(page, "#chat-options")?;
    let archived = menu_item(b, page, "Archive")?;
    let left = b.until(page, &format!("!{} && {} && document.getElementById('chat-title').textContent !== {}", row_of(&first_chat), row_of(&group), js(FIRST_CHAT)), wait);
    s.ok("Archive, from its ⋯: the chat leaves the list, and another opens", archived && left, b.eval(page, "document.getElementById('chats').innerText")?);
    b.click(page, "#sidebar-new-chat")?;
    b.eval(page, &fill("#workspace-search", "water garden"))?;
    let rows = js(&format!("#search-results [data-fragment={}]", js(&first_chat)));
    let marked = b.until(page, &format!("[...document.querySelectorAll({rows})].some((r) => r.querySelector('.meta')?.textContent === 'Archived')"), wait);
    s.ok("search still finds it, marked archived", marked, b.eval(page, "document.getElementById('search-results').innerText")?);
    b.click(page, &hit)?;
    // the launcher gone (it fades out over the page), its chat open
    b.until(page, &format!("!document.getElementById('search-dialog').open && document.getElementById('chat-title').textContent === {}", js(FIRST_CHAT)), wait);
    b.click(page, "#chat-options")?;
    let unarchived = menu_item(b, page, "Unarchive")?;
    let back = b.until(page, &row_of(&first_chat), wait);
    s.ok("Unarchive, from its ⋯, brings it back", unarchived && back, "");

    // the phone's picture below is of the Reader's chat, opened from its face
    let reader = agent_ids(api, me.session, &["reader"])?;
    open_team_chat(b, page, &reader[0])?;
    Ok(())
}

/// A chat's `unread` in the person's list (0 when its row leaves it out).
fn unread_of(api: &Api, session: &str, chat: &str) -> i64 {
    let r = shell(api, session, "GET", "/api/fragments", None, &[]);
    r.ok().and_then(|r| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == chat).map(|f| f["unread"].as_i64().unwrap_or(0)))).unwrap_or(-1)
}

/// The fragment of the chat the shell shows (its frame not hidden).
fn open_chat(b: &mut Browser, page: &Page) -> String {
    b.eval(page, "[...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden)?.dataset.fragment ?? ''").ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// Types `text` into the open chat's composer, in its frame (`host`), and
/// sends it, once its composer is ready. Whether it was sent.
fn say_in(s: &Suite, b: &mut Browser, page: &Page, host: &str, text: &str) -> bool {
    let typed = format!(
        "(() => {{ const t = document.getElementById('text'); if (!t || t.disabled || document.getElementById('say')?.dataset.ready !== '1') return false; t.value = {}; t.dispatchEvent(new Event('input', {{ bubbles: true }})); document.getElementById('say').requestSubmit(); return true; }})()",
        js(text)
    );
    s.eventually(std::time::Duration::from_secs(30), || b.eval_in_frame(page, host, &typed).ok() == Some(Value::Bool(true)))
}

/// Unread (docs/api.md, The shell): a chat not open counts on its row what
/// came since its person saw it, as their list does; opening it marks it
/// seen, here and in their list; and the open chat counts nothing as its
/// messages come. The first agent's chat, while the Reader's is open.
fn unread_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, reader_chat: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    let first = own_named(api, me.session, &format!("{}-chat", me.first))?;
    let ids = agent_ids(api, me.session, &[me.first])?;
    let row = format!("document.querySelector({})", js(&format!("#chats [data-key={}]", js(&format!("chat:{first}")))));
    let open = open_chat(b, page);
    let before = unread_of(api, me.session, &first);
    // a message to it from elsewhere (as the CLI or another device sends one), and its answer
    let said = "count these for me please";
    let r = shell(api, me.session, "POST", &format!("/api/f/{first}/channels/chat"), Some(&json!({ "id": "unread-ui-1", "body": { "text": said } })), &[])?;
    let answered = s.eventually(agent_wait, || replies(api, me.session, &first, &ids[0], said) == 1);
    let want = before + 2;
    let counted = s.eventually(wait, || unread_of(api, me.session, &first) == want);
    let badge = b.until(page, &format!("{row}?.querySelector('.unread')?.textContent === {}", js(&want.to_string())), wait);
    let _ = b.screenshot(page, &shots.join("desktop-unread.png"));
    s.ok(
        "a chat not open counts what came since its person saw it (their own message from elsewhere, and the answer): in their list, and as a badge on its row",
        r.status == 200 && open == reader_chat && answered && counted && badge,
        json!({ "before": before, "now": unread_of(api, me.session, &first), "badge": b.eval(page, &format!("{row}?.querySelector('.unread')?.textContent ?? null"))? }),
    );
    // opened: seen, here and in their list
    b.click(page, &format!("#chats [data-key={}]", js(&format!("chat:{first}"))))?;
    let gone = b.until(page, &format!("{row} && !{row}.querySelector('.unread')"), wait);
    let cleared = s.eventually(wait, || unread_of(api, me.session, &first) == 0);
    s.ok("opening it marks it seen: its badge goes, and their list counts nothing on it", gone && cleared, json!({ "unread": unread_of(api, me.session, &first) }));
    // open, its messages come and count nothing
    let more = "and one more while I look";
    let sent = say_in(s, b, page, &first, more);
    let answered = s.eventually(agent_wait, || replies(api, me.session, &first, &ids[0], more) == 1);
    let shown = s.eventually(wait, || b.eval_in_frame(page, &first, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.matches(more).count() >= 2)).unwrap_or(false));
    let none = s.eventually(wait, || unread_of(api, me.session, &first) == 0) && b.eval(page, &format!("!{row}.querySelector('.unread')"))? == true;
    s.ok("the chat open, on screen, counts nothing as its messages come (marked seen again)", sent && answered && shown && none, json!({ "unread": unread_of(api, me.session, &first) }));
    // the Reader is their one agent's team: its chat opens from its face
    let reader_id = agent_ids(api, me.session, &["reader"])?.remove(0);
    open_team_chat(b, page, &reader_id)?;
    b.until(page, &format!("document.getElementById('chat-title').textContent === 'Reader' && {}", js(reader_chat) + " === [...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden)?.dataset.fragment"), wait);
    Ok(())
}

/// A new chat (docs/api.md, The shell, the launcher): from the Reader's
/// chat, the launcher's Enter makes a chat with the person's one agent
/// (not the open chat's), labelled beside its own (`<first>-chat-2`), the
/// words posted whole as its first message: one record though the first
/// answer was lost and it was sent again (its id, kept); its first message
/// titles it (48 characters at most) and the agent answers there. Nothing
/// typed starts nothing. New chat, from the agent's menu, opens an empty
/// chat with it; asked again before anything is said there, that one, not
/// another. The agent's own chat keeps its label (`fragment ask`, after,
/// finds it).
fn new_chat_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    let first_id = agent_ids(api, me.session, &[me.first])?.remove(0);
    let chats_with = |prefix: &str| -> Vec<String> {
        shell(api, me.session, "GET", "/api/fragments", None, &[])
            .map(|l| l.body["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str().map(label_of)).filter(|l| l.starts_with(prefix)).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let own_chat = format!("{}-chat", me.first);
    let before = chats_with(&own_chat);
    b.click(page, "#sidebar-new-chat")?;
    let open = b.until(page, "document.getElementById('search-dialog').open && document.activeElement === document.getElementById('workspace-search')", wait);
    // nothing typed: Enter starts nothing
    b.eval(page, "(document.getElementById('search-form').requestSubmit(), true)")?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    let nothing = chats_with(&own_chat) == before && b.eval(page, "document.getElementById('search-dialog').open")? == true;
    s.ok("the launcher opens on its input, and with nothing typed, Enter starts nothing", open && nothing, json!({ "before": before, "now": chats_with(&own_chat) }));
    // words: the first row starts a chat with their agent
    let said = "Plan my reading for October: three novels and one history of maps";
    b.eval(page, &fill("#workspace-search", said))?;
    let offered = b.until(page, &format!("document.querySelector('#search-results .search-start')?.textContent.includes({})", js(&format!("Start a new chat with {}", me.first_title))), wait);
    let _ = b.screenshot(page, &shots.join("desktop-launcher.png"));
    // its first answer lost: the platform has the record, the page is told it failed
    b.eval(
        page,
        "(() => { const real = window.fetch; window.fetch = async (url, o) => { if (/\\/channels\\/chat$/.test(url) && o?.method === 'POST' && !window.__lost) { window.__lost = true; await real(url, o); throw new TypeError('the answer was lost'); } return real(url, o); }; return true; })()",
    )?;
    b.eval(page, "(document.getElementById('search-form').requestSubmit(), true)")?;
    let lost = b.until(page, "window.__lost === true && document.getElementById('search-status').textContent.includes(\"wasn't sent\") && document.getElementById('search-dialog').open", wait);
    let status = b.eval(page, "document.getElementById('search-status').textContent")?;
    // sent again: the same chat, the same record
    b.eval(page, "(document.getElementById('search-form').requestSubmit(), true)")?;
    let opened = b.until(page, "!document.getElementById('search-dialog').open", wait);
    let fresh = own_named(api, me.session, &format!("{own_chat}-2"))?;
    let shows = open_chat(b, page);
    let records = shell(api, me.session, "GET", &format!("/api/f/{fresh}/channels/chat?after=0&limit=100"), None, &[])?;
    let sent = records.body["records"].as_array().into_iter().flatten().filter(|r| r["body"]["text"] == said).count();
    let members = shell(api, me.session, "GET", &format!("/api/f/{fresh}/members"), None, &[])?;
    let agents: Vec<(Value, Value)> = members.body["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").map(|m| (m["principal"].clone(), m["role"].clone())).collect();
    s.ok(
        "from the Reader's chat, Enter makes a chat with their agent (not the open chat's), labelled beside its own, and posts the words once, though the first answer was lost and it was sent again",
        offered && lost && opened && shows == fresh && agents == vec![(json!(first_id), json!("editor"))] && sent == 1,
        json!({ "status": status, "shows": shows, "fresh": fresh, "agents": agents, "sent": sent }),
    );
    let title: String = format!("{}…", said.chars().take(47).collect::<String>().trim_end());
    let answered = s.eventually(agent_wait, || replies(api, me.session, &fresh, &first_id, said) == 1);
    let titled = b.until(page, &format!("document.getElementById('chat-title').textContent === {} && document.querySelector({})?.querySelector('.label')?.textContent === {}", js(&title), js(&format!("#chats [data-key={}]", js(&format!("chat:{fresh}")))), js(&title)), wait);
    let manifest = shell(api, me.session, "GET", &format!("/api/f/{fresh}/manifest"), None, &[])?;
    let first_row = b.eval(page, "document.querySelector('#chats .chat-row')?.dataset.key ?? null")?;
    let _ = b.screenshot(page, &shots.join("desktop-new-chat-titled.png"));
    s.ok(
        "its first message titles it (48 characters at most), the agent answers there, and it heads their chats, the newest",
        answered && titled && manifest.body["meta"]["title"] == title.as_str() && first_row == format!("chat:{fresh}").as_str(),
        json!({ "title": manifest.body["meta"]["title"], "want": title, "first": first_row }),
    );
    // New chat, from their agent's menu: an empty chat with it; asked again before anything is said, that one
    b.click(page, "#sidebar-agent")?;
    let picked = menu_item(b, page, "New chat")?;
    let blank = b.until(page, "document.getElementById('chat-title').textContent === 'New chat'", wait);
    let third = own_named(api, me.session, &format!("{own_chat}-3"))?;
    let empty = s.eventually(wait, || b.eval_in_frame(page, &third, "!!document.querySelector('#messages .empty h2') && !document.querySelector('#messages .msg')").ok() == Some(Value::Bool(true)));
    let _ = b.screenshot(page, &shots.join("desktop-new-chat.png"));
    // the Reader is their one agent's team: its chat opens from its face
    let reader_id = agent_ids(api, me.session, &["reader"])?.remove(0);
    open_team_chat(b, page, &reader_id)?;
    b.until(page, "document.getElementById('chat-title').textContent === 'Reader'", wait);
    b.click(page, "#sidebar-agent")?;
    let again = menu_item(b, page, "New chat")? && b.until(page, "document.getElementById('chat-title').textContent === 'New chat'", wait);
    let made = chats_with(&own_chat);
    s.ok(
        "New chat, from their agent's menu, opens an empty chat with it; asked again before anything is said there, it opens that one, not another",
        picked && blank && empty && again && open_chat(b, page) == third && made.len() == before.len() + 2,
        json!({ "chats": made }),
    );
    let own = own_named(api, me.session, &own_chat)?;
    s.ok("the agent's own chat keeps its label, apart from the new ones", label_of(&own) == own_chat && label_of(&fresh) == format!("{own_chat}-2"), json!([own, fresh]));
    // the Reader is their one agent's team: its chat opens from its face
    let reader_id = agent_ids(api, me.session, &["reader"])?.remove(0);
    open_team_chat(b, page, &reader_id)?;
    b.until(page, "document.getElementById('chat-title').textContent === 'Reader'", wait);
    Ok(())
}

/// An app beside the chat (docs/api.md, The shell; docs/chat-records.md,
/// "The page"): a link in an agent's reply to another fragment's page gets
/// "Open", which opens that page in the viewer beside the chat, at its
/// path, signed in as its row's window is; a link to anything else gets
/// none, and a page that asks the shell to open a URL that is no
/// fragment's opens nothing.
fn open_beside_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, app: &str, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    // in the Reader's own chat (a way to ask for an app left it for the one agent's)
    let ids = agent_ids(api, me.session, &["reader"])?;
    open_team_chat(b, page, &ids[0])?;
    b.until(page, "document.getElementById('chat-title').textContent === 'Reader'", wait);
    let host = open_chat(b, page);
    let pane = format!("document.querySelector({})", js(&format!("#stack .pane[data-key={}]", js(&format!("app:{app}")))));
    // its window closed first, so Open is what opens it
    b.eval(page, &format!("({pane}?.querySelector('button[aria-label=Close]')?.click(), true)"))?;
    let closed = b.until(page, &format!("!{pane}"), wait);
    let link = api.site_url(app, "?from=chat");
    let said = format!("Here it is: {link}, or [open the app]({link}) and the platform: {}/settings", api.base);
    let sent = say_in(s, b, page, &host, &said);
    let answered = s.eventually(agent_wait, || replies(api, me.session, &host, &ids[0], &link) == 1);
    let offers = format!(
        "(() => {{ const m = [...document.querySelectorAll('.msg.agent')].filter((m) => m.innerText.includes({l})).pop(); if (!m) return null; \
         return [...m.querySelectorAll('.md a')].filter((a) => ['http:', 'https:'].includes(a.protocol)).map((a) => ({{ href: a.href, open: a.nextElementSibling?.classList.contains('open-beside') ?? false }})); }})()",
        l = js(&link)
    );
    let mut links = Value::Null;
    let _ = s.eventually(wait, || {
        links = b.eval_in_frame(page, &host, &offers).unwrap_or(Value::Null);
        links.as_array().is_some_and(|l| l.len() == 3)
    });
    let want = json!([{ "href": link, "open": true }, { "href": link, "open": true }, { "href": format!("{}/settings", api.base), "open": false }]);
    s.ok("autolinks and markdown links in an agent's reply are offered Open for fragment pages; the platform is not", closed && sent && answered && links == want, json!({ "links": links, "want": want }));
    let _ = b.screenshot(page, &shots.join("desktop-open-offered.png"));
    let clicked = b.eval_in_frame(
        page,
        &host,
        &format!("(() => {{ const m = [...document.querySelectorAll('.msg.agent')].filter((m) => m.innerText.includes({})).pop(); const o = m?.querySelector('.open-beside'); o?.click(); return !!o; }})()", js(&link)),
    )?;
    let src = js(&format!("/auth/frame?name={app}&return=%2F%3Ffrom%3Dchat"));
    let opened = b.until(page, &format!("{pane}?.querySelector('iframe')?.dataset.src === {src} && document.getElementById('layout').classList.contains('right-open')"), wait);
    let landed = s.eventually(wait, || b.eval_in_frame(page, app, "location.search").ok() == Some(json!("?from=chat")));
    let _ = b.screenshot(page, &shots.join("desktop-open-beside.png"));
    s.ok(
        "Open opens that page beside the chat, in the viewer, at its path, signed in by the mint",
        clicked == true && opened && landed,
        b.eval(page, "[...document.querySelectorAll('#stack .pane')].map((p) => [p.dataset.key, p.querySelector('iframe')?.dataset.src])")?,
    );
    // a page that names no fragment's URL opens nothing
    let panes = "document.querySelectorAll('#stack .pane').length";
    let count = b.eval(page, panes)?;
    for url in ["https://example.com/", &format!("{}/settings", api.base), &api.site_url("not-a-name", "")] {
        b.eval_in_frame(page, &host, &format!("(parent.postMessage({{ fragment: 'open', url: {} }}, '*'), true)", js(url)))?;
    }
    std::thread::sleep(std::time::Duration::from_secs(1));
    let after = b.eval(page, panes)?;
    s.ok("asked to open a URL that is no fragment's page (another site, the platform, a host that is no name), the shell opens nothing", after == count, json!([count, after]));
    Ok(())
}

/// Another agent of the person's, made through the public API as the CLI
/// or an agent makes one (the shell makes only the first): its fragment,
/// its job, assigned to their computer, its own chat with it in it, and
/// its hello. Its fragment, its chat and its identity.
fn extra_agent(api: &Api, session: &str, label: &str, title: &str, job: &str) -> Result<(String, String, String)> {
    let call = |method: &'static str, path: &str, body: Value| shell(api, session, method, path, Some(&body), &[]);
    let name = |r: &Reply| r.body["name"].as_str().unwrap_or("").to_string();
    let made = call("POST", "/api/fragments", json!({ "label": label, "template": "agent", "title": title }))?;
    anyhow::ensure!(made.status == 200, "the agent: {made}");
    let agent = name(&made);
    let computers = shell(api, session, "GET", "/api/computers", None, &[])?;
    let computer = computers.body["computers"][0]["computer"].as_str().unwrap_or("").to_string();
    let assigned = call("PUT", &format!("/api/computers/{computer}/agents/{agent}"), json!({}))?;
    let id = assigned.body["agents"].as_array().into_iter().flatten().find(|a| a["fragment"] == agent.as_str()).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    anyhow::ensure!(fragment_core::npub::is_identity(&id), "its computer took it: {assigned}");
    let files = json!([{ "path": "SOUL.md", "text": format!("{job}\n") }, { "path": "agent.json", "text": "{\n  \"tier\": \"cheap\"\n}\n" }]);
    let wrote = call("POST", &format!("/api/f/{agent}/files"), json!({ "key": "agent-job", "message": "its job", "files": files }))?;
    let deployed = call("POST", &format!("/api/f/{agent}/deploy"), json!({}))?;
    anyhow::ensure!(wrote.status == 200 && deployed.status == 200, "its job: {wrote} {deployed}");
    let made = call("POST", "/api/fragments", json!({ "label": format!("{label}-chat"), "template": "chat", "title": title }))?;
    anyhow::ensure!(made.status == 200, "its chat: {made}");
    let chat = name(&made);
    let joined = call("PUT", &format!("/api/f/{chat}/members/{id}"), json!({ "role": "editor" }))?;
    let hello = call("POST", &format!("/api/f/{agent}/channels/tasks"), json!({ "id": "hello", "body": { "kind": "routine", "chat": chat, "text": "You were just made: say hello." } }))?;
    anyhow::ensure!(joined.status == 200 && hello.status == 200, "it joins its chat: {joined} {hello}");
    Ok((agent, chat, id))
}

/// The person's agents (the computer's), by fragment label: their identities.
fn agent_ids(api: &Api, session: &str, labels: &[&str]) -> Result<Vec<String>> {
    let r = shell(api, session, "GET", "/api/computers", None, &[])?;
    let agents = r.body["computers"][0]["agents"].as_array().cloned().unwrap_or_default();
    let ids: Vec<String> = labels
        .iter()
        .map(|l| agents.iter().find(|a| a["fragment"].as_str().map(label_of) == Some(*l)).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string())
        .collect();
    anyhow::ensure!(ids.iter().all(|i| fragment_core::npub::is_identity(i)), "the agents' identities: {r}");
    Ok(ids)
}

/// What `@` lists in a framed chat's composer once its word is `typed`:
/// each option's agent, and whether it is one the message adds (not in
/// the chat yet). None while its composer is not ready.
fn mention_list(b: &mut Browser, page: &Page, host: &str, typed: &str) -> Value {
    let expr = format!(
        "(() => {{ const t = document.getElementById('text'); if (!t || t.disabled) return null; t.focus(); t.value = {}; t.setSelectionRange(t.value.length, t.value.length); \
         t.dispatchEvent(new Event('input', {{ bubbles: true }})); \
         return [...document.querySelectorAll('#mentions:not([hidden]) button')].map((b) => ({{ agent: b.dataset.agent, outside: b.classList.contains('outside') }})); }})()",
        js(typed)
    );
    b.eval_in_frame(page, host, &expr).unwrap_or(Value::Null)
}

/// Polls `mention_list` until it is `want`, at most `wait`: what it last listed.
fn mentions_until(s: &Suite, b: &mut Browser, page: &Page, host: &str, typed: &str, want: &Value, wait: std::time::Duration) -> Value {
    let mut last = Value::Null;
    let _ = s.eventually(wait, || {
        last = mention_list(b, page, host, typed);
        &last == want
    });
    last
}

/// A person's agents in any chat's `@` (decision 8; docs/chat-records.md,
/// "The page"): in a chat of their own, `@` lists its agent, then their
/// other agent, marked as one the message adds; picking it and sending
/// adds it to the chat (the shell's add, as an editor, after the lead), and
/// it answers there while the lead does not. In a chat someone else owns,
/// `@` lists that chat's own agents only: the shell hands a person's agents
/// only to the pages of their own fragments.
fn roster_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, me: &Person, shots: &std::path::Path) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let agent_wait = std::time::Duration::from_secs(120);
    let ids = agent_ids(api, me.session, &["reader", me.first])?;
    let (reader, first) = (ids[0].clone(), ids[1].clone());
    let chat = own_named(api, me.session, "reader-chat")?;
    let host = chat.clone();
    open_team_chat(b, page, &reader)?;
    let want = json!([{ "agent": reader, "outside": false }, { "agent": first, "outside": true }]);
    let listed = mentions_until(s, b, page, &host, "@", &want, wait);
    let placeholder = b.eval_in_frame(page, &host, "document.getElementById('text')?.placeholder ?? null").unwrap_or(Value::Null);
    s.ok(
        "in a chat of their own, @ lists its agent, then their other agent, marked as one the message adds",
        listed == want && placeholder.as_str().is_some_and(|p| p.ends_with(", or @ someone else")),
        json!({ "listed": listed, "placeholder": placeholder }),
    );
    let _ = b.screenshot(page, &shots.join("desktop-roster.png"));
    // the roster says each agent's computer's phase, as the platform last said it
    let phases = b.eval_in_frame(
        page,
        &host,
        "new Promise((done) => { addEventListener('message', (e) => { if (e.data?.fragment === 'agents') done(e.data.agents.map((a) => [a.identity, a.phase ?? null])); }); \
         parent.postMessage({ fragment: 'agents?' }, '*'); setTimeout(() => done(null), 5000); })",
    )?;
    let phase = shell(api, me.session, "GET", "/api/computers", None, &[])?.body["computers"][0]["phase"].clone();
    let mut want_phases = vec![json!([reader, phase]), json!([first, phase])];
    let mut got_phases = phases.as_array().cloned().unwrap_or_default();
    want_phases.sort_by_key(Value::to_string);
    got_phases.sort_by_key(Value::to_string);
    s.ok("the shell's roster names each agent with its computer's phase (awake)", phase == "awake" && got_phases == want_phases, json!({ "roster": phases, "computer": phase }));

    // a page's own add (no click in the shell) asks its person in the
    // shell's dialog, and adds no one while it waits, nor on Cancel
    let agents_in = |api: &Api| -> Vec<Value> {
        shell(api, me.session, "GET", &format!("/api/f/{chat}/members"), None, &[])
            .map(|r| r.body["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").map(|m| m["principal"].clone()).collect())
            .unwrap_or_default()
    };
    let posted = b.eval_in_frame(
        page,
        &host,
        &format!(
            "(() => {{ addEventListener('message', (e) => {{ if (e.data?.fragment === 'agent-added' && e.data.nonce === 'e2e-no-click') document.documentElement.dataset.e2eAdded = JSON.stringify(e.data); }}); \
             parent.postMessage({{ fragment: 'add-agent', identity: {}, nonce: 'e2e-no-click' }}, '*'); return true; }})()",
            js(&first)
        ),
    )?;
    let dialog = "document.getElementById('add-agent-dialog')";
    let asked = b.until(page, &format!("{dialog}.open && {dialog}.dataset.agent === {}", js(&first)), wait);
    let text = b.eval(page, "document.getElementById('add-agent-text').textContent")?;
    let armed_late = b.eval(page, "document.getElementById('add-agent-go').disabled")? == true;
    std::thread::sleep(std::time::Duration::from_secs(2));
    let while_open = agents_in(api);
    b.click(page, "#add-agent-cancel")?;
    let mut answer = Value::Null;
    let _ = s.eventually(wait, || {
        answer = b.eval_in_frame(page, &host, "document.documentElement.dataset.e2eAdded ?? null").unwrap_or(Value::Null);
        !answer.is_null()
    });
    let after = agents_in(api);
    s.ok(
        "a page's add asks the person in the shell's own dialog (its Add armed only after a moment), adds no one while left open, and Cancel answers declined",
        posted == true
            && asked
            && text.as_str().is_some_and(|t| t.contains(me.first_title) && t.contains("Reader"))
            && armed_late
            && while_open == vec![json!(reader)]
            && after == vec![json!(reader)]
            && answer.as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()).is_some_and(|a| a["ok"] == false && a["error"] == "declined"),
        json!({ "text": text, "whileOpen": while_open, "after": after, "answer": answer }),
    );

    // picked with the keyboard, then sent
    let partial: String = me.first.chars().take(3).collect();
    let only = json!([{ "agent": first, "outside": true }]);
    let narrowed = mentions_until(s, b, page, &host, &format!("@{partial}"), &only, wait);
    let picked = b.eval_in_frame(
        page,
        &host,
        "(() => { const t = document.getElementById('text'); t.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })); return t.value; })()",
    )?;
    let said = "hello from the roster";
    let sent = b.eval_in_frame(
        page,
        &host,
        &format!("(() => {{ const t = document.getElementById('text'); t.value += {}; t.dispatchEvent(new Event('input', {{ bubbles: true }})); document.getElementById('say').requestSubmit(); return true; }})()", js(said)),
    )?;
    s.ok("typing its name's start narrows @ to it, and Enter picks it", narrowed == only && picked == format!("@{} ", me.first).as_str() && sent == true, json!({ "narrowed": narrowed, "picked": picked }));
    // sending asks the person first, in the shell: they press Add
    let asked = b.until(page, &format!("{dialog}.open && {dialog}.dataset.agent === {}", js(&first)), wait);
    let unsent = agents_in(api) == vec![json!(reader)] && replies(api, me.session, &chat, &first, said) == 0;
    let _ = b.screenshot(page, &shots.join("desktop-roster-confirm.png"));
    let armed = b.until(page, "!document.getElementById('add-agent-go').disabled", wait);
    b.click(page, "#add-agent-go")?;
    let closed = b.until(page, &format!("!{dialog}.open"), wait);
    s.ok("sending asks the person in the shell's dialog first, and nothing is added or sent until they press Add", asked && unsent && armed && closed, json!({ "asked": asked, "unsent": unsent }));
    let mut members = Value::Null;
    let added = s.eventually(wait, || {
        members = shell(api, me.session, "GET", &format!("/api/f/{chat}/members"), None, &[]).map(|r| r.body).unwrap_or_default();
        let agents: Vec<(Value, Value)> = members["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").map(|m| (m["principal"].clone(), m["role"].clone())).collect();
        agents == vec![(json!(reader), json!("editor")), (json!(first), json!("editor"))]
    });
    s.ok("sending adds it to the chat first, an editor after the lead (the shell's add)", added, &members);
    let answered = s.eventually(agent_wait, || replies(api, me.session, &chat, &first, said) == 1);
    let shown = s.eventually(wait, || b.eval_in_frame(page, &host, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.matches(said).count() >= 2)).unwrap_or(false));
    s.ok(
        "and it answers there, framed in the shell, while the chat's lead does not",
        answered && shown && replies(api, me.session, &chat, &reader, said) == 0,
        json!({ "answered": answered, "shown": shown }),
    );
    let _ = b.screenshot(page, &shots.join("desktop-roster-added.png"));

    // a chat someone else owns, with the person's first agent in it
    let me_id = super::signin::who(api, me.session)?["id"].as_str().unwrap_or("").to_string();
    let other = api.person()?;
    let theirs = api.qualified(&other, "their-chat")?;
    let r = api.create_with(&other, json!({ "name": theirs, "template": "chat", "title": "Their chat" }))?;
    anyhow::ensure!(r.status == 200, "their chat: {r}");
    for (who, role) in [(first.as_str(), "editor"), (me_id.as_str(), "editor")] {
        let r = api.signed(&other, "PUT", &format!("/api/f/{theirs}/members/{who}"), Some(&json!({ "role": role })))?;
        anyhow::ensure!(r.status == 200, "a member of their chat: {r}");
    }
    let row = format!("#chats [data-key={}]", js(&format!("chat:{theirs}")));
    let there = b.until(page, &format!("document.querySelector({})", js(&row)), wait);
    b.click(page, &row)?;
    let host = theirs.clone();
    let want = json!([{ "agent": first, "outside": false }]);
    let listed = mentions_until(s, b, page, &host, "@", &want, wait);
    // the roster would have come by now (it is asked as the page mounts)
    std::thread::sleep(std::time::Duration::from_secs(2));
    let still = mention_list(b, page, &host, "@");
    s.ok(
        "in a chat someone else owns, @ lists that chat's agents only: the person's other agent is not offered",
        there && listed == want && still == want,
        json!({ "listed": listed, "after": still }),
    );
    // back to the Reader's own chat, from its face
    open_team_chat(b, page, &reader)?;
    Ok(())
}

/// A CLI home in the run's scratch (`name`), its key approved for the
/// person `session` signs in, as `fragment login` leaves one.
fn cli_home(s: &mut Suite, api: &Api, session: &str, name: &str) -> Result<std::path::PathBuf> {
    let home = s.dir(name);
    let keys = fragment_nip98::Keys::generate();
    api.approve(session, &keys)?;
    let config = home.join(if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" }).join("fragment");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("config.json"), json!({ "secret_key": keys.secret_hex() }).to_string())?;
    Ok(home)
}

/// The composer of `chat`'s page, framed: what it holds, whether it has
/// its page's focus, what of it is selected, whether it quotes a message.
const COMPOSER: &str = "(() => { const t = document.getElementById('text'); return { value: t?.value ?? null, focused: document.activeElement === t, selected: t ? t.value.slice(t.selectionStart, t.selectionEnd) : null, quoting: !document.getElementById('quoting')?.hidden }; })()";

/// How many records `chat`'s chat channel holds.
fn chat_count(api: &Api, session: &str, chat: &str) -> usize {
    shell(api, session, "GET", &format!("/api/f/{chat}/channels/chat?after=0&limit=1000"), None, &[]).ok().and_then(|r| r.body["records"].as_array().map(Vec::len)).unwrap_or(usize::MAX)
}

/// The ways to ask for an app the shell offers, three at a time (Paul's
/// twelve, 2026-10-16, in his order: `APP_PROMPTS` in cell/shell/shell.js),
/// and the label of the die that deals three more.
const APP_PROMPTS: [&str; 12] = [
    "Make a shared grocery list my family can add to from their phones",
    "Tell me on my phone as soon as … comes back in stock or drops in price",
    "Every morning, read these sites and send me a short summary: …",
    "Make a page for our trip to … where we log who paid for what and it shows who owes whom",
    "Build a habit tracker that shows my weekly streak for: …",
    "Make an RSVP page for my dinner on Saturday that tells me who's coming",
    "Make a reading list where I paste a link and it keeps the title and a one-line summary",
    "Build a chore chart for our house that rotates the jobs each week between: …",
    "Make a page where my team votes on … and everyone sees the results live",
    "Make a page where friends pick which evenings work for …, and it shows the best night",
    "Make a status board that pings me when a build or deploy fails for …",
    "Make a trip planner for … that my friends can add ideas and dates to",
];
const DIE_LABEL: &str = "Show three more ideas";

/// A JSON array's strings.
fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_string)).collect()
}

/// Asking for an app (Paul, 2026-10-14: "rather than spawning generic apps
/// we teach the user how to prompt for new apps"), on a desktop's width:
/// the empty desktop's three ways to ask (`asks`) and the dock's Add's,
/// three at a time (Paul, 2026-10-16), the same three, no templates; one
/// pressed goes into the open chat's composer (one of theirs), focused, in
/// place of what was typed and of a quote, its `…` selected, and nothing
/// is sent (the chat's records stay as they were). The die deals three
/// more: never one showing, all twelve in four hands before any again,
/// the empty desktop's the same hand; one it dealt goes into the composer
/// as the first did. The chat it went to. Asked from a teammate's chat
/// (the Reader's, open when it starts), it goes to their one agent's chat
/// instead, which opens (decision 62, proposed): apps are the one agent's.
fn app_prompts_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, asks: &Value, shots: &std::path::Path) -> Result<String> {
    let wait = std::time::Duration::from_secs(30);
    let from = open_chat(b, page);
    let held = chat_count(api, session, &from);
    let first = asks[0].as_str().unwrap_or("").to_string();
    b.eval(page, "(document.querySelector('#desktop-empty .desktop-suggestion')?.click(), true)")?;
    let mut open = String::new();
    let moved = s.eventually(wait, || {
        open = open_chat(b, page);
        !open.is_empty() && open != from
    });
    anyhow::ensure!(own_named(api, session, label_of(&open)).is_ok_and(|n| n == open), "the open chat is theirs: {open}");
    let chat = open.as_str();
    let listed = b.eval(page, &format!("!!document.querySelector({})", js(&format!("#chats [data-key={}]", js(&format!("chat:{chat}"))))))? == true;
    let filled = s.eventually(wait, || b.eval_in_frame(page, chat, COMPOSER).ok().is_some_and(|v| v["value"] == first.as_str() && v["focused"] == true));
    let before = chat_count(api, session, chat);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = chat_count(api, session, chat);
    s.ok(
        "a way to ask on the empty desktop, from a teammate's chat, goes into their one agent's chat (one of their own chats), which opens: in its composer, focused, and nothing is sent, there or in the teammate's",
        !first.is_empty() && moved && listed && filled && after == before && chat_count(api, session, &from) == held,
        json!({ "from": from, "to": chat, "listed": listed, "composer": b.eval_in_frame(page, chat, COMPOSER).unwrap_or(Value::Null), "records": [before, after] }),
    );
    // a reply to quote below: the one agent answers there (its newest chat
    // may be one with nothing said yet)
    let said = say_in(s, b, page, chat, "Hello again");
    let answered = s.eventually(std::time::Duration::from_secs(120), || b.eval_in_frame(page, chat, "!!document.querySelector('.msg.agent:not(.streaming) .actions .reply')").ok() == Some(Value::Bool(true)));
    anyhow::ensure!(said && answered, "the one agent's answer in {chat}");

    // the dock's Add
    b.click(page, "#add-app")?;
    let pane = "#stack .pane[data-key=prompts]";
    let hand = format!("[...document.querySelectorAll('{pane} .app-prompt')].map((b) => b.textContent)");
    let three = format!("document.querySelectorAll('{pane} .app-prompt').length === 3");
    let listed = b.until(page, &three, wait);
    let shown = b.eval(
        page,
        &format!("({{ title: document.querySelector('{pane} .pane-title')?.textContent ?? null, prompts: {hand}, die: document.querySelector('{pane} .prompt-dice')?.getAttribute('aria-label') ?? null, templates: !!document.querySelector('{pane} form, {pane} input, .catalog') }})"),
    )?;
    let prompts = strings(&shown["prompts"]);
    s.ok(
        "the dock's Add opens ways to ask their agent for an app, in a window on the desktop, and no templates: three at a time, the first three of the twelve (the empty desktop's), some with a … to make theirs, and a die for three more",
        listed
            && shown["title"] == "Ask for an app"
            && shown["templates"] == false
            && prompts == APP_PROMPTS[..3]
            && strings(asks) == prompts
            && prompts.iter().any(|p| p.contains('…'))
            && shown["die"] == DIE_LABEL,
        &shown,
    );
    b.until(page, "document.getAnimations().every((a) => a.playState !== 'running' || a.effect?.getComputedTiming().iterations === Infinity)", wait);
    let _ = b.screenshot(page, &shots.join("desktop-prompts.png"));
    // a reply's quote in the composer: an app's request is no reply, so the prompt clears it
    b.eval_in_frame(page, chat, "([...document.querySelectorAll('.msg.agent .actions .reply')].pop()?.click(), true)")?;
    let quoted = s.eventually(wait, || b.eval_in_frame(page, chat, "!document.getElementById('quoting').hidden").ok() == Some(Value::Bool(true)));
    let pick = prompts.iter().position(|p| p.contains('…')).unwrap_or(0);
    let text = prompts.get(pick).cloned().unwrap_or_default();
    let before = chat_count(api, session, chat);
    b.eval(page, &format!("(document.querySelectorAll('{pane} .app-prompt')[{pick}]?.click(), true)"))?;
    let composed = s.eventually(wait, || {
        b.eval_in_frame(page, chat, COMPOSER).ok().is_some_and(|v| v["value"] == text.as_str() && v["focused"] == true && v["selected"] == "…" && v["quoting"] == false)
    });
    let closed = b.until(page, &format!("!document.querySelector('{pane}')"), wait);
    // the shell's focus is the chat's frame, so typing goes there
    let framed = b.eval(page, &format!("document.activeElement?.dataset?.fragment === {}", js(chat)))? == true;
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = chat_count(api, session, chat);
    let _ = b.screenshot(page, &shots.join("desktop-prompt-composed.png"));
    s.ok(
        "one pressed goes into the open chat's composer in place of what was typed, and clears the reply's quote set just before (a request is no reply), focused, its … selected to type over; the window closes, and nothing is sent",
        quoted && composed && closed && framed && after == before,
        json!({ "composer": b.eval_in_frame(page, chat, COMPOSER).unwrap_or(Value::Null), "records": [before, after], "quoted": quoted, "framed": framed }),
    );

    // its die, the window open again on the hand it showed: four deals
    b.click(page, "#add-app")?;
    let reopened = b.until(page, &three, wait);
    let mut hands = vec![strings(&b.eval(page, &hand)?)];
    for _ in 0..4 {
        let last = js(&serde_json::to_string(&hands[hands.len() - 1])?);
        b.click(page, &format!("{pane} .prompt-dice"))?;
        b.until(page, &format!("(() => {{ const h = {hand}; return h.length === 3 && JSON.stringify(h) !== {last}; }})()"), wait);
        hands.push(strings(&b.eval(page, &hand)?));
    }
    let focused = b.eval(page, &format!("document.activeElement === document.querySelector('{pane} .prompt-dice')"))? == true;
    let desktop = strings(&b.eval(page, "[...document.querySelectorAll('#desktop-empty .desktop-suggestion')].map((b) => b.textContent)")?);
    let fresh = hands.windows(2).all(|w| {
        let dealt: std::collections::BTreeSet<&String> = w[1].iter().collect();
        dealt.len() == 3 && w[1].iter().all(|p| APP_PROMPTS.contains(&p.as_str()) && !w[0].contains(p))
    });
    let cycled: std::collections::BTreeSet<&String> = hands[..4].iter().flatten().collect();
    s.ok(
        "its die deals three more each time, none of the three showing: the first hand and three deals show all twelve, none twice, and the next deal none of the last; the empty desktop's is the same hand, and the die keeps the focus",
        reopened && hands[0] == APP_PROMPTS[..3] && fresh && cycled.len() == 12 && desktop == hands[4] && focused,
        json!({ "hands": hands, "desktop": desktop, "focused": focused }),
    );
    b.until(page, "document.getAnimations().every((a) => a.playState !== 'running' || a.effect?.getComputedTiming().iterations === Infinity)", wait);
    let _ = b.screenshot(page, &shots.join("desktop-prompts-dealt.png"));
    // one it dealt, pressed
    let text = hands[4][0].clone();
    let slot = if text.contains('…') { "…" } else { "" };
    let before = chat_count(api, session, chat);
    b.eval(page, &format!("(document.querySelector('{pane} .app-prompt')?.click(), true)"))?;
    let composed = s.eventually(wait, || b.eval_in_frame(page, chat, COMPOSER).ok().is_some_and(|v| v["value"] == text.as_str() && v["focused"] == true && v["selected"] == slot));
    let closed = b.until(page, &format!("!document.querySelector('{pane}')"), wait);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = chat_count(api, session, chat);
    s.ok(
        "one the die dealt, pressed, goes into the composer as the first did, focused, its … (if it has one) selected; the window closes, and nothing is sent",
        composed && closed && after == before,
        json!({ "composer": b.eval_in_frame(page, chat, COMPOSER).unwrap_or(Value::Null), "records": [before, after] }),
    );
    Ok(open)
}

/// A `compose` from any page but the shell's does nothing: from an app's
/// page (another origin, to every frame of the shell) and from the chat's
/// own page to itself, the composer keeps what it held. Then it is emptied
/// for the checks after.
fn compose_elsewhere_ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str, chat: &str, app: &str) -> Result<()> {
    let wait = std::time::Duration::from_secs(30);
    let held = b.eval_in_frame(page, chat, COMPOSER)?;
    let before = chat_count(api, session, chat);
    let from_app = s.eventually(wait, || {
        b.eval_in_frame(page, app, "(() => { for (let i = 0; i < parent.frames.length; i++) parent.frames[i].postMessage({ fragment: 'compose', text: 'from an app of theirs' }, '*'); return true; })()").ok() == Some(Value::Bool(true))
    });
    b.eval_in_frame(page, chat, "(window.postMessage({ fragment: 'compose', text: 'from its own page' }, '*'), true)")?;
    std::thread::sleep(std::time::Duration::from_secs(2));
    let now = b.eval_in_frame(page, chat, COMPOSER)?;
    let after = chat_count(api, session, chat);
    s.ok(
        "a compose from an app's page (another origin) or from the chat's own page does nothing: the composer keeps what it held, and nothing is sent",
        from_app && held["value"].is_string() && now["value"] == held["value"] && after == before,
        json!({ "held": held, "now": now, "records": [before, after] }),
    );
    b.eval_in_frame(page, chat, "(() => { const t = document.getElementById('text'); t.value = ''; t.dispatchEvent(new Event('input', { bubbles: true })); t.blur(); return true; })()")?;
    Ok(())
}

/// `fragment ask` as a person (an agent's runs in its computer: the hosted
/// lane's, and our Hermes image's): their direct chat with the agent, its
/// answer waited for and printed; the same `--id` again posts nothing and
/// finds the same answer; `--chat` adds the agent to a chat it is not in
/// first. Invalid: an agent that is none of theirs, an empty question.
fn ask_cli(s: &mut Suite, api: &Api, me: &Person) -> Result<()> {
    let home = cli_home(s, api, me.session, "ask-cli-home")?;
    let ids = agent_ids(api, me.session, &["reader"])?;
    let reader = ids[0].clone();
    let said = "hello from the cli";
    let args = ["ask", "reader", said, "--wait", "60", "--id", "ask-e2e-1", "--json"];
    let r = s.cli_json(api, &home, &args)?;
    let answer = |r: &Value| r["answer"]["replies"].as_array().into_iter().flatten().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n");
    s.ok(
        "fragment ask, as a person: their direct chat with the agent, the question to it, its answer waited for",
        r["chat"] == own_named(api, me.session, "reader-chat")?.as_str() && r["asked"]["identity"] == reader.as_str() && r["record"]["body"]["to"] == json!([reader]) && r["answer"]["outcome"] == "idle" && answer(&r).contains(said) && answer(&r).starts_with("echo:"),
        &r,
    );
    let again = s.cli_json(api, &home, &args)?;
    s.ok(
        "the same --id again posts nothing, and finds the same answer at once",
        again["replayed"] == true && again["record"]["seq"] == r["record"]["seq"] && again["answer"]["turn"] == r["answer"]["turn"] && !again["answer"]["turn"].is_null(),
        &again,
    );
    let made = shell(api, me.session, "POST", "/api/fragments", Some(&json!({ "label": "ask-here", "template": "chat", "title": "Ask here" })), &[])?;
    let here = made.body["name"].as_str().unwrap_or("").to_string();
    let r = s.cli_json(api, &home, &["ask", "reader", "and here?", "--chat", &here, "--wait", "60", "--json"])?;
    s.ok(
        "--chat: an agent not in that chat is added first, and answers there",
        made.status == 200 && r["chat"] == here.as_str() && r["added"] == json!([reader]) && r["answer"]["outcome"] == "idle" && answer(&r).contains("and here?"),
        &r,
    );
    let refused = |s: &Suite, args: &[&str]| {
        let out = s.cli(api, &home, args);
        (out.status.code(), serde_json::from_slice::<Value>(&out.stdout).unwrap_or_default())
    };
    let (none_exit, none) = refused(s, &["ask", "nobody", "hi", "--json"]);
    let (empty_exit, empty) = refused(s, &["ask", "reader", "  ", "--json"]);
    s.ok(
        "an agent that is none of theirs is not_found, and an empty question invalid_usage (exit 2)",
        none_exit == Some(1) && none["error"]["code"] == "not_found" && empty_exit == Some(2) && empty["error"]["code"] == "invalid_usage",
        json!({ "none": none, "empty": empty }),
    );
    Ok(())
}
