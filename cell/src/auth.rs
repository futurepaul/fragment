//! Sign-in at the router (phase 4 slice B; the registry's half is
//! `registry/signin.rs`). WorkOS AuthKit authenticates people on the
//! platform origin; the platform holds no key for them, only sessions.
//!
//! Platform origin:
//!
//!   GET  /                        who is signed in, and links to sign in or out
//!   GET  /auth/login?return=&login_hint=&invitation_token=   → WorkOS (a state cookie binds the
//!                                 round trip; an invitation's token lets its invitee sign up, since
//!                                 sign-up is off: WorkOS's "User invitation URL" points here)
//!   GET  /auth/link?return=       the same, adding a second sign-in to the signed-in person
//!   GET  /auth/callback           WorkOS → the code exchanged here → a session cookie
//!   GET  /auth/logout             a button; POST ends the session (and its site sessions)
//!   GET  /auth/fragment?name=&return=      a single-use redemption for one fragment's origin
//!   GET  /cli?key=<npub>          the signed-in person approves a CLI key
//!   POST /cli/approve             (the form)
//!
//! A fragment's origin: `__signin?token=&return=` redeems the platform's
//! redemption for this fragment only and sets its own session cookie;
//! `__signin` without a token starts at the platform; `__signout` drops
//! the cookie. The session cookie is looked up live on every request.

use fragment_core::{npub, site};
use fragment_proto::ErrorCode;
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::{ask_registry, facts_of, Signer};

pub const SESSION_COOKIE: &str = "fragment_session";
pub const SITE_COOKIE: &str = "fragment_site";
const LOGIN_COOKIE: &str = "fragment_login";
const LOGIN_HINT_MAX: usize = 320;
const INVITATION_TOKEN_MAX: usize = 256;

fn secure(url: &Url) -> bool {
    url.scheme() == "https"
}

pub fn set_cookie(name: &str, value: &str, path: &str, max_age_s: i64, secure: bool) -> String {
    let s = if secure { "; Secure" } else { "" };
    format!("{name}={value}; Path={path}; Max-Age={max_age_s}; HttpOnly; SameSite=Lax{s}")
}

fn cookie_of(req: &Request, name: &str) -> CellResult<Option<String>> {
    let cookies = req.headers().get("cookie")?.unwrap_or_default();
    Ok(site::cookie(&cookies, name).filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_string))
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A path on this origin to come back to (never another origin).
pub fn safe_return(r: Option<String>) -> String {
    match r {
        Some(p) if p.starts_with('/') && !p.starts_with("//") && !p.contains('\\') && p.len() <= 2048 => p,
        _ => "/".into(),
    }
}

/// `back` (a path from `safe_return`) under `base`, percent-encoded.
fn under(base: &str, back: &str) -> CellResult<String> {
    let base = Url::parse(base).map_err(|e| CellError::host(e.to_string()))?;
    Ok(base.join(back.trim_start_matches('/')).map_err(|e| CellError::invalid(format!("return: {e}")))?.to_string())
}

fn redirect(to: &str, cookies: &[String]) -> CellResult<Response> {
    let mut resp = Response::empty()?.with_status(302);
    resp.headers_mut().set("location", to)?;
    resp.headers_mut().set("cache-control", "no-store")?;
    for c in cookies {
        resp.headers_mut().append("set-cookie", c)?;
    }
    Ok(resp)
}

