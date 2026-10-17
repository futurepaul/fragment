//! Its owner's browser (docs/explorations/owner-browser.md, a spike), the
//! Computer DO's side: the computer carries CDP between its guest and a
//! browser its owner connected, and reads none of it
//! (`fragment_core::owner_browser`).
//!
//! - **Connecting.** Its owner mints a one-time ticket (`browser_ticket`:
//!   Settings' Connect), which their browser's extension spends on this
//!   computer's origin (`/__browser`). That first connection makes the
//!   browser a link: a secret the computer keeps hashed and sends once, on
//!   the new socket, with the link's id. With it the extension comes back
//!   on its own (a dropped network, a restart, a sleep), asking this
//!   origin for a ticket of its own (`POST /__browser/ticket`, the link in
//!   its body: an `Authorization` here is a signer's), until it is
//!   forgotten.
//! - **One browser at a time.** The newest connection replaces the one
//!   before it (closed `REPLACED`), and every guest socket that one
//!   carried ends. At most `LINKS_MAX` browsers stay linked.
//! - **Forgetting.** Its owner forgets a browser from anywhere
//!   (`DELETE /api/computers/{id}/browser/links/{link}`: Settings), or the
//!   extension forgets itself (`POST /__browser/forget`): its link is gone,
//!   and its socket closed `FORGOTTEN`.
//! - **Carrying.** The guest dials `browser.fragment.internal` for a CDP
//!   socket, accepted only while a browser is connected; each of its
//!   frames goes to the browser named by its guest socket, and back. It
//!   asks first, if it likes, as a CDP client asks a browser (`GET
//!   /json/version`): the connected browser's name and its socket, or
//!   that none is connected. The bridge asks before each of its owner's
//!   turns, and tells the agent.
//! - **Told.** Its view lists the browsers linked, which is connected and
//!   when each was last heard (`browsers`), and its owner's open pages are
//!   told each time that changes.

use fragment_core::owner_browser::{self, Said, FORGOTTEN, REPLACED, WIPED};
use fragment_proto::computer::{BrowserTicket, ComputerBrowser};

use super::*;

/// What a browser's request carries to the Computer DO: the ticket it
/// spends, or the link it comes back with.
const TICKET_HEADER: &str = "x-fragment-computer-ticket";
const LINK_HEADER: &str = "x-fragment-computer-link";
/// The tags its owner's browser's socket carries: `OWNER_TAG`,
/// `owner-<hex>` (the one `MetaKey::BrowserOwner` names while it is the
/// connected one), and `link-<id>` (its link). A guest's CDP socket carries
/// `GUEST_TAG` and `cdp-<hex>`, the name its frames go by.
pub(super) const OWNER_TAG: &str = "browser-owner";
pub(super) const GUEST_TAG: &str = "browser-guest";
/// The guest's CDP sockets open at once to its owner's browser: a
/// browser tool's own, and a supervisor's beside it, for each agent.
const GUESTS_MAX: usize = 2 * AGENTS_MAX as usize;
/// Browsers one computer keeps linked; making one past it forgets the one
/// heard from longest ago.
const LINKS_MAX: u64 = 8;
/// A browser's name until its extension says one.
const UNNAMED: &str = "A browser";
/// Names tried for a new guest socket (four random bytes each, among at
/// most `GUESTS_MAX` open): a clash is retried, never looped on.
const SOCKET_NAMES_TRIED: usize = 8;

/// `computer/browser-ticket`'s and `computer/browser-forget`'s bodies: who
/// asks (the router's signer), and the link forgotten.
#[derive(Deserialize)]
pub(super) struct TicketAsk {
    identity: String,
}
#[derive(Deserialize)]
pub(super) struct ForgetAsk {
    identity: String,
    link: String,
}

/// The name a guest's CDP socket goes by, from its tags (`cdp-<hex>`).
pub(super) fn guest_socket(tags: &[String]) -> Option<String> {
    tags.iter().find_map(|t| t.strip_prefix("cdp-").filter(|s| owner_browser::valid_socket(s)).map(str::to_string))
}

