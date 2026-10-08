//! The `Principal` cell: one Durable Object per identity (`id:…`), holding
//! the index of the fragments it belongs to (what `GET /api/fragments`
//! lists). The fragments are the authority; each delivers its changes here
//! from an outbox, versioned so a late delivery never undoes a newer one.
//! A fragment's row in its owner's list also carries its sharing (who may
//! open it, its members and guests), sent with each change to them, and
//! every row its agents (a chat's lead first), sent with each change to
//! them; a chat's row shows its newest message from this person's search
//! (below). So the platform's page reads this one cell and wakes no
//! fragment. Which keys an identity holds is the registry's (registry.rs).
//!
//! Three things here are the person's own, not a fragment's:
//!
//! - **Archived**, a flag on their row (`PUT /api/fragments/{name}/archived`):
//!   their view of the fragment, which no fragment's change touches. It
//!   goes when they leave the fragment, or it is made again.
//! - **Search** (docs/cloudflare-v1.md, decision 9 and lesson 12): the text
//!   of the messages in the fragments they are in, which each fragment sends
//!   from an outbox of its own (search.rs), in FTS5. An entry is keyed by its
//!   fragment and its place in that fragment's log (`n`), so a delivery sent
//!   twice, or late, adds nothing twice. Entries are fenced by the row: a
//!   delivery is taken only while the row names a role, at the incarnation
//!   it was sent from, and only the messages of the channels the row names
//!   as searched; the row's removal (or a new incarnation) drops them all,
//!   and a row that names other channels drops the rest; a search reads
//!   only entries of rows that name a role. At most
//!   `SEARCH_ENTRIES_PER_FRAGMENT_MAX` a fragment and `SEARCH_ENTRIES_MAX` in
//!   all are kept, the oldest going first. A chat's newest entry is its
//!   row's `preview`; a chat with none here (no message every member may
//!   read, or its entries gone past the total) shows none.
//! - **Watching** (`GET /api/fragments/watch`; Paul on p5, 2026-10-05: an
//!   app his agent made did not show in his sidebar until he reloaded):
//!   the person's open shells, and any CLI, hold a socket here, hibernated
//!   (the router decided whose list it is). Each change this list applies
//!   (a row's, newer than the one it holds: a fragment made, shared with
//!   them, changed, left or deleted; their archiving; or a chat's message
//!   new here, its preview) is told to every
//!   one as `{type: "changed"}`, naming nothing: each page reads the list
//!   again with its own credential, so a socket that outlives its session
//!   learns only that something changed. A socket closed or lost misses
//!   nothing a page needs: it reads the list again as it reconnects. At
//!   most `LIST_WATCHERS_MAX` at once; nothing is read from them.
//!
//!   Their computer tells the same sockets when what its owner is told of
//!   it changes (`/changed`, from the Computer DO: `tell_changed`), so a
//!   page reads it again with the list.
//!
//! **Wiped** (docs/api.md, Operators): a wipe of its person (or of the
//! person who owns its agent) closes its sockets and empties it in one
//! step, leaving one row that says so (`wiped`). From then it takes
//! nothing and lists nothing: a change still on its way from a fragment's
//! outbox is taken (so the outbox forgets it) and kept nowhere, so nothing
//! of a wiped person's list comes back. An identity is never made again.

use std::cell::Cell;

use fragment_core::npub;
use fragment_core::search::{self, Query};
use fragment_proto::{limits, valid_channel_name, Archived, ErrorCode, FragmentKind, FragmentList, ListedFragment, MessageHit, Role, SearchAnswer, Sharing};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memberships (
  fragment TEXT PRIMARY KEY, role TEXT, incarnation INTEGER NOT NULL, version INTEGER NOT NULL,
  sharing TEXT, face TEXT, archived INTEGER NOT NULL DEFAULT 0, searched TEXT);
CREATE TABLE IF NOT EXISTS search_entries (
  id INTEGER PRIMARY KEY, fragment TEXT NOT NULL, n INTEGER NOT NULL, channel TEXT NOT NULL, seq INTEGER NOT NULL,
  at INTEGER NOT NULL, text TEXT NOT NULL, UNIQUE (fragment, n));
