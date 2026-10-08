//! A deleted fragment's end (docs/api.md, `DELETE /api/f/{name}`). The
//! delete ends the life in one synchronous step and answers once one round
//! of its members' lists has been told (`INDEX_FLUSH_MAX`, its owner's
//! first). What it leaves (the rest of those lists, the app facet's
//! database, the blobs) the alarm cleans after, each part idempotent and
//! retried with a backoff, so a delete answers as soon at `MEMBERS_MAX`
//! members as at one. Telling 1000 lists in turn before answering took
//! 300 s on the e2e preview (2026-10-06), and the delete then wiped the
//! outbox that held any it failed to tell.
//!
//! The ended life's rows live in tables of their own (`SCHEMA`, here),
//! which the delete keeps while it drops the life's own and makes them
//! again empty: the name is free at once, and a life made again meanwhile
//! is untouched by the old one's cleanup (each list's change names its
//! incarnation, and a list keeps the newer life's row). Once nothing is
//! left to clean, with no life or claim, the object's storage goes whole
//! (`deleteAll`).
//!
//! A delete keeps the life's code.storage repo (made again, the name finds
//! it). A wipe's end of a life (`end_life_wiped`: docs/api.md, Operators)
//! is the same end with its repo recorded beside it (`ended_repos`), which
//! the alarm deletes with the rest, retried as the rest are: the life is
//! cleaned up only once its repo is gone too.
//!
//! **A try that fails** is told apart (`fragment_core::ended`): a part
//! already gone is done (a repo code.storage says is gone, or was never
//! there); a refusal the same call cannot pass is held at once; anything
//! else is tried again, backing off, until `TRIES_MAX` failed tries hold
//! it. A held part is tried daily, and by each wipe call. Each part's last
//! error is kept (`ended_errors`) and named: the `ended` lever and a
//! wipe's report say what is left of each life and why, so a cleanup that
//! cannot finish says so instead of "still cleaning" (p5, 2026-10-08: a
//! wipe waited on its person's own lists, which refused every change, and
//! nothing said so).
//!
//! **A wipe never waits on the lists it empties.** A wipe's `wipe/end`
//! names the wiped person and their agents; their lists are emptied by the
//! wipe's `lists` step, after this one, and take nothing after (principal.rs,
//! wiped). Their rows here are dropped once tried: a list of theirs that
//! refuses every change (one from before its table had a column master
//! writes) holds no wipe.

use fragment_core::ended::{self as rules, Failure};
use fragment_core::npub;
use fragment_core::wipe::{LifeLeft, LIVES_SHOWN_MAX};
use fragment_proto::{limits, ErrorCode, Identity, IdentityKind};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{missing, Caller, FragmentCell, MetaKey, CLAIM_TTL_MS};
use crate::js;
use crate::members::INDEX_FLUSH_MAX;
use crate::routed::Signed;

/// The ended lives (one row each, until all is cleaned) and the lists each
/// has still to tell.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ended (
  incarnation INTEGER PRIMARY KEY, name TEXT NOT NULL, npub TEXT NOT NULL, facet TEXT NOT NULL, ended_at INTEGER NOT NULL,
  stored INTEGER NOT NULL DEFAULT 1, attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ended_index (
  incarnation INTEGER NOT NULL, principal TEXT NOT NULL, version INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL, PRIMARY KEY (incarnation, principal));