/// The link a browser's socket came with, from its tags (`link-<id>`).
fn link_of(tags: &[String]) -> Option<String> {
    tags.iter().find_map(|t| t.strip_prefix("link-").filter(|s| owner_browser::valid_link_id(s)).map(str::to_string))
}

impl ComputerCell {
    /// Its owner's one-time ticket for connecting a browser (Settings').
    pub(super) fn browser_ticket(&self, b: &TicketAsk) -> CellResult<Value> {
        if b.identity != self.must(MetaKey::Owner)? {
            return Err(CellError::new(ErrorCode::Forbidden, "only the computer's owner connects their browser"));
        }
        self.mint("browser_tickets", &b.identity)
    }

    /// A ticket a linked browser asks for itself, to come back with.
    async fn link_ticket(&self, req: &Request) -> CellResult<Response> {
        let link = self.link_by_secret(req)?;
        json_response(&self.mint("link_tickets", &link)?)
    }

    /// A one-time ticket in `table`, for `whose`: its owner's
    /// (`browser_tickets`, to connect a browser) or a linked browser's
    /// (`link_tickets`, to come back). The token is given once, kept hashed.
    fn mint(&self, table: &str, whose: &str) -> CellResult<Value> {
        let column = match table {
            "browser_tickets" => "identity",
            "link_tickets" => "link",
            t => unreachable!("a ticket table of this file's: {t}"),
        };
        let token = js::random_hex::<24>();
        let expires = js::now_ms() + TICKET_TTL_MS;
        self.exec(&format!("INSERT INTO {table} (hash, {column}, expires_at) VALUES (?, ?, ?)"), vec![sha_hex(&token).into(), whose.into(), expires.into()])?;
        self.trim(table, TICKETS_MAX)?;
        Ok(json!({ "ticket": token, "expiresAt": expires }))
    }

    /// Every guest socket ends (`1001`, `why`): the browser that carried
    /// them is gone.
    fn end_guests(&self, why: &str) {
        for guest in self.state.get_websockets_with_tag(GUEST_TAG) {
            let _ = guest.close(Some(1001), Some(why));
        }
    }

