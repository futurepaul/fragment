//! The fragment platform on Cloudflare Workers, in Rust.
//!
//! The router (this file's `fetch`) verifies NIP-98, bounds request
//! bodies, decides which fragment a request is for, and hands it to that
//! fragment's supervisor (`fragment.rs`). On the control API it asks the
//! registry (`registry.rs`) which identity the signing key belongs to first,
//! and hands on the identity and the key; a signed request the registry
//! cannot answer for is refused (503), never let through. A site request's
//! signer or session goes on unresolved (`routed::Credential`): the
//! fragment asks the registry only when its answer depends on who is
//! asking, so a page anyone who may see it gets alike costs no hop. The
//! public routes are docs/api.md's.
//!
//! A browser on a fragment's origin is its person through that origin's own
//! session cookie (`__signin`), looked up live like a key when it matters.
//! Which of a browser's cookies count is decided here, from the Fetch
//! Metadata it sends (`Fetched`): another fragment's page is one site with
//! this one, so only those headers say whose page asked. A socket, which
//! has no CORS, is taken only from the fragment's own page
//! (`own_page_socket`), and its cookies count only when it names one.
//!
//! With a suffix configured, `/f/<name>/…` redirects to the fragment's own
//! host: fragments sharing one origin could act as each other's visitors.
//! `__watch` and `__live` stay reachable there for the CLI, which carries
//! no cookies.
//!
//! When the suffix moves (`FRAGMENT_LEGACY_HOST_SUFFIX`: fragment.club's
//! fragments to fragment.boats, the platform staying on fragment.club), a
//! fragment's old host sends a browser to its new one, and the suffix's own
//! name sends it to the platform.

mod agents;
mod ai;
mod auth;
mod blobs;
mod card;
mod config;
mod connections;
mod channels;
mod computer;
mod deliveries;
mod cs;
mod error;
mod files;
mod fragment;
mod jobs;
mod js;
mod keys;
mod ledger;
mod levers;
mod live;
mod members;
mod meter;
mod models;
mod ops;
mod plane;
mod principal;
mod publish;
mod push;
mod registry;
mod routed;
mod search;
mod runs_on;
mod serve;
mod share;
mod shell;
mod subscriptions;

use fragment_core::access;
use fragment_core::body::{LimitedBody, TooLarge};
use fragment_core::frames::{self, Framed};
use fragment_core::npub;
use fragment_nip98::Payload;
use fragment_proto::{limits, valid_fragment_name, CreateFragment, ErrorBody, ErrorCode, IdentityKind, Register};
use futures_util::TryStreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use config::Config;
use error::{CellError, CellResult};
use registry::calls::{self, Call};
use routed::{Credential, Mode, Routed, Signed};

pub use computer::{ComputerCell, ComputerEgress};
pub use fragment::FragmentCell;
pub use principal::PrincipalCell;
pub use ledger::LedgerCell;
pub use registry::RegistryCell;

/// Client headers a fragment's supervisor sees; everything else, and any
/// `x-fragment-*` a client sends, stays at the router.
const PASSED_HEADERS: [&str; 9] =
    ["content-type", "cookie", "origin", "accept", "if-none-match", "upgrade", "x-pierre-event", "x-pierre-signature", "range"];

/// A WebSocket upgrade's own handshake, passed too, so the fragment's
/// Durable Object accepts the upgrade the client asked for.
const WEBSOCKET_HEADERS: [&str; 4] = ["sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"];

/// `PUT /api/fragments/{name}/archived`'s body, `{archived}`, is a few bytes.
const ARCHIVED_BODY_MAX_BYTES: usize = 1024;

#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> Result<()> {
    // a branch deployment's queue is named for its branch after this
    if batch.queue().starts_with(meter::QUEUE) {
        return meter::consume(batch, env).await;
    }
    deliveries::consume(batch, env).await
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    match route(req, &env, &ctx).await {
        Ok(resp) => Ok(resp),
        Err(e) => e.response(),
    }
}

/// A request body of at most `max` bytes, read as it arrives: a declared
/// length over `max` is refused unread, and a body without one (chunked)
/// is refused at the chunk that crosses `max`, so an upload never fills
/// the router's memory before it is measured.
pub(crate) async fn read_body(req: &mut Request, max: usize) -> CellResult<Vec<u8>> {
    let too_large = |e: TooLarge| CellError::too_large("request body", e.bytes, e.max);
    let declared: Option<usize> = req.headers().get("content-length")?.and_then(|l| l.parse().ok());
    let mut body = LimitedBody::new(max, declared).map_err(too_large)?;
    if req.inner().body().is_none() {
        return Ok(body.finish());
    }
    let mut stream = req.stream()?;
    // bounded: LimitedBody refuses the chunk that would cross `max`, and the read stops
    while let Some(chunk) = stream.try_next().await? {
        body.push(&chunk).map_err(too_large)?;
    }
    Ok(body.finish())
}

/// The header the shell's own requests carry: a request from another
/// origin cannot send it without a preflight the platform never answers.
pub const SHELL_HEADER: &str = "x-fragment-shell";

/// Who asks an API request, unresolved: the key that signed it (NIP-98),
/// or, from the platform's own page (the shell), the person's platform
/// session.
enum Caller {
    Key(String),
    Session(String),
}

/// A request's caller: its signature when it has one; else the platform
/// session, only for the shell's own requests (`shell_session`); else the
/// unsigned request's 401.
fn caller(env: &Env, req: &Request, url: &Url, payload: Payload<'_>) -> CellResult<Caller> {
    if req.headers().get("authorization")?.is_none() {
        if let Some(token) = shell_session(Config::from_env(env), req, url)? {
            return Ok(Caller::Session(token));
        }
    }
    authenticate(req, url, payload).map(Caller::Key)
}

/// The platform session of a request from the shell: on the platform's
/// host, `Sec-Fetch-Site: same-origin` (a fragment's page is one site with
/// the platform where they share a zone, and its fetch carries the Lax
/// cookie; only the fetch metadata tells them apart), the shell's header,
/// and for a write the platform's exact Origin.
fn shell_session(cfg: &Config, req: &Request, url: &Url) -> CellResult<Option<String>> {
    let host = url.host_str().unwrap_or_default();
    if !cfg.is_platform_host(host) || req.headers().get(SHELL_HEADER)?.as_deref() != Some("1") {
        return Ok(None);
    }
    if req.headers().get("sec-fetch-site")?.as_deref() != Some("same-origin") {
        return Ok(None);
    }
    if !matches!(req.method(), Method::Get | Method::Head) {
        let platform = cfg.platform(url);
        if req.headers().get("origin")?.is_none_or(|o| o.trim_end_matches('/') != platform) {
            return Ok(None);
        }
    }
    auth::platform_session_token(req, url)
}

