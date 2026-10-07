//! The `Principal` cell: one Durable Object per identity (`id:…`), holding
//! the index of the fragments it belongs to (what `GET /api/fragments`
//! lists). The fragments are the authority; each delivers its changes here
//! from an outbox, versioned so a late delivery never undoes a newer one.
//! A fragment's row in its owner's list also carries its sharing (who may
//! open it, its members and guests), sent with each change to them, so the
//! platform's page reads this one cell and wakes no fragment.
//! Which keys an identity holds is the registry's (registry.rs).
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
//!   it was sent from, and the row's removal (or a new incarnation) drops
//!   them all; a search reads only entries of rows that name a role. At most
//!   `SEARCH_ENTRIES_PER_FRAGMENT_MAX` a fragment and `SEARCH_ENTRIES_MAX` in
//!   all are kept, the oldest going first.
//! - **Watching** (`GET /api/fragments/watch`; Paul on p5, 2026-10-05: an
//!   app his agent made did not show in his sidebar until he reloaded):
//!   the person's open shells, and any CLI, hold a socket here, hibernated
//!   (the router decided whose list it is). Each change this list applies
//!   (a row's, newer than the one it holds: a fragment made, shared with
//!   them, changed, left or deleted; or their archiving) is told to every
//!   one as `{type: "changed"}`, naming nothing: each page reads the list
//!   again with its own credential, so a socket that outlives its session
//!   learns only that something changed. A socket closed or lost misses
//!   nothing a page needs: it reads the list again as it reconnects. At
//!   most `LIST_WATCHERS_MAX` at once; nothing is read from them.

use fragment_core::search::{self, Query};
use fragment_proto::{limits, valid_channel_name, Archived, ErrorCode, FragmentKind, FragmentList, ListedFragment, MessageHit, Role, SearchAnswer, Sharing};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memberships (
  fragment TEXT PRIMARY KEY, role TEXT, incarnation INTEGER NOT NULL, version INTEGER NOT NULL,
  sharing TEXT, face TEXT, archived INTEGER NOT NULL DEFAULT 0);
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

/// The tag of the sockets that watch this list (`/watch`).
const WATCH_TAG: &str = "watch";
/// A watching socket's first frame, as it opens.
const HELLO: &str = r#"{"type":"hello"}"#;
/// What a watching socket is told of a change: no more.
const CHANGED: &str = r#"{"type":"changed"}"#;

/// The columns `/list` and a search read of a row.
const LISTED: &str = "SELECT fragment AS name, role, sharing, face, archived FROM memberships WHERE role IS NOT NULL ORDER BY fragment";

#[durable_object]
pub struct PrincipalCell {
    state: State,
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
}

/// What a person's list shows of a fragment.
#[derive(serde::Serialize, Deserialize)]
struct Face {
    #[serde(default)]
    kind: FragmentKind,
    #[serde(default)]
    title: Option<String>,
}

/// A `memberships` row as `/list` reads it.
#[derive(Deserialize)]
struct Listed {
    name: String,
    role: Role,
    sharing: Option<String>,
    face: Option<String>,
    archived: i64,
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
        PrincipalCell { state }
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
            (m, p) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {p}", m.as_ref()))),
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
    /// their search entries.
    fn index(&self, c: IndexChange) -> CellResult<bool> {
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
        let gone = c.role.is_none() || reborn;
        // a change that carries no face (a fragment from before faces)
        // keeps the one the row has; no change touches the person's archiving
        // but the one that ends what it was about
        self.rows(
            "INSERT INTO memberships (fragment, role, incarnation, version, sharing, face, archived) VALUES (?, ?, ?, ?, ?, ?, 0)
             ON CONFLICT (fragment) DO UPDATE SET role = excluded.role, incarnation = excluded.incarnation, version = excluded.version,
               sharing = excluded.sharing, face = COALESCE(excluded.face, memberships.face),
               archived = CASE WHEN ? THEN 0 ELSE memberships.archived END",
            vec![
                c.fragment.as_str().into(),
                role,
                SqlStorageValue::Integer(c.incarnation),
                SqlStorageValue::Integer(c.version),
                sharing,
                face,
                SqlStorageValue::Integer(i64::from(gone)),
            ],
        )?;
        if gone {
            self.rows("DELETE FROM search_entries WHERE fragment = ?", vec![c.fragment.as_str().into()])?;
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
    /// at the incarnation they come from; each entry once. Then the
    /// fragment's entries past its limit, and everyone's past the total,
    /// go, the oldest first.
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
            "SELECT 1 AS ok FROM memberships WHERE fragment = ? AND role IS NOT NULL AND incarnation = ?",
            vec![b.fragment.as_str().into(), SqlStorageValue::Integer(b.incarnation)],
        )?;
        if fenced.is_empty() {
            return Ok(SearchApplied { member: false, applied: 0 });
        }
        let mut applied = 0u64;
        for e in &b.entries {
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

/// A row as a list shows it.
fn listed(r: Listed) -> CellResult<ListedFragment> {
    let sharing = match r.sharing.as_deref().map(serde_json::from_str::<Sharing>) {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => return Err(CellError::host(format!("{}'s stored sharing: {e}", r.name))),
        None => None,
    };
    let face = r.face.as_deref().and_then(|f| serde_json::from_str::<Face>(f).ok());
    let (kind, title) = face.map_or((FragmentKind::App, None), |f| (f.kind, f.title));
    Ok(ListedFragment { name: r.name, role: r.role, kind, title, sharing, archived: r.archived != 0 })
}