fn page(status: u16, title: &str, body: &str) -> CellResult<Response> {
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{t}</title>
<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:34rem;margin:12vh auto;padding:0 16px;color:#1d2126;background:#f6f7f8}}
h1{{font-size:1.4rem}}code{{background:#e8eaed;padding:1px 5px;border-radius:4px}}button{{font:inherit;padding:.5em 1.1em;border-radius:8px;border:1px solid #1d2126;background:#1d2126;color:#fff;cursor:pointer}}
a{{color:#2a5bd7}}@media (prefers-color-scheme:dark){{body{{background:#15181b;color:#e6e8ea}}code{{background:#262b30}}button{{background:#e6e8ea;color:#15181b;border-color:#e6e8ea}}a{{color:#7aa2f7}}}}</style>
<h1>{t}</h1>{body}"#,
        t = esc(title)
    );
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    Ok(Response::ok(html)?.with_status(status).with_headers(h))
}

fn query(url: &Url, k: &str) -> Option<String> {
    url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned())
}

/// The signed-in person on the platform origin, or `None`.
async fn platform_session(req: &Request, env: &Env) -> CellResult<Option<(String, Signer)>> {
    let Some(token) = cookie_of(req, SESSION_COOKIE)? else { return Ok(None) };
    match ask_registry(env, "/session", &json!({ "token": token })).await {
        Ok(v) => {
            let (id, kind, owner) = facts_of(&v)?;
            Ok(Some((token, Signer { key: None, id, kind, owner })))
        }
        Err(e) if e.code == ErrorCode::Unauthenticated => Ok(None),
        Err(e) => Err(e),
    }
}

/// The session a fragment's own cookie carries, for that fragment only.
pub async fn site_session(req: &Request, env: &Env, name: &str) -> CellResult<Option<Signer>> {
    let Some(token) = cookie_of(req, SITE_COOKIE)? else { return Ok(None) };
    match ask_registry(env, "/session", &json!({ "token": token, "fragment": name })).await {
        Ok(v) => {
            let (id, kind, owner) = facts_of(&v)?;
            Ok(Some(Signer { key: None, id, kind, owner }))
        }
        // a stale cookie is no session: the request goes on unsigned
        Err(e) if e.code == ErrorCode::Unauthenticated => Ok(None),
        Err(e) => Err(e),
    }
}

/// A POST from a browser comes from the platform's own pages.
fn same_origin(req: &Request, platform: &str) -> CellResult<()> {
    match req.headers().get("origin")? {
        Some(o) if o.trim_end_matches('/') != platform => Err(CellError::new(ErrorCode::Forbidden, "this form posts from the platform's own page")),
        _ => Ok(()),
    }
}

/// The platform bar's budget: what is left of this month's allowance.
async fn budget_line(env: &Env, id: &str) -> String {
    use fragment_core::budget::dollars;
    let Some(org) = crate::ledger::org_of(id) else { return String::new() };
    match crate::ledger::ask(env, &org, Method::Get, "/status", None).await {
        Ok(v) => {
            let (allowance, used) = (v["allowanceMicros"].as_i64().unwrap_or(0), v["spentMicros"].as_i64().unwrap_or(0) + v["reservedMicros"].as_i64().unwrap_or(0));
            let warn = if v["warn"] == true { " <b>Most of it is used.</b>" } else { "" };
            format!(
                "<p>AI this month ({}): <b>{}</b> of {} left.{warn}</p>",
                esc(v["period"].as_str().unwrap_or("")),
                dollars((allowance - used).max(0)),
                dollars(allowance)
            )
        }
        Err(_) => String::new(),
    }
}

async fn email_of(env: &Env, id: &str) -> String {
    ask_registry(env, "/view", &json!({ "identity": id, "by": id }))
        .await
        .ok()
        .and_then(|v| v["subjects"][0]["email"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn to_login(platform: &str, back: &str) -> CellResult<Response> {
    redirect(&format!("{platform}/auth/login?return={}", enc(back)), &[])
}

async fn begin(env: &Env, cfg: &Config, url: &Url, link: Option<String>) -> CellResult<Response> {
    let workos = cfg.workos()?;
    let platform = cfg.platform(url);
    let return_to = safe_return(query(url, "return"));
    let v = ask_registry(env, "/login/begin", &json!({ "returnTo": return_to, "linkTo": link })).await?;
    let state = v["state"].as_str().ok_or_else(|| CellError::host("the registry answered no state"))?;
    let mut to = format!(
        "{}/user_management/authorize?client_id={}&redirect_uri={}&response_type=code&provider=authkit&state={state}",
        workos.api,
        enc(&workos.client_id),
        enc(&format!("{platform}/auth/callback"))
    );
    if let Some(hint) = query(url, "login_hint").filter(|h| !h.is_empty() && h.len() <= LOGIN_HINT_MAX) {
        to += &format!("&login_hint={}", enc(&hint));
    }
    let token = query(url, "invitation_token")
        .filter(|t| !t.is_empty() && t.len() <= INVITATION_TOKEN_MAX && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    if let Some(token) = token {
        to += &format!("&invitation_token={token}");
    }
    redirect(&to, &[set_cookie(LOGIN_COOKIE, state, "/auth", 600, secure(url))])
}

/// The `sid` claim of an access token WorkOS answered the exchange with
/// (read, not trusted from a browser: it came from WorkOS over TLS).
fn sid_of(access_token: &str) -> Option<String> {
    use base64::Engine;
    let payload = access_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims["sid"].as_str().map(str::to_string)
}

async fn callback(req: &Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let workos = cfg.workos()?;
    if let Some(error) = query(url, "error") {
        let why = query(url, "error_description").unwrap_or_default();
        return page(400, "Sign-in did not finish", &format!("<p>WorkOS said <code>{}</code>: {}</p><p><a href=\"/auth/login\">Try again</a></p>", esc(&error), esc(&why)));
    }
    let state = query(url, "state").unwrap_or_default();
    let bound = cookie_of(req, LOGIN_COOKIE)?;
    if state.is_empty() || bound.as_deref() != Some(state.as_str()) {
        return page(400, "Sign-in did not start here", "<p>This sign-in began in another browser, or too long ago.</p><p><a href=\"/auth/login\">Start again</a></p>");
    }
    let code = query(url, "code").ok_or_else(|| CellError::invalid("the callback carries no code"))?;
    let body = json!({ "client_id": workos.client_id, "client_secret": workos.api_key, "grant_type": "authorization_code", "code": code });
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.to_string().into()));
    let exchange = Request::new_with_init(&format!("{}/user_management/authenticate", workos.api), &init)?;
    let mut resp = Fetch::Request(exchange).send().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS did not answer: {e}")))?;
    let status = resp.status_code();
    let answer: Value = resp.json().await.unwrap_or(Value::Null);
    if status != 200 {
        let why = answer["error_description"].as_str().or(answer["message"].as_str()).unwrap_or("no reason given");
        return Err(CellError::new(ErrorCode::UpstreamFailed, format!("WorkOS refused the sign-in ({status}): {why}")));
    }
    let subject = answer["user"]["id"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| CellError::new(ErrorCode::UpstreamFailed, "WorkOS answered no user id"))?;
    let email = answer["user"]["email"].as_str().unwrap_or("");
    let sid = answer["access_token"].as_str().and_then(sid_of);
    let done = ask_registry(
        env,
        "/login/finish",
        &json!({ "state": state, "issuer": workos.issuer(), "subject": subject, "email": email, "workosSid": sid }),
    )
    .await?;
    let token = done["token"].as_str().ok_or_else(|| CellError::host("the registry answered no session"))?;
    let back = safe_return(done["returnTo"].as_str().map(str::to_string));
    redirect(
        &under(&format!("{}/", cfg.platform(url)), &back)?,
        &[
            set_cookie(SESSION_COOKIE, token, "/", crate::registry::SESSION_TTL_MS / 1000, secure(url)),
            set_cookie(LOGIN_COOKIE, "", "/auth", 0, secure(url)),
        ],
    )
}

/// Sign-in's routes on the platform origin (the router sends only these).
pub async fn platform(mut req: Request, env: &Env, cfg: &Config, url: &Url, segments: &[&str]) -> CellResult<Response> {
    let method = req.method();
    let platform = cfg.platform(url);
    {
        match (method, segments) {
            (Method::Get, [""]) => {
                let body = match platform_session(&req, env).await? {
                    Some((_, who)) => {
                        let email = email_of(env, &who.id).await;
                        format!(
                            "<p>Signed in as <b>{}</b> (<code>{}</code>).</p>{}<p><a href=\"/auth/logout\">Sign out</a> · <a href=\"/auth/link\">Add another sign-in to you</a></p>",
                            esc(&email),
                            esc(&who.id),
                            budget_line(env, &who.id).await
                        )
                    }
                    None => "<p>Places for people and agents.</p><p><a href=\"/auth/login\">Sign in</a></p>".to_string(),
                };
                page(200, "fragment", &body)
            }
            (Method::Get, ["auth", "login"]) => begin(env, cfg, url, None).await,
            (Method::Get, ["auth", "link"]) => match platform_session(&req, env).await? {
                Some((token, _)) => begin(env, cfg, url, Some(token)).await,
                None => to_login(&platform, "/auth/link"),
            },
            (Method::Get, ["auth", "callback"]) => callback(&req, env, cfg, url).await,
            (Method::Get, ["auth", "logout"]) => page(200, "Sign out", "<form method=\"post\" action=\"/auth/logout\"><button>Sign out</button></form>"),
            (Method::Post, ["auth", "logout"]) => {
                same_origin(&req, &platform)?;
                let clear = set_cookie(SESSION_COOKIE, "", "/", 0, secure(url));
                let Some(token) = cookie_of(&req, SESSION_COOKIE)? else { return redirect("/", &[clear]) };
                let sid = match ask_registry(env, "/logout", &json!({ "token": token })).await {
                    Ok(v) => v["workosSid"].as_str().map(str::to_string),
                    Err(e) if e.code == ErrorCode::Unauthenticated => None,
                    Err(e) => return Err(e),
                };
                match (sid, cfg.workos()) {
                    (Some(sid), Ok(w)) => redirect(
                        &format!("{}/user_management/sessions/logout?session_id={}&return_to={}", w.api, enc(&sid), enc(&format!("{platform}/"))),
                        &[clear],
                    ),
                    _ => redirect("/", &[clear]),
                }
            }
            (Method::Get, ["auth", "fragment"]) => {
                let name = query(url, "name").filter(|n| fragment_proto::valid_fragment_name(n)).ok_or_else(|| CellError::invalid("name a fragment"))?;
                let back = safe_return(query(url, "return"));
                let Some((token, _)) = platform_session(&req, env).await? else {
                    return to_login(&platform, &format!("/auth/fragment?name={name}&return={}", enc(&back)));
                };
                let v = ask_registry(env, "/redeem/mint", &json!({ "token": token, "fragment": name, "returnTo": back })).await?;
                let redeem = v["redeem"].as_str().ok_or_else(|| CellError::host("the registry answered no redemption"))?;
                redirect(&format!("{}__signin?token={redeem}", cfg.canonical(url, &name)), &[])
            }
            (Method::Get, ["cli"]) => {
                let key = query(url, "key").unwrap_or_default();
                let Some(hex) = npub::parse(&key) else { return page(400, "Not a key", "<p>This link names no key. Run <code>fragment login</code> again.</p>") };
                let Some((_, who)) = platform_session(&req, env).await? else { return to_login(&platform, &format!("/cli?key={}", enc(&key))) };
                let email = email_of(env, &who.id).await;
                let npub = npub::encode(&hex);
                let tail = &npub[npub.len() - 8..];
                page(
                    200,
                    "Add a key to you",
                    &format!(
                        "<p>A <code>fragment</code> CLI wants to act as <b>{}</b>. Its key ends in <code>{tail}</code>; check that your terminal shows the same ending.</p>
<form method=\"post\" action=\"/cli/approve\"><input type=\"hidden\" name=\"key\" value=\"{}\"><button>Add this key</button></form>
<p>Didn't run <code>fragment login</code>? Close this page.</p>",
                        esc(if email.is_empty() { &who.id } else { &email }),
                        esc(&npub)
                    ),
                )
            }
            (Method::Post, ["cli", "approve"]) => {
                same_origin(&req, &platform)?;
                let bytes = req.bytes().await?;
                let key = url::form_urlencoded::parse(&bytes).find(|(k, _)| k == "key").map(|(_, v)| v.into_owned()).unwrap_or_default();
                let hex = npub::parse(&key).ok_or_else(|| CellError::invalid("the form names no key"))?;
                let Some((token, _)) = platform_session(&req, env).await? else {
                    return Err(CellError::new(ErrorCode::Unauthenticated, "sign in first"));
                };
                ask_registry(env, "/cli/approve", &json!({ "token": token, "key": hex })).await?;
                page(200, "Key added", "<p>Go back to your terminal: <code>fragment login</code> finishes on its own.</p>")
            }
            (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
        }
    }
}

/// Whether a path on a fragment's origin is sign-in's.
pub fn is_fragment_route(rest: &str) -> bool {
    matches!(rest, "__signin" | "__signout")
}

/// `__signin` and `__signout` on a fragment's own origin.
pub async fn fragment(env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str, path_mode: bool) -> CellResult<Response> {
    let cookie_path = if path_mode { format!("/f/{name}/") } else { "/".to_string() };
    let base = cfg.canonical(url, name);
    {
        match rest {
            "__signin" => match query(url, "token") {
                None => {
                    let back = safe_return(query(url, "return"));
                    redirect(&format!("{}/auth/fragment?name={name}&return={}", cfg.platform(url), enc(&back)), &[])
                }
                Some(redeem) => {
                    let v = ask_registry(env, "/redeem", &json!({ "redeem": redeem, "fragment": name })).await?;
                    let token = v["token"].as_str().ok_or_else(|| CellError::host("the registry answered no session"))?;
                    let back = safe_return(v["returnTo"].as_str().map(str::to_string));
                    redirect(
                        &under(&base, &back)?,
                        &[set_cookie(SITE_COOKIE, token, &cookie_path, crate::registry::SESSION_TTL_MS / 1000, secure(url))],
                    )
                }
            },
            "__signout" => redirect(&base, &[set_cookie(SITE_COOKIE, "", &cookie_path, 0, secure(url))]),
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {rest}"))),
        }
    }
}