/// The key that signed the request (NIP-98), not yet resolved.
fn authenticate(req: &Request, url: &Url, payload: Payload<'_>) -> CellResult<String> {
    let header = req.headers().get("authorization")?;
    let now_s = js::now_ms() / 1000;
    fragment_nip98::verify_request(header.as_deref(), req.method().as_ref(), url, payload, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::new(ErrorCode::Unauthenticated, e.to_string()))
}

/// Asks the registry cell one of its calls (`registry/calls.rs`: the path,
/// the body, and the answer are one definition both ends compile against).
/// Its refusals pass through; not reaching it, a failure inside it, or an
/// answer that does not decode is `registry_unavailable`: nothing signed
/// is decided without it (docs/finite-integration.md, rule 7).
pub(crate) async fn ask_registry<C: Call>(env: &Env, call: &C) -> CellResult<C::Answer> {
    let unavailable = |why: String| CellError::new(ErrorCode::RegistryUnavailable, format!("the identity registry did not answer ({why}); try again shortly"));
    let body = serde_json::to_string(call).map_err(|e| CellError::host(format!("a registry call: {e}")))?;
    let ask = || async {
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(body.as_str().into()));
        let req = Request::new_with_init(&format!("https://registry.internal{}", C::PATH), &init)?;
        let mut resp = env.durable_object("REGISTRY")?.get_by_name(registry::NAME)?.fetch_with_request(req).await?;
        let status = resp.status_code();
        let bytes = resp.bytes().await?;
        Ok::<_, worker::Error>((status, bytes))
    };
    // Asked once: a throw may come after the registry acted (a failure
    // inside it, a connection dropped mid-answer), and its calls are not
    // idempotent (a second Mint is a second redemption, a second
    // ClaimUsername answers "taken" to the person who got the name).
    let (status, bytes) = ask().await.map_err(|e| unavailable(e.to_string()))?;
    if status == 200 {
        let answer = serde_json::from_slice::<C::Answer>(&bytes).map_err(|e| unavailable(format!("its answer: {e}")))?;
        return C::checked(answer);
    }
    match serde_json::from_slice::<ErrorBody>(&bytes) {
        Ok(e) if status < 500 => Err(CellError::new(e.error, e.message)),
        Ok(e) => Err(unavailable(e.message)),
        Err(_) => Err(unavailable(format!("status {status}"))),
    }
}

/// The identity a request's signed URL names in `for`: an agent acting
/// for whoever asked it (ROADMAP decision 17). At most one, an identity.
fn acting_for(url: &Url) -> CellResult<Option<String>> {
    let mut named = url.query_pairs().filter(|(k, _)| k == "for").map(|(_, v)| v.into_owned());
    let first = named.next();
    if named.next().is_some() {
        return Err(CellError::invalid("`for` is named once"));
    }
    match first {
        Some(id) if !npub::is_identity(&id) => Err(CellError::invalid(format!("`for` names an identity (id:…), not {id:?}"))),
        first => Ok(first),
    }
}

/// The signer of a request that must be signed, resolved, with the body
/// the router read. It acts as itself: `for` is honored on a fragment's
/// routes only (`signer_for`).
pub(crate) async fn signer(env: &Env, req: &Request, url: &Url, body: &[u8]) -> CellResult<Signed> {
    let who = signer_of(env, req, url, Payload::Read(body)).await?;
    if who.acting_for.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    Ok(who)
}

/// `signer`, honoring `for`: an agent's request acts for the identity it
/// names, capped (the fragment decides: `fragment_core::access`).
pub(crate) async fn signer_for(env: &Env, req: &Request, url: &Url, body: &[u8]) -> CellResult<Signed> {
    signer_of(env, req, url, Payload::Read(body)).await
}

/// The signer for any payload: a body the router read, or one it streams
/// through unread (a blob, whose URL names its hash). `for` is inside the
/// signed URL, and only an agent may name it: a person acts as themselves.
async fn signer_of(env: &Env, req: &Request, url: &Url, payload: Payload<'_>) -> CellResult<Signed> {
    let acting_for = acting_for(url)?;
    let (identity, key) = match caller(env, req, url, payload)? {
        Caller::Key(key) => (ask_registry(env, &calls::Resolve { key: key.clone() }).await?, Some(key)),
        // the shell's: a person, signed in on the platform
        Caller::Session(token) => (ask_registry(env, &calls::Session { token, fragment: None, frame: false }).await?.identity, None),
    };
    if acting_for.is_some() && (identity.kind != IdentityKind::Agent || identity.owner.is_none()) {
        return Err(CellError::new(ErrorCode::Forbidden, "only an agent acts for someone (`for`); a person acts as themselves"));
    }
    Ok(Signed { identity, key, acting_for })
}

/// Who is asking a site request, unresolved: a signature names its key
/// (verified here, which needs no registry: a bad one is still 401); a
/// browser, its session on this origin, as far as its cookies count
/// (`Fetched`), a frame's navigation by its frame cookie first. `payload`
/// is the body a signature covers (a blob upload's, streamed: its hash).
fn site_credential(req: &Request, url: &Url, payload: Payload<'_>, name: &str, mode: Mode, fetched: Fetched) -> CellResult<Option<Credential>> {
    if req.headers().get("authorization")?.is_some() {
        return Ok(Some(Credential::Key(authenticate(req, url, payload)?)));
    }
    let path_mode = mode == Mode::Path;
    let site = if fetched.site { auth::site_token(req, name, url, path_mode)?.map(Credential::Session) } else { None };
    let frame = if fetched.frame { auth::frame_token(req, name, url, path_mode)?.map(Credential::Frame) } else { None };
    Ok(if fetched.framed { frame.or(site) } else { site.or(frame) })
}

fn is_socket(req: &Request) -> CellResult<bool> {
    Ok(req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")))
}

/// A WebSocket has no CORS: a browser opens one from any page to any host,
/// and every fragment's origin is one site with the others, so a page on
/// another fragment's origin (its author's code, or an agent's) would bring
/// this origin's cookies along (its session, its share link, its visitor)
/// and read the fragment as them. A browser names the page on every
/// upgrade (`Origin`), so a socket is taken only from the fragment's own
/// page: another origin, `null` included, is refused before anything of
/// the visitor's is read.
fn own_page_socket(req: &Request, cfg: &Config, url: &Url, name: &str) -> CellResult<()> {
    if !is_socket(req)? {
        return Ok(());
    }
    match req.headers().get("origin")? {
        Some(o) if o != cfg.origin(url, name) => Err(CellError::new(ErrorCode::Forbidden, "a fragment's socket opens from its own page")),
        _ => Ok(()),
    }
}

/// Which of a browser's cookies count on a fragment's origin, from the
/// Fetch Metadata it sends (`Sec-Fetch-*`, which no page's script sets;
/// docs/fragment-boats.md, decision 3). Every fragment is one site with
/// the others, so a SameSite=Lax cookie rides along on another fragment's
/// images, scripts, fetches, and forms; these headers say whose page asked:
///
/// - the fragment's own page (`same-origin`): all of them;
/// - a top-level navigation (GET or HEAD, from anywhere): the origin's own
///   (`fragment_site`, `fragview`, `fragment_anon`), as Lax means them;
/// - a frame's navigation (an `iframe`, or an `object` or `embed`, which
///   show a page too): the frame cookie (`/auth/frame`'s), and the answer
///   shows only in the page that framed it (`bound`);
/// - anything else from another page (an image, a script, a fetch, a form
///   or a POST navigation): none, so it is served as to a stranger;
/// - no Fetch Metadata (a browser from before 2023, or not a browser): the
///   origin's own, as before;
/// - a socket: those of a page that names itself (`own_page_socket` took
///   it only from this one's); an upgrade that names no page is no
///   browser's, so its cookies are no one's (the CLI signs instead).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fetched {
    /// The origin's own cookies count.
    pub site: bool,
    /// The frame cookie counts.
    pub frame: bool,
    /// A frame's navigation.
    pub framed: bool,
    /// A navigation: a top-level one or a frame's, or one without Fetch Metadata.
    pub navigation: bool,
}