    /// The link whose secret a browser's request carries.
    fn link_by_secret(&self, req: &Request) -> CellResult<String> {
        let secret = req.headers().get(LINK_HEADER)?.filter(|s| owner_browser::valid_link(s));
        let found = match secret {
            Some(s) => self.rows("SELECT id FROM browser_links WHERE hash = ?", vec![sha_hex(&s).into()])?,
            None => vec![],
        };
        found.first().and_then(|r| r["id"].as_str().map(str::to_string)).ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "this browser was forgotten: connect it again from Settings"))
    }

    /// Its owner's browser connecting: the extension's socket, spending a
    /// ticket once, its owner's (which makes the browser its link) or its
    /// link's. One browser at a time: the newest replaces the one before
    /// it, and every guest socket that one carried ends.
    async fn browser_owner(&self, req: &Request) -> CellResult<Response> {
        if req.headers().get("upgrade")?.is_none_or(|u| !u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("the owner's browser connects with a WebSocket"));
        }
        let ticket = req.headers().get(TICKET_HEADER)?.ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "this connection names no ticket"))?;
        let now = js::now_ms();
        let hash = sha_hex(&ticket);
        let owners = self.rows("SELECT identity FROM browser_tickets WHERE hash = ? AND expires_at >= ?", vec![hash.as_str().into(), now.into()])?;
        let links = self.rows("SELECT link FROM link_tickets WHERE hash = ? AND expires_at >= ?", vec![hash.as_str().into(), now.into()])?;
        self.exec("DELETE FROM browser_tickets WHERE hash = ? OR expires_at < ?", vec![hash.as_str().into(), now.into()])?;
        self.exec("DELETE FROM link_tickets WHERE hash = ? OR expires_at < ?", vec![hash.as_str().into(), now.into()])?;
        let (link, made) = match (owners.first(), links.first().and_then(|r| r["link"].as_str())) {
            (Some(t), _) => {
                assert_eq!(t["identity"].as_str(), Some(self.must(MetaKey::Owner)?.as_str()), "a browser ticket is minted for the computer's owner alone");
                let (id, secret) = (js::random_hex::<4>(), js::random_hex::<32>());
                self.exec("INSERT INTO browser_links (id, hash, name, created_at, seen_at) VALUES (?, ?, ?, ?, ?)", vec![id.as_str().into(), sha_hex(&secret).into(), UNNAMED.into(), now.into(), now.into()])?;
                self.exec(&format!("DELETE FROM browser_links WHERE id NOT IN (SELECT id FROM browser_links ORDER BY seen_at DESC LIMIT {LINKS_MAX})"), vec![])?;
                (id, Some(secret))
            }
            // forgotten between its ticket and its socket: no more
            (None, Some(link)) if !self.rows("SELECT id FROM browser_links WHERE id = ?", vec![link.into()])?.is_empty() => (link.to_string(), None),
            _ => return Err(CellError::new(ErrorCode::Unauthenticated, "this connection's ticket was used or is too old: connect your browser again from Settings")),
        };
        self.end_guests("your owner connected another browser");
        for owner in self.state.get_websockets_with_tag(OWNER_TAG) {
            let _ = owner.close(Some(REPLACED), Some("another browser connected"));
        }
        let conn = js::random_hex::<4>();
        self.set_meta(MetaKey::BrowserOwner, &conn)?;
        self.exec("UPDATE browser_links SET seen_at = ? WHERE id = ?", vec![now.into(), link.as_str().into()])?;
        let pair = WebSocketPair::new()?;
        self.state.accept_websocket_with_tags(&pair.server, &[OWNER_TAG, &format!("owner-{conn}"), &format!("link-{link}")]);
        if let Some(secret) = made {
            pair.server.send_with_str(owner_browser::link_frame(&link, &secret))?;
        }
        console_log!("{}", json!({ "computer": "browser", "connected": conn, "link": link }));
        self.tell_owner().await;
        Ok(Response::from_websocket(pair.client)?)
    }

    /// Its owner forgets a browser (Settings'), or the browser forgets
    /// itself (its extension's Disconnect): its link is gone, and its socket
    /// closed `FORGOTTEN`.
    pub(super) async fn browser_forget(&self, b: &ForgetAsk) -> CellResult<Value> {
        if b.identity != self.must(MetaKey::Owner)? {
            return Err(CellError::new(ErrorCode::Forbidden, "only the computer's owner forgets their browser"));
        }
        self.forget_link(&b.link).await?;
        Ok(json!({ "forgotten": b.link }))
    }

    async fn forget_link(&self, link: &str) -> CellResult<()> {
        if !owner_browser::valid_link_id(link) || self.rows("SELECT id FROM browser_links WHERE id = ?", vec![link.into()])?.is_empty() {
            return Err(CellError::new(ErrorCode::NotFound, "no such browser"));
        }
        self.exec("DELETE FROM browser_links WHERE id = ?", vec![link.into()])?;
        self.exec("DELETE FROM link_tickets WHERE link = ?", vec![link.into()])?;
        for ws in self.state.get_websockets_with_tag(&format!("link-{link}")) {
            let _ = ws.close(Some(FORGOTTEN), Some("its owner forgot this browser"));
        }
        console_log!("{}", json!({ "computer": "browser", "forgotten": link }));
        self.tell_owner().await;
        Ok(())
    }

    /// The guest's CDP socket (`egress_browser`): accepted only while its
    /// owner's browser is connected, named by hex its frames carry there.
    async fn browser_guest(&self, req: &Request) -> CellResult<Response> {
        if req.headers().get("upgrade")?.is_none_or(|u| !u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("the owner's browser is a CDP WebSocket"));
        }
        if self.owner_socket()?.is_none() {
            return Err(not_connected());
        }
        if self.state.get_websockets_with_tag(GUEST_TAG).len() >= GUESTS_MAX {
            return Err(CellError::new(ErrorCode::RateLimited, format!("at most {GUESTS_MAX} sockets to the owner's browser at once")));
        }
        // a name no open socket has
        let socket = (0..SOCKET_NAMES_TRIED).map(|_| js::random_hex::<4>()).find(|s| self.guest(s).is_none());
        let Some(socket) = socket else { return Err(CellError::host("no free name for a guest socket")) };
        let pair = WebSocketPair::new()?;
        self.state.accept_websocket_with_tags(&pair.server, &[GUEST_TAG, &format!("cdp-{socket}")]);
        Ok(Response::from_websocket(pair.client)?)
    }

    /// Whether its owner's browser is there, answered as a browser's
    /// DevTools answers `GET /json/version`: the connected one's name and
    /// the guest's socket, or the platform's words that none is connected.
    fn browser_version(&self) -> CellResult<Response> {
        match self.browsers()?.into_iter().find(|b| b.connected) {
            Some(b) => json_response(&owner_browser::version(&b.name)),
            None => Err(not_connected()),
        }
    }

    /// Its owner's connected browser's socket, if one is.
    fn owner_socket(&self) -> CellResult<Option<WebSocket>> {
        let Some(conn) = self.meta(MetaKey::BrowserOwner)? else { return Ok(None) };
        Ok(self.state.get_websockets_with_tag(&format!("owner-{conn}")).into_iter().next())
    }

    /// The guest's CDP socket `socket`, if it is open.
    fn guest(&self, socket: &str) -> Option<WebSocket> {
        self.state.get_websockets_with_tag(&format!("cdp-{socket}")).into_iter().next()
    }

    /// A frame from the owner's browser: its name, its keepalive, or a
    /// frame for the guest socket it names.
    pub(super) async fn owner_said(&self, owner: &WebSocket, message: WebSocketIncomingMessage) -> CellResult<()> {
        let WebSocketIncomingMessage::String(frame) = message else {
            let _ = owner.close(Some(1003), Some("CDP is text"));
            return Ok(());
        };
        match owner_browser::read(&frame) {
            Some(Said::Ping) => owner.send_with_str(owner_browser::PONG)?,
            Some(Said::Hello { name }) => {
                if let Some(link) = link_of(&self.state.get_tags(owner)) {
                    self.exec("UPDATE browser_links SET name = ? WHERE id = ?", vec![name.into(), link.into()])?;
                    self.tell_owner().await;
                }
            }
            Some(Said::Message { socket, message }) => match self.guest(socket) {
                Some(guest) => guest.send_with_str(message)?,
                // gone already: the extension hears so, and lets it go
                None => owner.send_with_str(owner_browser::frame(socket, owner_browser::CLOSE))?,
            },
            Some(Said::Close { socket }) => {
                if let Some(guest) = self.guest(socket) {
                    let _ = guest.close(Some(1000), Some("your owner's browser ended it"));
                }
            }
            None => console_log!("{}", json!({ "computer": "browser", "dropped": frame.chars().take(64).collect::<String>() })),
        }
        Ok(())
    }

    /// A frame from one of the guest's CDP sockets: to the owner's browser,
    /// named.
    pub(super) fn guest_said(&self, guest: &WebSocket, socket: &str, message: WebSocketIncomingMessage) -> CellResult<()> {
        let WebSocketIncomingMessage::String(message) = message else {
            let _ = guest.close(Some(1003), Some("CDP is text"));
            return Ok(());
        };
        match self.owner_socket()? {
            Some(owner) => owner.send_with_str(owner_browser::frame(socket, &message))?,
            None => {
                let _ = guest.close(Some(1001), Some("your owner's browser disconnected"));
            }
        }
        Ok(())
    }

    /// One of its browser sockets closed: the owner's browser (the
    /// connected one takes every guest socket with it, and is last heard
    /// now), or a guest's CDP socket (the browser hears it ended). `false`
    /// for a socket of neither kind.
    pub(super) async fn browser_socket_ended(&self, tags: &[String]) -> bool {
        if tags.iter().any(|t| t == OWNER_TAG) {
            let current = self.meta(MetaKey::BrowserOwner).ok().flatten().map(|conn| format!("owner-{conn}"));
            if current.is_some_and(|c| tags.contains(&c)) {
                let link = link_of(tags);
                let noted = self.delete_meta(MetaKey::BrowserOwner).and_then(|_| match &link {
                    Some(l) => self.exec("UPDATE browser_links SET seen_at = ? WHERE id = ?", vec![js::now_ms().into(), l.as_str().into()]),
                    None => Ok(()),
                });
                if let Err(e) = noted {
                    console_error!("{}", json!({ "computer": "browser-disconnected", "link": link, "error": e.message }));
                }
                self.end_guests("your owner's browser disconnected");
                console_log!("{}", json!({ "computer": "browser", "disconnected": link }));
                self.tell_owner().await;
            }
            return true;
        }
        if let Some(socket) = guest_socket(tags) {
            if let Ok(Some(owner)) = self.owner_socket() {
                let _ = owner.send_with_str(owner_browser::frame(&socket, owner_browser::CLOSE));
            }
            return true;
        }
        false
    }

    /// The browsers linked, newest heard first, and which is connected.
    pub(super) fn browsers(&self) -> CellResult<Vec<ComputerBrowser>> {
        let connected = self.owner_socket()?.and_then(|ws| link_of(&self.state.get_tags(&ws)));
        let rows = self.rows("SELECT id, name, seen_at FROM browser_links ORDER BY seen_at DESC", vec![])?;
        Ok(rows
            .iter()
            .map(|r| {
                let id = r["id"].as_str().unwrap_or_default().to_string();
                ComputerBrowser { connected: connected.as_deref() == Some(id.as_str()), name: r["name"].as_str().unwrap_or(UNNAMED).to_string(), seen_at: r["seen_at"].as_i64().unwrap_or(0), id }
            })
            .collect())
    }

    /// A wipe of its owner: its browser, and what it carried, connected no more.
    pub(super) fn browser_wiped(&self) {
        for ws in self.state.get_websockets_with_tag(OWNER_TAG) {
            let _ = ws.close(Some(WIPED), Some("its owner was wiped"));
        }
        self.end_guests("its owner was wiped");
    }

    /// The test lever `drop-browser`: the connected browser's socket ends as
    /// a dropped network ends it, so its extension comes back on its own.
    pub(super) fn browser_dropped(&self) -> CellResult<bool> {
        let Some(owner) = self.owner_socket()? else { return Ok(false) };
        let _ = owner.close(Some(1011), Some("dropped (a test lever)"));
        Ok(true)
    }

    /// A request its `KIND_HEADER` says is the browser's: its socket, its
    /// link's ticket or forgetting, or the guest's CDP socket.
    pub(super) async fn browser_route(&self, kind: &str, req: &Request) -> CellResult<Response> {
        match kind {
            "browser" => self.browser_owner(req).await,
            "browser-link-ticket" => self.link_ticket(req).await,
            "browser-link-forget" => {
                let link = self.link_by_secret(req)?;
                self.forget_link(&link).await?;
                json_response(&json!({ "forgotten": link }))
            }
            "browser-guest" => self.browser_guest(req).await,
            "browser-version" => self.browser_version(),
            k => unreachable!("a browser kind the router named: {k}"),
        }
    }
}

