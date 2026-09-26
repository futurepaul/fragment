//! The fragment platform on celld, in Rust.
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

mod agents;
mod ai;
mod auth;
mod blobs;
mod config;
mod channels;
mod deliveries;
mod cs;
mod error;
mod files;
mod fragment;
mod jobs;
mod js;
mod keys;
mod ledger;
mod live;
mod members;
mod ops;
mod plane;
mod principal;
mod publish;
mod push;
mod registry;
mod routed;
mod serve;
mod share;
mod subscriptions;

use fragment_core::body::{LimitedBody, TooLarge};
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

pub use fragment::FragmentCell;
pub use principal::PrincipalCell;
pub use ledger::LedgerCell;
pub use registry::RegistryCell;

/// Client headers a fragment's supervisor sees; everything else, and any
/// `x-fragment-*` a client sends, stays at the router.
const PASSED_HEADERS: [&str; 9] =
    ["content-type", "cookie", "origin", "accept", "if-none-match", "upgrade", "x-pierre-event", "x-pierre-signature", "range"];

/// A WebSocket upgrade's own handshake, passed too: when the fragment's cell
/// lives on another node, celld tunnels the upgrade there and answers the
/// client's key itself, so it needs the client's handshake, not just
/// `Upgrade` (without these, every live socket that lands on the other node
/// fails: 502 "the cell accepted a WebSocket for a request that did not
/// upgrade").
const WEBSOCKET_HEADERS: [&str; 4] = ["sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"];