pub(crate) fn fetched(req: &Request) -> CellResult<Fetched> {
    if is_socket(req)? {
        let named = req.headers().get("origin")?.is_some();
        return Ok(Fetched { site: named, frame: named, framed: false, navigation: false });
    }
    let header = |k: &str| req.headers().get(k).map(Option::unwrap_or_default);
    let site = header("sec-fetch-site")?;
    if site.is_empty() {
        return Ok(Fetched { site: true, frame: false, framed: false, navigation: true });
    }
    let get = matches!(req.method(), Method::Get | Method::Head);
    let dest = header("sec-fetch-dest")?;
    let framed = get && matches!(dest.as_str(), "iframe" | "frame" | "object" | "embed");
    let top = get && dest == "document" && header("sec-fetch-mode")? == "navigate";
    let own = site == "same-origin";
    Ok(Fetched { site: own || top, frame: own || framed, framed, navigation: top || framed })
}

/// A navigation's answer: it differs by the kind of navigation (`Vary`),
/// and a frame's (`ancestors`: `fragment_core::frames::ancestors`) shows
/// only in the pages that names: the platform's for a frame session its
/// mint made (`/auth/frame`), this origin's own, or, for a stranger's
/// answer, those and the platform's. It is never reused from a cache
/// without that. An app's own policy stays: a second one only narrows it.
fn bound(resp: Response, ancestors: Option<&str>) -> CellResult<Response> {
    let h = resp.headers().clone();
    h.append("vary", "sec-fetch-dest")?;
    if let Some(ancestors) = ancestors {
        h.append("content-security-policy", &format!("frame-ancestors {ancestors}"))?;
        h.set("cache-control", "private, no-cache")?;
    }
    Ok(resp.with_headers(h))
}

/// The identity a path names: `None` for `me`, whoever signed.
fn named_identity(who: &str) -> CellResult<Option<String>> {
    if who == "me" {
        return Ok(None);
    }
    if npub::is_identity(who) {
        return Ok(Some(who.to_string()));
    }
    Err(CellError::invalid(format!("{who:?} is not an identity (id:…) or `me`")))
}

/// An operator's undo of a username taken by mistake (it was chosen once,
/// and URLs name it): refused while its person owns a fragment under it.
async fn release_username(env: &Env, username: &str) -> CellResult<Response> {
    let holder = ask_registry(env, &calls::FindUsername { username: username.to_string() }).await?;
    let list = Request::new("https://principal.internal/list", Method::Get)?;
    let listed: fragment_proto::FragmentList = env.durable_object("PRINCIPAL")?.get_by_name(&holder.identity.id)?.fetch_with_request(list).await?.json().await?;
    let owned: Vec<&str> = listed
        .fragments
        .iter()
        .filter(|f| f.role == fragment_proto::Role::Owner)
        .map(|f| f.name.as_str())
        .filter(|n| fragment_proto::split_fragment_name(n).is_some_and(|(_, u)| u == username))
        .collect();
    if !owned.is_empty() {
        return Err(CellError::new(ErrorCode::AlreadyExists, format!("{username} owns fragments under it ({}): its URLs name it", owned.join(", "))));
    }
    json_answer(&ask_registry(env, &calls::ReleaseUsername { username: username.to_string() }).await?)
}

/// Makes a fragment for a person, under their username: the API's create
/// (the shell's catalog calls it), the one door every fragment is made
/// through (an agent's or a template's included). An agent makes one for
/// its owner: the owner's (billed to them, in their list), under their
/// username, with the agent an editor of it. Its maker's ledger is asked
/// first: a guest makes none (Paul, 2026-10-03), nor does someone whose
/// fragments are read-only past the overdraft.
pub(crate) async fn create_fragment(env: &Env, cfg: &Config, url: &Url, mut create: CreateFragment, principal: Signed) -> CellResult<Response> {
    let (maker, agent) = match principal.kind {
        IdentityKind::Person => (principal, None),
        IdentityKind::Agent => {
            let owner = principal.owner.clone().ok_or_else(|| CellError::host(format!("{} {} has no owner", principal.kind.as_str(), principal.id)))?;
            let identity = fragment_proto::Identity { id: owner, kind: IdentityKind::Person, owner: None, username: principal.username.clone(), held: None };
            (Signed::new(identity, None), Some(principal.identity.id))
        }
    };
    assert_eq!(maker.kind, IdentityKind::Person, "a fragment is a person's: an agent's maker is its owner");
    let username = maker.username.clone().ok_or_else(|| CellError::invalid(format!("choose a username first (sign in at {}/)", cfg.platform(url))))?;
    create.name = qualify(&create.name, &username)?;
    may_create(env, &maker.id).await?;
    let body = serde_json::to_vec(&create).map_err(|e| CellError::host(e.to_string()))?;
    // a fresh request: nothing of the caller's but what the router decided
    let bare = Request::new(url.as_str(), Method::Post)?;
    let routed = Routed { name: create.name.clone(), url: url.clone(), mode: None, signed: Some(maker.clone()), credential: None };
    let made = forward(env, &bare, bytes_body(body), Forward { routed, inner: "/create".into(), extra: vec![] }).await?;
    if let (Some(agent), 200) = (agent, made.status_code()) {
        let put = Request::new(url.as_str(), Method::Put)?;
        let role = serde_json::to_vec(&json!({ "role": "editor" })).map_err(|e| CellError::host(e.to_string()))?;
        let routed = Routed { name: create.name.clone(), url: url.clone(), mode: None, signed: Some(maker), credential: None };
        let mut added = forward(env, &put, bytes_body(role), Forward { routed, inner: format!("/api/members/{agent}"), extra: vec![] }).await?;
        if added.status_code() != 200 {
            return Err(CellError::host(format!("{} was made, but its agent was not made an editor: {}", create.name, added.text().await.unwrap_or_default())));
        }
    }
    Ok(made)
}

