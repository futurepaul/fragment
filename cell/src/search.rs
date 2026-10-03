//! A fragment's half of search (docs/api.md, Search; the person's half is
//! principal.rs): it sends the text of its messages to the lists of its
//! people, the members who are not agents (agents need no search).
//!
//! It is the index's delivery again, for a log instead of a row:
//!
//! - **The log.** A record appended to a channel every member may read
//!   (its read role `viewer` or weaker) whose body is a message with text
//!   (`fragment_core::search::record_text`) is written to `search_log` in
//!   the same turn as the record, numbered `n` (never reused while the
//!   fragment lives). The log keeps its newest
//!   `limits::SEARCH_ENTRIES_PER_FRAGMENT_MAX`, as a person's list does. A
//!   channel only some members may read is never searched (decision 9 is
//!   about chats, and both of a chat's channels are every member's).
//! - **The outbox.** Each person holds a cursor, `search_outbox`: the last
//!   `n` their list took. It follows their index row (`index_change`): made
//!   at 0 when a role is named (a new member gets the log from its start),
//!   gone with the role (the row's removal drops their entries there). A
//!   cursor behind the log is due (`next_at`); one caught up is idle
//!   (`NULL`) until the log grows. The alarm sends each due cursor its next
//!   batch (`flush_search`, after the index's, so a new member's row is
//!   there first); a failure, or a list that holds no role yet, waits with
//!   the outboxes' backoff (`fragment_core::backoff`). A batch sent twice
//!   adds nothing twice: the list keys each entry by `(fragment, n)`.

use fragment_core::search;
use fragment_proto::{limits, Role};
use serde::Deserialize;
use serde_json::Value;
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{FragmentCell, MetaKey};
use crate::js;
use crate::principal::{SearchApplied, SearchBatch, SearchEntry};

/// Due cursors one flush sends a batch to (more wait for the next pass,
/// the alarm armed again at once).
const FLUSH_PEOPLE_MAX: i64 = 32;
/// How long a flush holds the cursors it is sending: another flush skips
/// them meanwhile, and a flush that died lets them go after this.
const CLAIM_MS: i64 = 60_000;

/// A due cursor, as a flush claims it.
#[derive(Deserialize)]
struct Cursor {
    principal: String,
    sent: i64,
    attempts: i64,
}

impl FragmentCell {
    /// Logs a record's text for search, when it is a message's on a channel
    /// every member may read (`read`). Runs in its append's turn.
    pub(crate) fn log_search(&self, read: Role, channel: &str, seq: i64, at: i64, body: &Value) -> CellResult<()> {
        if read > Role::Viewer {
            return Ok(());
        }
        let Some(text) = search::record_text(body) else { return Ok(()) };
        assert!(seq >= 1 && text.len() <= limits::SEARCH_TEXT_MAX_BYTES, "a logged record has a place and bounded text");
        self.exec(
            "INSERT INTO search_log (channel, seq, at, text) VALUES (?, ?, ?, ?)",
            vec![channel.into(), SqlStorageValue::Integer(seq), SqlStorageValue::Integer(at), text.into()],
        )?;
        self.exec(
            "DELETE FROM search_log WHERE n <= (SELECT MAX(n) FROM search_log) - ?",
            vec![SqlStorageValue::Integer(limits::SEARCH_ENTRIES_PER_FRAGMENT_MAX)],
        )?;
        // the idle cursors are behind now; one backing off keeps its wait.
        // Waking one is what needs the alarm: a cursor already due has it
        // armed, since every arming counts every due cursor (`arm`).
        let woke = self.rows("UPDATE search_outbox SET next_at = ? WHERE next_at IS NULL RETURNING principal", vec![SqlStorageValue::Integer(js::now_ms())])?;
        if !woke.is_empty() {
            self.search_woke.set(true);
        }
        Ok(())
    }

    /// A person's cursor follows their index row: made (from the log's
    /// start) when a role is named and they have none, gone with the role.
    /// An agent member gets none. Runs in the index change's turn.
    pub(crate) fn search_follows(&self, principal: &str, role: Option<Role>) -> CellResult<()> {
        match role {
            Some(_) => self.exec(
                "INSERT INTO search_outbox (principal, sent, attempts, next_at)
                 SELECT principal, 0, 0, ? FROM members WHERE principal = ? AND (kind IS NULL OR kind != 'agent')
                 ON CONFLICT (principal) DO NOTHING",
                vec![SqlStorageValue::Integer(js::now_ms()), principal.into()],
            ),
            None => self.exec("DELETE FROM search_outbox WHERE principal = ?", vec![principal.into()]),
        }
    }

    /// When a cursor is next due (for the alarm): the MIN of an index.
    pub(crate) fn search_due_at(&self) -> CellResult<Option<i64>> {
        Ok(self.rows("SELECT MIN(next_at) AS at FROM search_outbox", vec![])?.first().and_then(|r| r["at"].as_i64()))
    }

