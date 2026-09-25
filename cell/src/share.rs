//! Sharing, on the platform's origin (phase 7, decision 4): the share sheet
//! and accepting an invite. Both are the platform's pages, signed in by its
//! session cookie, and each acts through the fragment's own handlers
//! (members.rs) as the signed-in person, as `/auth/new` makes a fragment
//! for them: the fragment decides who may do what (only its owner changes
//! anything; a member may see who is in; anyone else gets a 403 page).
//!
//!   GET  /share/<name>         the sheet: who is in (usernames and pictures) and their roles;
//!                              for the owner, inviting by username, pending invites, roles,
//!                              removing, who may open it, its share link (copy, a new one),
//!                              and, when its fragment.json asks for `frame`, letting it show
//!                              the owner's other fragments inside it (`__frame`): plainly for
//!                              the platform's desktop, with a warning for any other page
//!   POST /share/<name>         one of the owner's changes (`action`), then back to the sheet;
//!                              an invite answers the sheet with the link to send
//!   GET  /join/<name>?token=   what the invite grants, and a Join button
//!   POST /join/<name>          joins, then → the fragment, signed in on its origin
//!
//! A fragment's page (its author's code, or an agent that rewrote the
//! desktop) is one site with the platform, so the platform's session rides
//! along on its fetches, forms, and frames. Neither page can be driven by
//! one:
//!
//! - it cannot read them: they send no CORS headers, and they refuse
//!   every frame (`auth::unframed`);
//! - it cannot post to them: every POST's Origin must be the platform's
//!   (`auth::same_origin`) and carry a form token bound to the session
//!   (`fragment_core::form`), which only the page itself holds;
//! - it cannot script a window it opened on them: they answer
//!   `Cross-Origin-Opener-Policy: same-origin` (`auth::unopened`), so the
//!   window is severed from its opener (the desktop opens it with
//!   `noopener` too, but the desktop is code its owner's agent rewrites);
//! - it cannot hand them a grant: the sheet reads nothing from its URL, so
//!   no link prefills what a click would approve (the join page's token is
//!   the invite itself, and an invite by username is its invitee's alone);
//! - the click that opened one cannot confirm it: their buttons arm
//!   `form::DELAY_MS` after the page shows, and a form sent sooner is
//!   refused.

use std::collections::BTreeMap;

use fragment_core::{form, npub};
use fragment_proto::{ErrorBody, ErrorCode, Invite, InviteList, Member, MemberList, Role, Visibility};
use serde_json::{json, Value};
use worker::*;

use crate::ask_registry;
use crate::auth::{self, enc, esc};
use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;
use crate::registry::calls::{self, Profile};
use crate::routed::{Routed, Signed};

/// A form of a few short fields (the longest: an invite's token).
const FORM_MAX_BYTES: usize = 4 * 1024;
/// An invite's token: 24 random bytes, hex (members.rs).
const INVITE_TOKEN_LEN: usize = 48;
/// An invite's id: 8 random bytes, hex.
const INVITE_ID_LEN: usize = 16;
const DAY_MS: i64 = 24 * 3600 * 1000;