/// `GET /api/fragments/watch`: a socket on which the caller's list says
/// it changed (principal.rs, Watching). A key signs it (the CLI's); the
/// shell opens it with the platform session, which a socket carries
/// without the shell's header (a browser sets none on an upgrade), so it
/// counts only on the platform's host from the platform's own page: a
/// socket has no CORS, and a browser names its page on every upgrade
/// (`Origin`). One from any other page, a fragment's (one site with the
/// platform, so its cookie rides along) or none, is no one's.
async fn watch_list(req: &Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    if !is_socket(req)? {
        return Err(CellError::invalid("/api/fragments/watch is a WebSocket; send Upgrade: websocket"));
    }
    let identity = match req.headers().get("authorization")? {
        Some(_) => signer(env, req, url, &[]).await?.identity.id,
        None => {
            let platform = cfg.platform(url);
            let own_page = cfg.is_platform_host(url.host_str().unwrap_or_default())
                && req.headers().get("origin")?.is_some_and(|o| o.trim_end_matches('/') == platform);
            let token = if own_page { auth::platform_session_token(req, url)? } else { None };
            let token = token.ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "sign in: a list is watched from the platform's own page, or signed"))?;
            ask_registry(env, &calls::Session { token, fragment: None, frame: false }).await?.identity.id
        }
    };
    // a fresh request: the upgrade's handshake, nothing else of the caller's
    let headers = Headers::new();
    headers.set("upgrade", "websocket")?;
    headers.set("connection", "Upgrade")?;
    for k in WEBSOCKET_HEADERS {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let watch = Request::new_with_init("https://principal.internal/watch", &init)?;
    Ok(env.durable_object("PRINCIPAL")?.get_by_name(&identity)?.fetch_with_request(watch).await?)
}

/// Whether `maker`'s ledger lets them make a fragment (`Spend::Create`):
/// its refusal is theirs to read, 403 for a guest and 402 past the
/// overdraft. A ledger that does not answer refuses nothing, as a write's
/// does (meter.rs `writable`): making a fragment is the product, and an
/// outage lets at most a guest's fragment through, billed nothing.
async fn may_create(env: &Env, maker: &str) -> CellResult<()> {
    let may = ledger::MaySpend { spend: fragment_core::ledger::Spend::Create, fragment: None, by_owner: true };
    match ledger::ask(env, maker, &may).await {
        Ok(_) => Ok(()),
        Err(e) if e.refused.is_some() => Err(CellError::new(e.code, e.message)),
        Err(e) => {
            console_error!("{}", json!({ "event": "create.standing-unknown", "maker": maker, "message": e.message }));
            Ok(())
        }
    }
}

fn qualify(name: &str, username: &str) -> CellResult<String> {
    if fragment_proto::valid_label(name) {
        return Ok(fragment_proto::fragment_name(name, username));
    }
    match fragment_proto::split_fragment_name(name) {
        Some((_, u)) if u == username => Ok(name.to_string()),
        Some(_) => Err(CellError::new(ErrorCode::Forbidden, format!("you make fragments under your own username ({username})"))),
        None => Err(CellError::invalid(
            "a fragment's name is a label (lowercase letters, digits, and single dashes, at most 63), optionally followed by .<your username>",
        )),
    }
}

/// A picture's type from its first bytes (a request's content-type is not trusted).
pub(crate) fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// `GET /api/users/<username>` and `.../picture`: anyone may see who a
/// username is, and their picture.
async fn users(env: &Env, rest: &[&str]) -> CellResult<Response> {
    let [username, tail @ ..] = rest else { return Err(CellError::new(ErrorCode::NotFound, "name a username")) };
    if !fragment_proto::valid_username(username) {
        return Err(CellError::new(ErrorCode::NotFound, format!("no one is {username}")));
    }
    let holder = ask_registry(env, &calls::FindUsername { username: username.to_string() }).await?;
    match tail {
        [] => {
            let picture = holder.picture.as_ref().map(|p| format!("/api/users/{username}/picture?v={}", &p.sha[..12]));
            json_answer(&json!({ "id": holder.identity.id, "kind": holder.identity.kind, "username": username, "picture": picture }))
        }
        ["picture"] => {
            let Some(picture) = holder.picture else {
                return Err(CellError::new(ErrorCode::NotFound, format!("{username} has no picture")));
            };
            let blob =
                js::blob_get(env.as_ref(), &format!("pictures/{}", picture.sha), None).await?.ok_or_else(|| CellError::host("a picture's bytes are missing"))?;
            let headers = Headers::new();
            headers.set("content-type", &picture.mime)?;
            headers.set("cache-control", "public, max-age=300")?;
            headers.set("x-content-type-options", "nosniff")?;
            Ok(Response::from_body(ResponseBody::Stream(blob.body))?.with_headers(headers))
        }
        _ => Err(CellError::new(ErrorCode::NotFound, "no such route")),
    }
}

fn key_in_path(k: &str) -> CellResult<String> {
    npub::parse(k).ok_or_else(|| CellError::invalid(format!("{k:?} is not an npub or a 64-hex key")))
}

/// The key a key proof in a body proves, for this request and its signer.
fn proven_key(proof: &str, req: &Request, url: &Url, signer_key: &str) -> CellResult<String> {
    let now_s = js::now_ms() / 1000;
    let key = fragment_nip98::verify_proof(proof, req.method().as_ref(), url.as_str(), signer_key, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::invalid(format!("proof: {e}")))?;
    if key == signer_key {
        return Err(CellError::invalid("proof: the new key must not be the key that signs the request"));
    }
    Ok(key)
}

fn json_answer<T: serde::Serialize>(v: &T) -> CellResult<Response> {
    Ok(Response::from_json(v)?)
}

/// Whose ledger a signer reads: a person's own; an agent's owner's.
fn payer_of(who: &Signed) -> CellResult<String> {
    match who.kind {
        IdentityKind::Person => Ok(who.id.clone()),
        IdentityKind::Agent => who.owner.clone().ok_or_else(|| CellError::host("an agent without an owner")),
    }
}

/// An operator's command id, kept apart from every other kind's on the
/// person's ledger (`<kind>:<id>`): a grant's `g1` is not a plan's.
fn commanded(kind: &str, id: &str) -> CellResult<String> {
    if !fragment_core::price::printable(id, LEDGER_ID_MAX_BYTES) {
        return Err(CellError::invalid(format!("a command's id is 1-{LEDGER_ID_MAX_BYTES} printable ASCII characters, no spaces")));
    }
    Ok(format!("{kind}:{id}"))
}

/// A command's id as an operator names it (`commanded` prefixes it).
const LEDGER_ID_MAX_BYTES: usize = 128;

