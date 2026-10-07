//! MCP clients (docs/api.md, Connected clients), driven over HTTP as a
//! client and its person's browser drive them: the platform's
//! authorization server's discovery, a client's registration (or its
//! metadata document), the person's sign-in through WorkOS and their yes,
//! the code exchanged with PKCE, the refresh token renewed and replaced,
//! and a connection ended from the person's list or by its client.

use std::sync::Arc;

use anyhow::{Context, Result};
use fragment_core::{form, oauth};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{url_enc, Api, Call, Reply};
use crate::Suite;

/// Where the e2e's clients say they send the person back: a port on this
/// computer nothing listens on (its answers are read from `Location`).
const REDIRECT: &str = "http://127.0.0.1:9/callback";

/// A client, as one connects a person: its id, where it sends them back,
/// and the PKCE verifier of the authorization under way.
struct Client {
    id: String,
    redirect: String,
    verifier: String,
}

impl Client {
    /// A client that registers itself (RFC 7591), as a client without a
    /// metadata document does.
    fn register(api: &Api, name: &str) -> Result<Client> {
        let r = api.unsigned("POST", "/oauth/register", Some(&json!({ "client_name": name, "redirect_uris": [REDIRECT] })))?;
        anyhow::ensure!(r.status == 201, "registering {name}: {r}");
        Ok(Client::named(r.body["client_id"].as_str().context("a client id")?))
    }

    fn named(id: &str) -> Client {
        Client { id: id.to_string(), redirect: REDIRECT.to_string(), verifier: fresh_verifier() }
    }

    /// The authorization request's path, for `resource`, with `state`.
    fn authorize(&self, resource: &str, state: &str) -> String {
        format!(
            "/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&resource={}&state={state}",
            url_enc(&self.id),
            url_enc(&self.redirect),
            oauth::challenge_of(&self.verifier),
            url_enc(resource)
        )
    }

    /// The person signed in as `session` says yes to it acting on
    /// `resource`, with a form token as their page held one: the code
    /// sent back.
    fn code(&self, api: &Api, session: &str, resource: &str) -> Result<String> {
        let r = answer(api, session, &self.authorize(resource, "s"), &purpose(&self.id, resource), "allow", &api.base)?;
        anyhow::ensure!(r.status == 303, "the yes: {r}");
        sent_back(&r, "code").context("a code sent back")
    }

    /// The code exchanged for tokens (PKCE: this client's verifier).
    fn exchange(&self, api: &Api, code: &str) -> Result<Reply> {
        token(api, &[("grant_type", "authorization_code"), ("code", code), ("client_id", &self.id), ("redirect_uri", &self.redirect), ("code_verifier", &self.verifier)])
    }

    /// A connection made: the person's yes, and the code exchanged.
    fn connect(&self, api: &Api, session: &str, resource: &str) -> Result<Value> {
        let r = self.exchange(api, &self.code(api, session, resource)?)?;
        anyhow::ensure!(r.status == 200 && r.body["access_token"].is_string(), "the exchange: {r}");
        Ok(r.body)
    }