const SHEET_STYLE: &str = "body{margin:4vh auto}h2{font-size:1rem;margin:1.6em 0 .4em}.sub{color:#6b7280;margin-top:-.6em}
ul.people{list-style:none;padding:0;margin:0}.people li{display:flex;align-items:center;gap:8px;padding:6px 0;border-bottom:1px solid #8882;flex-wrap:wrap}
.av{width:28px;height:28px;border-radius:50%;object-fit:cover;background:#8884;display:inline-grid;place-items:center;font-size:13px;font-weight:600;flex:none}
.who{flex:1;min-width:8rem}.role,.hint{color:#6b7280;font-size:.9em}form.inline{display:inline}
input,select{font:inherit;padding:.35em .5em;border-radius:8px;border:1px solid #aab;background:inherit;color:inherit}
input[readonly]{width:100%;box-sizing:border-box;font-size:.85em}label{display:block}
button{padding:.35em .9em}button:disabled{opacity:.45;cursor:default}button.quiet{background:none;color:inherit;border-color:#aab}
.flash{padding:2px 14px;border-radius:10px;background:#8882}.flash.error{background:#e5484d33}";

/// Arms the page's buttons `DELAY_MS` after it shows (again each time it
/// is shown), and copies a link.
fn script() -> String {
    format!(
        r#"<script>(() => {{
  const acting = () => document.querySelectorAll("button[data-arm]");
  let timer = 0;
  const arm = () => {{
    clearTimeout(timer);
    for (const b of acting()) b.disabled = true;
    timer = setTimeout(() => {{ for (const b of acting()) b.disabled = false; }}, {delay});
  }};
  arm();
  addEventListener("pageshow", arm);
  document.addEventListener("visibilitychange", () => {{ if (document.visibilityState === "visible") arm(); }});
  for (const b of document.querySelectorAll("button[data-copy]")) b.onclick = () => {{
    const input = document.getElementById(b.dataset.copy);
    input.select();
    (navigator.clipboard ? navigator.clipboard.writeText(input.value) : Promise.reject()).then(() => {{ b.textContent = "Copied"; }}, () => document.execCommand("copy"));
  }};
}})();</script>"#,
        delay = form::DELAY_MS
    )
}

/// One of sharing's pages: the platform's look, its protections, and
/// armed buttons. `forms_here`: its forms post only to this origin (the
/// join page's redirects on to the fragment's, which `form-action` would
/// refuse).
pub(crate) fn sheet_page(status: u16, title: &str, body: &str, forms_here: bool) -> CellResult<Response> {
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{t}</title>
<style>{style}
{sheet}</style>
<h1>{t}</h1>{body}{script}"#,
        t = esc(title),
        style = auth::STYLE,
        sheet = SHEET_STYLE,
        script = script(),
    );
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    auth::unframed(&h)?;
    auth::unopened(&h)?;
    let forms = if forms_here { " form-action 'self';" } else { "" };
    h.set(
        "content-security-policy",
        &format!("default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline';{forms} base-uri 'none'; frame-ancestors 'none'"),
    )?;
    // the join page's URL holds its token: no other origin hears it
    h.set("referrer-policy", "same-origin")?;
    Ok(Response::ok(html)?.with_status(status).with_headers(h))
}

/// A page that only says something (a refusal, a stale link): `html`,
/// escaped by the caller.
fn notice(status: u16, title: &str, html: &str) -> CellResult<Response> {
    sheet_page(status, title, &format!("<p>{html}</p>"), true)
}

fn purpose(what: &str, name: &str) -> String {
    format!("{what}:{name}")
}

pub(crate) fn label(name: &str) -> &str {
    fragment_proto::split_fragment_name(name).map_or(name, |(label, _)| label)
}

fn role_phrase(role: Role) -> &'static str {
    match role {
        Role::Editor => "an editor (they can change it)",
        Role::Owner => "its owner",
        _ => "a viewer (they can see it)",
    }
}

/// The route's fragment, or a 404 page.
fn fragment_named(name: &str) -> Option<String> {
    fragment_proto::valid_fragment_name(name).then(|| name.to_string())
}

/// Sharing's routes on the platform origin (the router sends only these).
pub async fn route(req: Request, env: &Env, cfg: &Config, url: &Url, segments: &[&str]) -> CellResult<Response> {
    let method = req.method();
    let (what, name) = match segments {
        [what @ ("share" | "join"), name] => (*what, *name),
        _ => return Err(CellError::new(ErrorCode::NotFound, format!("no route {}", url.path()))),
    };
    let Some(name) = fragment_named(name) else { return notice(404, "No such fragment", "This link names no fragment.") };
    match (method, what) {
        (Method::Get, "share") => sheet(&req, env, cfg, url, &name).await,
        (Method::Post, "share") => share_post(req, env, cfg, url, &name).await,
        (Method::Get, "join") => join_page(&req, env, cfg, url, &name).await,
        (Method::Post, "join") => join_post(req, env, cfg, url, &name).await,
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// A fragment's handler at `inner`, asked as the signed-in person: its
/// answer, or its refusal as it made it.
pub(crate) async fn ask(env: &Env, url: &Url, name: &str, who: &Signed, method: Method, inner: &str, body: Option<Value>) -> CellResult<Value> {
    // the page's own query string is nothing the fragment reads
    let mut at = url.clone();
    at.set_query(None);
    let headers = Headers::new();
    Routed { name: name.to_string(), url: at, mode: None, signed: Some(who.clone()), credential: None }.to_headers(&headers)?;
    if body.is_some() {
        headers.set("content-type", "application/json")?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(b) = body {
        init.with_body(Some(b.to_string().into()));
    }
    let req = Request::new_with_init(&format!("https://fragment.internal{inner}"), &init)?;
    let mut resp = env.durable_object("FRAGMENT")?.get_by_name(name)?.fetch_with_request(req).await?;
    let status = resp.status_code();
    let v: Value = resp.json().await.map_err(|e| CellError::host(format!("{inner} answered {status}, not JSON: {e}")))?;
    if status == 200 {
        return Ok(v);
    }
    match serde_json::from_value::<ErrorBody>(v) {
        Ok(e) => Err(CellError::new(e.error, e.message)),
        Err(_) => Err(CellError::host(format!("{inner} answered {status}"))),
    }
}

fn decoded<T: serde::de::DeserializeOwned>(v: Value, what: &str) -> CellResult<T> {
    serde_json::from_value(v).map_err(|e| CellError::host(format!("{what}: {e}")))
}

/// Usernames and pictures for identities, asked of the registry
/// `PROFILES_MAX` at a time. One it cannot answer shows as its id.
async fn profiles(env: &Env, mut ids: Vec<String>) -> BTreeMap<String, Profile> {
    ids.sort();
    ids.dedup();
    let asked = ids.chunks(crate::registry::PROFILES_MAX).map(|chunk| {
        let call = calls::Profiles { ids: chunk.to_vec() };
        async move { ask_registry(env, &call).await }
    });
    let mut all = BTreeMap::new();
    for answer in futures_util::future::join_all(asked).await {
        match answer {
            Ok(a) => all.extend(a.profiles),
            Err(e) => console_error!("the share sheet's profiles: {:?} {}", e.code, e.message),
        }
    }
    all
}

/// Someone as a page shows them: a picture (or an initial), and their
/// username, or whose agent they are.
fn person(id: &str, p: Option<&Profile>, me: &str) -> String {
    let (name, initial) = match p {
        Some(Profile { kind: fragment_proto::IdentityKind::Agent, username, .. }) => {
            let u = username.as_deref().unwrap_or("someone");
            (format!("{}'s agent", esc(u)), "✦".to_string())
        }
        Some(Profile { username: Some(u), .. }) => (format!("@{}", esc(u)), u.chars().next().unwrap_or('?').to_uppercase().to_string()),
        _ => (format!("<code>{}</code>", esc(&npub::display(id))), "?".to_string()),
    };
    let picture = match p.and_then(|p| p.picture.as_deref()) {
        Some(src) => format!("<img class=\"av\" src=\"{}\" alt=\"\">", esc(src)),
        None => format!("<span class=\"av\" aria-hidden=\"true\">{}</span>", esc(&initial)),
    };
    let you = if id == me { " <span class=\"role\">(you)</span>" } else { "" };
    format!("{picture}<span class=\"who\">{name}{you}</span>")
}

/// What the sheet shows, read as the signed-in person.
struct Sheet {
    name: String,
    me: String,
    /// The person's own role (`None`: they read through an agent of theirs).
    role: Option<Role>,
    members: Vec<Member>,
    /// The owner's only.
    invites: Vec<Invite>,
    visibility: Visibility,
    /// The share link: the owner's only.
    link: Option<String>,
    /// Whether its owner lets it frame their fragments (`None`: its
    /// fragment.json does not ask for `frame`).
    frame: Option<bool>,
    /// It is the platform's desktop, its code the template's (the owner's
    /// only): the sheet says what it does, without a warning.
    platform_desktop: bool,
    profiles: BTreeMap<String, Profile>,
    /// The page's form token.
    form: String,
}

impl Sheet {
    fn owner(&self) -> bool {
        self.role == Some(Role::Owner)
    }
}

/// What happened, shown above the sheet.
enum Flash {
    Invited { who: String, role: Role, link: String },
    Refused(String),
}

async fn load(env: &Env, cfg: &Config, url: &Url, name: &str, who: &Signed, session: &str) -> CellResult<Sheet> {
    let (members, status) =
        futures_util::future::join(ask(env, url, name, who, Method::Get, "/api/members", None), ask(env, url, name, who, Method::Get, "/api/status", None)).await;
    let members: MemberList = decoded(members?, "the members")?;
    let status = status?;
    let role = members.members.iter().find(|m| m.principal == who.id).map(|m| m.role);
    let invites = if role == Some(Role::Owner) {
        decoded::<InviteList>(ask(env, url, name, who, Method::Get, "/api/invites", None).await?, "the invites")?.invites
    } else {
        vec![]
    };
    let visibility: Visibility = decoded(status["visibility"].clone(), "the status's visibility")?;
    let frame = status["frame"].as_bool();
    let platform_desktop = match (role, frame) {
        (Some(Role::Owner), Some(_)) => ask(env, url, name, who, Method::Get, "/api/grants/frame", None).await?["template"].as_bool().unwrap_or(false),
        _ => false,
    };
    let link = match (role, status["viewToken"].as_str()) {
        (Some(Role::Owner), Some(token)) => Some(format!("{}?view={token}", cfg.canonical(url, name))),
        _ => None,
    };
    let ids = members.members.iter().map(|m| m.principal.clone()).chain(invites.iter().filter_map(|i| i.invitee.clone())).collect();
    Ok(Sheet {
        name: name.to_string(),
        me: who.id.clone(),
        role,
        members: members.members,
        invites,
        visibility,
        link,
        frame,
        platform_desktop,
        profiles: profiles(env, ids).await,
        form: form::issue(session, &purpose("share", name), js::now_ms()),
    })
}

/// What letting it show your fragments inside it means: plain for the
/// platform's desktop (its code is the platform's), a warning for any other
/// page that asks (its code is whoever last changed it).
fn frame_hint(platform_desktop: bool, granted: bool) -> &'static str {
    match (platform_desktop, granted) {
        (true, true) => "<p class=\"hint\">It shows your fragments inside it, signed in as you, so you can use them side by side. If its code is ever changed, it asks you here again.</p>",
        (true, false) => "<p class=\"hint\">It can show your fragments inside it, signed in as you, so you can use them side by side. You stopped it.</p>",
        (false, true) => "<p class=\"hint\">It shows your other fragments inside it, signed in as you. The platform can't vouch for its code: whoever changes it could put its own buttons on top of your fragments and catch your clicks. Stop it if you no longer trust that code.</p>",
        (false, false) => "<p class=\"hint\">It asks to show your other fragments inside it, signed in as you. The platform can't vouch for its code: whoever changes that code (you, someone you share it with, or an agent) could put its own buttons on top of your fragments and catch your clicks. Allow it only if you trust that code.</p>",
    }
}

/// A form of the owner's: one action, with the page's token.
fn action_form(sheet: &Sheet, action: &str, fields: &str, button: &str, class: &str) -> String {
    format!(
        "<form method=\"post\" class=\"{class}\"><input type=\"hidden\" name=\"form\" value=\"{f}\"><input type=\"hidden\" name=\"action\" value=\"{action}\">{fields}<button data-arm disabled>{button}</button></form>",
        f = esc(&sheet.form),
    )
}

fn render(sheet: &Sheet, flash: Option<Flash>) -> String {
    let mut out = format!("<p class=\"sub\"><code>{}</code></p>", esc(&sheet.name));
    match flash {
        Some(Flash::Invited { who, role, link }) => out += &format!(
            "<div class=\"flash\"><p>Invited <b>@{w}</b> as {r}. Send them this link: it works for them alone, once, for {d} days.</p>\
             <p><input id=\"invite-link\" readonly value=\"{l}\"> <button type=\"button\" data-copy=\"invite-link\">Copy</button></p></div>",
            w = esc(&who),
            r = role_phrase(role),
            d = fragment_proto::limits::INVITE_TTL_DEFAULT_S / 86400,
            l = esc(&link),
        ),
        Some(Flash::Refused(why)) => out += &format!("<div class=\"flash error\"><p>{}</p></div>", esc(&why)),
        None => {}
    }
    out += "<h2>People</h2><ul class=\"people\">";
    for m in &sheet.members {
        out += "<li>";
        out += &person(&m.principal, sheet.profiles.get(&m.principal), &sheet.me);
        if sheet.owner() && m.role != Role::Owner {
            let member = format!("<input type=\"hidden\" name=\"member\" value=\"{}\">", esc(&m.principal));
            let choice = |r: Role| format!("<option value=\"{v}\"{s}>{v}</option>", v = r.as_str(), s = if m.role == r { " selected" } else { "" });
            let select = format!("<select name=\"role\" aria-label=\"role\">{}{}</select> ", choice(Role::Viewer), choice(Role::Editor));
            out += &action_form(sheet, "role", &format!("{member}{select}"), "Set", "inline");
            out += " ";
            out += &action_form(sheet, "remove", &member, "Remove", "inline quiet");
        } else {
            out += &format!("<span class=\"role\">{}</span>", m.role.as_str());
        }
        out += "</li>";
    }
    out += "</ul>";
    if !sheet.owner() {
        let who = match sheet.visibility {
            Visibility::Members => "only the people in it",
            Visibility::Link => "the people in it, and anyone with its link",
            Visibility::Public => "anyone",
        };
        out += &format!("<p class=\"hint\">{} can be opened by {who}. Only its owner changes who is in.</p>", esc(label(&sheet.name)));
        return out;
    }
    out += "<h2>Invite</h2>";
    out += &action_form(
        sheet,
        "invite",
        "<input name=\"username\" required minlength=\"3\" maxlength=\"32\" pattern=\"@?[a-z0-9]([a-z0-9-]*[a-z0-9])?\" placeholder=\"username\" autocomplete=\"off\" aria-label=\"username\"> \
         <select name=\"role\" aria-label=\"role\"><option value=\"viewer\">viewer</option><option value=\"editor\">editor</option></select> ",
        "Invite",
        "",
    );
    out += "<p class=\"hint\">You get a link to send them; it works for them alone.</p>";
    if !sheet.invites.is_empty() {
        out += "<h2>Invited</h2><ul class=\"people\">";
        let now = js::now_ms();
        for i in &sheet.invites {
            out += "<li>";
            out += &match &i.invitee {
                Some(id) => person(id, sheet.profiles.get(id), &sheet.me),
                None => format!("<span class=\"who\">anyone with its link <span class=\"role\">({} left)</span></span>", i.uses_left),
            };
            let days = (i.expires_at - now) / DAY_MS;
            let until = if days >= 1 { format!("{days} more day{}", if days == 1 { "" } else { "s" }) } else { "less than a day".into() };
            out += &format!("<span class=\"role\">{}, for {until}</span> ", i.role.as_str());
            out += &action_form(sheet, "uninvite", &format!("<input type=\"hidden\" name=\"invite\" value=\"{}\">", esc(&i.id)), "Revoke", "inline quiet");
            out += "</li>";
        }
        out += "</ul>";
    }
    out += "<h2>Who can open it</h2>";
    let option = |v: Visibility, text: &str| {
        let checked = if sheet.visibility == v { " checked" } else { "" };
        format!("<label><input type=\"radio\" name=\"visibility\" value=\"{}\"{checked}> {text}</label>", v.as_str())
    };
    let options = [option(Visibility::Members, "Only the people in it"), option(Visibility::Link, "Anyone with the link"), option(Visibility::Public, "Anyone")].concat();
    out += &action_form(sheet, "visibility", &format!("{options}<p>"), "Save", "");
    out += "</p>";
    if let Some(link) = sheet.link.as_deref().filter(|_| sheet.visibility != Visibility::Members) {
        out += &format!(
            "<h2>Link</h2><p><input id=\"link\" readonly value=\"{}\"> <button type=\"button\" data-copy=\"link\">Copy</button></p>",
            esc(link)
        );
        out += &action_form(sheet, "rotate", "", "New link", "quiet");
        out += "<p class=\"hint\">A new link stops the old one working.</p>";
    }
    if let Some(granted) = sheet.frame {
        out += "<h2>Your fragments inside it</h2>";
        out += frame_hint(sheet.platform_desktop, granted);
        let field = format!("<input type=\"hidden\" name=\"granted\" value=\"{}\">", if granted { "no" } else { "yes" });
        out += &action_form(sheet, "frame", &field, if granted { "Stop" } else { "Allow" }, if granted { "quiet" } else { "" });
    }
    out
}

async fn sheet(req: &Request, env: &Env, cfg: &Config, url: &Url, name: &str) -> CellResult<Response> {
    let platform = cfg.platform(url);
    let Some((session, live)) = auth::platform_session(req, env, url).await? else { return auth::to_login(&platform, &format!("/share/{name}")) };
    let who = Signed::new(live.identity, None);
    shown(env, cfg, url, name, &who, &session, None, 200).await
}

/// The sheet as it is now, with `flash` above it; a person it is not
/// shown to gets a refusal page.
#[allow(clippy::too_many_arguments)]
async fn shown(env: &Env, cfg: &Config, url: &Url, name: &str, who: &Signed, session: &str, flash: Option<Flash>, status: u16) -> CellResult<Response> {
    let title = format!("Share {}", label(name));
    match load(env, cfg, url, name, who, session).await {
        Ok(sheet) => sheet_page(status, &title, &render(&sheet, flash), true),
        Err(e) if matches!(e.code, ErrorCode::Forbidden | ErrorCode::Unauthenticated) => {
            notice(403, "Not yours to share", &format!("You are not in {}, so it is not yours to share.", esc(label(name))))
        }
        Err(e) if e.code == ErrorCode::NotFound => notice(404, "No such fragment", &format!("There is no {}.", esc(name))),
        Err(e) => Err(e),
    }
}

fn form_fields(bytes: &[u8]) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(bytes).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// A POST's person, once its Origin and form token hold: the token is
/// checked against the session cookie before the registry is asked who it
/// names.
pub(crate) async fn poster(req: &mut Request, env: &Env, url: &Url, platform: &str, purpose: &str) -> CellResult<Result<(String, Signed, BTreeMap<String, String>), Response>> {
    auth::same_origin(req, platform)?;
    let bytes = crate::read_body(req, FORM_MAX_BYTES).await?;
    let fields = form_fields(&bytes);
    let Some(session) = auth::cookie_of(req, auth::SESSION_COOKIE, auth::secure(url), "/")? else {
        return Ok(Err(notice(401, "Sign in first", "This page's session ended. Sign in, then open it again.")?));
    };
    if let Err(refused) = form::check(&session, purpose, fields.get("form").map_or("", String::as_str), js::now_ms()) {
        return Ok(Err(notice(403, "Not sent", refused.message())?));
    }
    match ask_registry(env, &calls::Session { token: session.clone(), fragment: None, frame: false }).await {
        Ok(live) => Ok(Ok((session, Signed::new(live.identity, None), fields))),
        Err(e) if e.code == ErrorCode::Unauthenticated => Ok(Err(notice(401, "Sign in first", "This page's session ended. Sign in, then open it again.")?)),
        Err(e) => Err(e),
    }
}

async fn share_post(mut req: Request, env: &Env, cfg: &Config, url: &Url, name: &str) -> CellResult<Response> {
    let platform = cfg.platform(url);
    let (session, who, fields) = match poster(&mut req, env, url, &platform, &purpose("share", name)).await? {
        Ok(posted) => posted,
        Err(page) => return Ok(page),
    };
    let field = |k: &str| fields.get(k).map_or("", |v| v.trim());
    let member = || match field("member") {
        m if npub::is_identity(m) => Ok(m.to_string()),
        m => Err(CellError::invalid(format!("{m:?} is not a member"))),
    };
    let role = || match Role::parse(field("role")) {
        Some(r @ (Role::Viewer | Role::Editor)) => Ok(r),
        _ => Err(CellError::invalid("a role is viewer or editor")),
    };
    let done = match field("action") {
        "invite" => match invite(env, url, name, &who, field("username"), role()).await {
            Ok((username, role, token)) => {
                let link = format!("{platform}/join/{name}?token={token}");
                return shown(env, cfg, url, name, &who, &session, Some(Flash::Invited { who: username, role, link }), 200).await;
            }
            Err(e) => Err(e),
        },
        "role" => match (member(), role()) {
            (Ok(m), Ok(r)) => ask(env, url, name, &who, Method::Put, &format!("/api/members/{m}"), Some(json!({ "role": r }))).await,
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        "remove" => match member() {
            Ok(m) => ask(env, url, name, &who, Method::Delete, &format!("/api/members/{m}"), None).await,
            Err(e) => Err(e),
        },
        "uninvite" => match field("invite") {
            id if id.len() == INVITE_ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit()) => {
                ask(env, url, name, &who, Method::Delete, &format!("/api/invites/{id}"), None).await
            }
            _ => Err(CellError::invalid("name an invite")),
        },
        "visibility" => match Visibility::parse(field("visibility")) {
            Some(v) => ask(env, url, name, &who, Method::Put, "/api/visibility", Some(json!({ "visibility": v }))).await,
            None => Err(CellError::invalid("choose who can open it")),
        },
        // the share link only: the inbox's token and the webhook's secret are
        // integrations', rotated with the CLI
        "rotate" => ask(env, url, name, &who, Method::Post, "/api/rotate", Some(json!({ "scopes": ["view"] }))).await,
        "frame" => ask(env, url, name, &who, Method::Put, "/api/grants/frame", Some(json!({ "granted": field("granted") == "yes" }))).await,
        _ => Err(CellError::invalid("no such change")),
    };
    match done {
        Ok(_) => Ok(auth::redirect(&format!("/share/{name}"), &[])?.with_status(303)),
        Err(e) if matches!(e.code, ErrorCode::InvalidRequest | ErrorCode::NotFound | ErrorCode::AlreadyExists | ErrorCode::Forbidden) => {
            let status = e.code.status();
            shown(env, cfg, url, name, &who, &session, Some(Flash::Refused(e.message)), status).await
        }
        Err(e) => Err(e),
    }
}

/// Invites the person holding `username`, for them alone: the username as
/// they hold it, the role, and the invite's token.
async fn invite(env: &Env, url: &Url, name: &str, who: &Signed, username: &str, role: CellResult<Role>) -> CellResult<(String, Role, String)> {
    let role = role?;
    let username = username.trim_start_matches('@').to_ascii_lowercase();
    if !fragment_proto::valid_username(&username) {
        return Err(CellError::invalid(format!("{username:?} is not a username")));
    }
    let holder = match ask_registry(env, &calls::FindUsername { username: username.clone() }).await {
        Err(e) if e.code == ErrorCode::NotFound => return Err(CellError::new(ErrorCode::NotFound, format!("no one is @{username}"))),
        found => found?,
    };
    if holder.identity.id == who.id {
        return Err(CellError::invalid("that is you: you are in already"));
    }
    let made = ask(env, url, name, who, Method::Post, "/api/invites", Some(json!({ "role": role, "uses": 1, "invitee": holder.identity.id }))).await?;
    let made: Invite = decoded(made, "the invite")?;
    let token = made.token.ok_or_else(|| CellError::host("an invite made answered no token"))?;
    Ok((username, role, token))
}

/// The invite a join page's URL names: its token, if it is one.
fn invite_token(url: &Url) -> Option<String> {
    auth::query(url, "token").filter(|t| t.len() == INVITE_TOKEN_LEN && t.bytes().all(|b| b.is_ascii_hexdigit()))
}

async fn join_page(req: &Request, env: &Env, cfg: &Config, url: &Url, name: &str) -> CellResult<Response> {
    let platform = cfg.platform(url);
    let Some(token) = invite_token(url) else { return notice(400, "Not an invite", "This link holds no invite. Ask for it again.") };
    let Some((session, live)) = auth::platform_session(req, env, url).await? else {
        return auth::to_login(&platform, &format!("/join/{name}?token={token}"));
    };
    let who = Signed::new(live.identity, None);
    let preview = match ask(env, url, name, &who, Method::Post, "/api/join/preview", Some(json!({ "token": token }))).await {
        Ok(p) => p,
        Err(e) if e.code == ErrorCode::NotFound => {
            return notice(404, "This invite is not good", "It was used, revoked, or it expired (or its fragment was deleted). Ask for a new one.")
        }
        Err(e) => return Err(e),
    };
    let role: Role = decoded(preview["role"].clone(), "the invite's role")?;
    let current: Option<Role> = decoded(preview["current"].clone(), "the person's role")?;
    let invited_by = preview["invitedBy"].as_str().unwrap_or_default().to_string();
    let invitee = preview["invitee"].as_str().map(str::to_string);
    let ids = [Some(invited_by.clone()), invitee.clone(), Some(who.id.clone())].into_iter().flatten().collect();
    let names = profiles(env, ids).await;
    let me = person(&who.id, names.get(&who.id), "");
    let title = format!("Join {}", label(name));
    if let Some(invitee) = invitee.filter(|i| *i != who.id) {
        let them = person(&invitee, names.get(&invitee), "");
        return sheet_page(
            403,
            &title,
            &format!("<p>This invite is for someone else:</p><ul class=\"people\"><li>{them}</li></ul><p>You are signed in as:</p><ul class=\"people\"><li>{me}</li></ul><p><a href=\"/auth/logout\">Sign out</a></p>"),
            true,
        );
    }
    let open = format!("/auth/fragment?name={}&return=/", enc(name));
    if current.is_some_and(|c| c >= role) {
        let c = current.map_or("", Role::as_str);
        return sheet_page(200, &title, &format!("<p>You are in {} already, as {c}.</p><p><a href=\"{}\">Open it</a></p>", esc(label(name)), esc(&open)), true);
    }
    let inviter = person(&invited_by, names.get(&invited_by), "");
    let form = form::issue(&session, &purpose("join", name), js::now_ms());
    let body = format!(
        "<ul class=\"people\"><li>{inviter}</li></ul><p>invites you to <b>{l}</b> as {r}.</p>\
         <form method=\"post\"><input type=\"hidden\" name=\"form\" value=\"{f}\"><input type=\"hidden\" name=\"token\" value=\"{t}\"><button data-arm disabled>Join {l}</button></form>\
         <p class=\"hint\">Signed in as {me_text}. Didn't expect this? Close this page.</p>",
        l = esc(label(name)),
        r = role_phrase(role),
        f = esc(&form),
        t = esc(&token),
        me_text = match names.get(&who.id).and_then(|p| p.username.as_deref()) {
            Some(u) => format!("@{}", esc(u)),
            None => format!("<code>{}</code>", esc(&who.id)),
        },
    );
    // no forms-here rule: joining redirects on to the fragment's origin
    sheet_page(200, &title, &body, false)
}

async fn join_post(mut req: Request, env: &Env, cfg: &Config, url: &Url, name: &str) -> CellResult<Response> {
    let platform = cfg.platform(url);
    let (_, who, fields) = match poster(&mut req, env, url, &platform, &purpose("join", name)).await? {
        Ok(posted) => posted,
        Err(page) => return Ok(page),
    };
    let token = fields.get("token").filter(|t| t.len() == INVITE_TOKEN_LEN && t.bytes().all(|b| b.is_ascii_hexdigit()));
    let Some(token) = token else { return notice(400, "Not an invite", "This form holds no invite.") };
    match ask(env, url, name, &who, Method::Post, "/api/join", Some(json!({ "token": token }))).await {
        // signed in on the fragment's own origin, and there
        Ok(_) => Ok(auth::redirect(&format!("/auth/fragment?name={}&return=/", enc(name)), &[])?.with_status(303)),
        Err(e) if e.code == ErrorCode::NotFound => notice(404, "This invite is not good", "It was used, revoked, or it expired. Ask for a new one."),
        Err(e) if matches!(e.code, ErrorCode::Forbidden | ErrorCode::InvalidRequest) => notice(e.code.status(), "Not joined", &esc(&e.message)),
        Err(e) => Err(e),
    }
}