CREATE INDEX IF NOT EXISTS ended_index_due ON ended_index (next_at);
CREATE TABLE IF NOT EXISTS ended_repos (
  incarnation INTEGER PRIMARY KEY, repo TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ended_errors (
  incarnation INTEGER NOT NULL, part TEXT NOT NULL, error TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (incarnation, part));
";

/// The parts of an ended life whose last error `ended_errors` keeps.
const PART_LISTS: &str = "lists";
const PART_STORED: &str = "stored";
const PART_REPO: &str = "repo";

/// Each ended life, oldest first, and what it has left: its lists still to
/// tell, its app's database or blobs (`stored`), its repo (`repos`), the
/// most failed tries of a part left (`tries`), and the last error of a part
/// left (`part: error`). `attempts` is the stored part's own count.
const LIVES_LEFT: &str = "SELECT e.incarnation, e.name, e.stored, e.attempts,
  (SELECT COUNT(*) FROM ended_index i WHERE i.incarnation = e.incarnation) AS lists,
  (SELECT COUNT(*) FROM ended_repos r WHERE r.incarnation = e.incarnation) AS repos,
  MAX(CASE WHEN e.stored = 1 THEN e.attempts ELSE 0 END,
      (SELECT COALESCE(MAX(i.attempts), 0) FROM ended_index i WHERE i.incarnation = e.incarnation),
      (SELECT COALESCE(MAX(r.attempts), 0) FROM ended_repos r WHERE r.incarnation = e.incarnation)) AS tries,
  (SELECT x.part || ': ' || x.error FROM ended_errors x WHERE x.incarnation = e.incarnation AND (
      (x.part = 'stored' AND e.stored = 1)
      OR (x.part = 'lists' AND EXISTS (SELECT 1 FROM ended_index i WHERE i.incarnation = e.incarnation))
      OR (x.part = 'repo' AND EXISTS (SELECT 1 FROM ended_repos r WHERE r.incarnation = e.incarnation)))
    ORDER BY x.at DESC LIMIT 1) AS error
FROM ended e ORDER BY e.incarnation";

/// Ended lives one pass clears the storage of.
const CLEARED_PER_PASS: i64 = 4;
/// Pages of blobs (R2's, up to 1000 keys each) one pass deletes of one life.
const BLOB_PAGES_PER_PASS: usize = 10;
/// Ended lives' repos one pass deletes (one code.storage call each).
const REPOS_PER_PASS: i64 = 4;

/// `wipe/end`'s body: the person a wipe ends this fragment for, and their
/// agents (whose lists the wipe empties, as it empties theirs).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WipeEnd {
    owner: String,
    #[serde(default)]
    agents: Vec<String>,
}

/// `wipe/leave`'s body: a wiped person, or an agent of theirs, who leaves.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WipeLeave {
    principal: String,
    kind: IdentityKind,
}

/// A life a delete ended: whose list it tells first.
pub(crate) struct Ended {
    pub(crate) owner: String,
}

/// A list an ended life has still to tell.
#[derive(Deserialize)]
struct Untold {
    incarnation: i64,
    principal: String,
    version: i64,
    attempts: i64,
    name: String,
}

/// An ended life's repo, still to delete (a wipe's).
#[derive(Deserialize)]
struct EndedRepo {
    incarnation: i64,
    repo: String,
    attempts: i64,
}

/// An ended life whose facet's database or blobs are still to go.
#[derive(Deserialize)]
struct Stored {
    incarnation: i64,
    name: String,
    npub: String,
    facet: String,
    attempts: i64,
}