    fn refresh(&self, api: &Api, refresh: &str) -> Result<Reply> {
        token(api, &[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", &self.id)])
    }
}

/// A PKCE verifier: 64 random hex characters.
fn fresh_verifier() -> String {
    Keys::generate().pubkey_hex().to_string()
}

/// The consent page's form purpose (cell/src/oauth.rs `purpose`).
fn purpose(client_id: &str, resource: &str) -> String {
    format!("connect:{client_id}:{resource}")
}

/// The person's answer to the consent page, sent from `origin` with a
/// form token their page held, made long enough ago that its buttons armed.
fn answer(api: &Api, session: &str, path: &str, purpose: &str, answer: &str, origin: &str) -> Result<Reply> {
    let made = crate::api::now_ms() - form::DELAY_MS - 50;
    let token = form::issue(session, purpose, made);
    api.call(Call {
        method: "POST",
        url: format!("{}{path}", api.base),
        body: Some(format!("form={}&answer={answer}", url_enc(&token)).into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("origin", origin.to_string())],
        ..Call::default()
    })
}

/// A parameter of the answer a client was sent back with.
fn sent_back(r: &Reply, key: &str) -> Option<String> {
    let to = reqwest::Url::parse(&r.header("location")).ok()?;
    to.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

fn token(api: &Api, form: &[(&str, &str)]) -> Result<Reply> {
    let body = form.iter().map(|(k, v)| format!("{k}={}", url_enc(v))).collect::<Vec<_>>().join("&");
    api.call(Call {
        method: "POST",
        url: format!("{}/oauth/token", api.base),
        body: Some(body.into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        ..Call::default()
    })
}

fn with_session(api: &Api, path: &str, session: &str) -> Result<Reply> {
    api.call(Call { method: "GET", url: format!("{}{path}", api.base), cookie: Some(format!("fragment_session={session}")), ..Call::default() })
}

/// A person signed in through WorkOS (locally, the fake), with a key of
/// theirs approved: their platform session, the key, and their email.
fn person(api: &Api) -> Result<(String, Keys, String)> {
    let keys = Keys::generate();
    let email = format!("mcp-{}@e2e.test", &keys.pubkey_hex()[..12]);
    let session = api.sign_in(&email)?;
    let me = api.approve(&session, &keys)?;
    anyhow::ensure!(me.status == 200, "approving a key: {me}");
    Ok((session, keys, email))
}

pub fn mcp(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("mcp", &[]) {
        return Ok(());
    }
    let (session, keys, email) = person(api)?;
    let name = s.named(api, &keys, "mcp")?;
    let r = api.create_with(&keys, json!({ "name": name, "template": "inbox", "visibility": "members" }))?;
    anyhow::ensure!(r.status == 200, "create {name}: {r}");
    s.owned(&r.body, &keys);
    let resource = api.site_url(&name, "__mcp");
    authorization_server(s, api, &session, &keys, &email, &resource)?;
    fragment_server(s, api, &session, &keys, &name)?;
    limits(s, api, &session, &keys, &resource)?;
    Ok(())
}

/// The platform as an MCP client's OAuth 2.1 authorization server.
fn authorization_server(s: &mut Suite, api: &Api, session: &str, keys: &Keys, email: &str, resource: &str) -> Result<()> {
    let r = api.unsigned("GET", "/.well-known/oauth-authorization-server", None)?;
    let m = &r.body;
    s.ok(
        "its metadata names the platform as the issuer, its endpoints there, PKCE's S256, public clients, and metadata documents",
        r.status == 200
            && m["issuer"] == api.base.as_str()
            && m["token_endpoint"] == format!("{}/oauth/token", api.base)
            && m["registration_endpoint"] == format!("{}/oauth/register", api.base)
            && m["code_challenge_methods_supported"] == json!(["S256"])
            && m["token_endpoint_auth_methods_supported"] == json!(["none"])
            && m["client_id_metadata_document_supported"] == true
            && m["authorization_response_iss_parameter_supported"] == true,
        &r,
    );

    // registration (RFC 7591)
    let r = api.unsigned("POST", "/oauth/register", Some(&json!({ "client_name": "E2E Claude", "redirect_uris": [REDIRECT], "token_endpoint_auth_method": "client_secret_post" })))?;
    s.ok(
        "a client registers as a public client of the code grant, whatever it asked",
        r.status == 201 && r.body["client_id"].is_string() && r.body["token_endpoint_auth_method"] == "none" && r.body["redirect_uris"] == json!([REDIRECT]),
        &r,
    );
    let client = Client::named(r.body["client_id"].as_str().unwrap_or_default());
    let r = api.unsigned("POST", "/oauth/register", Some(&json!({ "client_name": "x", "redirect_uris": ["http://evil.example/cb"] })))?;
    s.ok("a redirect URI that is neither https nor this computer's is refused", r.status == 400 && r.body["error"] == "invalid_redirect_uri", &r);
    let r = api.unsigned("POST", "/oauth/register", Some(&json!({ "client_name": "x" })))?;
    s.ok("a registration without redirect URIs is refused", r.status == 400 && r.body["error"] == "invalid_redirect_uri", &r);

    // the authorization request: its client and redirect URI first, never sent back
    let other = Client::named("0000000000000000000000000000000f");
    let r = with_session(api, &other.authorize(resource, "s"), session)?;
    s.ok("a client no one registered is a page, never a redirect", r.status == 400 && r.header("location").is_empty() && r.text.contains("can't connect"), &r);
    let astray = Client { redirect: "http://127.0.0.1:9/elsewhere".into(), ..Client::named(&client.id) };
    let r = with_session(api, &astray.authorize(resource, "s"), session)?;
    s.ok("a redirect URI it did not register is a page, never a redirect", r.status == 400 && r.header("location").is_empty(), &r);
    let loopback = Client { redirect: "http://127.0.0.1:61001/callback".into(), ..Client::named(&client.id) };
    let r = with_session(api, &loopback.authorize(resource, "s"), session)?;
    s.ok("on this computer, its redirect URI's port is the client's to choose (RFC 8252)", r.status == 200, &r);
    let path = client.authorize(resource, "st8").replace("&code_challenge_method=S256", "");
    let r = with_session(api, &path, session)?;
    s.ok(
        "without PKCE's S256 it is refused, back to the client with its state and the issuer",
        r.status == 302 && sent_back(&r, "error").as_deref() == Some("invalid_request") && sent_back(&r, "state").as_deref() == Some("st8") && sent_back(&r, "iss").as_deref() == Some(api.base.as_str()),
        &r,
    );
    let r = with_session(api, &client.authorize("https://example.com/mcp", "s"), session)?;
    s.ok("a resource that is no MCP server of the platform's is refused (invalid_target)", r.status == 302 && sent_back(&r, "error").as_deref() == Some("invalid_target"), &r);

    // signed out: through sign-in (WorkOS), and back to the question
    let path = client.authorize(resource, "s");
    let r = api.unsigned("GET", &path, None)?;
    let login = r.header("location");
    s.ok("signed out, it sends the person to sign in first", r.status == 302 && login.contains("/auth/login?return="), &r);
    if api.signs_in_by_levers() {
        s.skip("signing in through WorkOS comes back to the question", "a hosted run's people sign in through the levers, not WorkOS");
    } else {
        let start = api.call(Call { method: "GET", url: login.clone(), ..Call::default() })?;
        let bound = start.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).unwrap_or_default();
        // the person types their email at WorkOS
        let workos = api.external(&format!("{}&login_hint={}", start.header("location"), url_enc(email)))?;
        let done = api.call(Call { method: "GET", url: workos.header("location"), cookie: Some(bound), ..Call::default() })?;
        s.ok("signing in through WorkOS (the fake) comes back to the question", done.status == 302 && done.header("location") == format!("{}{path}", api.base), &done);
    }

    // the question, and the answers
    let r = with_session(api, &path, session)?;
    s.ok(
        "signed in, the person is asked, on a page no other may frame: the client's name, where it sends them back, and what it reaches",
        r.status == 200
            && r.text.contains("E2E Claude")
            && r.text.contains("127.0.0.1")
            && r.text.contains("a program on this computer")
            && super::signin::unframed(&r),
        &r,
    );
    let r = answer(api, session, &path, &purpose(&client.id, resource), "allow", "http://page--mallory.fragment.localhost")?;
    s.ok("a yes from another origin is refused", r.status == 403 && r.header("location").is_empty(), &r);
    let r = answer(api, session, &path, &purpose(&client.id, "https://example.com/mcp"), "allow", &api.base)?;
    s.ok("a yes with another form's token is refused", r.status == 403 && r.header("location").is_empty(), &r);
    let r = answer(api, session, &path, &purpose(&client.id, resource), "deny", &api.base)?;
    s.ok("a no goes back as access_denied", r.status == 303 && sent_back(&r, "error").as_deref() == Some("access_denied"), &r);
    let r = answer(api, session, &path, &purpose(&client.id, resource), "allow", &api.base)?;
    let code = sent_back(&r, "code").unwrap_or_default();
    s.ok(
        "a yes sends a code back, with the client's state and the issuer (RFC 9207)",
        r.status == 303 && r.header("location").starts_with(REDIRECT) && !code.is_empty() && sent_back(&r, "state").as_deref() == Some("s") && sent_back(&r, "iss").as_deref() == Some(api.base.as_str()),
        &r,
    );

    // the code, with PKCE
    let wrong = Client { verifier: fresh_verifier(), ..Client::named(&client.id) };
    let r = wrong.exchange(api, &code)?;
    s.ok("a code with another verifier is refused (invalid_grant)", r.status == 400 && r.body["error"] == "invalid_grant", &r);
    let r = client.exchange(api, &code)?;
    s.ok("and spent: its verifier can no longer have it", r.status == 400 && r.body["error"] == "invalid_grant", &r);
    let code = client.code(api, session, resource)?;
    let r = token(api, &[("grant_type", "authorization_code"), ("code", &code), ("client_id", &other.id), ("redirect_uri", REDIRECT), ("code_verifier", &client.verifier)])?;
    s.ok("a code shown by another client is refused", r.status == 400 && r.body["error"] == "invalid_grant", &r);
    let code = client.code(api, session, resource)?;
    let r = client.exchange(api, &code)?;
    let tokens = r.body.clone();
    s.ok(
        "its code and verifier are exchanged for a bearer access token and a refresh token, never cached",
        r.status == 200 && tokens["token_type"] == "Bearer" && tokens["expires_in"] == oauth::ACCESS_TTL_MS / 1000 && tokens["refresh_token"].is_string() && r.header("cache-control") == "no-store",
        &r,
    );
    let r = client.exchange(api, &code)?;
    s.ok("a code is exchanged once", r.status == 400 && r.body["error"] == "invalid_grant", &r);

    // refresh: renewed, and replaced
    let refresh = tokens["refresh_token"].as_str().unwrap_or_default().to_string();
    let r = client.refresh(api, &refresh)?;
    let renewed = r.body.clone();
    s.ok(
        "a refresh token renews the access token and is replaced",
        r.status == 200 && renewed["access_token"] != tokens["access_token"] && renewed["refresh_token"] != tokens["refresh_token"],
        &r,
    );
    let r = client.refresh(api, &refresh)?;
    s.ok("a refresh token is used once", r.status == 400 && r.body["error"] == "invalid_grant", &r);
    let refresh = renewed["refresh_token"].as_str().unwrap_or_default().to_string();
    let r = token(api, &[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", &client.id), ("resource", &format!("{}/mcp", api.base))])?;
    s.ok("a refresh for another resource is refused (invalid_target)", r.status == 400 && r.body["error"] == "invalid_target", &r);
    let r = client.refresh(api, &refresh)?;
    let refresh = r.body["refresh_token"].as_str().unwrap_or_default().to_string();
    s.ok("and changes nothing", r.status == 200, &r);

    // the person's list: theirs alone, and an end to each
    let r = api.signed(keys, "GET", "/api/oauth/connections", None)?;
    let listed = r.body["connections"].as_array().cloned().unwrap_or_default();
    let mine = listed.iter().find(|c| c["clientId"] == client.id.as_str()).cloned().unwrap_or(Value::Null);
    s.ok(
        "the person's list names the client and the resource it acts on",
        r.status == 200 && mine["client"] == "E2E Claude" && mine["resource"] == resource && mine["expiresAt"].as_i64() > mine["createdAt"].as_i64(),
        &r,
    );
    let (_, stranger, _) = person(api)?;
    let id = mine["id"].as_str().unwrap_or_default().to_string();
    let r = api.signed(&stranger, "DELETE", &format!("/api/oauth/connections/{id}"), None)?;
    s.ok("no one else ends it", r.status == 404, &r);
    let r = api.signed(keys, "DELETE", &format!("/api/oauth/connections/{id}"), None)?;
    s.ok("its person ends it", r.status == 200 && r.body["ok"] == true, &r);
    let r = client.refresh(api, &refresh)?;
    s.ok("an ended connection's refresh token is refused", r.status == 400 && r.body["error"] == "invalid_grant", &r);

    // its client ends it (RFC 7009)
    let tokens = client.connect(api, session, resource)?;
    let refresh = tokens["refresh_token"].as_str().unwrap_or_default();
    let revoke = |token: &str, client_id: &str| -> Result<Reply> {
        let body = format!("token={}&client_id={}", url_enc(token), url_enc(client_id));
        api.call(Call { method: "POST", url: format!("{}/oauth/revoke", api.base), body: Some(body.into_bytes()), content_type: Some("application/x-www-form-urlencoded"), ..Call::default() })
    };
    let r = revoke(refresh, &other.id)?;
    s.ok("another client's revocation is answered and changes nothing", r.status == 200 && client.refresh(api, refresh)?.status == 200, &r);
    let tokens = client.connect(api, session, resource)?;
    let r = revoke(tokens["access_token"].as_str().unwrap_or_default(), &client.id)?;
    let after = client.refresh(api, tokens["refresh_token"].as_str().unwrap_or_default())?;
    s.ok("its client's revocation of either token ends the connection", r.status == 200 && after.body["error"] == "invalid_grant", &after);

    settings(s, api, session, keys, &client, resource)?;
    metadata_document(s, api, session, resource)?;
    Ok(())
}

/// The person's settings, in a browser: their connected clients, each
/// with an End.
fn settings(s: &mut Suite, api: &Api, session: &str, keys: &Keys, client: &Client, resource: &str) -> Result<()> {
    if !s.runs("mcp", &[crate::Need::Chrome]) {
        s.skip("settings list a connected client, and End ends it", "it needs Chrome, and none is installed here (chrome)");
        return Ok(());
    }
    let tokens = client.connect(api, session, resource)?;
    let listed = api.signed(keys, "GET", "/api/oauth/connections", None)?;
    let newest = listed.body["connections"][0]["id"].as_str().unwrap_or_default().to_string();
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the mcp section (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = std::time::Duration::from_secs(20);
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", session)?;
    let page = chrome.open(&format!("{}/settings", api.base))?;
    let row = format!("document.querySelector('#settings-clients [data-connection=\"{newest}\"]')");
    let listed = chrome.until(&page, &format!("!!{row} && {row}.innerText.includes('E2E Claude')"), wait);
    s.ok("settings list the connected client by its name", listed, chrome.eval(&page, "document.getElementById('settings-clients')?.innerText ?? ''")?);
    chrome.eval(&page, &format!("{row}.querySelector('button').click()"))?;
    let gone = chrome.until(&page, &format!("!{row} && !!document.getElementById('settings-clients')"), wait);
    let after = client.refresh(api, tokens["refresh_token"].as_str().unwrap_or_default())?;
    s.ok("End ends it: its refresh token is refused", gone && after.body["error"] == "invalid_grant", &after);
    Ok(())
}

/// A client named by its metadata document's URL (CIMD), read as each
/// authorization begins: here, a local server's (a preview reads only
/// public ones).
fn metadata_document(s: &mut Suite, api: &Api, session: &str, resource: &str) -> Result<()> {
    if s.hosted() {
        s.skip("a client named by its metadata document's URL connects", "a preview reads only a public https document, and the run serves one locally");
        return Ok(());
    }
    let doc = Arc::new(std::sync::Mutex::new(Value::Null));
    let served = Arc::clone(&doc);
    let server = fragment_fakes::http::Server::start(0, Arc::new(move |_| fragment_fakes::http::Response::json(200, &served.lock().expect("the document").clone())))?;
    let id = format!("{}/client.json", server.url);
    *doc.lock().expect("the document") = json!({ "client_id": id, "client_name": "E2E Code", "redirect_uris": ["http://localhost/callback", "http://127.0.0.1/callback"] });
    let client = Client { redirect: "http://127.0.0.1:61002/callback".into(), ..Client::named(&id) };
    let r = with_session(api, &client.authorize(resource, "s"), session)?;
    s.ok("a client named by its metadata document's URL is asked for by the document's name", r.status == 200 && r.text.contains("E2E Code"), &r);
    let tokens = client.connect(api, session, resource);
    s.ok("and connects as any client does", tokens.is_ok(), format!("{tokens:?}"));
    *doc.lock().expect("the document") = json!({ "client_id": "https://elsewhere.example/client.json", "client_name": "E2E Code", "redirect_uris": ["http://127.0.0.1/callback"] });
    let r = with_session(api, &client.authorize(resource, "s"), session)?;
    s.ok("a document that names another URL is refused, on a page", r.status == 400 && r.header("location").is_empty(), &r);
    Ok(())
}

/// A JSON-RPC request to a fragment's `__mcp`, with `token` (when one),
/// as a legacy client sends it (the version in a header after
/// `initialize`), or a modern one (`modern`: the version in `_meta`, the
/// method and the tool's name mirrored in headers).
fn rpc(api: &Api, name: &str, token: Option<&str>, method: &str, params: Value, modern: bool) -> Result<Reply> {
    let mut extra = vec![];
    if let Some(token) = token {
        extra.push(("authorization", format!("Bearer {token}")));
    }
    let mut params = params;
    if modern {
        params["_meta"] = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": { "name": "e2e", "version": "1" } });
        extra.push(("mcp-protocol-version", "2026-07-28".into()));
        extra.push(("mcp-method", method.to_string()));
        if let Some(tool) = params["name"].as_str() {
            extra.push(("mcp-name", tool.to_string()));
        }
    } else {
        extra.push(("mcp-protocol-version", "2025-06-18".into()));
    }
    let id = if method.starts_with("notifications/") { Value::Null } else { json!(1) };
    let mut body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    if !id.is_null() {
        body["id"] = id;
    }
    api.call(Call {
        method: "POST",
        url: api.site_url(name, "__mcp"),
        body: Some(body.to_string().into_bytes()),
        content_type: Some("application/json"),
        extra: extra.iter().map(|(k, v)| (*k, v.clone())).collect(),
        ..Call::default()
    })
}

/// A tool call's result, through `rpc`.
fn call(api: &Api, name: &str, token: &str, tool: &str, arguments: Value) -> Result<Value> {
    let r = rpc(api, name, Some(token), "tools/call", json!({ "name": tool, "arguments": arguments }), true)?;
    anyhow::ensure!(r.status == 200, "tools/call {tool}: {r}");
    Ok(r.body)
}

/// A fragment's operations as an MCP server at its own `__mcp`.
fn fragment_server(s: &mut Suite, api: &Api, session: &str, keys: &Keys, name: &str) -> Result<()> {
    let resource = api.site_url(name, "__mcp");
    let metadata_url = api.site_url(name, ".well-known/oauth-protected-resource/__mcp");
    let r = rpc(api, name, None, "tools/list", json!({}), false)?;
    s.ok(
        "without a token, a fragment's __mcp is 401, naming its protected resource's metadata",
        r.status == 401 && r.header("www-authenticate") == format!("Bearer resource_metadata=\"{metadata_url}\""),
        format!("{r} {}", r.header("www-authenticate")),
    );
    let r = api.call(Call { method: "GET", url: metadata_url, ..Call::default() })?;
    s.ok(
        "its metadata (RFC 9728) names the resource and the platform as its authorization server",
        r.status == 200 && r.body["resource"] == resource.as_str() && r.body["authorization_servers"] == json!([api.base]),
        &r,
    );
    let client = Client::register(api, "E2E Claude")?;
    let tokens = client.connect(api, session, &resource)?;
    let token = tokens["access_token"].as_str().unwrap_or_default().to_string();
    let r = api.call(Call { method: "GET", url: resource.clone(), extra: vec![("authorization", format!("Bearer {token}"))], ..Call::default() })?;
    s.ok("a GET is 405: it offers no stream", r.status == 405, &r);
    let r = api.call(Call {
        method: "POST",
        url: resource.clone(),
        body: Some(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_vec()),
        content_type: Some("application/json"),
        extra: vec![("authorization", format!("Bearer {token}")), ("origin", api.site_origin(name))],
        ..Call::default()
    })?;
    s.ok("a page's call (an Origin) is 403, its own page's too", r.status == 403, &r);

    // the legacy era: initialize, then the version in a header
    let r = rpc(api, name, Some(&token), "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "e2e", "version": "1" } }), false)?;
    s.ok(
        "a legacy client initializes: its version, the tools capability, and the fragment named",
        r.status == 200 && r.body["result"]["protocolVersion"] == "2025-06-18" && r.body["result"]["capabilities"]["tools"].is_object() && r.body["result"]["serverInfo"]["title"] == name,
        &r,
    );
    let r = rpc(api, name, Some(&token), "notifications/initialized", json!({}), false)?;
    s.ok("its notification is taken (202)", r.status == 202, &r);
    let r = rpc(api, name, Some(&token), "tools/list", json!({}), false)?;
    let tools = r.body["result"]["tools"].as_array().cloned().unwrap_or_default();
    let named = |n: &str| tools.iter().find(|t| t["name"] == n).cloned().unwrap_or(Value::Null);
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    s.ok(
        "tools/list is the manifest's operations, by name: a query read-only, a mutation idempotent by the id it requires, each described",
        names == ["add", "ingest", "list"]
            && named("list")["annotations"]["readOnlyHint"] == true
            && named("add")["annotations"]["idempotentHint"] == true
            && named("add")["inputSchema"]["required"] == json!(["id", "input"])
            && named("add")["inputSchema"]["properties"]["input"]["required"] == json!(["text", "source"])
            && named("add")["description"].as_str().is_some_and(|d| d.starts_with("Adds an item")),
        &r,
    );

    // the modern era: no handshake, its version and headers on each request
    let r = rpc(api, name, Some(&token), "server/discover", json!({}), true)?;
    s.ok(
        "a modern client discovers it: its versions, complete",
        r.status == 200 && r.body["result"]["resultType"] == "complete" && r.body["result"]["supportedVersions"].as_array().is_some_and(|v| v.contains(&json!("2026-07-28"))),
        &r,
    );
    let r = api.call(Call {
        method: "POST",
        url: resource.clone(),
        body: Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } }).to_string().into_bytes()),
        content_type: Some("application/json"),
        extra: vec![("authorization", format!("Bearer {token}")), ("mcp-protocol-version", "2026-07-28".into())],
        ..Call::default()
    })?;
    s.ok("a modern request without its Mcp-Method header is 400 (header mismatch)", r.status == 400 && r.body["error"]["code"] == -32020, &r);
    let r = call(api, name, &token, "add", json!({ "id": "m1", "input": { "text": "from a client", "source": "e2e" } }))?;
    s.ok(
        "tools/call on a mutation runs it through __op's path: its result, not replayed",
        r["result"]["isError"] == false && r["result"]["structuredContent"]["replayed"] == false && r["result"]["structuredContent"]["result"]["id"].is_i64() && r["result"]["resultType"] == "complete",
        &r,
    );
    let again = call(api, name, &token, "add", json!({ "id": "m1", "input": { "text": "from a client", "source": "e2e" } }))?;
    s.ok("the same id again is a replay: the first result, nothing run", again["result"]["structuredContent"]["replayed"] == true && again["result"]["structuredContent"]["result"] == r["result"]["structuredContent"]["result"], &again);
    let r = call(api, name, &token, "add", json!({ "id": "m1", "input": { "text": "something else", "source": "e2e" } }))?;
    s.ok("another input under it is the call's error, for the model to read", r["result"]["isError"] == true && r["result"]["content"][0]["text"].as_str().is_some_and(|t| t.starts_with("conflicting_body")), &r);
    let r = call(api, name, &token, "add", json!({ "id": "m2", "input": { "text": 3, "source": "e2e" } }))?;
    s.ok("an input its schema refuses is the call's error", r["result"]["isError"] == true && r["result"]["content"][0]["text"].as_str().is_some_and(|t| t.starts_with("invalid_request")), &r);
    let r = call(api, name, &token, "list", json!({ "input": {} }))?;
    let items = r["result"]["structuredContent"]["result"]["items"].as_array().cloned().unwrap_or_default();
    s.ok("tools/call on a query reads what the mutation wrote", items.len() == 1 && items[0]["text"] == "from a client", &r);
    let r = call(api, name, &token, "nope", json!({}))?;
    s.ok("an operation it lacks is an unknown tool (-32602)", r["error"]["code"] == -32602, &r);
    let r = call(api, name, &token, "add", json!({ "text": "x" }))?;
    s.ok("arguments that are not {id, input} are invalid params (-32602)", r["error"]["code"] == -32602, &r);
    let events = api.signed(keys, "GET", &format!("/api/f/{name}/events?tail=50"), None)?;
    let me = api.identity(keys)?;
    let called: Vec<&Value> = events.body["events"].as_array().map(|e| e.iter().filter(|e| e["kind"] == "client.called").collect()).unwrap_or_default();
    s.ok(
        "events say which client acted, once for the mutation (its replay says nothing)",
        called.len() == 1 && called[0]["data"]["client"] == "E2E Claude" && called[0]["data"]["op"] == "add" && called[0]["data"]["principal"] == me.as_str(),
        &events,
    );

    // a viewer's connection: the tools they may call, and a refusal of the rest
    let (viewer_session, viewer, _) = person(api)?;
    let r = api.signed(keys, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "adding a viewer: {r}");
    let theirs = client.connect(api, &viewer_session, &resource)?;
    let viewer_token = theirs["access_token"].as_str().unwrap_or_default();
    let r = rpc(api, name, Some(viewer_token), "tools/list", json!({}), true)?;
    let names: Vec<&str> = r.body["result"]["tools"].as_array().map(|t| t.iter().filter_map(|t| t["name"].as_str()).collect()).unwrap_or_default();
    s.ok("a viewer's client lists only what a viewer may call", names == ["list"], &r);
    let r = call(api, name, viewer_token, "add", json!({ "id": "v1", "input": { "text": "x", "source": "e2e" } }))?;
    s.ok("and a mutation needing an editor is the call's refusal (forbidden)", r["result"]["isError"] == true && r["result"]["content"][0]["text"].as_str().is_some_and(|t| t.starts_with("forbidden")), &r);

    // a token is its fragment's alone, and an ended connection's is refused
    let other = s.named(api, keys, "mcp-b")?;
    let r = api.create_with(keys, json!({ "name": other, "template": "inbox" }))?;
    anyhow::ensure!(r.status == 200, "create {other}: {r}");
    s.owned(&r.body, keys);
    let r = rpc(api, &other, Some(&token), "tools/list", json!({}), true)?;
    s.ok("a token for one fragment is refused at another (401, invalid_token)", r.status == 401 && r.header("www-authenticate").contains("invalid_token"), &r);
    let listed = api.signed(keys, "GET", "/api/oauth/connections", None)?;
    let id = listed.body["connections"][0]["id"].as_str().unwrap_or_default().to_string();
    api.signed(keys, "DELETE", &format!("/api/oauth/connections/{id}"), None)?;
    let r = rpc(api, name, Some(&token), "tools/list", json!({}), true)?;
    s.ok("an ended connection's access token is refused at once (401)", r.status == 401, &r);
    Ok(())
}

/// A person's codes and connections are bounded, and a code expires.
fn limits(s: &mut Suite, api: &Api, session: &str, keys: &Keys, resource: &str) -> Result<()> {
    let client = Client::register(api, "E2E Many")?;
    let first = client.code(api, session, resource)?;
    for _ in 0..oauth::CODES_PER_PERSON_MAX {
        client.code(api, session, resource)?;
    }
    let r = client.exchange(api, &first)?;
    s.ok(&format!("a person holds their newest {} codes: the oldest is refused", oauth::CODES_PER_PERSON_MAX), r.status == 400 && r.body["error"] == "invalid_grant", &r);
    // from none: the first made is the oldest
    let listed = api.signed(keys, "GET", "/api/oauth/connections", None)?;
    for c in listed.body["connections"].as_array().cloned().unwrap_or_default() {
        api.signed(keys, "DELETE", &format!("/api/oauth/connections/{}", c["id"].as_str().unwrap_or_default()), None)?;
    }
    let firsts = client.connect(api, session, resource)?;
    for _ in 0..oauth::CONNECTIONS_PER_PERSON_MAX {
        client.connect(api, session, resource)?;
    }
    let r = api.signed(keys, "GET", "/api/oauth/connections", None)?;
    let held = r.body["connections"].as_array().map_or(0, Vec::len) as u64;
    let gone = client.refresh(api, firsts["refresh_token"].as_str().unwrap_or_default())?;
    s.ok(
        &format!("a person keeps their newest {} connections: the oldest ends", oauth::CONNECTIONS_PER_PERSON_MAX),
        held == oauth::CONNECTIONS_PER_PERSON_MAX && gone.body["error"] == "invalid_grant",
        format!("{held} held; {gone}"),
    );
    if !s.runs("mcp", &[crate::Need::Deployment]) {
        s.skip("a code past its minutes is refused", "it needs the registry's levers, which a shared preview never lends a run (deployment)");
        return Ok(());
    }
    let code = client.code(api, session, resource)?;
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "signins": "expire" })))?;
    anyhow::ensure!(r.status == 200, "expiring codes: {r}");
    let r = client.exchange(api, &code)?;
    s.ok("a code past its minutes is refused", r.status == 400 && r.body["error"] == "invalid_grant", &r);
    Ok(())
}
