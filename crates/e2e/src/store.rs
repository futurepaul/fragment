//! The code store a local run's node keeps its git in (docs/self-host.md,
//! seam 5). By default it is the code.storage fake in this process, whose
//! levers the lanes pull. `FRAGMENT_E2E_CODESTORE` puts it outside:
//!
//! - `external`: a store already running, named as the cell names it in
//!   production (`CODESTORAGE_API_URL`, `CODESTORAGE_ORG`, and the org key's
//!   file, `CODESTORAGE_PRIVATE_KEY_FILE`). The fake as a process of its own
//!   (`fake-codestorage`) proves the mode;
//! - `macrofiche`: macrofiche (`MACROFICHE_BIN`), started on the run's
//!   scratch with an org key made for the run, and stopped with it.
//!
//! An external store is reached only through the contract's routes
//! (macrofiche's docs/contract.md, section 5), with tokens the harness signs
//! with the org key, as the cell signs its own. The harness's writes are
//! another writer's: a commit pack on main, and a deploy's ref move (live
//! created at main's tip, else merged to it). No store registers the cell's
//! webhook (the contract's question 2), so after each the owner's `refresh`
//! moves the pins, as the CLI's does after its own writes, and the poll is
//! the backstop.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use fragment_core::codestorage::{self as cs, Claims, FileChange, OrgKey};
use serde_json::{json, Value};

/// Which store a local run uses.
pub const CODESTORE_VAR: &str = "FRAGMENT_E2E_CODESTORE";
/// The subject of the harness's own tokens (the cell's are
/// `fragment-runtime`, an editor's `editor:<id>`).
const SUBJECT: &str = "e2e-harness";
/// The harness's tokens live as long as the cell's own.
const TOKEN_TTL_S: i64 = 300;
/// One call's deadline, as the cell's (contract, section 1).
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Pages of the org's repos a lookup by name reads, 100 a page: the cell's
/// bound (`cell/src/cs.rs`).
const REPO_PAGES_MAX: usize = 100;
const REPO_PAGE: usize = 100;
/// Tries of one write that loses its compare-and-swap to another writer
/// (the cell commits beside the harness: a trigger's write): the cell's.
const CAS_TRIES: usize = 5;
/// The newest commits a count reads: the contract's largest history page.
pub const HISTORY_MAX: usize = 100;
/// Who the harness's commits name.
const AUTHOR: (&str, &str) = ("e2e", "e2e@users.fragment");

/// Where a local run's git lives.
pub enum Choice {
    Fake,
    External,
    Macrofiche,
}

impl Choice {
    /// `FRAGMENT_E2E_CODESTORE`: `fake` (or unset), `external` or `macrofiche`.
    pub fn from_env() -> Result<Choice> {
        match std::env::var(CODESTORE_VAR).as_deref() {
            Err(_) | Ok("fake") => Ok(Choice::Fake),
            Ok("external") => Ok(Choice::External),
            Ok("macrofiche") => Ok(Choice::Macrofiche),
            Ok(other) => bail!("{CODESTORE_VAR} is fake, external or macrofiche, not {other:?}"),
        }
    }
}

/// A store outside the run, through its REST API.
pub struct External {
    pub url: String,
    pub org: String,
    key: OrgKey,
    http: reqwest::blocking::Client,
    /// What the run's banner and skips call it.
    pub label: String,
}

/// A store's answer: its status and its body.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body[..self.body.len().min(300)]).into_owned()
    }
}

/// A repo-scoped path's repo: the url form a create answered, which both
/// shapes (the fake's UUID, the service's name) keep to these bytes, so it
/// needs no escaping in a path.
fn checked_repo(repo: &str) -> Result<&str> {
    if repo.is_empty() || !repo.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.') {
        bail!("not a repo the harness names in a path: {repo:?}");
    }
    Ok(repo)
}

impl External {
    pub fn new(url: &str, org: &str, key_pem: &str, label: String) -> Result<External> {
        let key = OrgKey::from_pem(key_pem).map_err(|e| anyhow::anyhow!("{e}"))?;
        let http = reqwest::blocking::Client::builder().timeout(CALL_TIMEOUT).build()?;
        Ok(External { url: url.trim_end_matches('/').to_string(), org: org.to_string(), key, http, label })
    }

