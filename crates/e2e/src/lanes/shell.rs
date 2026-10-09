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
    list_watch(s, api, &session, &id)
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
    s.ok("the chat's row shows its newest message (the agent's step after it is none)", row(&r)["preview"] == said, &r);
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
        told_new == json!({ "type": "changed" }) && row(&r)["preview"] == "Basil wants water too",
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
    let removed = shell(api, session, "DELETE", &format!("/api/f/{chat}/members/{lead}"), None, &[])?;
    let (my_row, their_row) = rows()?;
    s.ok("the lead removed, every row names the agent left", removed.status == 200 && my_row["agents"] == json!([second]) && their_row["agents"] == json!([second]), json!([my_row, their_row]));
    let removed = shell(api, session, "DELETE", &format!("/api/f/{chat}/members/{second}"), None, &[])?;
    let (my_row, their_row) = rows()?;
    s.ok("and the last, none", removed.status == 200 && my_row["agents"].is_null() && their_row["agents"].is_null(), json!([my_row, their_row]));
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
    b.close(page)?;

    let email = format!("shell-ui-{}@e2e.test", crate::api::now_s());
    let session = api.sign_in(&email)?;
    b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let page = b.open(&format!("{}/", api.base))?;
    b.viewport(&page, 1280, 800, false)?;

    // first run: the default agent, made while the shell waits with no
    // question asked (Paul, 2026-10-03), and nothing to choose before it
    let creating = b.until(&page, "document.querySelector('#first-run-card .creating-steps')", wait);
    s.ok("signed in, the shell makes their default agent at once, asking nothing, and says so", creating, b.eval(&page, "document.getElementById('first-run-card').innerText.slice(0, 200)")?);
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
    let first_label = label_of(&chat).trim_end_matches("-chat").to_string();
    let host = chat.clone();
    let settings = shell(api, &session, "GET", &format!("/api/f/{}/file?path=agent.json", own_named(api, &session, &first_label)?), None, &[])?;
    s.ok("the shell's first agent names the cheap tier (DeepSeek V4 Flash)", settings.status == 200 && settings.body["tier"] == "cheap", &settings);
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
    let settings = shell(api, &session, "GET", &format!("/api/f/{}/file?path=agent.json", own_named(api, &session, "reader")?), None, &[])?;
    s.ok("an agent made from the sidebar also names the cheap tier", settings.status == 200 && settings.body["tier"] == "cheap", &settings);

    // both agents in one chat, search, and archiving, as the person uses them
    let first_title = row.as_str().unwrap_or("").to_string();
    let me = Person { session: &session, first: &first_label, first_title: &first_title };
    groups_ui(s, api, &mut b, &page, &me, &shots)?;
    // the person's agents in any chat's @, and one asking another
    roster_ui(s, api, &mut b, &page, &me, &shots)?;
    ask_cli(s, api, &me)?;

    // an app's window
    b.click(&page, "#add-app")?;
    let catalog = b.until(&page, "document.querySelector('.catalog form')", wait);
    s.ok("Add an app opens the catalog in the viewer", catalog, "");
    // named by its maker: its label, and its title
    b.eval(&page, &fill(".catalog form input[name=label]", "groceries"))?;
    b.eval(&page, "document.querySelector('.catalog form')?.requestSubmit()")?;
    let window = b.until(&page, "document.querySelectorAll('#apps .row[data-key]').length === 1 && document.querySelector('.viewer iframe')", wait);
    s.ok("an app from the catalog opens in a window beside the chat", window, "");
    // its row shows its preview card once the platform has shot it: read with
    // the shell's session, shown as a blob URL (the shell's CSP allows `img-src blob:`)
    let carded = b.until(
        &page,
        "(() => { const i = document.querySelector('#apps .row[data-key] .app-card.shot img'); return !!i && i.src.startsWith('blob:') && i.complete && i.naturalWidth === 1280 && i.naturalHeight === 800; })()",
        super::site::CARD_WAIT,
    );
    s.ok(
        "the app's row in the sidebar shows its preview card, a blob URL of the 1280×800 shot",
        carded,
        b.eval(&page, "[...document.querySelectorAll('#apps .row')].map((r) => r.querySelector('.app-card')?.outerHTML.slice(0, 160))")?,
    );
    // deployed (its card is shot after), its face is the name it was given, not its template's
    let named = b.eval(&page, "document.querySelector('#apps .row[data-key] .label')?.textContent ?? null")?;
    s.ok("and its row is titled with the name its maker gave it, not its template's", named == "groceries", &named);
    let peeked = b.eval(
        &page,
        "(() => { const r = document.querySelector('#apps .row.app-row'); r?.dispatchEvent(new PointerEvent('pointerenter', { pointerType: 'mouse' })); const p = document.querySelector('.card-peek'); return !!p && !p.hidden && p.querySelector('img').src.startsWith('blob:') && p.getBoundingClientRect().width === 320; })()",
    )?;
    s.ok("a pointer over the row shows the card larger, beside the sidebar", peeked == true, &peeked);
    let _ = b.screenshot(&page, &shots.join("desktop-app-card.png"));
    b.eval(&page, "(document.querySelector('#apps .row.app-row')?.dispatchEvent(new PointerEvent('pointerleave')), true)")?;
    let _ = b.screenshot(&page, &shots.join("desktop-app.png"));
    sidebar_live(s, api, &mut b, &page, &session, &shots)?;

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
    add_skills_ui(s, api, &mut b, &page, &session)?;
    skills_ui(s, api, &mut b, &page, &session)?;
    connections_ui(s, api, &mut b, &page, &session, &email, &chat)?;
    models_ui(s, api, &mut b, &page, &session, &shots)?;
    computer_ui(s, api, &mut b, &page, &session, &shots)?;
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
    println!("      (screenshots: {})", shots.display());
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
    let _ = b.screenshot(page, &s.dir("shell-ui").join("desktop-connections.png"));
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
/// beside Workers AI's, and the one picked is its agent.json's `model`,
/// its tier and colour kept, which its computer runs its next turn on.
/// Workers AI again takes the model away; disconnecting takes the offer.
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
    // each agent offers the provider's models beside Workers AI's
    let pickers = b.eval(page, "[...document.querySelectorAll('[data-model-for]')].map((p) => [p.dataset.modelFor, p.dataset.read === '1', [...p.options].map((o) => [o.value, o.textContent])])")?;
    let first = pickers[0].clone();
    let lead = first[0].as_str().unwrap_or("").to_string();
    let read = b.until(page, &format!("document.querySelector('[data-model-for={lead:?}]')?.dataset.read === '1'"), wait);
    let options = first[2].as_array().cloned().unwrap_or_default();
    s.ok(
        "then each agent offers the provider's models, after Workers AI's (included), which it runs on now",
        read && !lead.is_empty() && options.first().is_some_and(|o| o[0] == "" && o[1] == "Workers AI (included)") && options.iter().any(|o| o[0] == "openrouter anthropic/claude-sonnet-5.5" && o[1] == "Claude Sonnet 5.5"),
        &pickers,
    );
    let agent_json = || shell(api, session, "GET", &format!("/api/f/{lead}/file?path=agent.json"), None, &[]).map(|r| r.body).unwrap_or(Value::Null);
    let before = agent_json();
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
        "picked, it is the agent's agent.json model ({provider, id}), its tier and colour kept",
        saved && after["model"] == json!({ "provider": "openrouter", "id": "anthropic/claude-sonnet-5.5" }) && after["tier"] == before["tier"] && after["color"] == before["color"],
        json!({ "before": before, "after": after }),
    );
    b.reload(page)?;
    let shown = b.until(page, &format!("document.querySelector('[data-model-for={lead:?}]')?.dataset.read === '1' && document.querySelector('[data-model-for={lead:?}]').value === 'openrouter anthropic/claude-sonnet-5.5'"), wait);
    let back = pick(b, "")?;
    let gone = agent_json();
    s.ok("settings shows it again as picked; Workers AI again takes the model away, its tier kept", shown && back && gone.get("model").is_none() && gone["tier"] == before["tier"], &gone);
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
    let ready = b.until(&second, "!document.getElementById('layout').hidden && document.querySelectorAll('#apps .row[data-key]').length === 1", wait);
    // what the first tab shows: its open chat (counting its frame's loads) and its rows, marked
    let marked = b.eval(
        page,
        "(() => { const f = [...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden); window.__loads = 0; f?.addEventListener('load', () => window.__loads++); \
         window.__open = f?.dataset.fragment ?? null; const rows = [...document.querySelectorAll('#chats .row[data-key], #apps .row[data-key]')]; rows.forEach((r) => { r.__kept = true; }); return rows.length; })()",
    )?;
    let row = |name: &str| format!("document.querySelector({})", js(&format!("#apps .row[data-key={}]", js(&format!("app:{name}")))));
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
        "an app the person's agent makes for them shows in the open shell's sidebar within a few seconds, with no reload, and in a second tab",
        ready && made.status == 200 && shown,
        json!({ "made": made.status, "firstInMs": first_ms, "secondInMs": second_ms, "first": b.eval(page, "[...document.querySelectorAll('#apps .row')].map((r) => r.dataset.key)")?, "second": b.eval(&second, "[...document.querySelectorAll('#apps .row')].map((r) => r.dataset.key)")? }),
    );
    let carded = b.until(page, &format!("{}?.querySelector('.app-card.shot img')?.src.startsWith('blob:')", row(&name)), super::site::CARD_WAIT);
    s.ok("its row shows its preview card once the platform shoots its deploy", wrote.status == 200 && deployed.status == 200 && carded, json!([wrote.status, deployed.status]));
    let kept = b.eval(
        page,
        "(() => { const f = [...document.querySelectorAll('#frames iframe')].find((f) => !f.hidden); return { loads: window.__loads, open: f?.dataset.fragment ?? null, was: window.__open, \
         kept: [...document.querySelectorAll('#chats .row[data-key], #apps .row[data-key]')].filter((r) => r.__kept).length }; })()",
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
    let lent = both(b, &format!("{}?.querySelector('.meta')?.textContent === 'viewer'", row(&theirs)));
    s.ok("an app someone else shares with them shows in both tabs, as a viewer's", r.status == 200 && lent, &r);
    let there = b.eval(page, &format!("!!{}", row(&name)))? == true;
    let r = shell(api, session, "DELETE", &format!("/api/f/{name}"), None, &[])?;
    let gone = both(b, &format!("!{}", row(&name)));
    s.ok("an app deleted elsewhere leaves both tabs' sidebars", there && r.status == 200 && gone, &r);
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
        listed && want.len() == 12 && got == want && got.iter().any(|(c, n)| c == "software-development" && n == "apps-finite"),
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
    let id_of = |label: &str| agents.iter().find(|a| a["fragment"].as_str().map(label_of) == Some(label)).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    let first_label = me.first;
    let (lead, other) = (id_of("reader"), id_of(first_label));
    anyhow::ensure!(fragment_core::npub::is_identity(&lead) && fragment_core::npub::is_identity(&other), "the two agents' identities: {r}");

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
    // made once its row is in the sidebar and its chat's heading has drawn its two agents
    let made = b.until(
        page,
        "document.querySelector('#chats .agent-row[data-group=\"2\"]') && !document.getElementById('new-group-dialog').open \
         && document.querySelectorAll('#agent-mark .avatar-stack .agent-avatar').length === 2",
        wait,
    );
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
    let host = group.clone();
    let shown = s.eventually(wait, || {
        b.eval_in_frame(page, &host, "document.body.innerText").ok().and_then(|v| v.as_str().map(|t| t.matches("echo:").count() >= 3 && t.contains("are you there") && t.contains("and you"))).unwrap_or(false)
    });
    s.ok("the group's chat, framed in the shell, shows both agents' answers", shown, &host);
    let _ = b.screenshot(page, &shots.join("desktop-group.png"));

    // each agent's own screen (its own desktop): from the group's menu the
    // person picks whose, and its frame lands on the computer's screen port
    // at `?agent=<that agent>`, the image's to read (the platform carries it)
    let fragment_of = |label: &str| agents.iter().find_map(|a| a["fragment"].as_str().filter(|f| label_of(f) == label)).unwrap_or("").to_string();
    let (reader, first) = (fragment_of("reader"), fragment_of(first_label));
    b.click(page, "#agent-heading")?;
    let items = b.eval(page, "[...document.querySelectorAll('#menu button')].map((b) => b.textContent)")?;
    let mine = format!("{}'s screen", me.first_title);
    let offered = items.as_array().is_some_and(|l| l.iter().any(|t| t == "Reader's screen") && l.iter().any(|t| t.as_str() == Some(mine.as_str())));
    s.ok("a group chat's menu offers each of its agents' screens", offered, &items);
    let picked = menu_item(b, page, "Reader's screen")?;
    let landed = picked && screen_landed(s, b, page, &reader);
    let frames = b.eval(page, "[...document.querySelectorAll('#stack iframe')].map((f) => f.dataset.src)")?;
    s.ok("picking one opens that agent's screen: its frame lands on the computer's screen port at ?agent=<that agent>", landed, &frames);

    // search: a message's words, and clicking one opens its chat
    let first_chat = own_named(api, me.session, &format!("{first_label}-chat"))?;
    b.click(page, "#search-agents")?;
    b.eval(page, &fill("#workspace-search", "water garden"))?;
    let hit = format!("#search-results .search-message[data-fragment={}]", js(&first_chat));
    let found = b.until(page, &format!("document.querySelector({})", js(&hit)), wait);
    s.ok("searching a message's words lists it under Messages, in its chat", found, b.eval(page, "document.getElementById('search-results').innerText")?);
    let _ = b.screenshot(page, &shots.join("desktop-search.png"));
    b.click(page, &hit)?;
    let opened = b.until(page, &format!("!document.getElementById('search-dialog').open && document.getElementById('chat-title').textContent === {}", js(me.first_title)), wait);
    s.ok("clicking it opens its chat", opened, b.eval(page, "document.getElementById('chat-title').textContent")?);
    // a direct chat's menu opens its own agent's screen
    b.click(page, "#agent-heading")?;
    let picked = menu_item(b, page, "Its screen")?;
    let landed = picked && screen_landed(s, b, page, &first);
    let frames = b.eval(page, "[...document.querySelectorAll('#stack iframe')].map((f) => f.dataset.src)")?;
    s.ok("a direct chat's menu opens its own agent's screen (Its screen: ?agent=<its agent>)", landed, &frames);

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
    b.click(page, &format!("#chats [data-key={}]", js(&format!("chat:{}", own_named(api, me.session, "reader-chat")?))))?;
    Ok(())
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
    b.click(page, &format!("#chats [data-key={}]", js(&format!("chat:{chat}"))))?;
    let want = json!([{ "agent": reader, "outside": false }, { "agent": first, "outside": true }]);
    let listed = mentions_until(s, b, page, &host, "@", &want, wait);
    let placeholder = b.eval_in_frame(page, &host, "document.getElementById('text')?.placeholder ?? null").unwrap_or(Value::Null);
    s.ok(
        "in a chat of their own, @ lists its agent, then their other agent, marked as one the message adds",
        listed == want && placeholder.as_str().is_some_and(|p| p.ends_with(", or @ someone else")),
        json!({ "listed": listed, "placeholder": placeholder }),
    );
    let _ = b.screenshot(page, &shots.join("desktop-roster.png"));

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
    b.click(page, &format!("#chats [data-key={}]", js(&format!("chat:{chat}"))))?;
    Ok(())
}

/// `fragment ask` as a person (an agent's runs in its computer: the hosted
/// lane's, and our Hermes image's): their direct chat with the agent, its
/// answer waited for and printed; the same `--id` again posts nothing and
/// finds the same answer; `--chat` adds the agent to a chat it is not in
/// first. Invalid: an agent that is none of theirs, an empty question.
fn ask_cli(s: &mut Suite, api: &Api, me: &Person) -> Result<()> {
    let home = s.dir("ask-cli-home");
    let keys = fragment_nip98::Keys::generate();
    api.approve(me.session, &keys)?;
    let config = home.join(if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" }).join("fragment");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("config.json"), json!({ "secret_key": keys.secret_hex() }).to_string())?;
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