impl FragmentCell {
    /// Ends this life in one synchronous step, with no await: a crash
    /// leaves the life whole, or ended with all its cleanup recorded, and
    /// a delete asked again after an error records nothing twice.
    /// Records what the alarm cleans after, stops the app, closes the
    /// sockets, and drops the life's tables, made again empty.
    pub(crate) fn end_life(&self) -> CellResult<Ended> {
        let [name, created_at, owner, npub, version] =
            self.metas([MetaKey::Name, MetaKey::CreatedAt, MetaKey::Owner, MetaKey::Npub, MetaKey::IndexVersion])?;
        let name = name.ok_or_else(|| missing(MetaKey::Name))?;
        let incarnation: i64 = created_at.and_then(|c| c.parse().ok()).ok_or_else(|| missing(MetaKey::CreatedAt))?;
        let owner = owner.ok_or_else(|| missing(MetaKey::Owner))?;
        let npub = npub.ok_or_else(|| missing(MetaKey::Npub))?;
        // newer than every change this life sent: each list takes it
        let version = version.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
        let facet = self.app_facet()?;
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO ended (incarnation, name, npub, facet, ended_at, next_at) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (incarnation) DO NOTHING",
            vec![SqlStorageValue::Integer(incarnation), name.as_str().into(), npub.into(), facet.as_str().into(), now.clone(), now.clone()],
        )?;
        // every member's list, and any a change still waits for (a member
        // removed whose list was not told yet)
        self.exec(
            "INSERT INTO ended_index (incarnation, principal, version, next_at)
             SELECT ?, principal, ?, ? FROM (SELECT principal FROM members UNION SELECT principal FROM index_outbox) WHERE true
             ON CONFLICT (incarnation, principal) DO NOTHING",
            vec![SqlStorageValue::Integer(incarnation), SqlStorageValue::Integer(version), now],
        )?;
        // its database goes with the alarm; nothing runs in it from now
        if let Err(e) = js::abort_app_facet(&self.raw, &facet, "the fragment was deleted") {
            console_warn!("{name}: stopping the app facet {facet}: {}", e.message);
        }
        for ws in self.state.get_websockets() {
            let _ = ws.close(Some(4004), Some("the fragment was deleted"));
        }
        for table in fragment_core::ddl::tables(crate::fragment::SCHEMA) {
            self.exec(&format!("DROP TABLE IF EXISTS {table}"), vec![])?;
        }
        self.sql().exec(crate::fragment::SCHEMA, None)?;
        assert!(self.meta(MetaKey::CreatedAt)?.is_none(), "an ended life leaves no fragment");
        Ok(Ended { owner })
    }

    /// A wipe's end of this life (docs/api.md, Operators): `end_life`, and
    /// its repo recorded for the alarm to delete, in the same synchronous
    /// step (no await): a crash leaves the life whole, or ended with its
    /// repo's delete recorded. A delete keeps its repo; a wipe never does.
    pub(crate) fn end_life_wiped(&self) -> CellResult<Ended> {
        let [created_at, repo] = self.metas([MetaKey::CreatedAt, MetaKey::Repo])?;
        let incarnation: i64 = created_at.and_then(|c| c.parse().ok()).ok_or_else(|| missing(MetaKey::CreatedAt))?;
        let repo = repo.ok_or_else(|| missing(MetaKey::Repo))?;
        assert!(!repo.is_empty(), "a created fragment names its repo");
        self.exec(
            "INSERT INTO ended_repos (incarnation, repo, next_at) VALUES (?, ?, ?) ON CONFLICT (incarnation) DO NOTHING",
            vec![SqlStorageValue::Integer(incarnation), repo.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        let ended = self.end_life()?;
        assert_eq!(self.ended_repo_rows(incarnation)?, 1, "the ended life's repo is recorded beside it");
        Ok(ended)
    }

    fn ended_repo_rows(&self, incarnation: i64) -> CellResult<u64> {
        let rows = self.rows("SELECT COUNT(*) AS n FROM ended_repos WHERE incarnation = ?", vec![SqlStorageValue::Integer(incarnation)])?;
        rows.first().and_then(|r| r["n"].as_u64()).ok_or_else(|| CellError::host("COUNT answered no row"))
    }

    /// How many ended lives this object has still to clean up (a wipe asks:
    /// its fragments are cleaned once none is left).
    pub(crate) fn ended_left(&self) -> CellResult<u64> {
        self.count("SELECT COUNT(*) AS n FROM ended")
    }

    /// When an ended life's cleanup is next due, if one has any left.
    pub(crate) fn ended_due_at(&self) -> CellResult<Option<i64>> {
        let rows = self.rows(
            "SELECT MIN(at) AS at FROM (SELECT MIN(next_at) AS at FROM ended_index UNION ALL SELECT MIN(next_at) FROM ended WHERE stored = 1
               UNION ALL SELECT MIN(next_at) FROM ended_repos)",
            vec![],
        )?;
        Ok(rows.first().and_then(|r| r["at"].as_i64()))
    }

    /// The alarm's part: tells a batch of lists, clears what storage is
    /// due, and forgets each life with nothing left. Nothing here fails
    /// the alarm: what fails waits, doubling, and is tried again, until it
    /// is held (`fragment_core::ended`).
    pub(crate) async fn drain_ended(&self) {
        self.tell_ended(None).await;
        self.clear_ended().await;
        self.clear_repos().await;
        self.forget_ended().await;
    }

    /// A part's try failed: it waits (or is held) as `fragment_core::ended`
    /// says, by `update` (its `attempts = ?, next_at = ?` first, then
    /// `key`), and its error is kept as the life's last for `part`.
    fn failed_part(&self, incarnation: i64, part: &str, attempts: i64, failure: &Failure, update: &str, key: Vec<SqlStorageValue>) -> CellResult<rules::Retry> {
        let now = js::now_ms();
        let r = rules::after_failure(attempts, failure, now);
        self.exec(update, [vec![SqlStorageValue::Integer(r.attempts), SqlStorageValue::Integer(r.next_at)], key].concat())?;
        self.exec(
            "INSERT INTO ended_errors (incarnation, part, error, at) VALUES (?, ?, ?, ?)
             ON CONFLICT (incarnation, part) DO UPDATE SET error = excluded.error, at = excluded.at",
            vec![SqlStorageValue::Integer(incarnation), part.into(), rules::kept_error(failure.message()).into(), SqlStorageValue::Integer(now)],
        )?;
        Ok(r)
    }

    /// Tells due lists their ended life is gone, at most `INDEX_FLUSH_MAX`,
    /// all at once, `first`'s list first.
    pub(crate) async fn tell_ended(&self, first: Option<&str>) {
        let now = js::now_ms();
        let due: Vec<Untold> = match self.typed(
            "SELECT i.incarnation, i.principal, i.version, i.attempts, e.name FROM ended_index i JOIN ended e USING (incarnation)
             WHERE i.next_at <= ? ORDER BY i.principal = ? DESC, i.next_at LIMIT ?",
            vec![SqlStorageValue::Integer(now), first.unwrap_or("").into(), SqlStorageValue::Integer(INDEX_FLUSH_MAX)],
        ) {
            Ok(due) => due,
            Err(e) => return console_error!("the ended lives' lists did not read ({:?}): {}", e.code, e.message),
        };
        assert!(due.len() as i64 <= INDEX_FLUSH_MAX, "a flush tells a bounded batch");
        let told = join_all(due.iter().map(|u| {
            let body = json!({ "fragment": u.name, "role": null, "incarnation": u.incarnation, "version": u.version });
            async move { self.tell_index(&u.principal, &body).await }
        }))
        .await;
        let (mut failed, mut first_error) = (0usize, None::<String>);
        for (u, told) in due.iter().zip(told) {
            let key = vec![SqlStorageValue::Integer(u.incarnation), u.principal.as_str().into()];
            let done = match told {
                Ok(()) => self.exec("DELETE FROM ended_index WHERE incarnation = ? AND principal = ?", key),
                Err(failure) => {
                    // the error names whose list it was
                    let failure = match failure {
                        Failure::Transient(m) => Failure::Transient(format!("{}: {m}", u.principal)),
                        Failure::Refused(m) => Failure::Refused(format!("{}: {m}", u.principal)),
                    };
                    failed += 1;
                    first_error.get_or_insert_with(|| failure.message().to_string());
                    let update = "UPDATE ended_index SET attempts = ?, next_at = ? WHERE incarnation = ? AND principal = ?";
                    self.failed_part(u.incarnation, PART_LISTS, u.attempts, &failure, update, key).map(|_| ())
                }
            };
            if let Err(e) = done {
                console_error!("{}: the ended life's row for {} did not write ({:?}): {}", u.name, u.principal, e.code, e.message);
            }
        }
        if let Some(first) = first_error {
            // one line a pass, however many failed: the first says why
            let fragment = due.first().map(|u| u.name.as_str());
            console_error!("{}", json!({ "event": "ended.lists-untold", "fragment": fragment, "asked": due.len(), "failed": failed, "first": first }));
        }
    }

    /// Deletes the app facet's database and the blobs of each ended life
    /// due, at most `BLOB_PAGES_PER_PASS` pages of blobs each: one with
    /// more is due again at once.
    async fn clear_ended(&self) {
        let due: Vec<Stored> = match self.typed(
            "SELECT incarnation, name, npub, facet, attempts FROM ended WHERE stored = 1 AND next_at <= ? ORDER BY next_at LIMIT ?",
            vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(CLEARED_PER_PASS)],
        ) {
            Ok(due) => due,
            Err(e) => return console_error!("the ended lives did not read ({:?}): {}", e.code, e.message),
        };
        for s in due {
            let cleared = async {
                js::delete_app_facet(&self.raw, &s.facet).await?;
                self.delete_blobs_under(&format!("{}/", s.npub), BLOB_PAGES_PER_PASS).await
            }
            .await;
            let inc = SqlStorageValue::Integer(s.incarnation);
            let wrote = match cleared {
                Ok(true) => self.exec("UPDATE ended SET stored = 0 WHERE incarnation = ?", vec![inc]),
                Ok(false) => Ok(()),
                Err(e) => {
                    // the runtime's or R2's: a later try may pass
                    let failure = Failure::Transient(format!("its app's database or blobs ({:?}): {}", e.code, e.message));
                    let update = "UPDATE ended SET attempts = ?, next_at = ? WHERE incarnation = ?";
                    self.failed_part(s.incarnation, PART_STORED, s.attempts, &failure, update, vec![inc]).map(|r| {
                        console_error!("{}: the ended life's facet or blobs did not go (try {}): {}", s.name, r.attempts, failure.message());
                    })
                }
            };
            if let Err(e) = wrote {
                console_error!("{}: the ended life's row did not write ({:?}): {}", s.name, e.code, e.message);
            }
        }
    }

    /// Deletes the repos of the ended lives due (a wipe's), at most
    /// `REPOS_PER_PASS`, and forgets each one code.storage says is gone
    /// (deleted now, before, or never there).
    async fn clear_repos(&self) {
        let due: Vec<EndedRepo> = match self.typed(
            "SELECT incarnation, repo, attempts FROM ended_repos WHERE next_at <= ? ORDER BY next_at LIMIT ?",
            vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(REPOS_PER_PASS)],
        ) {
            Ok(due) => due,
            Err(e) => return console_error!("the ended lives' repos did not read ({:?}): {}", e.code, e.message),
        };
        assert!(due.len() as i64 <= REPOS_PER_PASS, "a pass deletes a bounded batch of repos");
        let cs = match self.cs() {
            Ok(cs) => cs,
            Err(e) => return console_error!("no code.storage to delete the ended lives' repos ({:?}): {}", e.code, e.message),
        };
        for r in due {
            let inc = SqlStorageValue::Integer(r.incarnation);
            let wrote = match cs.delete_repo(&r.repo).await {
                Ok(gone) => {
                    console_log!("{}", json!({ "event": "ended.repo-deleted", "repo": r.repo, "incarnation": r.incarnation, "gone": format!("{gone:?}") }));
                    self.exec("DELETE FROM ended_repos WHERE incarnation = ?", vec![inc])
                }
                Err(failure) => {
                    let update = "UPDATE ended_repos SET attempts = ?, next_at = ? WHERE incarnation = ?";
                    self.failed_part(r.incarnation, PART_REPO, r.attempts, &failure, update, vec![inc]).map(|after| {
                        console_error!("the ended life {}'s repo {} did not go (try {}, held: {}): {}", r.incarnation, r.repo, after.attempts, rules::held(after.attempts), failure.message());
                    })
                }
            };
            if let Err(e) = wrote {
                console_error!("the ended life {}'s repo row did not write ({:?}): {}", r.incarnation, e.code, e.message);
            }
        }
    }

    /// Forgets each ended life with nothing left to clean. The last one
    /// forgotten, with no life made since and no create claiming the name,
    /// lets the object's storage go whole, as a delete's did before it
    /// answered.
    async fn forget_ended(&self) {
        let forgot = self.rows(
            "DELETE FROM ended WHERE stored = 0 AND NOT EXISTS (SELECT 1 FROM ended_index i WHERE i.incarnation = ended.incarnation)
               AND NOT EXISTS (SELECT 1 FROM ended_repos r WHERE r.incarnation = ended.incarnation)
             RETURNING name, incarnation, ended_at",
            vec![],
        );
        let forgot = match forgot {
            Ok(f) => f,
            Err(e) => return console_error!("the ended lives were not forgotten ({:?}): {}", e.code, e.message),
        };
        let now = js::now_ms();
        for f in &forgot {
            let (name, ended_at) = (f["name"].as_str().unwrap_or("?"), f["ended_at"].as_i64().unwrap_or(now));
            console_log!("{name}: its life ended at {} is cleaned up, {} ms after its delete", f["incarnation"], now - ended_at);
        }
        if forgot.is_empty() {
            return;
        }
        // their errors with them
        if let Err(e) = self.exec("DELETE FROM ended_errors WHERE incarnation NOT IN (SELECT incarnation FROM ended)", vec![]) {
            console_error!("the ended lives' errors were not forgotten ({:?}): {}", e.code, e.message);
        }
        let released = async {
            let [created_at, claimed_at] = self.metas([MetaKey::CreatedAt, MetaKey::ClaimedAt])?;
            let claimed = claimed_at.and_then(|c| c.parse::<i64>().ok()).is_some_and(|at| now - at < CLAIM_TTL_MS);
            if created_at.is_some() || claimed || !self.rows("SELECT 1 FROM ended LIMIT 1", vec![])?.is_empty() {
                return Ok(false);
            }
            self.state.storage().delete_all().await?;
            self.sql().exec(crate::fragment::SCHEMA, None)?;
            self.sql().exec(SCHEMA, None)?;
            Ok::<bool, CellError>(true)
        }
        .await;
        if let Err(e) = released {
            console_error!("an ended fragment's storage was not released ({:?}): {}", e.code, e.message);
        }
    }

    /// The test lever `ended` (ops.rs): each ended life, and what is left
    /// of its cleanup (`lists`, `stored`, `repos`, the most failed `tries`
    /// of a part left, whether one is `held`, and its last `error`). It
    /// answers on an object with no life.
    pub(crate) fn ended_view(&self) -> CellResult<Value> {
        let lives = self.rows(&format!("{LIVES_LEFT} LIMIT ?"), vec![SqlStorageValue::Integer(LIVES_SHOWN_MAX as i64)])?;
        let lives: Vec<Value> = lives
            .into_iter()
            .map(|mut l| {
                let tries = l["tries"].as_i64().unwrap_or(0);
                l["held"] = json!(rules::held(tries));
                l
            })
            .collect();
        Ok(json!({ "ended": lives, "dueAt": self.ended_due_at()? }))
    }

    /// What each ended life has left (at most `LIVES_SHOWN_MAX`), as a
    /// wipe's report says it.
    fn ended_lives(&self) -> CellResult<Vec<LifeLeft>> {
        #[derive(Deserialize)]
        struct Row {
            incarnation: i64,
            stored: i64,
            lists: i64,
            repos: i64,
            tries: i64,
            error: Option<String>,
        }
        let rows: Vec<Row> = self.typed(&format!("{LIVES_LEFT} LIMIT ?"), vec![SqlStorageValue::Integer(LIVES_SHOWN_MAX as i64)])?;
        assert!(rows.len() <= LIVES_SHOWN_MAX, "a bounded few lives");
        Ok(rows
            .into_iter()
            .map(|r| LifeLeft {
                incarnation: r.incarnation,
                lists: u64::try_from(r.lists).unwrap_or(0),
                stored: r.stored == 1,
                repo: r.repos > 0,
                tries: u64::try_from(r.tries).unwrap_or(0),
                error: r.error,
            })
            .collect())
    }

    /// A wipe empties the lists of the person it wipes and of their agents
    /// (its `lists` step, after its cleanup): this object's ended lives do
    /// not wait to tell them, once tried. Answers how many rows went.
    fn forget_wiped_lists(&self, owner: &str, agents: &[String]) -> CellResult<usize> {
        let whom: Vec<&str> = std::iter::once(owner).chain(agents.iter().map(String::as_str)).collect();
        let whom = serde_json::to_string(&whom).map_err(|e| CellError::host(format!("the wiped principals: {e}")))?;
        let gone = self.rows("DELETE FROM ended_index WHERE principal IN (SELECT value FROM json_each(?)) RETURNING incarnation", vec![whom.into()])?;
        Ok(gone.len())
    }

    /// A wipe's call is the retry: every part of every ended life here is
    /// due now, held ones too (`fragment_core::ended`), one try a call.
    fn ended_due_now(&self) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec("UPDATE ended_index SET next_at = ?1 WHERE next_at > ?1", vec![now.clone()])?;
        self.exec("UPDATE ended SET next_at = ?1 WHERE stored = 1 AND next_at > ?1", vec![now.clone()])?;
        self.exec("UPDATE ended_repos SET next_at = ?1 WHERE next_at > ?1", vec![now])
    }

    /// A wipe's calls (docs/api.md, Operators; the router's cell/src/wipe.rs),
    /// only from inside the platform (`wipe::WIPE_HEADER`):
    ///
    /// - `end {owner, agents?}`: ends this fragment's life for the wiped
    ///   person who owns it, its repo with it (`end_life_wiped`), and cleans
    ///   up what one pass of its alarm would, every part due at once (a
    ///   wipe's call is the retry); answers `{ended, left, lives}`, `left`
    ///   the ended lives still to clean (0: done), `lives` what each has
    ///   left (`fragment_core::wipe::LifeLeft`). The lists of `owner` and
    ///   `agents` are not waited on: the wipe empties them. Again, with no
    ///   life, it only cleans: a wipe asks until nothing is left. A
    ///   fragment someone else owns is refused (403), and nothing of it
    ///   changes.
    /// - `leave {principal, kind}`: the wiped person, or an agent of theirs,
    ///   leaves this fragment of someone else's, as a member leaves (their
    ///   membership, subscriptions and the browsers' pushes they asked for
    ///   go); `{left}`, false when they were no member (or it is gone).
    pub(crate) async fn wipe_route(&self, route: &str, body: &[u8]) -> CellResult<Value> {
        match route {
            "end" => {
                let b: WipeEnd = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("wipe/end: {e}")))?;
                if !npub::is_identity(&b.owner) || !b.agents.iter().all(|a| npub::is_identity(a)) {
                    return Err(CellError::invalid("wipe/end names the owner and their agents by identity"));
                }
                if b.agents.len() as u64 > limits::AGENTS_PER_OWNER_MAX {
                    return Err(CellError::invalid(format!("wipe/end names at most {} agents", limits::AGENTS_PER_OWNER_MAX)));
                }
                let [created_at, owner, name] = self.metas([MetaKey::CreatedAt, MetaKey::Owner, MetaKey::Name])?;
                let ended = match (created_at, owner) {
                    (None, _) => false,
                    (Some(_), Some(owner)) if owner == b.owner => {
                        let ended = self.end_life_wiped()?;
                        console_log!("{}", json!({ "event": "wipe.fragment-ended", "fragment": name, "owner": owner }));
                        self.tell_ended(Some(&ended.owner)).await;
                        true
                    }
                    (Some(_), Some(_)) => return Err(CellError::new(ErrorCode::Forbidden, format!("{} is someone else's: a wipe ends only its person's", name.unwrap_or_default()))),
                    (Some(_), None) => return Err(missing(MetaKey::Owner)),
                };
                // the lists the wipe empties itself: tried once (just now, or
                // by an earlier call or pass), never waited on
                let forgot = self.forget_wiped_lists(&b.owner, &b.agents)?;
                if !ended {
                    // a wipe's call is the retry: every part due now
                    self.ended_due_now()?;
                }
                // what one pass of the alarm cleans, now; the rest is its
                self.drain_ended().await;
                self.schedule().await?;
                let left = self.ended_left()?;
                let lives = if left > 0 { self.ended_lives()? } else { vec![] };
                assert!(!ended || self.meta(MetaKey::CreatedAt)?.is_none(), "an ended life leaves no fragment");
                assert!(lives.len() as u64 <= left, "each life named is one left");
                Ok(json!({ "ended": ended, "left": left, "lives": lives, "forgot": forgot }))
            }
            "leave" => {
                let b: WipeLeave = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("wipe/leave: {e}")))?;
                if !npub::is_identity(&b.principal) {
                    return Err(CellError::invalid("wipe/leave names who leaves by identity"));
                }
                if self.meta(MetaKey::CreatedAt)?.is_none() {
                    return Ok(json!({ "left": false }));
                }
                // the browsers they asked pushes for here, member or not
                let pushes = self.rows("DELETE FROM push_subs WHERE principal = ? RETURNING id", vec![b.principal.as_str().into()])?.len();
                // as themselves: leaving is any member's own (members.rs)
                let identity = Identity { id: b.principal.clone(), kind: b.kind, owner: None, username: None, held: None };
                let url = url::Url::parse("https://fragment.internal/wipe/leave").expect("a constant URL parses");
                let caller = Caller { signed: Some(Signed::new(identity, None)), unresolved: None, url, site: false };
                let left = match self.remove_member(&caller, "me").await {
                    Ok(_) => true,
                    Err(e) if e.code == ErrorCode::NotFound => false,
                    Err(e) => return Err(e),
                };
                assert!(self.member_role(&b.principal)?.is_none(), "who left is no member");
                Ok(json!({ "left": left, "pushes": pushes }))
            }
            other => Err(CellError::new(ErrorCode::NotFound, format!("no wipe route {other}"))),
        }
    }
}