/// An operator command's body, as proto's type for it (unknown fields refused).
fn command_body<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> CellResult<T> {
    serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("{what}: {e}")))
}

/// `/api/ledger…` (docs/ledger.md; docs/api.md, Ledger): `GET` reads the
/// signer's (an agent's: its owner's); the deployment's operators grant
/// credit and set a person's plan, seat and overdraft, the person named by
/// username or identity.
async fn ledger_route(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let who = signer(env, &req, url, &body).await?;
    if let (Method::Get, []) = (req.method(), rest) {
        return json_answer(&ledger::ask(env, &payer_of(&who)?, &ledger::Status {}).await?);
    }
    let (Method::Post, [person, command]) = (req.method(), rest) else {
        return Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", req.method().as_ref(), url.path())));
    };
    if !cfg.is_operator(who.key.as_deref(), &who.id)? {
        return Err(CellError::new(ErrorCode::Forbidden, "only the deployment's operators grant credit or set plans, seats and overdrafts"));
    }
    let person = match *person {
        "me" => who.id.clone(),
        id if npub::is_identity(id) => id.to_string(),
        username if fragment_proto::valid_username(username) => ask_registry(env, &calls::FindUsername { username: username.to_string() }).await?.identity.id,
        other => return Err(CellError::invalid(format!("{other:?} is not a username, an identity (id:…), or `me`"))),
    };
    match *command {
        "grant" => {
            let mut g: fragment_proto::ledger::GrantCredit = command_body(&body, "grant")?;
            // who granted it is who signs
            if g.by != who.id {
                return Err(CellError::invalid(format!("a grant's `by` is the operator who signs it ({})", who.id)));
            }
            g.id = commanded("grant", &g.id)?;
            json_answer(&ledger::ask(env, &person, &g).await?)
        }
        "plan" => {
            let mut c: fragment_proto::ledger::SetPlan = command_body(&body, "plan")?;
            c.id = commanded("plan", &c.id)?;
            json_answer(&ledger::ask(env, &person, &c).await?)
        }
        "seat" => {
            let mut c: fragment_proto::ledger::SetSeat = command_body(&body, "seat")?;
            c.id = commanded("seat", &c.id)?;
            json_answer(&ledger::ask(env, &person, &c).await?)
        }
        "overdraft" => {
            let mut c: fragment_proto::ledger::SetOverdraft = command_body(&body, "overdraft")?;
            c.id = commanded("overdraft", &c.id)?;
            json_answer(&ledger::ask(env, &person, &c).await?)
        }
        other => Err(CellError::new(ErrorCode::NotFound, format!("no ledger command {other:?}: grant, plan, seat, or overdraft"))),
    }
}

