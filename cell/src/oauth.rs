//! Connected clients at the router (docs/api.md, Connected clients; the
//! registry's half is `registry/oauth.rs`, the pure rules
//! `fragment_core::oauth`). The platform is the OAuth 2.1 authorization
//! server MCP clients (Claude, ChatGPT, any other) connect a person
//! through, as the MCP authorization spec describes it:
//!
//!   GET  /.well-known/oauth-authorization-server   its metadata (RFC 8414)
//!   POST /oauth/register     a client registers (RFC 7591); or it names its
//!                            metadata document's URL as its id (CIMD), read
//!                            here at each authorization
//!   GET  /oauth/authorize    the person, signed in through WorkOS as the shell
//!                            is, is asked: "Let X act as you on Y?" (it reads
//!                            only, unless they tick "also change things")
//!   POST /oauth/authorize    their answer (the page's form): a code, sent back
//!   POST /oauth/token        a code (with PKCE) or a refresh token, for tokens
//!   POST /oauth/revoke       the connection a token names ends (RFC 7009)
//!   GET  /api/oauth/connections, DELETE /api/oauth/connections/{id}
//!                            a person's own, as the shell's settings list them
//!
//! A connection acts as its person (decision for Paul: not as an agent of
//! theirs `for` them) on one resource, an MCP server of the platform's:
//! a fragment's own (`<origin>/__mcp`) or the platform's (`/mcp`). Its
//! tokens reach no other. The consent page is one of sharing's (share.rs):
//! unframed, a form token bound to the session, a button that arms after a
//! moment, and it shows where the client sends the person back. These
//! endpoints answer no CORS: a client is a program or a server, never a
//! page.

use std::time::Duration;

use fragment_core::oauth::{self, Client, Error, Refused};
use fragment_core::{egress, npub};
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::*;

use crate::auth::{self, esc};
use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::{self, By, Issued};
use crate::{ask_registry, share};

/// How long reading a client's metadata document may take.
const CLIENT_DOCUMENT_DEADLINE: Duration = Duration::from_secs(5);

/// What a connection reaches: one of the platform's MCP servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resource {
    /// `<platform>/mcp`: the platform's verbs.
    Platform,
    /// `<fragment origin>/__mcp`: that fragment's operations.
    Fragment(String),
}

/// The resource a resource indicator names, with its canonical form (the
/// one a token is bound to), or `None`: no MCP server of this platform's.
pub(crate) fn resource_of(cfg: &Config, raw: &str) -> Option<(Resource, String)> {
    let url = oauth::resource(raw)?;
    let origin = url.origin().ascii_serialization();
    if origin == cfg.platform() && url.path() == "/mcp" {
        return Some((Resource::Platform, url.into()));
    }
    let name = cfg.fragment_of_host(url.host_str()?)?;
    (origin == cfg.outside_origin(&name) && url.path() == "/__mcp").then(|| (Resource::Fragment(name), url.into()))
}

/// An OAuth endpoint's JSON answer: never cached (RFC 6749 5.1).
fn answer(status: u16, v: &impl serde::Serialize) -> CellResult<Response> {
    let mut resp = Response::from_json(v)?.with_status(status);
    resp.headers_mut().set("cache-control", "no-store")?;
    resp.headers_mut().set("pragma", "no-cache")?;
    Ok(resp)
}

/// A refusal, as OAuth's endpoints answer one (RFC 6749 5.2).
fn refusal(r: &Refused) -> CellResult<Response> {
    answer(r.error.status(), &json!({ "error": r.error.as_str(), "error_description": r.why }))
}

fn refused(error: Error, why: impl Into<String>) -> Refused {
    Refused { error, why: why.into() }
}

/// A form's fields (a token or revocation request).
async fn form(req: &mut Request) -> CellResult<Result<Vec<(String, String)>, Refused>> {
    let typed = req.headers().get("content-type")?.is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
    if !typed {
        return Ok(Err(refused(Error::InvalidRequest, "send the request as application/x-www-form-urlencoded")));
    }
    let bytes = crate::read_body(req, oauth::REQUEST_MAX_BYTES).await?;
    Ok(Ok(url::form_urlencoded::parse(&bytes).into_owned().collect()))
}

