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
use crate::Suite;

/// A request as the shell's page sends it: the session cookie, the
/// shell's header, `Sec-Fetch-Site: same-origin`, and its Origin.
fn shell(api: &Api, session: &str, method: &'static str, path: &str, body: Option<&Value>, extra: &[(&'static str, String)]) -> Result<Reply> {
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
    s.ok("and its script, from the release", script.status == 200 && script.text.contains("SOUL.md"), &script.status);
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
    s.ok("which its page reads", soul.status == 200 && soul.text.contains("tomatoes"), &soul.status);

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
    Ok(())
}

/// A fragment's page, as the shell's person sees it once signed in there.
fn shell_site(_s: &Suite, api: &Api, session: &str, name: &str, path: &str) -> Result<Reply> {
    let token = super::signin::site_cookie(api, session, name)?;
    api.call(Call { method: "GET", url: api.site_url(name, path), cookie: Some(format!("fragment_site={token}")), ..Call::default() })
}