/// Whether `kind` is one `browser_route` answers.
pub(super) fn is_browser_kind(kind: &str) -> bool {
    matches!(kind, "browser" | "browser-link-ticket" | "browser-link-forget" | "browser-guest" | "browser-version")
}

/// `/api/computers/{id}/browser…`, from its owner (`who`): a ticket to
/// connect a browser, or a browser forgotten.
pub(super) async fn api_route(env: &Env, who: &str, method: Method, id: &str, rest: &[&str]) -> CellResult<Response> {
    let v = owned(env, who, id).await?;
    match (method, rest) {
        (Method::Post, ["ticket"]) => {
            let t = ask(env, id, "computer/browser-ticket", &json!({ "identity": who })).await?;
            json_response(&ticket_on(&v.origin, &t)?)
        }
        (Method::Delete, ["links", link]) => json_response(&ask(env, id, "computer/browser-forget", &json!({ "identity": who, "link": link })).await?),
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/computers/{id}/browser/{}", m.as_ref(), rest.join("/")))),
    }
}

/// A browser's request on the computer's own origin (`/__browser…`),
/// through its extension alone: its socket (`?t=` a ticket), a ticket its
/// link asks for (`POST /__browser/ticket`), or its link forgotten (`POST
/// /__browser/forget`), each with `{"link": …}` as its body.
pub(super) async fn host_route(mut req: Request, env: &Env, url: &Url, id: &str) -> CellResult<Response> {
    if !req.headers().get("origin")?.is_some_and(|o| owner_browser::extension_origin(&o)) {
        return Err(CellError::new(ErrorCode::Forbidden, "the owner's browser connects through its extension"));
    }
    if req.method() == Method::Post {
        let body: Value = req.json().await.map_err(|_| CellError::invalid("name this browser's link: {\"link\": …}"))?;
        let link = body["link"].as_str().ok_or_else(|| CellError::invalid("name this browser's link: {\"link\": …}"))?.to_string();
        return link_route(env, url, id, &link).await;
    }
    match (req.method(), url.path()) {
        (Method::Get, "/__browser") => {
            let ticket = url.query_pairs().find(|(n, _)| n == "t").map(|(_, v)| v.into_owned()).ok_or_else(|| CellError::invalid("this connection names no ticket"))?;
            to_computer(env, id, upgrade_request(&req, "browser", &[(TICKET_HEADER, &ticket)])?).await
        }
        (m, p) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
    }
}