    /// A token for one call: `repo` is the claim (a repo, a new repo's name,
    /// a repo id, or the org), with exactly the scope it needs.
    fn token(&self, repo: &str, scope: &str) -> String {
        let now = crate::api::now_s();
        self.key.token(&Claims { iss: &self.org, sub: SUBJECT, repo, scopes: &[scope], iat: now, exp: now + TOKEN_TTL_S })
    }

    fn call(&self, method: &str, path: &str, query: &[(&str, &str)], claim: &str, scope: &str, body: Option<(&str, Vec<u8>)>) -> Result<Answer> {
        let method = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut req = self.http.request(method.clone(), format!("{}{path}", self.url)).query(query).bearer_auth(self.token(claim, scope));
        if let Some((content_type, bytes)) = body {
            req = req.header("content-type", content_type).body(bytes);
        }
        let r = req.send().with_context(|| format!("{method} {}{path}", self.url))?;
        let status = r.status().as_u16();
        let body = r.bytes().with_context(|| format!("{method} {path}: the body"))?.to_vec();
        Ok(Answer { status, body })
    }

    fn repo_call(&self, method: &str, repo: &str, op: &str, query: &[(&str, &str)], scope: &str, body: Option<(&str, Vec<u8>)>) -> Result<Answer> {
        let repo = checked_repo(repo)?;
        self.call(method, &format!("/api/repos/{repo}/{op}"), query, repo, scope, body)
    }

    /// A branch's head, `None` when it is absent (404, as the cell reads it).
    pub fn head(&self, repo: &str, branch: &str) -> Result<Option<String>> {
        self.head_with(repo, branch, &[])
    }

    /// An ephemeral branch's head: the service keeps those apart, read with
    /// `ephemeral=true` (contract, 5.9).
    pub fn head_ephemeral(&self, repo: &str, branch: &str) -> Result<Option<String>> {
        self.head_with(repo, branch, &[("ephemeral", "true")])
    }

    fn head_with(&self, repo: &str, branch: &str, extra: &[(&str, &str)]) -> Result<Option<String>> {
        let query: Vec<(&str, &str)> = [("name", branch)].into_iter().chain(extra.iter().copied()).collect();
        let a = self.repo_call("GET", repo, "branch", &query, "git:read", None)?;
        match a.status {
            200 => Ok(Some(cs::branch_head(&a.json()).with_context(|| format!("{branch}'s head is not a sha: {}", a.text()))?)),
            404 => Ok(None),
            s => bail!("GET branch {branch} of {repo}: {s} {}", a.text()),
        }
    }

    /// A file's bytes at a ref (a branch or a commit), `None` when absent.
    pub fn file(&self, repo: &str, at: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let a = self.repo_call("GET", repo, "file", &[("path", path), ("ref", at)], "git:read", None)?;
        match a.status {
            200 => Ok(Some(a.body)),
            404 => Ok(None),
            s => bail!("GET file {path} at {at} of {repo}: {s} {}", a.text()),
        }
    }

    /// The newest commits of a ref, first parents, newest first (at most
    /// `HISTORY_MAX`).
    pub fn history(&self, repo: &str, at: &str) -> Result<Vec<String>> {
        let limit = HISTORY_MAX.to_string();
        let a = self.repo_call("GET", repo, "commits", &[("ref", at), ("limit", &limit)], "git:read", None)?;
        if a.status != 200 {
            bail!("GET commits of {at} in {repo}: {} {}", a.status, a.text());
        }
        let commits = a.json()["commits"].as_array().cloned().with_context(|| format!("a history without commits: {}", a.text()))?;
        commits.iter().map(|c| c["sha"].as_str().filter(|s| cs::is_sha(s)).map(str::to_string).with_context(|| format!("a commit without a sha: {c}"))).collect()
    }

    /// A commit of `changes` on `branch`, compare-and-swapped on the head
    /// read just before it, as every writer of fragment's commits: (the
    /// head before, the commit). A try that loses to another writer reads
    /// the head again (`CAS_TRIES`).
    pub fn commit(&self, repo: &str, branch: &str, changes: &[(&str, Option<&[u8]>)], message: &str) -> Result<(Option<String>, String)> {
        let files: Vec<FileChange> = changes
            .iter()
            .map(|(path, bytes)| match bytes {
                Some(bytes) => FileChange::Upsert { path, bytes },
                None => FileChange::Delete { path },
            })
            .collect();
        for _ in 0..CAS_TRIES {
            let before = self.head(repo, branch)?;
            let pack = cs::commit_pack(branch, before.as_deref(), message, AUTHOR, &files);
            let a = self.repo_call("POST", repo, "commit-pack", &[], "git:write", Some(("application/x-ndjson", pack.into_bytes())))?;
            match a.status {
                200 | 201 => return Ok((before, cs::committed(&a.json()).with_context(|| format!("a commit pack's answer names no new head: {}", a.text()))?)),
                409 => continue,
                s => bail!("POST commit-pack on {repo}: {s} {}", a.text()),
            }
        }
        bail!("a commit on {repo}'s {branch} lost its compare-and-swap {CAS_TRIES} times")
    }