CREATE INDEX IF NOT EXISTS search_entries_at ON search_entries (at, id);
CREATE VIRTUAL TABLE IF NOT EXISTS search_text USING fts5(
  text, content = 'search_entries', content_rowid = 'id', tokenize = 'unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS search_entries_added AFTER INSERT ON search_entries BEGIN
  INSERT INTO search_text (rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS search_entries_dropped AFTER DELETE ON search_entries BEGIN
  INSERT INTO search_text (search_text, rowid, text) VALUES ('delete', old.id, old.text);
END;
";

/// The one row a wiped list keeps: when it was wiped. Apart from `SCHEMA`,
/// whose tables a wipe drops.
const WIPED_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wiped (at INTEGER NOT NULL);";
/// `SCHEMA`'s full-text table, which `ddl::tables` does not read (a virtual
/// table): a wipe drops it beside the rest.
const SEARCH_TEXT: &str = "search_text";
/// The rows one `wipe/view` page lists.
const WIPE_VIEW_PAGE: i64 = 200;

/// The tag of the sockets that watch this list (`/watch`).
const WATCH_TAG: &str = "watch";
/// A watching socket's first frame, as it opens.
const HELLO: &str = r#"{"type":"hello"}"#;
/// What a watching socket is told of a change: no more.
const CHANGED: &str = r#"{"type":"changed"}"#;

/// The columns `/list` and a search read of a row, and a chat's newest
/// search entry (`said`), read by its place in its fragment's log.
const LISTED: &str = "SELECT fragment AS name, role, sharing, face, archived,
  CASE WHEN json_extract(face, '$.kind') = 'chat'
    THEN (SELECT text FROM search_entries e WHERE e.fragment = m.fragment ORDER BY e.n DESC LIMIT 1) END AS said
  FROM memberships m WHERE role IS NOT NULL ORDER BY fragment";

#[durable_object]
pub struct PrincipalCell {
    state: State,
    /// Its person was wiped (`wiped`'s row, read as it starts): it takes
    /// nothing more.
    wiped: Cell<bool>,
}

/// `wipe/view`'s body: the page of rows after `after` (a fragment's name).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WipeView {
    #[serde(default)]
    after: Option<String>,
}

/// A row as a wipe reads it: any role, or none (a fragment left or ended).
#[derive(Serialize, Deserialize)]
struct WipeRow {
    fragment: String,
    role: Option<Role>,
}

/// A fragment's change to this key's membership. `incarnation` is the
/// fragment's creation time (a deleted and re-created fragment is a new
/// incarnation); `version` orders changes within one. The owner's row
/// carries the fragment's sharing, as it was when the change was sent.
#[derive(Deserialize)]
struct IndexChange {
    fragment: String,
    role: Option<Role>,
    incarnation: i64,
    version: i64,
    #[serde(default)]
    sharing: Option<Sharing>,
    /// What it is and its title, on every row a role names.
    #[serde(default)]
    face: Option<Face>,
    /// The channels its search holds (those every member may read), on
    /// every row a role names.
    #[serde(default)]
    searched: Option<Vec<String>>,
}

/// What a person's list shows of a fragment.
#[derive(serde::Serialize, Deserialize)]
struct Face {
    #[serde(default)]
    kind: FragmentKind,
    #[serde(default)]
    title: Option<String>,
    /// Its agent members, the first added first (`limits::LISTED_AGENTS_MAX`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agents: Vec<String>,
}

/// A `memberships` row as `/list` reads it.
#[derive(Deserialize)]
struct Listed {
    name: String,
    role: Role,
    sharing: Option<String>,
    face: Option<String>,
    archived: i64,
    said: Option<String>,
}

/// `PUT /archived`: the router's, for the signer (`SetArchived`, named).
#[derive(Deserialize)]
struct SetArchived {
    fragment: String,
    archived: bool,
}

/// `POST /search/entries`: a fragment's messages, for this person's search
/// (search.rs sends it). One definition both ends build against.
#[derive(Serialize, Deserialize)]
pub(crate) struct SearchBatch {
    pub fragment: String,
    pub incarnation: i64,
    pub entries: Vec<SearchEntry>,
}

/// One message: its place in its fragment's search log (`n`, from 1,
/// never reused within an incarnation), its record, and its text.
#[derive(Serialize, Deserialize)]
pub(crate) struct SearchEntry {
    pub n: i64,
    pub channel: String,
    pub seq: i64,
    pub at: i64,
    pub text: String,
}

/// What a delivery did: `member` is false when this list holds no role on
/// the fragment at that incarnation (not yet, or not any more), and took
/// nothing; `applied` counts the entries new here.
#[derive(Serialize, Deserialize)]
pub(crate) struct SearchApplied {
    pub member: bool,
    pub applied: u64,
}

/// A search's first pass: the newest matching entries' places.
#[derive(Deserialize)]
struct Hit {
    id: i64,
    fragment: String,
    channel: String,
    seq: i64,
    at: i64,
}

impl DurableObject for PrincipalCell {
    fn new(state: State, _env: Env) -> Self {
        let sql = state.storage().sql();
        sql.exec(SCHEMA, None).expect("the Principal schema applies");
        sql.exec(WIPED_SCHEMA, None).expect("the wiped row's schema applies");
        let marks: Vec<Value> = sql.exec("SELECT COUNT(*) AS n FROM wiped", None).and_then(|c| c.to_array()).expect("the wiped row reads");
        let wiped = marks.first().and_then(|r| r["n"].as_i64()).expect("COUNT answers a row") > 0;
        PrincipalCell { state, wiped: Cell::new(wiped) }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(r) => Ok(r),
            Err(e) => e.response(),
        }
    }

    // A watching socket says nothing the list reads.
    async fn websocket_message(&self, _ws: WebSocket, _message: WebSocketIncomingMessage) -> Result<()> {
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, code: usize, reason: String, _clean: bool) -> Result<()> {
        let code = if code == 1005 || code == 1006 { 1000 } else { code as u16 };
        let _ = ws.close(Some(code), Some(reason));
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

impl PrincipalCell {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<Value>> {
        Ok(self.sql().exec(q, binds)?.to_array::<Value>()?)
    }

    fn typed<T: serde::de::DeserializeOwned>(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<T>> {
        Ok(self.sql().exec(q, binds)?.to_array::<T>()?)
    }

    fn count(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<i64> {
        let rows = self.rows(q, binds)?;
        let n = rows.first().and_then(|r| r["n"].as_i64()).ok_or_else(|| CellError::host(format!("a count answered no number: {q}")))?;
        assert!(n >= 0, "a count is never negative");
        Ok(n)
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        let url = req.url()?;
        if let Some(route) = req.path().strip_prefix("/wipe/") {
            // only a wipe's orchestrator sets the header (routed.rs `marker`)
            if req.headers().get(crate::wipe::WIPE_HEADER)?.is_none() {
                return Err(CellError::new(ErrorCode::NotFound, format!("no route /wipe/{route}")));
            }
            let route = route.to_string();
            let body = req.bytes().await?;
            return Ok(Response::from_json(&self.wipe(&route, &body)?)?);
        }
        if self.wiped.get() {
            return self.wiped_answer(&req);
        }
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/index") => {
                let c: IndexChange = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                let applied = self.index(c)?;
                if applied {
                    self.tell();
                }
                Ok(Response::from_json(&json!({ "ok": true, "applied": applied }))?)
            }
            (Method::Get, "/list") => {
                // `GET /api/fragments`'s answer, whole: the router passes it through
                let rows: Vec<Listed> = self.typed(LISTED, vec![])?;
                let fragments = rows.into_iter().map(listed).collect::<CellResult<Vec<_>>>()?;
                Ok(Response::from_json(&FragmentList { fragments })?)
            }
            (Method::Put, "/archived") => {
                let s: SetArchived = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                let archived = self.archive(s)?;
                self.tell();
                Ok(Response::from_json(&archived)?)
            }
            (Method::Post, "/search/entries") => {
                let b: SearchBatch = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("body: {e}")))?;
                Ok(Response::from_json(&self.take_entries(b)?)?)
            }
            (Method::Get, "/search") => {
                let q = url.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned());
                let q = q.ok_or_else(|| CellError::invalid("name what to look for: ?q="))?;
                Ok(Response::from_json(&self.search(&q)?)?)
            }
            (Method::Get, "/watch") => self.watch(&req),
            // something of theirs the list does not hold changed (their
            // computer: what its owner is told of it), so their pages read
            // again; only the Worker's own code reaches here
            (Method::Post, "/changed") => {
                self.tell();
                Ok(Response::from_json(&json!({ "ok": true }))?)
            }
            (m, p) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
        }
    }

    /// What a wiped list answers: a change taken and kept nowhere, an empty
    /// list, a search that finds nothing, and no socket or archiving.
    fn wiped_answer(&self, req: &Request) -> CellResult<Response> {
        assert!(self.wiped.get(), "only a wiped list answers so");
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/index") => Ok(Response::from_json(&json!({ "ok": true, "applied": false }))?),
            (Method::Get, "/list") => Ok(Response::from_json(&FragmentList { fragments: vec![] })?),
            (Method::Post, "/search/entries") => Ok(Response::from_json(&SearchApplied { member: false, applied: 0 })?),
            (Method::Get, "/search") => Ok(Response::from_json(&SearchAnswer { fragments: vec![], messages: vec![] })?),
            (m, p) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {p}: this list's person was wiped", m.as_ref()))),
        }
    }

    /// A wipe's calls (docs/api.md, Operators; the router's cell/src/wipe.rs):
    /// `view {after?}` → `{wiped, rows: [{fragment, role}], more, entries}`,
    /// its rows a page at a time (any role, or none), and its search's
    /// entries; `end` → `{wiped, rows}`: its sockets closed and every table
    /// dropped and made again empty, with the row that says it was wiped,
    /// in one step (no await). Again, it changes nothing.
    fn wipe(&self, route: &str, body: &[u8]) -> CellResult<Value> {
        match route {
            "view" => {
                let b: WipeView = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("wipe/view: {e}")))?;
                let rows: Vec<WipeRow> = self.typed(
                    "SELECT fragment, role FROM memberships WHERE fragment > ? ORDER BY fragment LIMIT ?",
                    vec![b.after.unwrap_or_default().into(), SqlStorageValue::Integer(WIPE_VIEW_PAGE + 1)],
                )?;
                let more = rows.len() as i64 > WIPE_VIEW_PAGE;
                let rows: Vec<WipeRow> = rows.into_iter().take(WIPE_VIEW_PAGE as usize).collect();
                let entries = self.count("SELECT COUNT(*) AS n FROM search_entries", vec![])?;
                Ok(json!({ "wiped": self.wiped.get(), "rows": rows, "more": more, "entries": entries }))
            }
            "end" => {
                for ws in self.state.get_websockets() {
                    let _ = ws.close(Some(4003), Some("this person was wiped"));
                }
                let rows = self.count("SELECT COUNT(*) AS n FROM memberships", vec![])?;
                // one step, no await: emptied and marked, or neither
                let sql = self.sql();
                for table in fragment_core::ddl::tables(SCHEMA) {
                    sql.exec(&format!("DROP TABLE IF EXISTS {table}"), None)?;
                }
                sql.exec(&format!("DROP TABLE IF EXISTS {SEARCH_TEXT}"), None)?;
                sql.exec(SCHEMA, None)?;
                if !self.wiped.get() {
                    sql.exec("INSERT INTO wiped (at) VALUES (?)", vec![SqlStorageValue::Integer(crate::js::now_ms())])?;
                }
                self.wiped.set(true);
                assert_eq!(self.count("SELECT COUNT(*) AS n FROM memberships", vec![])?, 0, "a wiped list holds no row");
                assert_eq!(self.count("SELECT COUNT(*) AS n FROM wiped", vec![])?, 1, "a wiped list says so once");
                Ok(json!({ "wiped": true, "rows": rows }))
            }
            other => Err(CellError::new(ErrorCode::NotFound, format!("no wipe route {other}"))),
        }
    }

    /// A socket that watches this list (the router decided whose it is):
    /// hibernated, told of each change (`tell`), at most
    /// `LIST_WATCHERS_MAX` at once.
    fn watch(&self, req: &Request) -> CellResult<Response> {
        if !req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("a list is watched over a WebSocket; send Upgrade: websocket"));
        }
        let open = self.state.get_websockets_with_tag(WATCH_TAG).len();
        if open >= limits::LIST_WATCHERS_MAX {
            return Err(CellError::new(ErrorCode::RateLimited, format!("{open} pages watch this list, the most it tells; close one")));
        }
        let pair = WebSocketPair::new()?;
        self.state.accept_websocket_with_tags(&pair.server, &[WATCH_TAG]);
        pair.server.send_with_str(HELLO)?;
        Ok(Response::from_websocket(pair.client)?)
    }

    /// Tells every watching socket that the list changed. One that fails
    /// (closing as it is told) is its page's to reconnect, which reads the
    /// list again then.
    fn tell(&self) {
        let watching = self.state.get_websockets_with_tag(WATCH_TAG);
        assert!(watching.len() <= limits::LIST_WATCHERS_MAX, "a list holds at most its watchers");
        for ws in watching {
            let _ = ws.send_with_str(CHANGED);
        }
    }

    /// Applies a fragment's change to its row, when it is newer than the
    /// row's. A change that removes the person, or comes from a new
    /// incarnation, drops what the row held for them: their archiving and
    /// their search entries; one that names the channels searched drops
    /// the entries of any other.
    fn index(&self, c: IndexChange) -> CellResult<bool> {
        if c.searched.as_ref().is_some_and(|s| s.len() > limits::CHANNELS_MAX || !s.iter().all(|ch| valid_channel_name(ch))) {
            return Err(CellError::invalid(format!("{}'s searched channels are more than a fragment declares, or not channels' names", c.fragment)));
        }
        if c.face.as_ref().is_some_and(|f| f.agents.len() > limits::LISTED_AGENTS_MAX || !f.agents.iter().all(|a| npub::is_identity(a))) {
            return Err(CellError::invalid(format!("{}'s agents are more than a row names, or not identities", c.fragment)));
        }
        let stored = self.rows("SELECT incarnation, version FROM memberships WHERE fragment = ?", vec![c.fragment.as_str().into()])?;
        let (newer, reborn) = match stored.first() {
            None => (true, false),
            Some(r) => {
                let (inc, ver) = (r["incarnation"].as_i64().unwrap_or(0), r["version"].as_i64().unwrap_or(0));
                (c.incarnation > inc || (c.incarnation == inc && c.version > ver), c.incarnation != inc)
            }
        };
        if !newer {
            return Ok(false);
        }
        let role = c.role.map_or(SqlStorageValue::Null, |r| r.as_str().into());
        let sharing = match &c.sharing {
            Some(s) => serde_json::to_string(s).map_err(|e| CellError::host(format!("sharing: {e}")))?.into(),
            None => SqlStorageValue::Null,
        };
        let face = match &c.face {
            Some(f) => serde_json::to_string(f).map_err(|e| CellError::host(format!("face: {e}")))?.into(),
            None => SqlStorageValue::Null,
        };
        let searched = match &c.searched {
            Some(s) => serde_json::to_string(s).map_err(|e| CellError::host(format!("searched: {e}")))?.into(),
            None => SqlStorageValue::Null,
        };
        let gone = c.role.is_none() || reborn;
        // a change that carries no face (a fragment from before faces)
        // keeps the one the row has; no change touches the person's archiving
        // but the one that ends what it was about
        self.rows(
            "INSERT INTO memberships (fragment, role, incarnation, version, sharing, face, archived, searched) VALUES (?, ?, ?, ?, ?, ?, 0, ?)
             ON CONFLICT (fragment) DO UPDATE SET role = excluded.role, incarnation = excluded.incarnation, version = excluded.version,
               sharing = excluded.sharing, face = COALESCE(excluded.face, memberships.face), searched = excluded.searched,
               archived = CASE WHEN ? THEN 0 ELSE memberships.archived END",
            vec![
                c.fragment.as_str().into(),
                role,
                SqlStorageValue::Integer(c.incarnation),
                SqlStorageValue::Integer(c.version),
                sharing,
                face,
                searched.clone(),
                SqlStorageValue::Integer(i64::from(gone)),
            ],
        )?;
        if gone {
            self.rows("DELETE FROM search_entries WHERE fragment = ?", vec![c.fragment.as_str().into()])?;
        } else if c.searched.is_some() {
            self.rows(
                "DELETE FROM search_entries WHERE fragment = ? AND channel NOT IN (SELECT value FROM json_each(?))",
                vec![c.fragment.as_str().into(), searched],
            )?;
        }
        Ok(true)
    }

    /// The person archives a fragment of theirs, or brings it back: only a
    /// row that names a role (404 for any other, as for a fragment they
    /// cannot see). The same again answers the same.
    fn archive(&self, s: SetArchived) -> CellResult<Archived> {
        if !fragment_proto::valid_fragment_name(&s.fragment) {
            return Err(CellError::invalid(format!("{:?} is not a fragment's name (<label>.<username>)", s.fragment)));
        }
        let changed = self.rows(
            "UPDATE memberships SET archived = ? WHERE fragment = ? AND role IS NOT NULL RETURNING archived",
            vec![SqlStorageValue::Integer(i64::from(s.archived)), s.fragment.as_str().into()],
        )?;
        let Some(row) = changed.first() else {
            return Err(CellError::new(ErrorCode::NotFound, format!("no fragment {} of yours", s.fragment)));
        };
        assert_eq!(changed.len(), 1, "a fragment is one row");
        assert_eq!(row["archived"].as_i64(), Some(i64::from(s.archived)), "the row says what was set");
        Ok(Archived { name: s.fragment, archived: s.archived })
    }

    /// A fragment's messages, taken while this person holds a role on it
    /// at the incarnation they come from, and only those of the channels
    /// the row names as searched (a late batch's of a channel tightened
    /// since are not); each entry once. A row that names none yet (one
    /// from before rows did) takes nothing yet. Then the fragment's
    /// entries past its limit, and everyone's past the total, go, the
    /// oldest first. A chat's new message is its row's preview: the
    /// list's watchers are told.
    fn take_entries(&self, b: SearchBatch) -> CellResult<SearchApplied> {
        if !fragment_proto::valid_fragment_name(&b.fragment) {
            return Err(CellError::invalid(format!("{:?} is not a fragment's name", b.fragment)));
        }
        if b.entries.len() > limits::SEARCH_BATCH_MAX {
            return Err(CellError::invalid(format!("a delivery carries at most {} entries", limits::SEARCH_BATCH_MAX)));
        }
        for e in &b.entries {
            let valid = e.n >= 1 && e.seq >= 1 && e.at >= 0 && valid_channel_name(&e.channel) && e.text.len() <= limits::SEARCH_TEXT_MAX_BYTES;
            if !valid {
                return Err(CellError::invalid(format!("entry {} of {} is not one a fragment sends", e.n, b.fragment)));
            }
        }
        let fenced = self.rows(
            "SELECT searched, json_extract(face, '$.kind') = 'chat' AS chat FROM memberships WHERE fragment = ? AND role IS NOT NULL AND incarnation = ? AND searched IS NOT NULL",
            vec![b.fragment.as_str().into(), SqlStorageValue::Integer(b.incarnation)],
        )?;
        let Some(row) = fenced.first() else {
            return Ok(SearchApplied { member: false, applied: 0 });
        };
        let searched = row["searched"].as_str().expect("a fenced row names its searched channels");
        let searched: Vec<String> = serde_json::from_str(searched).map_err(|e| CellError::host(format!("{}'s stored searched channels: {e}", b.fragment)))?;
        let chat = row["chat"].as_i64() == Some(1);
        let mut applied = 0u64;
        for e in b.entries.iter().filter(|e| searched.contains(&e.channel)) {
            let added = self.rows(
                "INSERT INTO search_entries (fragment, n, channel, seq, at, text) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (fragment, n) DO NOTHING RETURNING id",
                vec![
                    b.fragment.as_str().into(),
                    SqlStorageValue::Integer(e.n),
                    e.channel.as_str().into(),
                    SqlStorageValue::Integer(e.seq),
                    SqlStorageValue::Integer(e.at),
                    e.text.as_str().into(),
                ],
            )?;
            applied += added.len() as u64;
        }
        assert!(applied <= b.entries.len() as u64, "each entry is taken at most once");
        // the fragment's newest; then everyone's newest
        self.rows(
            "DELETE FROM search_entries WHERE fragment = ? AND n <= (SELECT n FROM search_entries WHERE fragment = ? ORDER BY n DESC LIMIT 1 OFFSET ?)",
            vec![b.fragment.as_str().into(), b.fragment.as_str().into(), SqlStorageValue::Integer(limits::SEARCH_ENTRIES_PER_FRAGMENT_MAX)],
        )?;
        let over = self.count("SELECT COUNT(*) AS n FROM search_entries", vec![])? - limits::SEARCH_ENTRIES_MAX;
        if over > 0 {
            // kept to the total after every delivery, so one is over by at most its own
            assert!(over <= limits::SEARCH_BATCH_MAX as i64, "the total is kept after each delivery");
            self.rows("DELETE FROM search_entries WHERE id IN (SELECT id FROM search_entries ORDER BY at, id LIMIT ?)", vec![SqlStorageValue::Integer(over)])?;
        }
        if chat && applied > 0 {
            self.tell();
        }
        Ok(SearchApplied { member: true, applied })
    }

    /// `GET /api/search?q=`: the fragments whose title or label hold every
    /// word, then the newest messages that do, from fragments this person
    /// holds a role on now (a row's role is its fence: a removal arrives as
    /// its row's change, newer than any delivery before it).
    fn search(&self, q: &str) -> CellResult<SearchAnswer> {
        let query = Query::parse(q).map_err(|e| CellError::invalid(e.to_string()))?;
        let Some(fts) = query.fts() else {
            return Ok(SearchAnswer { fragments: vec![], messages: vec![] });
        };
        let rows: Vec<Listed> = self.typed(LISTED, vec![])?;
        let mut fragments = Vec::with_capacity(limits::SEARCH_FRAGMENTS_MAX);
        for row in rows {
            if fragments.len() == limits::SEARCH_FRAGMENTS_MAX {
                break;
            }
            let f = listed(row)?;
            if query.names(&f.name, f.title.as_deref()) {
                fragments.push(f);
            }
        }
        // the newest hits' places first; the snippets of those alone
        let hits: Vec<Hit> = self.typed(
            "SELECT e.id AS id, e.fragment AS fragment, e.channel AS channel, e.seq AS seq, e.at AS at
             FROM search_text JOIN search_entries e ON e.id = search_text.rowid
             JOIN memberships m ON m.fragment = e.fragment AND m.role IS NOT NULL
             WHERE search_text MATCH ? ORDER BY e.at DESC, e.id DESC LIMIT ?",
            vec![fts.as_str().into(), SqlStorageValue::Integer(limits::SEARCH_MESSAGES_MAX as i64)],
        )?;
        assert!(hits.len() <= limits::SEARCH_MESSAGES_MAX, "a search answers a bounded page");
        let mut messages = Vec::with_capacity(hits.len());
        for hit in hits {
            if !fragment_proto::valid_fragment_name(&hit.fragment) {
                continue;
            }
            // A bound number reaches SQLite as a REAL, and FTS5 takes a rowid
            // bound only from an INTEGER (any other is no bound at all, every
            // match answering): the CAST keeps it one row.
            let shown = self.rows(
                "SELECT rowid AS id, snippet(search_text, 0, '', '', '…', 24) AS s FROM search_text WHERE search_text MATCH ? AND rowid = CAST(? AS INTEGER)",
                vec![fts.as_str().into(), SqlStorageValue::Integer(hit.id)],
            )?;
            assert!(shown.len() <= 1 && shown.iter().all(|r| r["id"].as_i64() == Some(hit.id)), "a hit's snippet is its own");
            let snippet = shown.first().and_then(|r| r["s"].as_str()).map(|s| search::snippet(s).to_string()).unwrap_or_default();
            messages.push(MessageHit { fragment: hit.fragment, channel: hit.channel, seq: hit.seq, at: hit.at, snippet });
        }
        Ok(SearchAnswer { fragments, messages })
    }
}