    /// Sends due cursors their next batch, at most `FLUSH_PEOPLE_MAX` of
    /// them a pass, one batch each. Nothing here fails the alarm: a cursor
    /// whose send fails waits, doubling, and is tried again.
    pub(crate) async fn flush_search(&self) {
        let (Ok(name), Ok(Some(incarnation))) = (self.must(MetaKey::Name), self.meta(MetaKey::CreatedAt)) else { return };
        let Ok(incarnation) = incarnation.parse::<i64>() else { return };
        let now = js::now_ms();
        let claimed: CellResult<Vec<Cursor>> = self.typed(
            "UPDATE search_outbox SET next_at = ? WHERE principal IN
               (SELECT principal FROM search_outbox WHERE next_at <= ? ORDER BY next_at LIMIT ?)
             RETURNING principal, sent, attempts",
            vec![SqlStorageValue::Integer(now + CLAIM_MS), SqlStorageValue::Integer(now), SqlStorageValue::Integer(FLUSH_PEOPLE_MAX)],
        );
        let due = match claimed {
            Ok(due) => due,
            Err(e) => return console_error!("{name}: the search outbox did not read ({:?}): {}", e.code, e.message),
        };
        assert!(due.len() as i64 <= FLUSH_PEOPLE_MAX, "a flush sends a bounded set of cursors");
        for cursor in due {
            if let Err(e) = self.send_search(&name, incarnation, &cursor).await {
                console_error!("{name}: search entries for {} did not go ({:?}): {}", cursor.principal, e.code, e.message);
            }
        }
    }

    /// One cursor's next batch: sent, then the cursor moves (or idles, or
    /// backs off). Each update names the `sent` it read, so a cursor made
    /// again meanwhile (a member who left and came back) is left as made.
    async fn send_search(&self, name: &str, incarnation: i64, cursor: &Cursor) -> CellResult<()> {
        assert!(cursor.sent >= 0 && cursor.attempts >= 0, "a cursor counts up from 0");
        let entries: Vec<SearchEntry> = self.typed(
            "SELECT n, channel, seq, at, text FROM search_log WHERE n > ? ORDER BY n LIMIT ?",
            vec![SqlStorageValue::Integer(cursor.sent), SqlStorageValue::Integer(limits::SEARCH_BATCH_MAX as i64)],
        )?;
        let Some(last) = entries.last().map(|e| e.n) else {
            return self.exec(
                "UPDATE search_outbox SET next_at = NULL, attempts = 0 WHERE principal = ? AND sent = ?",
                vec![cursor.principal.as_str().into(), SqlStorageValue::Integer(cursor.sent)],
            );
        };
        assert!(last > cursor.sent && entries.len() <= limits::SEARCH_BATCH_MAX, "a batch moves its cursor forward, bounded");
        let batch = SearchBatch { fragment: name.to_string(), incarnation, entries };
        match self.deliver_search(&cursor.principal, &batch).await {
            Ok(SearchApplied { member: true, .. }) => {
                let more = !self.rows("SELECT n FROM search_log WHERE n > ? LIMIT 1", vec![SqlStorageValue::Integer(last)])?.is_empty();
                let next_at = if more { SqlStorageValue::Integer(js::now_ms()) } else { SqlStorageValue::Null };
                self.exec(
                    "UPDATE search_outbox SET sent = ?, attempts = 0, next_at = ? WHERE principal = ? AND sent = ?",
                    vec![SqlStorageValue::Integer(last), next_at, cursor.principal.as_str().into(), SqlStorageValue::Integer(cursor.sent)],
                )
            }
            // not taken: the list holds no role here yet (its row is on its
            // way, in the index outbox), or the send failed
            refused => {
                let attempts = cursor.attempts + 1;
                self.exec(
                    "UPDATE search_outbox SET attempts = ?, next_at = ? WHERE principal = ? AND sent = ?",
                    vec![
                        SqlStorageValue::Integer(attempts),
                        SqlStorageValue::Integer(js::now_ms() + fragment_core::backoff::outbox_retry_ms(attempts)),
                        cursor.principal.as_str().into(),
                        SqlStorageValue::Integer(cursor.sent),
                    ],
                )?;
                match refused {
                    Err(e) => Err(e),
                    Ok(_) => Err(CellError::new(fragment_proto::ErrorCode::NotFound, "their list holds no role here yet")),
                }
            }
        }
    }

    async fn deliver_search(&self, principal: &str, batch: &SearchBatch) -> CellResult<SearchApplied> {
        let body = serde_json::to_string(batch).map_err(|e| CellError::host(format!("a search batch: {e}")))?;
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
        let req = Request::new_with_init("https://principal.internal/search/entries", &init)?;
        let mut resp = self.env.durable_object("PRINCIPAL")?.get_by_name(principal)?.fetch_with_request(req).await?;
        if resp.status_code() != 200 {
            let why: Value = resp.json().await.unwrap_or(Value::Null);
            return Err(CellError::host(format!("their list answered {}: {why}", resp.status_code())));
        }
        resp.json::<SearchApplied>().await.map_err(|e| CellError::host(format!("their list's answer: {e}")))
    }
}