#[event(queue)]
async fn queue(batch: MessageBatch<deliveries::Delivery>, env: Env, _ctx: Context) -> Result<()> {
    deliveries::consume(batch, env).await
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match route(req, &env).await {
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

/// The key that signed the request (NIP-98), not yet resolved.
fn authenticate(req: &Request, url: &Url, payload: Payload<'_>) -> CellResult<String> {
    let header = req.headers().get("authorization")?;
    let now_s = js::now_ms() / 1000;
    fragment_nip98::verify_request(header.as_deref(), req.method().as_ref(), url, payload, now_s, limits::AUTH_WINDOW_S)
        .map_err(|e| CellError::new(ErrorCode::Unauthenticated, e.to_string()))
}

/// Tries at reaching the registry when the node refused its route for now
/// (a cell queued too long behind the others waking after a restart), with
/// a pause that doubles between them.
const REGISTRY_ATTEMPTS: u32 = 3;
const REGISTRY_RETRY_MS: u64 = 250;
/// How celld names that refusal in the error a Worker's fetch throws (its
/// routed-request error, `route failed: CapacityExhausted`): it is answered
/// only for a request still queued at the gate, so the registry never saw
/// it. A fetch's error reaches the Worker as its message alone.
const REFUSED_BEFORE_IT_RAN: &str = "CapacityExhausted";

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
    let mut attempt = 0;
    let (status, bytes) = loop {
        match ask().await {
            Ok(answer) => break answer,
            // Only that refusal is asked again. Any other throw may come
            // after the registry acted (a failure inside it, a connection
            // dropped mid-answer), and its calls are not idempotent: a
            // second Mint is a second redemption, a second ClaimUsername
            // answers "taken" to the person who got the name.
            Err(e) if attempt + 1 < REGISTRY_ATTEMPTS && e.to_string().contains(REFUSED_BEFORE_IT_RAN) => {
                Delay::from(std::time::Duration::from_millis(REGISTRY_RETRY_MS << attempt)).await;
                attempt += 1;
            }
            Err(e) => return Err(unavailable(e.to_string())),
        }
    };
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
async fn signer_for(env: &Env, req: &Request, url: &Url, body: &[u8]) -> CellResult<Signed> {
    signer_of(env, req, url, Payload::Read(body)).await
}

/// The signer for any payload: a body the router read, or one it streams
/// through unread (a blob, whose URL names its hash). `for` is inside the
/// signed URL, and only an agent may name it: a person acts as themselves.
async fn signer_of(env: &Env, req: &Request, url: &Url, payload: Payload<'_>) -> CellResult<Signed> {
    let key = authenticate(req, url, payload)?;
    let acting_for = acting_for(url)?;
    let identity = ask_registry(env, &calls::Resolve { key: key.clone() }).await?;
    if acting_for.is_some() && (identity.kind != IdentityKind::Agent || identity.owner.is_none()) {
        return Err(CellError::new(ErrorCode::Forbidden, "only an agent acts for someone (`for`); a person acts as themselves"));
    }
    Ok(Signed { identity, key: Some(key), acting_for })
}

/// Who is asking a site request, unresolved: a signature names its key
/// (verified here, which needs no registry: a bad one is still 401); a
/// browser, its session on this origin, as far as its cookies count
/// (`Fetched`), a frame's navigation by its frame cookie first.
fn site_credential(req: &Request, url: &Url, body: &[u8], name: &str, mode: Mode, fetched: Fetched) -> CellResult<Option<Credential>> {
    if req.headers().get("authorization")?.is_some() {
        return Ok(Some(Credential::Key(authenticate(req, url, Payload::Read(body))?)));
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
///   show a page too): the frame cookie (`__frame`), and the answer shows
///   only in the page that framed it (`bound`);
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

fn fetched(req: &Request) -> CellResult<Fetched> {
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
/// and a frame's shows only inside the page that framed it through
/// `__frame` (its session's `embedder`), or, with no frame session, inside
/// this origin's own pages, and is never reused from a cache without that.
/// An app's own policy stays: a second one only narrows it.
fn bound(resp: Response, framed: bool, embedder: Option<&str>) -> CellResult<Response> {
    let h = resp.headers().clone();
    h.append("vary", "sec-fetch-dest")?;
    if framed {
        h.append("content-security-policy", &format!("frame-ancestors {}", embedder.unwrap_or("'self'")))?;
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

/// How many fragments a removed computer leaves at once.
const LEAVES_AT_ONCE: usize = 16;

/// A removed computer (its keys revoked already: `RemoveComputer`) leaves
/// every fragment its list names, as a member may, so no grant names it
/// and its sockets close: `{id, removed, left, failed}`. A leave that
/// fails is named in `failed`; removing it again retries them. Its list
/// is fed from each fragment's outbox, so a membership granted a moment
/// before may not be there yet: that grant stays, naming an identity no
/// key signs as.
async fn remove_computer(env: &Env, url: &Url, removed: calls::RemovedComputer) -> CellResult<Response> {
    let id = removed.identity.id.clone();
    let list = Request::new("https://principal.internal/list", Method::Get)?;
    let listed: fragment_proto::FragmentList = env.durable_object("PRINCIPAL")?.get_by_name(&id)?.fetch_with_request(list).await?.json().await?;
    let (mut left, mut failed) = (vec![], vec![]);
    // bounded by the list, which is finite: a batch at a time, each leave one hop
    for batch in listed.fragments.chunks(LEAVES_AT_ONCE) {
        let leaves = batch.iter().map(|f| {
            let who = Signed::new(removed.identity.clone(), None);
            let routed = Routed { name: f.name.clone(), url: url.clone(), mode: None, signed: Some(who), credential: None };
            async move {
                let delete = Request::new(url.as_str(), Method::Delete)?;
                let answer = forward(env, &delete, None, Forward { routed, inner: "/api/members/me".into(), extra: vec![] }).await?;
                Ok::<u16, CellError>(answer.status_code())
            }
        });
        for (f, answer) in batch.iter().zip(futures_util::future::join_all(leaves).await) {
            match answer {
                // 404: not a member there (any more): nothing to leave
                Ok(200 | 404) => left.push(f.name.clone()),
                Ok(status) => failed.push(json!({ "fragment": f.name, "status": status })),
                Err(e) => failed.push(json!({ "fragment": f.name, "error": e.message })),
            }
        }
    }
    assert!(left.len() + failed.len() == listed.fragments.len(), "every listed fragment is left or named as failed");
    json_answer(&json!({ "id": id, "removed": removed.removed, "left": left, "failed": failed }))
}

/// Makes a fragment for a person, under their username: the API's create
/// and the platform's "new" page. An agent or a computer makes one for its
/// owner: the owner's (on their budget, in their list), under their
/// username, with its maker an editor of it.
pub(crate) async fn create_fragment(env: &Env, cfg: &Config, url: &Url, mut create: CreateFragment, principal: Signed) -> CellResult<Response> {
    let (maker, agent) = match principal.kind {
        IdentityKind::Person => (principal, None),
        IdentityKind::Agent | IdentityKind::Computer => {
            let owner = principal.owner.clone().ok_or_else(|| CellError::host(format!("{} {} has no owner", principal.kind.as_str(), principal.id)))?;
            let identity = fragment_proto::Identity { id: owner, kind: IdentityKind::Person, owner: None, username: principal.username.clone() };
            (Signed::new(identity, None), Some(principal.identity.id))
        }
    };
    let username = maker.username.clone().ok_or_else(|| CellError::invalid(format!("choose a username first (sign in at {}/)", cfg.platform(url))))?;
    create.name = qualify(&create.name, &username)?;
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

/// Whose budget a signer sees: a person's own org; an agent's owner's. A
/// computer sees none: it spends only through the fragments it is in,
/// whose owners pay, and reads nothing else of its owner's.
fn billing_org(who: &Signed) -> CellResult<String> {
    let person = match who.kind {
        IdentityKind::Person => who.id.as_str(),
        IdentityKind::Agent => who.owner.as_deref().ok_or_else(|| CellError::host("an agent without an owner"))?,
        IdentityKind::Computer => return Err(CellError::new(ErrorCode::Forbidden, "a computer has no budget: the fragments it works in bill their owners")),
    };
    ledger::org_of(person).ok_or_else(|| CellError::host("no billing org"))
}

/// `/api/budget…`: a billing org's month (ledger.rs).
async fn budget_route(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let who = signer(env, &req, url, &body).await?;
    match (req.method(), rest) {
        (Method::Get, []) => json_answer(&ledger::ask(env, &billing_org(&who)?, &ledger::Status {}).await?),
        (Method::Get, ["usage"]) => {
            let period = url.query_pairs().find(|(k, _)| k == "period").map(|(_, v)| v.into_owned());
            let period = match period {
                Some(p) if p.len() == 7 && p.as_bytes()[4] == b'-' && p.bytes().enumerate().all(|(i, b)| i == 4 || b.is_ascii_digit()) => Some(p),
                Some(_) => return Err(CellError::invalid("period is YYYY-MM")),
                None => None,
            };
            json_answer(&ledger::ask(env, &billing_org(&who)?, &ledger::Usage { period }).await?)
        }
        // an agent's turns spend its owner's month: their org's OpenRouter
        // key, whose limit is the allowance (OpenRouter stops it there)
        (Method::Post, ["key"]) => {
            if who.kind != IdentityKind::Agent {
                return Err(CellError::new(ErrorCode::Forbidden, "only an agent asks for its owner's model key"));
            }
            json_answer(&ledger::ask(env, &billing_org(&who)?, &ledger::Key {}).await?)
        }
        (Method::Post, [id, "top-up"]) => {
            if !cfg.is_operator(who.key.as_deref(), &who.id)? {
                return Err(CellError::new(ErrorCode::Forbidden, "only the fleet's operators top up budgets"));
            }
            let id = named_identity(id)?.unwrap_or_else(|| who.id.clone());
            let org = ledger::org_of(&id).ok_or_else(|| CellError::invalid("name a person"))?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let usd = v["usd"].as_f64().filter(|u| u.is_finite() && *u > 0.0).ok_or_else(|| CellError::invalid("usd is a positive number of dollars"))?;
            let micros = fragment_core::budget::micros(usd);
            json_answer(&ledger::ask(env, &org, &ledger::TopUp { micros, by: who.id.clone() }).await?)
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
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
            IdentityKind::Computer => Err(CellError::invalid("a computer pairs on the platform: `fragment login --computer <name>` on it, approved by its owner")),
            // FIN-11's trusted initial registration: the owner signs, and the
            // agent's key proves itself inside
            IdentityKind::Agent => {
                let owner_key = authenticate(&req, url, Payload::Read(&body))?;
                let proof = reg.proof.ok_or_else(|| CellError::invalid("registering an agent needs a proof by its key"))?;
                let key = proven_key(&proof, &req, url, &owner_key)?;
                json_answer(&ask_registry(env, &calls::RegisterAgent { owner: calls::By::Key(owner_key), key }).await?)
            }
        };
    }
    let signer_key = authenticate(&req, url, Payload::Read(&body))?;
    let by = || calls::By::Key(signer_key.clone());
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
            ask_registry(env, &calls::Resolve { key: signer_key.clone() }).await?;
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
            let key = proven_key(&add.proof, &req, url, &signer_key)?;
            json_answer(&ask_registry(env, &calls::AddKey(calls::KeyChange { identity, key, by: by() })).await?)
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
        (Method::Delete, [id]) => {
            let computer = named_identity(id)?.ok_or_else(|| CellError::invalid("name the computer to remove (id:…); `me` is not one"))?;
            let removed = ask_registry(env, &calls::RemoveComputer { by: by(), computer }).await?;
            remove_computer(env, url, removed).await
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
    init.with_method(req.method()).with_headers(headers);
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
    // a sign-in for a frame of this origin's own page, and nowhere else
    if rest == "__frame" && !(fetched.framed && req.headers().get("sec-fetch-site")?.as_deref() == Some("same-origin")) {
        return Err(CellError::new(ErrorCode::Forbidden, "__frame is a frame of this fragment's own page"));
    }
    // a GET or HEAD has no body to wait for
    let body = match req.method() {
        Method::Get | Method::Head => Vec::new(),
        _ => read_body(&mut req, limits::BODY_MAX_BYTES).await?,
    };
    let mut credential = site_credential(&req, url, &body, name, mode, fetched)?;
    // a frame's page shows only in the page its session was made for: that
    // session is asked for here, for the page's origin (`bound`)
    let (mut signed, mut embedder) = (None, None);
    if let (true, Some(Credential::Frame(token))) = (fetched.framed && rest != "__frame", &credential) {
        if let Some(live) = routed::site_session(env, token.clone(), name, true).await? {
            (signed, embedder) = (Some(Signed::new(live.identity, None)), live.embedder);
        }
        credential = None;
    }
    let routed = Routed { name: name.to_string(), url: url.clone(), mode: Some(mode), signed, credential };
    let mut resp = forward(env, &req, bytes_body(body), Forward { routed, inner: format!("/serve/{rest}"), extra: vec![] }).await?;
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
        true => bound(resp, fetched.framed, embedder.as_deref()),
        false => Ok(resp),
    }
}

async fn route(mut req: Request, env: &Env) -> CellResult<Response> {
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
    // any other name under the suffix is no one's: the platform answers on its own host only
    if host.and_then(|h| cfg.subdomain(h)).is_some() {
        return Err(CellError::new(ErrorCode::NotFound, "no fragment here: a fragment's host is <label>--<username>.<suffix>"));
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (req.method(), segments.as_slice()) {
        (_, [""] | ["auth", ..] | ["cli"] | ["cli", "approve"]) => {
            let segs = segments.clone();
            auth::platform(req, env, cfg, &url, &segs).await
        }
        (_, ["share" | "join", _]) => {
            let segs = segments.clone();
            share::route(req, env, cfg, &url, &segs).await
        }
        (Method::Get, ["healthz"]) => {
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
        (Method::Get, ["api", "fragments"]) => {
            let principal = signer_for(env, &req, &url, &[]).await?;
            if let Some(asker) = &principal.acting_for {
                return json_answer(&agents::reachable(env, &principal, asker).await?);
            }
            let list = Request::new("https://principal.internal/list", Method::Get)?;
            Ok(env.durable_object("PRINCIPAL")?.get_by_name(&principal.id)?.fetch_with_request(list).await?)
        }
        (_, ["api", "budget", rest @ ..]) => {
            let rest = rest.to_vec();
            budget_route(req, env, cfg, &url, &rest).await
        }
        (Method::Post, ["api", "test", "ledger"]) if cfg.test_hooks => {
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let org = v["identity"].as_str().and_then(ledger::org_of).ok_or_else(|| CellError::invalid("name an identity"))?;
            let offset_ms = v["offsetMs"].as_i64().ok_or_else(|| CellError::invalid("offsetMs"))?;
            json_answer(&ledger::ask(env, &org, &ledger::SetClock { offset_ms }).await?)
        }
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
        (_, ["api", "identities", rest @ ..]) => {
            let rest = rest.to_vec();
            identities(req, env, &url, &rest).await
        }
        (Method::Get, ["api", "test", "env"]) if cfg.test_hooks => json_answer(&Value::Object(js::env_vars(env.as_ref())?)),
        (Method::Post, ["api", "test", hook @ ("keys" | "fragment")]) if cfg.test_hooks => {
            /// The fragment a test hook's body names (the rest is the fragment's to read).
            #[derive(Deserialize)]
            struct TestTarget {
                fragment: String,
            }
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            let target: TestTarget = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            check_name(&target.fragment)?;
            let body = String::from_utf8(body).map_err(|_| CellError::invalid("body: not UTF-8"))?;
            let inner = routed::internal_request(&format!("test/{hook}"), &body)?;
            Ok(env.durable_object("FRAGMENT")?.get_by_name(&target.fragment)?.fetch_with_request(inner).await?)
        }
        (Method::Post, ["api", "test", "registry"]) if cfg.test_hooks => {
            let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
            let hook: calls::TestHook = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_answer(&ask_registry(env, &hook).await?)
        }
        // A blob's bytes stream through: the router never holds them. The
        // signature covers the URL, which names the bytes' hash; the
        // fragment checks the hash as they arrive. A payload tag may name
        // that hash too (older CLIs sign one), or be absent.
        (Method::Put, ["api", "f", name, "blobs", sha]) => {
            let declared: Option<u64> = req.headers().get("content-length")?.and_then(|l| l.parse().ok());
            match declared {
                None => return Err(CellError::invalid("a blob upload declares its content-length")),
                Some(n) if n > limits::BLOB_MAX_BYTES => return Err(CellError::too_large("a blob", n as usize, limits::BLOB_MAX_BYTES as usize)),
                Some(_) => {}
            }
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
            let owner_only = fragment_core::access::owner_only(method.as_ref(), rest);
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
            // owner-only actions never go through an agent, whatever it acts
            // for, nor a computer: only a person owns a fragment
            if owner_only && principal.as_ref().is_some_and(|p| p.kind != IdentityKind::Person) {
                return Err(CellError::new(ErrorCode::Forbidden, "an agent or a computer never manages members, invites, visibility, or links, nor deletes a fragment: its owner does"));
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