/// Tells `identity`'s open pages that something of theirs their list does
/// not hold changed (their computer's notices, docs/computers.md): each
/// reads the list, and their computer, again. A page that missed it reads
/// again as it reconnects.
pub(crate) async fn tell_changed(env: &Env, identity: &str) -> CellResult<()> {
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    let req = Request::new_with_init("https://principal.internal/changed", &init)?;
    let resp = env.durable_object("PRINCIPAL")?.get_by_name(identity)?.fetch_with_request(req).await?;
    if resp.status_code() != 200 {
        return Err(CellError::host(format!("{identity}'s list answered {} to a change", resp.status_code())));
    }
    Ok(())
}

/// A row as a list shows it.
fn listed(r: Listed) -> CellResult<ListedFragment> {
    let sharing = match r.sharing.as_deref().map(serde_json::from_str::<Sharing>) {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => return Err(CellError::host(format!("{}'s stored sharing: {e}", r.name))),
        None => None,
    };
    let face = r.face.as_deref().and_then(|f| serde_json::from_str::<Face>(f).ok());
    let (kind, title, agents) = face.map_or((FragmentKind::App, None, Vec::new()), |f| (f.kind, f.title, f.agents));
    let preview = r.said.as_deref().map(search::preview).filter(|p| !p.is_empty()).map(str::to_string);
    Ok(ListedFragment { name: r.name, role: r.role, kind, title, agents, preview, sharing, archived: r.archived != 0 })
}