/// A ticket the Computer DO minted (`t`), as the socket its browser opens
/// on the computer's own origin (`origin`, http or https).
fn ticket_on(origin: &str, t: &Value) -> CellResult<BrowserTicket> {
    let ticket = t["ticket"].as_str().ok_or_else(|| CellError::host("the computer minted no ticket"))?;
    Ok(BrowserTicket { url: format!("{}/__browser?t={ticket}", origin.replacen("http", "ws", 1)), expires_at: t["expiresAt"].as_i64().unwrap_or(0) })
}

/// A linked browser's own ask: a ticket to come back with, or its link
/// forgotten.
async fn link_route(env: &Env, url: &Url, id: &str, link: &str) -> CellResult<Response> {
    match url.path() {
        "/__browser/ticket" => {
            let mut answer = to_computer(env, id, object_request("browser-link-ticket", &[(LINK_HEADER, link)])?).await?;
            if answer.status_code() != 200 {
                return Ok(answer);
            }
            let t: Value = answer.json().await?;
            json_response(&ticket_on(&url.origin().ascii_serialization(), &t)?)
        }
        "/__browser/forget" => to_computer(env, id, object_request("browser-link-forget", &[(LINK_HEADER, link)])?).await,
        p => Err(CellError::new(ErrorCode::NotFound, format!("no route POST {p}"))),
    }
}

