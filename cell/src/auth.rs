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
//!   GET  /cli?key=<npub>&proof=   the signed-in person approves a CLI key: the link
//!                                 carries the key's own proof (a NIP-98 event by it for
//!                                 `POST <platform>/cli/approve`, ten minutes good), so
//!                                 approving adds the key at once
//!   POST /cli/approve             (the form)
//!
//! A fragment's origin: `__signin?token=&return=` redeems the platform's
//! redemption for this fragment only and sets its own session cookie;
//! `__signin` without a token starts at the platform; `__signout` ends
//! that session in the registry and drops the cookie. The session cookie
//! is looked up live on every request.

use fragment_core::{npub, site};
use fragment_proto::{ErrorCode, IdentityKind};
use serde_json::json;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::{ask_registry, signer_of, Signer};

pub const SESSION_COOKIE: &str = "fragment_session";
pub const SITE_COOKIE: &str = "fragment_site";
const LOGIN_COOKIE: &str = "fragment_login";
const LOGIN_HINT_MAX: usize = 320;
const INVITATION_TOKEN_MAX: usize = 256;
/// How long an approval link's proof is good.
const LINK_PROOF_WINDOW_S: i64 = 600;
const LINK_PROOF_MAX: usize = 4096;
/// The approval form: a key, and a proof of at most `LINK_PROOF_MAX` bytes
/// that form encoding may triple.
const APPROVE_FORM_MAX_BYTES: usize = 16 * 1024;
const _: () = assert!(APPROVE_FORM_MAX_BYTES >= 3 * LINK_PROOF_MAX + 256, "the form holds the longest proof, encoded");
/// The new-fragment form: a label and a template's name.
const NEW_FORM_MAX_BYTES: usize = 4 * 1024;

/// The key an approval link's proof is by, if it is good: a NIP-98 event
/// by that key for `POST <platform>/cli/approve`, made within ten minutes.
fn link_proof(platform: &str, key_hex: &str, proof: &str) -> CellResult<()> {
    let stale = || CellError::invalid("this approval link is not good (it is older than ten minutes, or for another key): run `fragment login` again for a fresh one");
    if proof.is_empty() || proof.len() > LINK_PROOF_MAX {
        return Err(stale());
    }
    let now_s = crate::js::now_ms() / 1000;
    let signer = fragment_nip98::verify(Some(&format!("Nostr {proof}")), "POST", &format!("{platform}/cli/approve"), &[], now_s, LINK_PROOF_WINDOW_S)
        .map_err(|_| stale())?;
    if signer != key_hex {
        return Err(stale());
    }
    Ok(())
}

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

/// Where a browser that signed in goes back to: the path it brought, on
/// `base`'s origin and never another (`site::return_url`).
fn back_to(base: &str, raw: Option<&str>) -> CellResult<String> {
    let base = Url::parse(base).map_err(|e| CellError::host(format!("{base}: {e}")))?;
    Ok(site::return_url(&base, raw).into())
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
        Ok(v) => Ok(Some((token, signer_of(&v, None)?))),
        Err(e) if e.code == ErrorCode::Unauthenticated => Ok(None),
        Err(e) => Err(e),
    }
}