    /// Live at main's tip, as a deploy moves it: created there the first
    /// time, else merged (`ff_prefer`, compare-and-swapped on live's head).
    /// Answers (live before, live after).
    pub fn go_live(&self, repo: &str, message: &str) -> Result<(Option<String>, String)> {
        for _ in 0..CAS_TRIES {
            let tip = self.head(repo, "main")?.with_context(|| format!("{repo} has no main to deploy"))?;
            let live = self.head(repo, "live")?;
            let a = match &live {
                Some(l) if *l == tip => return Ok((live, tip)),
                None => {
                    let body = json!({ "base_ref": tip, "target_branch": "live", "target_is_ephemeral": false });
                    self.repo_call("POST", repo, "branches/create", &[], "git:write", Some(("application/json", body.to_string().into_bytes())))?
                }
                Some(l) => {
                    let body = json!({
                        "target_branch": "live", "source_ref": "main", "strategy": "ff_prefer", "expected_target_sha": l,
                        "commit_message": message, "author": { "name": AUTHOR.0, "email": AUTHOR.1 },
                    });
                    self.repo_call("POST", repo, "merge", &[], "git:write", Some(("application/json", body.to_string().into_bytes())))?
                }
            };
            let after = match (a.status, &live) {
                (200 | 201, None) => a.json()["commit_sha"].as_str().map(str::to_string),
                (200 | 201, Some(_)) => a.json()["target"]["new_sha"].as_str().map(str::to_string),
                (409, _) => continue,
                (s, _) => bail!("moving {repo}'s live: {s} {}", a.text()),
            };
            return Ok((live, after.filter(|s| cs::is_sha(s)).with_context(|| format!("a ref move's answer names no new head: {}", a.text()))?));
        }
        bail!("live of {repo} moved under the harness {CAS_TRIES} times")
    }

    /// The url form of the repo named `name`, from the org's list as the
    /// cell finds it after a create answered 409.
    pub fn repo_url(&self, name: &str) -> Result<Option<String>> {
        let limit = REPO_PAGE.to_string();
        let mut cursor: Option<String> = None;
        for _ in 0..REPO_PAGES_MAX {
            let mut query = vec![("limit", limit.as_str())];
            if let Some(c) = &cursor {
                query.push(("cursor", c.as_str()));
            }
            let a = self.call("GET", "/api/repos", &query, &self.org, "org:read", None)?;
            if a.status != 200 {
                bail!("GET /api/repos: {} {}", a.status, a.text());
            }
            let page = a.json();
            if let Some(url) = cs::listed_repo_url(&page, name) {
                return Ok(Some(url));
            }
            cursor = cs::next_repos_cursor(&page);
            if cursor.is_none() {
                return Ok(None);
            }
        }
        bail!("the org lists more than {} repos", REPO_PAGES_MAX * REPO_PAGE)
    }

    /// A new repo named `name` (no branches), as the cell creates one:
    /// answers its url form.
    pub fn create_repo(&self, name: &str) -> Result<String> {
        let body = json!({ "repo_name": name, "default_branch": "main" });
        let a = self.call("POST", "/api/repos", &[], name, "repo:write", Some(("application/json", body.to_string().into_bytes())))?;
        if !matches!(a.status, 200 | 201) {
            bail!("POST /api/repos {name}: {} {}", a.status, a.text());
        }
        let id = cs::created_repo_id(&a.json()).with_context(|| format!("a create names no repo_id: {}", a.text()))?;
        let a = self.call("GET", &format!("/api/repo-urls/{}", checked_repo(&id)?), &[], &id, "org:read", None)?;
        if a.status != 200 {
            bail!("GET /api/repo-urls/{id}: {} {}", a.status, a.text());
        }
        cs::repo_url(&a.json()).with_context(|| format!("a repo url answer names no url: {}", a.text()))
    }
}