/// `/api/identities…`: the registry's public face. The signer's key is
/// checked here and resolved by the registry in the same turn as what it
/// asks (`calls::By`): one round trip a call, and a key revoked a moment
/// before cannot act.
async fn identities(mut req: Request, env: &Env, url: &Url, rest: &[&str]) -> CellResult<Response> {
    if acting_for(url)?.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let method = req.method();
    if let (Method::Post, []) = (&method, rest) {
        let reg: Register = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
        return match reg.kind {
            IdentityKind::Person => Err(CellError::invalid("people sign in: `fragment login` adds a key to you")),
            // FIN-11's trusted initial registration: the owner signs, and the
            // agent's key proves itself inside
            IdentityKind::Agent => {
                let owner_key = authenticate(&req, url, Payload::Read(&body))?;
                let proof = reg.proof.ok_or_else(|| CellError::invalid("registering an agent needs a proof by its key"))?;
                let key = proven_key(&proof, &req, url, &owner_key)?;
                json_answer(&ask_registry(env, &calls::RegisterAgent { owner: calls::By::Key(owner_key), key, fragment: None }).await?)
            }
        };
    }
    let caller = caller(env, &req, url, Payload::Read(&body))?;
    let by = || match &caller {
        Caller::Key(k) => calls::By::Key(k.clone()),
        Caller::Session(t) => calls::By::Session(t.clone()),
    };
    // a key's proof names the key that signed the request: the shell signs none
    let signer_key = || match &caller {
        Caller::Key(k) => Ok(k.clone()),
        Caller::Session(_) => Err(CellError::new(ErrorCode::Unauthenticated, "adding a key is signed by a key you hold (`fragment login`)")),
    };
    match (method, rest) {
        (Method::Put, ["me", "username"]) => {
            /// `PUT /api/identities/me/username`'s body.
            #[derive(Deserialize)]
            struct Choose {
                username: String,
            }
            let choose: Choose = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_answer(&ask_registry(env, &calls::ClaimUsername { by: by(), username: choose.username }).await?)
        }
        (Method::Put, ["me", "picture"]) => {
            if body.len() > limits::PICTURE_MAX_BYTES {
                return Err(CellError::too_large("a picture", body.len(), limits::PICTURE_MAX_BYTES));
            }
            let mime = picture_type(&body).ok_or_else(|| CellError::invalid("a picture is a PNG, JPEG, WebP, or GIF"))?;
            // Two round trips, on purpose: the bytes land in BLOBS before the
            // registry names them, so no one it does not know stores any.
            ask_registry(env, &calls::View { identity: None, by: by() }).await?;
            let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&body));
            js::blob_put_bytes(env.as_ref(), &format!("pictures/{sha}"), &body).await?;
            json_answer(&ask_registry(env, &calls::SetPicture { by: by(), sha, mime: mime.to_string() }).await?)
        }
        (Method::Get, [id]) => {
            let identity = named_identity(id)?;
            json_answer(&ask_registry(env, &calls::View { identity, by: by() }).await?)
        }
        (Method::Post, [id, "keys"]) => {
            let identity = named_identity(id)?;
            let add: fragment_proto::AddKey = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let key = proven_key(&add.proof, &req, url, &signer_key()?)?;
            json_answer(&ask_registry(env, &calls::AddKey(calls::KeyChange { identity, key, by: by() })).await?)
        }
        (Method::Put, [id, "held"]) => {
            /// `PUT /api/identities/{agent}/held`'s body.
            #[derive(Deserialize)]
            struct Held {
                held: Option<fragment_proto::Role>,
            }
            let agent = named_identity(id)?.ok_or_else(|| CellError::invalid("name the agent"))?;
            let b: Held = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_answer(&ask_registry(env, &calls::Hold { agent, held: b.held, by: by() }).await?)
        }
        (Method::Delete, [id, "keys", k]) => {
            let identity = named_identity(id)?;
            let key = key_in_path(k)?;
            json_answer(&ask_registry(env, &calls::RevokeKey(calls::KeyChange { identity, key, by: by() })).await?)
        }
        (Method::Get, [id, "keys", k]) => {
            let identity = named_identity(id)?;
            let key = key_in_path(k)?;
            json_answer(&ask_registry(env, &calls::CheckKey(calls::KeyChange { identity, key, by: by() })).await?)
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// A fragment named in an API path: `<label>.<username>`, or a bare label
/// for a signed caller's own (under its username; an agent's owner's).
fn named_fragment(name: &str, signer: Option<&Signed>) -> CellResult<String> {
    if valid_fragment_name(name) {
        return Ok(name.to_string());
    }
    if !fragment_proto::valid_label(name) {
        return Err(CellError::invalid("a fragment's name is <label>.<username>"));
    }
    match signer.and_then(|s| s.username.as_deref()) {
        Some(username) => Ok(fragment_proto::fragment_name(name, username)),
        None => Err(CellError::new(ErrorCode::NotFound, format!("no fragment {name}: name it as {name}.<username>"))),
    }
}

fn check_name(name: &str) -> CellResult<()> {
    if valid_fragment_name(name) {
        Ok(())
    } else {
        Err(CellError::invalid("a fragment's name is <label>.<username>"))
    }
}

/// A request for a fragment's supervisor: what the router decided, and
/// where it goes inside.
struct Forward {
    routed: Routed,
    /// The inner path; the query string travels only in `routed.url`.
    inner: String,
    /// Headers this route passes on purpose (the inbox's token and hop count).
    extra: Vec<(&'static str, String)>,
}

/// A blob upload declares its length, at most `limits::BLOB_MAX_BYTES`:
/// past that it is refused unread (the API's upload, and a page's at
/// `__blob/<sha256>`).
fn blob_length(req: &Request) -> CellResult<()> {
    let declared: Option<u64> = req.headers().get("content-length")?.and_then(|l| l.parse().ok());
    match declared {
        None => Err(CellError::invalid("a blob upload declares its content-length")),
        Some(n) if n > limits::BLOB_MAX_BYTES => Err(CellError::too_large("a blob", n as usize, limits::BLOB_MAX_BYTES as usize)),
        Some(_) => Ok(()),
    }
}

/// Bytes the router read, as a body to forward.
fn bytes_body(body: Vec<u8>) -> Option<worker::wasm_bindgen::JsValue> {
    (!body.is_empty()).then(|| worker::js_sys::Uint8Array::from(body.as_slice()).into())
}

/// Hands a request to the fragment's supervisor.
async fn forward(env: &Env, req: &Request, body: Option<worker::wasm_bindgen::JsValue>, f: Forward) -> CellResult<Response> {
    let headers = Headers::new();
    let cookies = fetched(req)?.site;
    for k in PASSED_HEADERS {
        if k == "cookie" && !cookies {
            continue;
        }
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    if is_socket(req)? {
        headers.set("connection", "Upgrade")?;
        for k in WEBSOCKET_HEADERS {
            if let Some(v) = req.headers().get(k)? {
                headers.set(k, &v)?;
            }
        }
    }
    f.routed.to_headers(&headers)?;
    for (k, v) in &f.extra {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    // a fragment's redirect (an app's) is the browser's to follow: followed
    // here, it came back to the fragment at its Location, outside its routes
    init.with_method(req.method()).with_headers(headers).with_redirect(RequestRedirect::Manual);
    if body.is_some() {
        init.with_body(body);
    }
    let inner = Request::new_with_init(&format!("https://fragment.internal{}", f.inner), &init)?;
    let stub = env.durable_object("FRAGMENT")?.get_by_name(&f.routed.name)?;
    Ok(stub.fetch_with_request(inner).await?)
}

/// Whether a request is a browser's navigation to a page (its `Accept`
/// names HTML): a refusal answers it as one (`auth::refused`), and an API
/// call, a fetch, or a request that asks for no HTML keeps the JSON.
fn shows_page(req: &Request, fetched: Fetched) -> CellResult<bool> {
    Ok(fetched.navigation && req.headers().get("accept")?.is_some_and(|a| a.contains("text/html")))
}

/// The answer without the fragment's mark on its refusal (`serve::REFUSAL`).
fn unmarked(resp: Response) -> CellResult<Response> {
    let h = resp.headers().clone();
    h.delete(serve::REFUSAL)?;
    Ok(resp.with_headers(h))
}

/// A request on a fragment's site; a refusal a browser navigated to is
/// answered as a page: the router's own here, the fragment's in `site`.
async fn serve(req: Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str, mode: Mode) -> CellResult<Response> {
    check_name(name)?;
    let fetched = fetched(&req)?;
    let page = shows_page(&req, fetched)?;
    match site(req, env, cfg, url, name, rest, mode, fetched, page).await {
        Err(e) if page && auth::is_refusal(e.code) => auth::refused(cfg, url, name, rest, fetched.framed, &e),
        answered => answered,
    }
}

#[allow(clippy::too_many_arguments)]
async fn site(mut req: Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str, mode: Mode, fetched: Fetched, page: bool) -> CellResult<Response> {
    if auth::is_fragment_route(rest) {
        return auth::fragment(&req, env, cfg, url, name, rest, mode == Mode::Path, fetched).await;
    }
    own_page_socket(&req, cfg, url, name)?;
    // a page's blob upload streams through, as the API's does: the router
    // never holds its bytes (the fragment checks who uploads, and the hash)
    let upload = match (req.method(), rest.strip_prefix("__blob/")) {
        (Method::Put, Some(sha)) => {
            blob_length(&req)?;
            Some(sha)
        }
        _ => None,
    };
    // a GET or HEAD has no body to wait for
    let body = match req.method() {
        Method::Get | Method::Head => Vec::new(),
        _ if upload.is_some() => Vec::new(),
        _ => read_body(&mut req, limits::BODY_MAX_BYTES).await?,
    };
    let payload = match upload {
        Some(sha) => Payload::Streamed { sha256_hex: sha },
        None => Payload::Read(&body),
    };
    let mut credential = site_credential(&req, url, payload, name, mode, fetched)?;
    // a frame's page shows only in the page its session was made for: that
    // session is asked for here, for the page's origin (`bound`). Only the
    // platform's page has one (its mint names it): a session for any
    // other is no one's, so no answer names another page.
    let platform = cfg.platform(url);
    let (mut signed, mut framed) = (None, Framed::Stranger);
    if let (true, Some(Credential::Frame(token))) = (fetched.framed, &credential) {
        if let Some(live) = routed::site_session(env, token.clone(), name, true).await? {
            if live.embedder.as_deref() == Some(platform.as_str()) {
                (signed, framed) = (Some(Signed::new(live.identity, None)), Framed::Session);
            }
        }
        credential = None;
    }
    // an upload from no one is refused here, its bytes never forwarded
    if upload.is_some() && signed.is_none() && credential.is_none() {
        return Err(CellError::new(ErrorCode::Unauthenticated, "sign in on this fragment's page to upload a blob"));
    }
    // a frame whose navigation this origin's own cookies count in is one
    // of its own page's (`fetched`: same-origin), as whoever they name
    if framed == Framed::Stranger && fetched.site {
        framed = Framed::OwnPage;
    }
    let routed = Routed { name: name.to_string(), url: url.clone(), mode: Some(mode), signed, credential };
    let body = match upload {
        Some(_) => req.inner().body().map(worker::wasm_bindgen::JsValue::from),
        None => bytes_body(body),
    };
    let mut resp = forward(env, &req, body, Forward { routed, inner: format!("/serve/{rest}"), extra: vec![] }).await?;
    // the fragment's own refusal, never its app's answer
    if resp.headers().has(serve::REFUSAL)? {
        resp = match page {
            true => {
                let e: ErrorBody = resp.json().await?;
                auth::refused(cfg, url, name, rest, fetched.framed, &CellError::new(e.error, e.message))?
            }
            false => unmarked(resp)?,
        };
    }
    match fetched.navigation {
        true => bound(resp, fetched.framed.then(|| frames::ancestors(framed, &platform)).as_deref()),
        false => Ok(resp),
    }
}

/// A URL's path and query, as a redirect to another host keeps them.
fn path_and_query(url: &Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    }
}

/// A move's redirect (docs/fragment-boats.md, slice 2). No cache keeps it,
/// so the move can still be undone: a browser caches a bare `308` for good.
fn moved(to: &str) -> CellResult<Response> {
    let mut resp = Response::empty()?.with_status(308);
    resp.headers_mut().set("location", to)?;
    resp.headers_mut().set("cache-control", "no-store")?;
    Ok(resp)
}

pub(crate) async fn route(mut req: Request, env: &Env, ctx: &Context) -> CellResult<Response> {
    let cfg = Config::from_env(env);
    // as its client named it: signatures, links, and cookies name the https URL
    let url = fragment_nip98::arrived_url(req.url()?, req.headers().get("x-forwarded-proto")?.as_deref());
    let path = url.path().to_string();
    // the platform's own host first: it may sit under the suffix
    let host = url.host_str().filter(|h| !cfg.is_platform_host(h));
    // A fragment's own host is all its own (`/api/…` included: apps have
    // routes there); the platform API answers on the platform's host.
    if let Some(name) = host.and_then(|h| cfg.fragment_of_host(h)) {
        let rest = path.trim_start_matches('/').to_string();
        return serve(req, env, cfg, &url, &name, &rest, Mode::Host).await;
    }
    // a computer's own origin: its ports, for its owner (computer.rs)
    if let Some(id) = host.and_then(|h| cfg.computer_of_host(h)) {
        let signer = match req.headers().get("authorization")? {
            Some(_) => Some(signer(env, &req, &url, &[]).await?.identity.id),
            None => None,
        };
        return computer::serve_host(req, env, &url, &id, signer).await;
    }
    // the suffix's own name, with the platform elsewhere, is the platform's
    if host.is_some_and(|h| cfg.is_suffix(h)) {
        return moved(&format!("{}{}", cfg.platform(&url), path_and_query(&url)));
    }
    // A fragment's old host: a browser's visit goes on to its new one. A
    // write or a socket is refused, so nothing acts where no one looks: a
    // page loaded before the move reloads onto the new host.
    if let Some(name) = host.and_then(|h| cfg.fragment_of_legacy_host(h)) {
        let to = format!("{}{}", cfg.origin(&url, &name), path_and_query(&url));
        if matches!(req.method(), Method::Get | Method::Head) && !is_socket(&req)? {
            return moved(&to);
        }
        return Err(CellError::new(ErrorCode::Moved, format!("this fragment moved to {to}")));
    }
    // any other name under the suffix, or the old one, is no one's: the platform answers on its own host only
    if host.and_then(|h| cfg.subdomain(h)).is_some() {
        return Err(CellError::new(ErrorCode::NotFound, "no fragment here: a fragment's host is <label>--<username>.<suffix>"));
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let ["api", "test", lever @ ..] = segments.as_slice() {
        // The levers exist only on a fleet with a test secret, for a
        // request that carries it: to anyone else they are the 404 any
        // missing route is, so a scan cannot tell a preview's from production.
        let carried = req.headers().get(fragment_core::levers::SECRET_HEADER)?;
        if !cfg.test_secret.as_ref().is_some_and(|secret| secret.admits(carried.as_deref())) {
            return Err(levers::no_route(&path));
        }
        let lever = lever.to_vec();
        return levers::route(req, env, cfg, &lever).await;
    }
    match (req.method(), segments.as_slice()) {
        (Method::Get | Method::Head, ["__shell", file @ ..]) => match shell::asset(&req, &file.join("/"))? {
            Some(resp) => Ok(resp),
            None => Err(CellError::new(ErrorCode::NotFound, format!("no shell file {}", file.join("/")))),
        },
        // the shell, for everyone: signed out it asks them to sign in,
        // without a username it asks for one; `/settings` opens its settings
        (Method::Get, [""] | ["settings"]) => Ok(shell::page(&req, cfg, &url)?),
        (_, ["auth", ..] | ["cli"] | ["cli", "approve"]) => {
            let segs = segments.clone();
            auth::platform(req, env, cfg, &url, &segs).await
        }
        (_, ["share" | "join", _]) => {
            let segs = segments.clone();
            share::route(req, env, cfg, &url, &segs).await
        }
        (Method::Get | Method::Head, ["healthz"]) => {
            let mut resp = Response::ok("ok")?;
            resp.headers_mut().set("x-fragment-deploy", &cfg.deploy_id)?;
            Ok(resp)
        }
        (Method::Post, ["api", "fragments"]) => {
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            let create: CreateFragment = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let principal = signer_for(env, &req, &url, &body).await?;
            // what an agent makes is its owner's: only its owner's turns make one
            if principal.acting_for.as_ref().is_some_and(|asker| principal.owner.as_ref() != Some(asker)) {
                return Err(CellError::new(ErrorCode::Forbidden, "an agent makes fragments for its owner, in its owner's turns only"));
            }
            create_fragment(env, cfg, &url, create, principal).await
        }
        // the agents' script, co-hosted: authenticated here, like the rest
        (_, ["api", "agents"]) | (_, ["api", "a", ..]) => {
            let segs = segments.clone();
            agents::route(req, env, &url, &segs).await
        }
        (Method::Get, ["api", "fragments", "watch"]) => watch_list(&req, env, cfg, &url).await,
        (Method::Get, ["api", "fragments"]) => {
            let principal = signer_for(env, &req, &url, &[]).await?;
            if let Some(asker) = &principal.acting_for {
                return json_answer(&agents::reachable(env, &principal, asker).await?);
            }
            let list = Request::new("https://principal.internal/list", Method::Get)?;
            Ok(env.durable_object("PRINCIPAL")?.get_by_name(&principal.id)?.fetch_with_request(list).await?)
        }
        // the signer's own view of a fragment of theirs (principal.rs): it
        // changes nothing of the fragment's, nor anyone else's list
        (Method::Put, ["api", "fragments", name, "archived"]) => {
            let body = read_body(&mut req, ARCHIVED_BODY_MAX_BYTES).await?;
            let set: fragment_proto::SetArchived = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let who = signer(env, &req, &url, &body).await?;
            let name = named_fragment(name, Some(&who))?;
            let inner = json!({ "fragment": name, "archived": set.archived }).to_string();
            let mut init = RequestInit::new();
            init.with_method(Method::Put).with_body(Some(inner.into()));
            let put = Request::new_with_init("https://principal.internal/archived", &init)?;
            Ok(env.durable_object("PRINCIPAL")?.get_by_name(&who.identity.id)?.fetch_with_request(put).await?)
        }
        // search over the signer's own list (principal.rs): agents need none,
        // and acting for someone (`for`) is not honored here
        (Method::Get, ["api", "search"]) => {
            let who = signer(env, &req, &url, &[]).await?;
            let mut asked = url.query_pairs().filter(|(k, _)| k == "q").map(|(_, v)| v.into_owned());
            let (Some(q), None) = (asked.next(), asked.next()) else {
                return Err(CellError::invalid("name what to look for, once: ?q="));
            };
            let mut inner = Url::parse("https://principal.internal/search").map_err(|e| CellError::host(e.to_string()))?;
            inner.query_pairs_mut().append_pair("q", &q);
            let search = Request::new(inner.as_str(), Method::Get)?;
            Ok(env.durable_object("PRINCIPAL")?.get_by_name(&who.identity.id)?.fetch_with_request(search).await?)
        }
        (_, ["api", "ledger", rest @ ..]) => {
            let rest = rest.to_vec();
            ledger_route(req, env, cfg, &url, &rest).await
        }
        (Method::Post, ["api", "models", "v1", "chat", "completions"]) => models::route(req, env, &url, ctx).await,
        (Method::Get, ["api", "users", rest @ ..]) => {
            let rest = rest.to_vec();
            users(env, &rest).await
        }
        (Method::Delete, ["api", "users", username]) => {
            let who = signer(env, &req, &url, &[]).await?;
            if !cfg.is_operator(who.key.as_deref(), &who.id)? {
                return Err(CellError::new(ErrorCode::Forbidden, "only the fleet's operators release a username"));
            }
            release_username(env, username).await
        }
        (method, ["api", "connections", rest @ ..]) => {
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            let who = signer(env, &req, &url, &body).await?;
            let rest = rest.to_vec();
            connections::route(env, &who.identity.id, who.identity.kind, method, &rest, &body).await
        }
        (method, ["api", "computers", rest @ ..]) => {
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            let who = signer(env, &req, &url, &body).await?;
            let rest = rest.to_vec();
            computer::route(env, &who.identity.id, who.identity.kind, method, &rest, &body).await
        }
        (_, ["api", "identities", rest @ ..]) => {
            let rest = rest.to_vec();
            identities(req, env, &url, &rest).await
        }
        // A blob's bytes stream through: the router never holds them. The
        // signature covers the URL, which names the bytes' hash; the
        // fragment checks the hash as they arrive. A payload tag may name
        // that hash too (older CLIs sign one), or be absent.
        (Method::Put, ["api", "f", name, "blobs", sha]) => {
            blob_length(&req)?;
            let principal = signer_of(env, &req, &url, Payload::Streamed { sha256_hex: sha }).await?;
            let name = named_fragment(name, Some(&principal))?;
            let body = req.inner().body().map(worker::wasm_bindgen::JsValue::from);
            let routed = Routed { name, url: url.clone(), mode: None, signed: Some(principal), credential: None };
            forward(env, &req, body, Forward { routed, inner: format!("/api/blobs/{sha}"), extra: vec![] }).await
        }
        (method, ["api", "f", name, rest @ ..]) => {
            if !valid_fragment_name(name) && !fragment_proto::valid_label(name) {
                return Err(CellError::invalid("a fragment's name is <label>.<username>"));
            }
            let reserved = access::reserved(method.as_ref(), rest);
            let inner = match (method, rest) {
                (Method::Delete, [] | [""]) => "/delete".to_string(),
                (_, [] | [""]) => return Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
                _ => format!("/api/{}", rest.join("/")),
            };
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            // code.storage signs its deliveries with the fragment's webhook
            // secret, and the inbox takes its token, instead of a signature.
            let mut extra = vec![];
            let principal = match inner.as_str() {
                "/api/webhook" => None,
                "/api/inbox" => {
                    for k in ["x-fragment-inbox-token", jobs::HOPS_HEADER] {
                        if let Some(v) = req.headers().get(k)? {
                            extra.push((k, v));
                        }
                    }
                    None
                }
                _ => Some(signer_for(env, &req, &url, &body).await?),
            };
            // An agent on a reserved route: deleting and the cap are never
            // its, and it shares only for its own owner, unheld (Paul,
            // 2026-10-04). Who it is decides that much here; the fragment
            // decides the rest from its owner's share there, and decides
            // again on its own (`access::agent_shares`).
            if let (Some(reserved), Some(agent)) = (reserved, principal.as_ref().filter(|p| p.kind != IdentityKind::Person)) {
                let for_owner = agent.acting_for.is_some() && agent.acting_for == agent.owner;
                let sharer = access::Sharer { for_owner, held: agent.held };
                if let Err(refusal) = access::agent_may_ask(reserved, sharer) {
                    return Err(CellError::new(ErrorCode::Forbidden, refusal.message()));
                }
            }
            let name = named_fragment(name, principal.as_ref())?;
            let routed = Routed { name, url: url.clone(), mode: None, signed: principal, credential: None };
            forward(env, &req, bytes_body(body), Forward { routed, inner, extra }).await
        }
        (_, ["f", name]) => {
            check_name(name)?;
            let to = Url::parse(&cfg.canonical(&url, name)).map_err(|e| CellError::host(e.to_string()))?;
            Ok(Response::redirect_with_status(to, 308)?)
        }
        (method, ["f", name, rest @ ..]) => {
            check_name(name)?;
            let rest = rest.join("/");
            if cfg.host_suffix.is_some() && rest != "__watch" && rest != "__live" {
                if !matches!(method, Method::Get | Method::Head) {
                    return Err(CellError::new(ErrorCode::NotFound, "fragments are served from their own origin"));
                }
                let mut to = Url::parse(&cfg.canonical(&url, name)).map_err(|e| CellError::host(e.to_string()))?;
                to.set_path(&format!("/{rest}"));
                to.set_query(url.query());
                return Ok(Response::redirect_with_status(to, 308)?);
            }
            serve(req, env, cfg, &url, name, &rest, Mode::Path).await
        }
        _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {path}"))),
    }
}