/// The authorization server's routes on the platform origin (the router sends only these).
pub async fn route(mut req: Request, env: &Env, cfg: &Config, url: &Url, segments: &[&str]) -> CellResult<Response> {
    match (req.method(), segments) {
        (Method::Get, [".well-known", "oauth-authorization-server"]) => answer(200, &oauth::metadata(&cfg.platform())),
        (Method::Post, ["oauth", "register"]) => register(&mut req, env).await,
        (Method::Get, ["oauth", "authorize"]) => {
            let a = match authorization(env, cfg, url).await? {
                Ok(a) => a,
                Err(answered) => return Ok(answered),
            };
            let back_here = format!("/oauth/authorize?{}", url.query().unwrap_or(""));
            match auth::platform_session(&req, env, url).await? {
                Some((token, live)) => consent_page(&token, &live, &a, url),
                None => auth::to_login(&cfg.platform(), &back_here),
            }
        }
        (Method::Post, ["oauth", "authorize"]) => {
            let a = match authorization(env, cfg, url).await? {
                Ok(a) => a,
                Err(answered) => return Ok(answered),
            };
            let (session, _, fields) = match share::poster(&mut req, env, url, &cfg.platform(), &purpose(&a)).await? {
                Ok(posted) => posted,
                Err(page) => return Ok(page),
            };
            if fields.get("answer").map(String::as_str) != Some("allow") {
                return Ok(a.back(cfg, &refused(Error::AccessDenied, "the person said no"))?.with_status(303));
            }
            let grant = calls::GrantCode {
                token: session,
                client_id: a.client_id.clone(),
                client: a.client.name.clone(),
                redirect_uri: a.redirect_uri.clone(),
                challenge: a.asked.challenge.clone(),
                resource: a.canonical.clone(),
                // reading alone unless the person ticked "also change things"
                writes: fields.get("writes").map(String::as_str) == Some("yes"),
            };
            let granted = ask_registry(env, &grant).await?;
            let iss = cfg.platform();
            let mut params = vec![("code", granted.code.as_str())];
            params.extend(a.asked.state.as_deref().map(|s| ("state", s)));
            params.push(("iss", &iss));
            Ok(auth::redirect(&oauth::redirect_with(&a.redirect_uri, &params), &[])?.with_status(303))
        }
        (Method::Post, ["oauth", "token"]) => token(&mut req, env, cfg).await,
        (Method::Post, ["oauth", "revoke"]) => {
            let pairs = match form(&mut req).await? {
                Ok(pairs) => pairs,
                Err(r) => return refusal(&r),
            };
            let one = |k: &str| oauth::one(&pairs, k);
            let (token, client_id) = match (one("token"), one("client_id")) {
                (Ok(Some(t)), Ok(Some(c))) => (t.to_string(), c.to_string()),
                (_, Ok(None)) => return refusal(&refused(Error::InvalidClient, "client_id is required: a client here is public, and names itself")),
                _ => return refusal(&refused(Error::InvalidRequest, "a revocation names its token and client_id, once each")),
            };
            ask_registry(env, &calls::RevokeToken { token, client_id }).await?;
            answer(200, &json!({}))
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// `POST /oauth/register` (RFC 7591): a public client of the code grant,
/// whatever else it asked; the answer says what was registered.
async fn register(req: &mut Request, env: &Env) -> CellResult<Response> {
    let bytes = crate::read_body(req, oauth::REQUEST_MAX_BYTES).await?;
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
        return refusal(&refused(Error::InvalidClientMetadata, "a registration is a JSON object"));
    };
    let client = match oauth::registration(&body) {
        Ok(client) => client,
        Err(r) => return refusal(&r),
    };
    let registered = ask_registry(env, &calls::RegisterClient(client.clone())).await?;
    answer(
        201,
        &json!({
            "client_id": registered.client_id,
            "client_id_issued_at": registered.issued_at_ms / 1000,
            "client_name": client.name,
            "redirect_uris": client.redirect_uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }),
    )
}

/// `POST /oauth/token`: a code or a refresh token, for tokens. A resource
/// it names is read as the authorization's was.
async fn token(req: &mut Request, env: &Env, cfg: &Config) -> CellResult<Response> {
    let pairs = match form(req).await? {
        Ok(pairs) => pairs,
        Err(r) => return refusal(&r),
    };
    let mut grant = match oauth::token_request(&pairs) {
        Ok(grant) => grant,
        Err(r) => return refusal(&r),
    };
    let (oauth::Grant::Code { resource, .. } | oauth::Grant::Refresh { resource, .. }) = &mut grant;
    if let Some(raw) = resource.take() {
        match resource_of(cfg, &raw) {
            Some((_, canonical)) => *resource = Some(canonical),
            None => return refusal(&refused(Error::InvalidTarget, format!("{raw} is no MCP server of this platform's"))),
        }
    }
    match ask_registry(env, &calls::IssueTokens(grant)).await? {
        Issued::Tokens(tokens) => answer(200, &tokens),
        Issued::Refused(r) => refusal(&r),
    }
}

/// The client a request names: its metadata document, read now (CIMD),
/// or its registration. `Err` says why there is none, for the person.
async fn client(env: &Env, cfg: &Config, client_id: &str) -> CellResult<Result<Client, String>> {
    let Some(url) = oauth::client_id_url(client_id, cfg.egress_local) else {
        return match ask_registry(env, &calls::FindClient { client_id: client_id.to_string() }).await {
            Ok(client) => Ok(Ok(client)),
            Err(e) if e.code == ErrorCode::NotFound => Ok(Err(e.message)),
            Err(e) => Err(e),
        };
    };
    // the platform's own fetch of a URL a stranger named: public addresses
    // only (but on a fleet that allows local egress), one bounded read, no
    // redirect followed
    let url = match egress::check(url.as_str(), cfg.egress_local) {
        Ok(url) => url,
        Err(why) => return Ok(Err(format!("{client_id}: {why}"))),
    };
    let headers = Headers::new();
    headers.set("accept", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers).with_redirect(RequestRedirect::Manual);
    let read = crate::cs::fetch_all(Request::new_with_init(url.as_str(), &init)?, CLIENT_DOCUMENT_DEADLINE, oauth::CLIENT_DOCUMENT_MAX_BYTES).await;
    Ok(match read {
        Ok(a) if a.status == 200 && !a.cut => oauth::client_document(client_id, &a.body).map_err(|r| r.why),
        Ok(a) if a.cut => Err(format!("{client_id}: its metadata document is over {} bytes", oauth::CLIENT_DOCUMENT_MAX_BYTES)),
        Ok(a) => Err(format!("{client_id} answered {} for its metadata document", a.status)),
        Err(e) => Err(format!("{client_id}: its metadata document was not read: {}", e.message)),
    })
}

/// An authorization request, checked: its client, where it sends the
/// person back, and what it asks.
struct Authorization {
    client_id: String,
    client: Client,
    redirect_uri: String,
    asked: oauth::Asked,
    resource: Resource,
    /// The resource's canonical form, which its tokens are bound to.
    canonical: String,
}

impl Authorization {
    /// A refusal sent back to the client (RFC 6749 4.1.2.1, with `iss`:
    /// RFC 9207).
    fn back(&self, cfg: &Config, r: &Refused) -> CellResult<Response> {
        sent_back(cfg, &self.redirect_uri, self.asked.state.as_deref(), r)
    }
}

fn sent_back(cfg: &Config, redirect_uri: &str, state: Option<&str>, r: &Refused) -> CellResult<Response> {
    let iss = cfg.platform();
    let mut params = vec![("error", r.error.as_str()), ("error_description", r.why.as_str())];
    params.extend(state.map(|s| ("state", s)));
    params.push(("iss", &iss));
    auth::redirect(&oauth::redirect_with(redirect_uri, &params), &[])
}

/// What a page says when a request names no client it knows, or a
/// redirect URI that is not its: never sent back, since where to send it
/// is what is in doubt.
fn not_sent_back(why: &str) -> CellResult<Response> {
    auth::page(400, "This app can't connect", &format!("<p>{}</p><p>Nothing was shared. Close this page and try connecting again from the app.</p>", esc(why)))
}

/// `/oauth/authorize`'s request, checked in order: its client and redirect
/// URI first (a refusal is a page), then the rest (a refusal goes back).
/// `Err` is that answer.
async fn authorization(env: &Env, cfg: &Config, url: &Url) -> CellResult<Result<Authorization, Response>> {
    if url.query().unwrap_or("").len() > oauth::AUTHORIZE_QUERY_MAX_BYTES {
        return Ok(Err(not_sent_back(&format!("its request is over {} bytes", oauth::AUTHORIZE_QUERY_MAX_BYTES))?));
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let Ok(Some(client_id)) = oauth::one(&pairs, "client_id") else {
        return Ok(Err(not_sent_back("its request names no client_id, once")?));
    };
    let client = match client(env, cfg, client_id).await? {
        Ok(client) => client,
        Err(why) => return Ok(Err(not_sent_back(&why)?)),
    };
    let redirect_uri = match oauth::one(&pairs, "redirect_uri") {
        Ok(Some(asked)) => asked.to_string(),
        Ok(None) if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        _ => return Ok(Err(not_sent_back("its request names no redirect_uri, once")?)),
    };
    if !client.redirect_uris.iter().any(|r| oauth::redirect_matches(r, &redirect_uri)) {
        return Ok(Err(not_sent_back(&format!("{redirect_uri} is not one of {}'s redirect URIs", client.name))?));
    }
    let state = oauth::one(&pairs, "state").ok().flatten().filter(|s| s.len() <= oauth::STATE_MAX_BYTES);
    let asked = match oauth::asked(&pairs) {
        Ok(asked) => asked,
        Err(r) => return Ok(Err(sent_back(cfg, &redirect_uri, state, &r)?)),
    };
    let Some((resource, canonical)) = resource_of(cfg, &asked.resource) else {
        let r = refused(Error::InvalidTarget, format!("{} is no MCP server of this platform's", asked.resource));
        return Ok(Err(sent_back(cfg, &redirect_uri, state, &r)?));
    };
    Ok(Ok(Authorization { client_id: client_id.to_string(), client, redirect_uri, asked, resource, canonical }))
}

/// What a consent page's form token is for: this client, on this resource.
fn purpose(a: &Authorization) -> String {
    format!("connect:{}:{}", a.client_id, a.canonical)
}

/// "Let X act as you on Y?": what the client calls itself, where it sends
/// the person back (the MCP authorization spec's must; a client sent back
/// only to this computer is any program here, so it says so), and what it
/// will reach. Sharing's protections (share.rs `sheet_page`).
fn consent_page(session: &str, live: &calls::LiveSession, a: &Authorization, url: &Url) -> CellResult<Response> {
    let you = match live.identity.username.as_deref() {
        Some(u) => format!("@{}", esc(u)),
        None => format!("<code>{}</code>", esc(&npub::display(&live.identity.id))),
    };
    let to = url::Url::parse(&a.redirect_uri).map_err(|e| CellError::host(format!("a checked redirect URI: {e}")))?;
    let host = esc(to.host_str().unwrap_or_default());
    let local = a.client.redirect_uris.iter().filter_map(|r| url::Url::parse(r).ok()).all(|r| oauth::loopback(&r));
    let (reach, changes) = match &a.resource {
        Resource::Platform => (
            "<b>your fragments</b>: listing them, and reading their status, files, members and events, as you can".to_string(),
            "making, writing, deploying and sharing them, and calling their operations",
        ),
        Resource::Fragment(name) => (
            format!("<b>{}</b> (<code>{}</code>): calling the operations it describes that read it, as you can", esc(share::label(name)), esc(name)),
            "calling the ones that change it (its mutations and jobs)",
        ),
    };
    let warning = if local {
        "<p class=\"flash error\">It sends you back to a program on this computer. Only allow it if you just asked one here to connect.</p>"
    } else {
        ""
    };
    let body = format!(
        "<p><b>{c}</b> wants to act as you, {you}, on {reach}. What it does there names it.</p>\
         <p class=\"hint\">It calls itself {c}, and sends you back to <b>{host}</b>.</p>{warning}\
         <form method=\"post\" action=\"/oauth/authorize?{q}\"><input type=\"hidden\" name=\"form\" value=\"{f}\">\
         <p><label><input type=\"checkbox\" name=\"writes\" value=\"yes\"> Also let it change things: {changes}. Without this, it only reads.</label></p>\
         <footer><button class=\"quiet\" name=\"answer\" value=\"deny\" data-arm disabled>Don't allow</button>\
         <button name=\"answer\" value=\"allow\" data-arm disabled>Allow</button></footer></form>\
         <p class=\"hint\">You can end it any time in your settings, under Connected clients.</p>",
        c = esc(&a.client.name),
        q = esc(url.query().unwrap_or("")),
        f = esc(&fragment_core::form::issue(session, &purpose(a), crate::js::now_ms())),
    );
    share::sheet_page(200, &format!("Connect {}?", a.client.name), &body, false)
}

/// `GET /api/oauth/connections` and `DELETE /api/oauth/connections/{id}`:
/// a person's connected clients, as the shell's settings list them, signed
/// or from the shell; the registry resolves who asks in the same turn.
pub async fn connections(req: &Request, env: &Env, url: &Url, rest: &[&str]) -> CellResult<Response> {
    if crate::acting_for(url)?.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    let by = match crate::caller(env, req, url, fragment_nip98::Payload::Read(&[]))? {
        crate::Caller::Key(key) => By::Key(key),
        crate::Caller::Session(token) => By::Session(token),
    };
    match (req.method(), rest) {
        (Method::Get, []) => crate::json_answer(&ask_registry(env, &calls::ListConnections { by }).await?),
        (Method::Delete, [id]) if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) => {
            ask_registry(env, &calls::Disconnect { by, id: id.to_string() }).await?;
            crate::json_answer(&json!({ "ok": true, "ended": id }))
        }
        (Method::Delete, [id]) => Err(CellError::new(ErrorCode::NotFound, format!("no connection {id:?} of yours"))),
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}