/// The guest's CDP socket to its owner's browser
/// (`ws://browser.fragment.internal/devtools/browser`): the Computer DO
/// takes it, and carries it while that browser is connected; and its ask
/// first (`GET /json/version`), which the Computer DO answers. Any of the
/// computer's agents may use it: one person's agents share their computer,
/// and are not fenced from each other (decision 44).
pub(super) async fn egress_browser(req: Request, env: &Env, computer: &str) -> CellResult<Response> {
    if req.method() == Method::Get && req.path() == owner_browser::VERSION_PATH {
        return to_computer(env, computer, object_request("browser-version", &[])?).await;
    }
    if !owner_browser::guest_path(&req.path()) {
        return Err(CellError::new(ErrorCode::NotFound, format!("the owner's browser is a CDP socket at {}, asked after at {}", owner_browser::GUEST_PATH, owner_browser::VERSION_PATH)));
    }
    to_computer(env, computer, upgrade_request(&req, "browser-guest", &[])?).await
}

/// A request of the Computer DO's own (`kind`), carrying `extra`.
fn object_request(kind: &str, extra: &[(&str, &str)]) -> CellResult<Request> {
    let headers = Headers::new();
    headers.set(KIND_HEADER, kind)?;
    for (k, v) in extra {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers);
    Ok(Request::new_with_init(&format!("https://computer.internal/{kind}"), &init)?)
}

/// The guest's answer while no browser of its owner's is connected.
fn not_connected() -> CellError {
    CellError::new(ErrorCode::NotConnected, "your owner's browser is not connected: they connect it from Settings, with the Fragment extension")
}

async fn to_computer(env: &Env, id: &str, req: Request) -> CellResult<Response> {
    Ok(env.durable_object("COMPUTER")?.get_by_name(id)?.fetch_with_request(req).await?)
}