/// The session a fragment's own cookie carries, for that fragment only.
pub async fn site_session(req: &Request, env: &Env, name: &str) -> CellResult<Option<Signer>> {
    let Some(token) = cookie_of(req, SITE_COOKIE)? else { return Ok(None) };
    match ask_registry(env, "/session", &json!({ "token": token, "fragment": name })).await {
        Ok(v) => Ok(Some(signer_of(&v, None)?)),
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

/// The fragments a person belongs to, as links.
async fn fragments_list(env: &Env, cfg: &Config, url: &Url, id: &str) -> String {
    let asked = async {
        let list = Request::new("https://principal.internal/list", Method::Get)?;
        let v: serde_json::Value = env.durable_object("PRINCIPAL")?.get_by_name(id)?.fetch_with_request(list).await?.json().await?;
        Ok::<_, worker::Error>(v)
    };
    let Ok(v) = asked.await else { return String::new() };
    let items: String = v["fragments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let name = f["name"].as_str()?;
            Some(format!("<li><a href=\"{}\">{}</a> <small>{}</small></li>", esc(&cfg.canonical(url, name)), esc(name), esc(f["role"].as_str().unwrap_or(""))))
        })
        .collect();
    if items.is_empty() { String::new() } else { format!("<h2>Your fragments</h2><ul>{items}</ul>") }
}

/// The "new fragment" form: a label and one of the platform's templates.
fn new_form(username: &str, platform: &str) -> String {
    let host = platform.split("://").nth(1).unwrap_or("fragment.club");
    let choices: String = crate::publish::TEMPLATES
        .iter()
        .enumerate()
        .map(|(i, (name, t))| {
            let (title, description) = crate::publish::describe(t);
            format!(
                "<p><label><input type=\"radio\" name=\"template\" value=\"{n}\"{c}> <b>{t}</b> {d}</label></p>",
                n = esc(name),
                c = if i == 0 { " checked" } else { "" },
                t = esc(if title.is_empty() { name } else { &title }),
                d = esc(&description),
            )
        })
        .collect();
    format!(
        "<h2>New fragment</h2><form method=\"post\" action=\"/auth/new\">{choices}\
         <p><input name=\"label\" required maxlength=\"63\" pattern=\"[a-z0-9]([a-z0-9-]*[a-z0-9])?\" placeholder=\"name\" style=\"font:inherit;padding:.4em .6em;border-radius:8px;border:1px solid #aab\"><code>.{u}.{h}</code> <button>Make it</button></p></form>",
        u = esc(username),
        h = esc(host),
    )
}

fn to_login(platform: &str, back: &str) -> CellResult<Response> {
    redirect(&format!("{platform}/auth/login?return={}", enc(back)), &[])
}

async fn begin(env: &Env, cfg: &Config, url: &Url, link: Option<String>) -> CellResult<Response> {
    let workos = cfg.workos()?;
    let platform = cfg.platform(url);
    let return_to = site::return_path(query(url, "return").as_deref());
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
    // the registry exchanges the code: WorkOS's API key is the node's (KEYS)
    let done = ask_registry(env, "/login/exchange", &json!({ "state": state, "code": code, "clientId": workos.client_id, "issuer": workos.issuer() })).await?;
    let token = done["token"].as_str().ok_or_else(|| CellError::host("the registry answered no session"))?;
    redirect(
        &back_to(&format!("{}/", cfg.platform(url)), done["returnTo"].as_str())?,
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
                    // a username first: fragments live under it (decision 16)
                    Some((_, who)) if who.username.is_none() && who.kind == IdentityKind::Person => {
                        let email = email_of(env, &who.id).await;
                        format!(
                            "<h1>Choose your username</h1><p>Signed in as <b>{}</b>. Your fragments will live at <code>&lt;name&gt;.<i>username</i>.{}</code>; a username is chosen once.</p>\
                             <form method=\"post\" action=\"/auth/username\"><p><input name=\"username\" required minlength=\"3\" maxlength=\"32\" pattern=\"[a-z0-9]([a-z0-9-]*[a-z0-9])?\" autocomplete=\"username\" style=\"font:inherit;padding:.4em .6em;border-radius:8px;border:1px solid #aab\"> <button>Take it</button></p></form>\
                             <p><a href=\"/auth/logout\">Sign out</a></p>",
                            esc(&email),
                            esc(cfg.platform(url).split("://").nth(1).unwrap_or("fragment.club")),
                        )
                    }
                    Some((_, who)) => {
                        let email = email_of(env, &who.id).await;
                        let username = who.username.clone().unwrap_or_default();
                        format!(
                            "<p><img src=\"/api/users/{u}/picture\" alt=\"\" width=\"48\" height=\"48\" style=\"border-radius:50%;vertical-align:middle;object-fit:cover\" onerror=\"this.remove()\"> Signed in as <b>{u}</b> ({e}, <code>{id}</code>).</p>{b}{f}{n}\
                             <form method=\"post\" action=\"/auth/picture\" enctype=\"multipart/form-data\"><p>Picture: <input type=\"file\" name=\"picture\" accept=\"image/png,image/jpeg,image/webp,image/gif\" required> <button>Set</button></p></form>\
                             <p><a href=\"/auth/logout\">Sign out</a> · <a href=\"/auth/link\">Add another sign-in to you</a></p>",
                            u = esc(&username),
                            e = esc(&email),
                            id = esc(&who.id),
                            b = budget_line(env, &who.id).await,
                            f = fragments_list(env, cfg, url, &who.id).await,
                            n = new_form(&username, &cfg.platform(url)),
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
            (Method::Post, ["auth", "username"]) => {
                let Some((_, who)) = platform_session(&req, env).await? else { return to_login(&platform, "/") };
                let form = req.form_data().await?;
                let Some(FormEntry::Field(username)) = form.get("username") else { return Err(CellError::invalid("choose a username")) };
                match ask_registry(env, "/username/claim", &json!({ "identity": who.id, "username": username.trim() })).await {
                    Ok(_) => redirect("/", &[]),
                    Err(e) if matches!(e.code, ErrorCode::AlreadyExists | ErrorCode::InvalidRequest) => {
                        page(400, "Choose your username", &format!("<p>{}</p><p><a href=\"/\">Try another</a></p>", esc(&e.message)))
                    }
                    Err(e) => Err(e),
                }
            }
            (Method::Post, ["auth", "new"]) => {
                same_origin(&req, &platform)?;
                let Some((_, who)) = platform_session(&req, env).await? else { return to_login(&platform, "/") };
                let bytes = crate::read_body(&mut req, NEW_FORM_MAX_BYTES).await?;
                let field = |name: &str| url::form_urlencoded::parse(&bytes).find(|(k, _)| k == name).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
                let create = fragment_proto::CreateFragment { name: field("label"), visibility: None, template: Some(field("template")) };
                let v: serde_json::Value = match crate::create_fragment(env, cfg, url, create, who).await {
                    Ok(mut made) if made.status_code() == 200 => made.json().await?,
                    Ok(mut made) => {
                        let v: serde_json::Value = made.json().await.unwrap_or_default();
                        return page(400, "New fragment", &format!("<p>{}</p><p><a href=\"/\">Back</a></p>", esc(v["message"].as_str().unwrap_or("it could not be made"))));
                    }
                    Err(e) => return page(400, "New fragment", &format!("<p>{}</p><p><a href=\"/\">Back</a></p>", esc(&e.message))),
                };
                let name = v["name"].as_str().ok_or_else(|| CellError::host("the create answered no name"))?;
                // signed in on its own origin, then there
                redirect(&format!("/auth/fragment?name={}&return=/", enc(name)), &[])
            }
            (Method::Post, ["auth", "picture"]) => {
                let Some((_, who)) = platform_session(&req, env).await? else { return to_login(&platform, "/") };
                let form = req.form_data().await?;
                let Some(FormEntry::File(file)) = form.get("picture") else { return Err(CellError::invalid("choose a picture")) };
                let bytes = file.bytes().await?;
                if bytes.len() > fragment_proto::limits::PICTURE_MAX_BYTES {
                    return page(400, "Picture", &format!("<p>A picture is at most {} KiB.</p><p><a href=\"/\">Back</a></p>", fragment_proto::limits::PICTURE_MAX_BYTES / 1024));
                }
                let Some(mime) = crate::picture_type(&bytes) else {
                    return page(400, "Picture", "<p>A picture is a PNG, JPEG, WebP, or GIF.</p><p><a href=\"/\">Back</a></p>");
                };
                let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
                crate::js::blob_put_bytes(env.as_ref(), &format!("pictures/{sha}"), &bytes).await?;
                ask_registry(env, "/picture/set", &json!({ "identity": who.id, "sha": sha, "mime": mime })).await?;
                redirect("/", &[])
            }
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
                let back = site::return_path(query(url, "return").as_deref());
                let Some((token, _)) = platform_session(&req, env).await? else {
                    return to_login(&platform, &format!("/auth/fragment?name={name}&return={}", enc(&back)));
                };
                let v = ask_registry(env, "/redeem/mint", &json!({ "token": token, "fragment": name, "returnTo": back })).await?;
                let redeem = v["redeem"].as_str().ok_or_else(|| CellError::host("the registry answered no redemption"))?;
                redirect(&format!("{}__signin?token={redeem}", cfg.canonical(url, &name)), &[])
            }
            (Method::Get, ["cli"]) => {
                let key = query(url, "key").unwrap_or_default();
                let proof = query(url, "proof").unwrap_or_default();
                let Some(hex) = npub::parse(&key) else { return page(400, "Not a key", "<p>This link names no key. Run <code>fragment login</code> again.</p>") };
                if let Err(e) = link_proof(&platform, &hex, &proof) {
                    return page(400, "This link has expired", &format!("<p>{}</p>", esc(&e.message)));
                }
                let Some((_, who)) = platform_session(&req, env).await? else {
                    return to_login(&platform, &format!("/cli?key={}&proof={}", enc(&key), enc(&proof)));
                };
                let email = email_of(env, &who.id).await;
                let npub = npub::encode(&hex);
                let tail = &npub[npub.len() - 8..];
                page(
                    200,
                    "Add a key to you",
                    &format!(
                        "<p>A <code>fragment</code> CLI wants to act as <b>{}</b>. Its key ends in <code>{tail}</code>; check that your terminal shows the same ending.</p>
<form method=\"post\" action=\"/cli/approve\"><input type=\"hidden\" name=\"key\" value=\"{}\"><input type=\"hidden\" name=\"proof\" value=\"{}\"><button>Add this key</button></form>
<p>Didn't run <code>fragment login</code>? Close this page.</p>",
                        esc(if email.is_empty() { &who.id } else { &email }),
                        esc(&npub),
                        esc(&proof)
                    ),
                )
            }
            (Method::Post, ["cli", "approve"]) => {
                same_origin(&req, &platform)?;
                // read before anyone is known to be signed in: bounded as it arrives
                let bytes = crate::read_body(&mut req, APPROVE_FORM_MAX_BYTES).await?;
                let field = |name: &str| url::form_urlencoded::parse(&bytes).find(|(k, _)| k == name).map(|(_, v)| v.into_owned()).unwrap_or_default();
                let hex = npub::parse(&field("key")).ok_or_else(|| CellError::invalid("the form names no key"))?;
                let Some((token, _)) = platform_session(&req, env).await? else {
                    return Err(CellError::new(ErrorCode::Unauthenticated, "sign in first"));
                };
                link_proof(&platform, &hex, &field("proof"))?;
                ask_registry(env, "/cli/add", &json!({ "token": token, "key": hex })).await?;
                page(200, "Key added", "<p>This key is yours now. A <code>fragment login</code> waiting in a terminal finishes on its own.</p>")
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
pub async fn fragment(req: &Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str, path_mode: bool) -> CellResult<Response> {
    let cookie_path = if path_mode { format!("/f/{name}/") } else { "/".to_string() };
    let base = cfg.canonical(url, name);
    {
        match rest {
            "__signin" => match query(url, "token") {
                // signed in here already (a page that embeds this one sends
                // every frame through __signin): straight back
                None if site_session(req, env, name).await?.is_some() => redirect(&back_to(&base, query(url, "return").as_deref())?, &[]),
                None => {
                    let back = site::return_path(query(url, "return").as_deref());
                    redirect(&format!("{}/auth/fragment?name={name}&return={}", cfg.platform(url), enc(&back)), &[])
                }
                Some(redeem) => {
                    let v = ask_registry(env, "/redeem", &json!({ "redeem": redeem, "fragment": name })).await?;
                    let token = v["token"].as_str().ok_or_else(|| CellError::host("the registry answered no session"))?;
                    redirect(
                        &back_to(&base, v["returnTo"].as_str())?,
                        &[set_cookie(SITE_COOKIE, token, &cookie_path, crate::registry::SESSION_TTL_MS / 1000, secure(url))],
                    )
                }
            },
            "__signout" => {
                // The session ends in the registry, so a copy of the cookie
                // is nobody too. The browser is signed out whatever the
                // registry answers: a registry that cannot end the session
                // is logged, and the session lasts until it expires or the
                // platform session ends (`/auth/logout` ends every one).
                if let Some(token) = cookie_of(req, SITE_COOKIE)? {
                    if let Err(e) = ask_registry(env, "/session/end", &json!({ "token": token, "fragment": name })).await {
                        console_error!("__signout on {name}: the registry did not end the session ({:?}): {}", e.code, e.message);
                    }
                }
                redirect(&base, &[set_cookie(SITE_COOKIE, "", &cookie_path, 0, secure(url))])
            }
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {rest}"))),
        }
    }
}
