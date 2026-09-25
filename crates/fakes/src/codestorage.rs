//! A fake code.storage (https://code.storage/docs): the documented HTTP
//! shapes for everything the cell and the CLI call, with the real service's
//! rules where they bite.
//!
//! - Repo identity is the url form: `POST /api/repos` answers a repo id,
//!   `GET /api/repo-urls/{id}` the url, and every repo-scoped call must use
//!   the url (the human name is a 404). `seed_repo` makes a repo whose url
//!   is its name, for tests that start from a remote world.
//! - JWTs are ES256, verified with the org key when one is configured: the
//!   `repo` claim is required on every call and must name the repo on
//!   repo-scoped ones; scopes are checked per route.
//! - Commit packs are NDJSON (`{"metadata": …}` first), with decoded chunks
//!   capped at 4 MiB, every upsert's stream ending in `eof: true`, and
//!   expected-parent CAS (409 `precondition_failed`).
//! - Each commit has its own tree; merges fast-forward when they can and
//!   otherwise write a merge commit with the source's tree; restore commits
//!   take an ancestor's tree; history walks first parents.
//! - Every branch move delivers a signed push webhook
//!   (`X-Pierre-Signature`) to the URLs registered for that repo.
//!
//! Test levers are methods on [`CodeStorage`]; none is an HTTP route.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest as _, Sha1};

use crate::http::{self, Request, Response, Server};
use fragment_core::codestorage::{Claims, OrgKey};

/// The documented cap on one decoded blob chunk.
pub const CHUNK_MAX: usize = 4 * 1024 * 1024;
const ZERO: &str = "0000000000000000000000000000000000000000";

pub struct Options {
    pub org: String,
    /// The org's private key (PKCS#8 PEM). With it the fake verifies every
    /// JWT, as the real service does; without it any bearer passes.
    pub org_key_pem: Option<String>,
    /// Entries per `files/metadata` page.
    pub page_size: usize,
    /// Keep repos in this file across restarts (`cargo xtask dev`).
    pub state_file: Option<PathBuf>,
    /// 0 picks a free port.
    pub port: u16,
    /// Also answer the fragment host's `storage-token` and `refresh` routes,
    /// for CLI unit tests that have no cell.
    pub host_routes: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { org: "fragment-dev".into(), org_key_pem: None, page_size: 1000, state_file: None, port: 0, host_routes: false }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    blob: String,
    size: u64,
    last_commit: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Commit {
    parents: Vec<String>,
    tree: BTreeMap<String, Entry>,
    message: String,
    author: String,
    at_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct Repo {
    name: String,
    url: String,
    repo_id: String,
    created_at_ms: i64,
    branches: BTreeMap<String, String>,
    ephemeral: BTreeSet<String>,
    commits: BTreeMap<String, Commit>,
    #[serde(with = "b64map")]
    blobs: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Hook {
    url: String,
    repo_url: String,
    secret: String,
}

/// One file change, as the levers take them: `None` deletes.
pub type Change<'a> = (&'a str, Option<&'a [u8]>);
type OwnedChange = (String, Option<Vec<u8>>);

/// A commit to write: `changes` on top of the branch's tip, or, with
/// `from`, on top of that commit's tree with it as a second parent (a
/// merge commit or a restore commit).
struct Write<'a> {
    branch: &'a str,
    message: &'a str,
    author: &'a str,
    changes: &'a [OwnedChange],
    from: Option<&'a str>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    repos: BTreeMap<String, Repo>,
    hooks: Vec<Hook>,
    counter: u64,
    #[serde(skip)]
    sabotage: u32,
    /// Per repo url: commit packs still to land with their answer dropped.
    #[serde(skip)]
    unanswered: BTreeMap<String, u32>,
    #[serde(skip)]
    commit_packs: u32,
    #[serde(skip)]
    refreshes: u32,
    #[serde(skip)]
    race: BTreeMap<String, Vec<OwnedChange>>,
    #[serde(skip)]
    deliveries: Vec<(String, Result<u16, String>)>,
    /// File reads answer 503 while set (an outage).
    #[serde(skip)]
    reads_failing: bool,
    /// Requests per (repo url, `"<method> <route>"`), so a check can count
    /// the round trips a cell makes (`requests`).
    #[serde(skip)]
    requests: BTreeMap<(String, String), u32>,
}

struct Inner {
    org: String,
    signing: Option<SigningKey>,
    page_size: usize,
    state_file: Option<PathBuf>,
    host_routes: bool,
    url: String,
    state: Mutex<State>,
}

/// A push webhook to deliver once the state lock is released (the cell's
/// handler calls back into the fake).
struct Delivery {
    url: String,
    secret: String,
    body: String,
}

pub struct CodeStorage {
    pub url: String,
    inner: Arc<Inner>,
    _server: Server,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_millis() as i64
}

fn blob_sha(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    hex::encode(h.finalize())
}

fn fresh_sha(counter: &mut u64, tag: &str) -> String {
    *counter += 1;
    hex::encode(Sha1::digest(format!("{tag}#{counter}").as_bytes()))
}

/// RFC 3339 UTC, seconds precision.
fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn problem(status: u16, detail: &str) -> Response {
    Response::json(status, &json!({ "type": "about:blank", "status": status, "detail": detail, "error": detail }))
}

fn cas_failed(branch: &str, current: &str) -> Response {
    Response::json(
        409,
        &json!({
            "commit": { "commit_sha": ZERO, "tree_sha": ZERO, "target_branch": branch, "pack_bytes": 0, "blob_count": 0 },
            "result": { "target_branch": branch, "branch": branch, "old_sha": current, "new_sha": current, "success": false,
                        "status": "precondition_failed", "message": "expected branch head did not match current tip" },
        }),
    )
}

impl Repo {
    fn new(name: &str, url: &str, repo_id: &str) -> Repo {
        Repo {
            name: name.into(),
            url: url.into(),
            repo_id: repo_id.into(),
            created_at_ms: now_ms(),
            branches: BTreeMap::new(),
            ephemeral: BTreeSet::new(),
            commits: BTreeMap::new(),
            blobs: BTreeMap::new(),
        }
    }

    /// A branch name or a commit sha → the commit sha.
    fn resolve(&self, r: &str) -> Option<String> {
        if let Some(sha) = self.branches.get(r) {
            return Some(sha.clone());
        }
        self.commits.contains_key(r).then(|| r.to_string())
    }

    /// Is `ancestor` on the first-parent chain from `head` (inclusive)?
    fn first_parent_reaches(&self, ancestor: &str, head: &str) -> bool {
        let mut cur = Some(head.to_string());
        for _ in 0..=self.commits.len() {
            let Some(sha) = cur else { return false };
            if sha == ancestor {
                return true;
            }
            cur = self.commits.get(&sha).and_then(|c| c.parents.first().cloned());
        }
        false
    }

    fn is_ancestor(&self, ancestor: &str, head: &str) -> bool {
        let mut stack = vec![head.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(sha) = stack.pop() {
            if sha == ancestor {
                return true;
            }
            if seen.insert(sha.clone()) {
                if let Some(c) = self.commits.get(&sha) {
                    stack.extend(c.parents.iter().cloned());
                }
            }
        }
        false
    }
}

impl State {
    fn repo_by(&self, name_or_url: &str) -> Option<&Repo> {
        self.repos.get(name_or_url).or_else(|| self.repos.values().find(|r| r.name == name_or_url))
    }

    fn url_of(&self, name_or_url: &str) -> Option<String> {
        self.repo_by(name_or_url).map(|r| r.url.clone())
    }

    /// Writes a commit; returns (old, new) and queues the push webhooks.
    fn commit(&mut self, url: &str, w: Write<'_>, out: &mut Vec<Delivery>) -> (String, String) {
        let Write { branch, message, author, changes, from } = w;
        let (tree_from, extra_parent) = (from, from);
        let tag = format!("{url}|{branch}|{message}");
        let sha = fresh_sha(&mut self.counter, &tag);
        let repo = self.repos.get_mut(url).expect("commit on a known repo");
        let old = repo.branches.get(branch).cloned();
        let base = tree_from.map(str::to_string).or_else(|| old.clone());
        let mut tree = base.and_then(|b| repo.commits.get(&b)).map(|c| c.tree.clone()).unwrap_or_default();
        for (path, bytes) in changes {
            match bytes {
                None => {
                    tree.remove(path);
                }
                Some(b) => {
                    let blob = blob_sha(b);
                    repo.blobs.insert(blob.clone(), b.clone());
                    tree.insert(path.clone(), Entry { blob, size: b.len() as u64, last_commit: sha.clone() });
                }
            }
        }
        let parents: Vec<String> = old.iter().cloned().chain(extra_parent.filter(|p| Some(*p) != old.as_deref()).map(str::to_string)).collect();
        repo.commits.insert(sha.clone(), Commit { parents, tree, message: message.into(), author: author.into(), at_ms: now_ms() });
        repo.branches.insert(branch.into(), sha.clone());
        let old = old.unwrap_or_else(|| ZERO.to_string());
        self.announce(url, branch, &old, &sha, out);
        (old, sha)
    }

    fn move_branch(&mut self, url: &str, branch: &str, to: &str, out: &mut Vec<Delivery>) -> String {
        let repo = self.repos.get_mut(url).expect("move on a known repo");
        let old = repo.branches.insert(branch.into(), to.into()).unwrap_or_else(|| ZERO.to_string());
        self.announce(url, branch, &old, to, out);
        old
    }

    fn announce(&self, url: &str, branch: &str, before: &str, after: &str, out: &mut Vec<Delivery>) {
        if before == after {
            return;
        }
        let repo = &self.repos[url];
        for hook in self.hooks.iter().filter(|h| h.repo_url == url) {
            let body = json!({
                "repository": { "id": repo.repo_id, "url": repo.url },
                "ref": format!("refs/heads/{branch}"),
                "before": before,
                "after": after,
                "customer_id": "fragment-dev",
                "pushed_at": iso(now_ms()),
            });
            out.push(Delivery { url: hook.url.clone(), secret: hook.secret.clone(), body: body.to_string() });
        }
    }
}

struct Jwt {
    repo: String,
    scopes: Vec<String>,
}

impl Inner {
    fn persist(&self, st: &State) {
        if let Some(path) = &self.state_file {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, serde_json::to_vec(st).expect("fake state serializes")).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    /// Checks the bearer token for `scope` (and the repo, on repo-scoped calls).
    /// Checks the bearer token; answers its repo claim ("" when the fake
    /// verifies no tokens).
    fn authorize(&self, req: &Request, scope: &str, repo_url: Option<&str>) -> Result<String, Response> {
        let Some(key) = &self.signing else { return Ok(String::new()) };
        let token = req.header("authorization").and_then(|h| h.strip_prefix("Bearer ")).ok_or_else(|| problem(401, "missing bearer token"))?;
        let jwt = verify_jwt(token, &VerifyingKey::from(key), &self.org).map_err(|e| problem(401, &e))?;
        if jwt.repo.is_empty() {
            return Err(problem(403, "the repo claim is required"));
        }
        if !jwt.scopes.iter().any(|s| s == scope) {
            return Err(problem(403, &format!("missing scope {scope}")));
        }
        if let Some(url) = repo_url {
            if jwt.repo != url {
                return Err(problem(403, "the token's repo claim names another repo"));
            }
        }
        Ok(jwt.repo)
    }

    /// Webhooks for a push over HTTP go out after the answer, as the real
    /// service queues them: a caller may see its push announced while it is
    /// still reading the answer, or after.
    fn handle(self: &Arc<Self>, req: &Request) -> Response {
        let mut deliveries = Vec::new();
        let resp = {
            let mut st = self.state.lock().expect("fake state lock");
            let resp = self.route(&mut st, req, &mut deliveries);
            if req.method != "GET" && req.method != "HEAD" {
                self.persist(&st);
            }
            resp
        };
        if !deliveries.is_empty() {
            let me = Arc::clone(self);
            std::thread::spawn(move || {
                for d in deliveries {
                    deliver(&d, &me.state);
                }
            });
        }
        resp
    }

    fn route(&self, st: &mut State, req: &Request, out: &mut Vec<Delivery>) -> Response {
        let path = req.path.as_str();
        let m = req.method.as_str();
        if path == "/healthz" {
            return Response::json(200, &json!({ "ok": true }));
        }
        if self.host_routes {
            if let Some(name) = path.strip_prefix("/api/f/").and_then(|p| p.strip_suffix("/storage-token")) {
                // an unknown name is a fresh repo with no branches yet
                let url = match st.url_of(name) {
                    Some(url) => url,
                    None => {
                        st.repos.insert(name.into(), Repo::new(name, name, &format!("repo_{name}")));
                        name.to_string()
                    }
                };
                let token = self.mint(&url, &["git:read", "git:write"], 900);
                // the host's whole answer (fragment_proto::StorageToken)
                return Response::json(200, &json!({ "token": token, "repo": url, "api": self.url, "expiresAt": now_ms() + 900_000 }));
            }
            if path.starts_with("/api/f/") && path.ends_with("/refresh") && m == "POST" {
                st.refreshes += 1;
                return Response::json(200, &json!({ "ok": true }));
            }
        }
        if path == "/api/repos" && m == "POST" {
            // the service names a new repo by the token's repo claim; the
            // body's optional repo_name must equal it
            let claim = match self.authorize(req, "repo:write", None) {
                Ok(c) => c,
                Err(r) => return r,
            };
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let named = body["repo_name"].as_str().filter(|s| !s.is_empty());
            if let Some(n) = named {
                if !claim.is_empty() && n != claim {
                    return problem(400, "repo_name must equal the JWT repo claim");
                }
            }
            let name = if claim.is_empty() { named.unwrap_or("").to_string() } else { claim };
            if name.is_empty() {
                return problem(400, "the repository name is required");
            }
            let name = name.as_str();
            if st.repo_by(name).is_some() {
                return problem(409, "repository already exists");
            }
            let repo_id = format!("repo_{}", &fresh_sha(&mut st.counter, name)[..20]);
            let url = uuid_like(&fresh_sha(&mut st.counter, &repo_id));
            st.repos.insert(url.clone(), Repo::new(name, &url, &repo_id));
            return Response::json(
                201,
                &json!({ "repo_id": repo_id, "repo_name": name, "http_url": format!("{}/{url}", self.url), "message": "repository created" }),
            );
        }
        if path == "/api/repos" && m == "GET" {
            if let Err(r) = self.authorize(req, "org:read", None) {
                return r;
            }
            // the service's paging: newest first, 20 a page by default, at
            // most 100, an opaque cursor; `q` matches the url form
            let limit = req.query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20usize).clamp(1, 100);
            let start: usize = match req.query.get("cursor") {
                None => 0,
                Some(c) => match c.strip_prefix("page-").and_then(|n| n.parse().ok()) {
                    Some(n) => n,
                    None => return problem(400, "invalid cursor"),
                },
            };
            let q = req.query.get("q").map(|q| q.trim().to_ascii_lowercase()).unwrap_or_default();
            let mut all: Vec<&Repo> = st.repos.values().filter(|r| q.is_empty() || r.url.to_ascii_lowercase().contains(&q)).collect();
            all.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms).then(b.repo_id.cmp(&a.repo_id)));
            let page: Vec<Value> = all
                .iter()
                .skip(start)
                .take(limit)
                .map(|r| json!({ "repo_id": r.repo_id, "repo_name": r.name, "url": r.url, "default_branch": "main", "created_at": iso(r.created_at_ms) }))
                .collect();
            let more = start + limit < all.len();
            let mut body = json!({ "repos": page, "has_more": more });
            if more {
                body["next_cursor"] = json!(format!("page-{}", start + limit));
            }
            return Response::json(200, &body);
        }
        if let Some(id) = path.strip_prefix("/api/repo-urls/") {
            if let Err(r) = self.authorize(req, "org:read", None) {
                return r;
            }
            return match st.repos.values().find(|r| r.repo_id == id) {
                Some(r) => Response::json(200, &json!({ "repo_id": r.repo_id, "repo_name": r.name, "url": r.url })),
                None => problem(404, "repository not found"),
            };
        }
        let Some((url, op)) = path.strip_prefix("/api/repos/").and_then(|p| p.split_once('/')) else {
            return problem(404, &format!("no route {m} {path}"));
        };
        if !st.repos.contains_key(url) {
            return problem(404, "repository not found");
        }
        *st.requests.entry((url.to_string(), format!("{m} {op}"))).or_default() += 1;
        let scope = if m == "POST" { "git:write" } else { "git:read" };
        if let Err(r) = self.authorize(req, scope, Some(url)) {
            return r;
        }
        let url = url.to_string();
        match (m, op) {
            ("GET", "branch") => {
                let name = req.query.get("name").map(String::as_str).unwrap_or("main");
                match st.repos[&url].branches.get(name) {
                    Some(sha) => Response::json(200, &json!({ "branch": { "name": name, "head_sha": sha, "created_at": iso(0) } })),
                    None => problem(404, "branch not found"),
                }
            }
            ("GET", "files/metadata") => self.tree_page(&st.repos[&url], req),
            ("GET" | "HEAD", "file") if st.reads_failing => problem(503, "file reads are unavailable (the fake's outage lever)"),
            ("GET" | "HEAD", "file") => file(&st.repos[&url], req),
            ("GET", "commits") => commits(&st.repos[&url], req),
            ("POST", "commit-pack") => self.commit_pack(st, &url, req, out),
            ("POST", "branches/create") => branch_create(st, &url, req, out),
            ("POST", "merge") => merge(st, &url, req, out),
            ("POST", "restore-commit") => restore(st, &url, req, out),
            _ => problem(404, &format!("no route {m} {op}")),
        }
    }

    fn tree_page(&self, repo: &Repo, req: &Request) -> Response {
        let r = req.query.get("ref").map(String::as_str).unwrap_or("main");
        let Some(sha) = repo.resolve(r) else { return problem(404, "ref not found") };
        let tree = &repo.commits[&sha].tree;
        let limit = req.query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(1000usize).clamp(1, 1000).min(self.page_size.max(1));
        let cursor = req.query.get("cursor").cloned().unwrap_or_default();
        let rows: Vec<(&String, &Entry)> = tree.range::<String, _>(cursor.clone()..).take(limit + 1).collect();
        let has_more = rows.len() > limit;
        let files: Vec<Value> = rows
            .iter()
            .take(limit)
            .map(|(p, e)| json!({ "path": p, "mode": "100644", "type": "blob", "size": e.size, "last_commit_sha": e.last_commit }))
            .collect();
        let next = if has_more { rows[limit].0.clone() } else { String::new() };
        Response::json(200, &json!({ "files": files, "commits": {}, "ref": r, "has_more": has_more, "next_cursor": next }))
    }

    fn commit_pack(&self, st: &mut State, url: &str, req: &Request, out: &mut Vec<Delivery>) -> Response {
        st.commit_packs += 1;
        if let Some(changes) = st.race.remove(url) {
            st.commit(url, Write { branch: "main", message: "racing writer", author: "racer", changes: &changes, from: None }, out);
        }
        if st.sabotage > 0 {
            st.sabotage -= 1;
            let n = st.counter;
            let competitor = [("competitor.txt".to_string(), Some(format!("competitor {n}").into_bytes()))];
            let (_, tip) = st.commit(url, Write { branch: "main", message: "competitor", author: "competitor", changes: &competitor, from: None }, out);
            return cas_failed("main", &tip);
        }
        let text = String::from_utf8_lossy(&req.body);
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let Some(Ok(first)) = lines.next().map(serde_json::from_str::<Value>) else { return problem(400, "empty pack or a bad metadata line") };
        let meta = &first["metadata"];
        if !meta.is_object() {
            return Response::json(400, &json!({ "error": "first payload must be metadata", "result": { "status": "invalid", "message": "first payload must be metadata" } }));
        }
        let Some(branch) = meta["target_branch"].as_str() else { return problem(400, "metadata needs target_branch") };
        let mut chunks: BTreeMap<String, (Vec<u8>, bool)> = BTreeMap::new();
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else { return problem(400, "a line is not JSON") };
            let Some(id) = v["blob_chunk"]["content_id"].as_str() else { return problem(400, "a line is not a blob_chunk") };
            let data = base64::engine::general_purpose::STANDARD.decode(v["blob_chunk"]["data"].as_str().unwrap_or("")).unwrap_or_default();
            if data.len() > CHUNK_MAX {
                return Response::json(
                    413,
                    &json!({ "result": { "status": "payload_too_large", "success": false, "message": format!("blob chunk size {} exceeds maximum {CHUNK_MAX}", data.len()) } }),
                );
            }
            let entry = chunks.entry(id.to_string()).or_default();
            entry.0.extend_from_slice(&data);
            entry.1 = v["blob_chunk"]["eof"].as_bool().unwrap_or(false);
        }
        let mut changes = Vec::new();
        for f in meta["files"].as_array().cloned().unwrap_or_default() {
            let (Some(path), Some(op)) = (f["path"].as_str(), f["operation"].as_str()) else { return problem(400, "a file needs path and operation") };
            if op == "delete" {
                changes.push((path.to_string(), None));
                continue;
            }
            match chunks.get(f["content_id"].as_str().unwrap_or("")) {
                Some((bytes, true)) => changes.push((path.to_string(), Some(bytes.clone()))),
                _ => return problem(400, &format!("incomplete content stream for {path}")),
            }
        }
        let current = st.repos[url].branches.get(branch).cloned().unwrap_or_else(|| ZERO.to_string());
        let expected = meta["expected_target_sha"].as_str().or_else(|| meta["expected_head_sha"].as_str());
        if let Some(exp) = expected {
            let exp = if exp.is_empty() { ZERO } else { exp };
            if exp != current {
                return cas_failed(branch, &current);
            }
        }
        let author = meta["author"]["name"].as_str().unwrap_or("unknown");
        let message = meta["commit_message"].as_str().unwrap_or("");
        let (old, new) = st.commit(url, Write { branch, message, author, changes: &changes, from: None }, out);
        if let Some(left @ 1..) = st.unanswered.get_mut(url) {
            *left -= 1;
            return Response::unanswered();
        }
        Response::json(
            201,
            &json!({
                "commit": { "commit_sha": new, "tree_sha": new, "target_branch": branch, "pack_bytes": req.body.len(), "blob_count": changes.len() },
                "result": { "target_branch": branch, "branch": branch, "old_sha": old, "new_sha": new, "success": true, "status": "ok" },
            }),
        )
    }

    fn mint(&self, repo: &str, scopes: &[&str], ttl_s: i64) -> String {
        match &self.signing {
            None => "fake-token".to_string(),
            Some(k) => {
                let now = now_ms() / 1000;
                let key = OrgKey::from_pem(&k.to_pkcs8_pem_string()).expect("the fake's own key re-encodes");
                key.token(&Claims { iss: &self.org, sub: "fake", repo, scopes, iat: now, exp: now + ttl_s })
            }
        }
    }
}

trait PemString {
    fn to_pkcs8_pem_string(&self) -> String;
}

impl PemString for SigningKey {
    fn to_pkcs8_pem_string(&self) -> String {
        use p256::pkcs8::EncodePrivateKey;
        self.to_pkcs8_pem(Default::default()).expect("a P-256 key encodes").to_string()
    }
}

fn file(repo: &Repo, req: &Request) -> Response {
    let r = req.query.get("ref").map(String::as_str).unwrap_or("main");
    let path = req.query.get("path").map(String::as_str).unwrap_or("");
    let Some(entry) = repo.resolve(r).and_then(|sha| repo.commits[&sha].tree.get(path).cloned()) else {
        return problem(404, "file not found");
    };
    let bytes = repo.blobs.get(&entry.blob).cloned().unwrap_or_default();
    let len = bytes.len().to_string();
    Response::bytes(200, "application/octet-stream", bytes)
        .with_header("content-length", &len)
        .with_header("etag", &format!("\"{}\"", entry.blob))
        .with_header("x-blob-sha", &entry.blob)
        .with_header("x-last-commit-sha", &entry.last_commit)
}

fn commits(repo: &Repo, req: &Request) -> Response {
    let r = req.query.get("ref").map(String::as_str).unwrap_or("main");
    let limit = req.query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(30usize).clamp(1, 100);
    let Some(mut cur) = repo.resolve(r) else { return problem(404, "ref not found") };
    let mut list = Vec::new();
    while list.len() < limit {
        let Some(c) = repo.commits.get(&cur) else { break };
        list.push(json!({ "sha": cur, "message": c.message, "author_name": c.author, "author_email": format!("{}@fake", c.author), "date": iso(c.at_ms), "parent_shas": c.parents }));
        match c.parents.first() {
            Some(p) => cur = p.clone(),
            None => break,
        }
    }
    let has_more = list.len() == limit;
    Response::json(200, &json!({ "commits": list, "has_more": has_more }))
}

fn branch_create(st: &mut State, url: &str, req: &Request, out: &mut Vec<Delivery>) -> Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let Some(target) = body["target_branch"].as_str().filter(|s| !s.is_empty()) else { return problem(400, "target_branch is required") };
    let Some(base) = st.repos[url].resolve(body["base_ref"].as_str().unwrap_or("")) else { return problem(404, "base_ref not found") };
    if target == "main" {
        return problem(409, "cannot replace the default branch");
    }
    if st.repos[url].branches.contains_key(target) {
        return problem(409, &format!("branch already exists: {target}"));
    }
    let ephemeral = body["target_is_ephemeral"].as_bool().unwrap_or(false);
    if ephemeral {
        st.repos.get_mut(url).expect("known repo").ephemeral.insert(target.to_string());
    }
    st.move_branch(url, target, &base, out);
    Response::json(
        201,
        &json!({ "message": "branch created", "target_branch": target, "target_is_ephemeral": ephemeral, "commit_sha": base }),
    )
}

fn merge(st: &mut State, url: &str, req: &Request, out: &mut Vec<Delivery>) -> Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let target = body["target_branch"].as_str().unwrap_or("");
    let Some(source) = st.repos[url].resolve(body["source_ref"].as_str().unwrap_or("")) else { return problem(404, "source_ref not found") };
    let Some(old) = st.repos[url].branches.get(target).cloned() else { return problem(404, &format!("branch not found: {target}")) };
    if let Some(exp) = body["expected_target_sha"].as_str() {
        if exp != old {
            return cas_failed(target, &old);
        }
    }
    let (new, strategy) = if source == old || st.repos[url].is_ancestor(&old, &source) {
        st.move_branch(url, target, &source, out);
        (source, "ff")
    } else {
        let msg = body["commit_message"].as_str().unwrap_or("merge").to_string();
        let author = body["author"]["name"].as_str().unwrap_or("unknown").to_string();
        let (_, new) = st.commit(url, Write { branch: target, message: &msg, author: &author, changes: &[], from: Some(&source) }, out);
        (new, "merge_commit")
    };
    Response::json(
        200,
        &json!({ "target": { "branch": target, "old_sha": old, "new_sha": new, "strategy": strategy }, "result": { "success": true, "status": "ok" } }),
    )
}

fn restore(st: &mut State, url: &str, req: &Request, out: &mut Vec<Delivery>) -> Response {
    let text = String::from_utf8_lossy(&req.body);
    let Some(Ok(first)) = text.lines().find(|l| !l.trim().is_empty()).map(serde_json::from_str::<Value>) else { return problem(400, "a bad metadata line") };
    let meta = if first["metadata"].is_object() { &first["metadata"] } else { &first };
    let target = meta["target_branch"].as_str().unwrap_or("");
    let Some(old) = st.repos[url].branches.get(target).cloned() else { return problem(404, &format!("branch not found: {target}")) };
    // an unknown base is not an ancestor either: 412, as the spec words it
    let base = st.repos[url].resolve(meta["base_ref"].as_str().unwrap_or("")).unwrap_or_default();
    if !st.repos[url].first_parent_reaches(&base, &old) {
        return problem(412, "base_ref is not an ancestor of the target's tip");
    }
    if let Some(exp) = meta["expected_target_sha"].as_str() {
        if exp != old {
            return cas_failed(target, &old);
        }
    }
    let msg = meta["commit_message"].as_str().unwrap_or("restore").to_string();
    let author = meta["author"]["name"].as_str().unwrap_or("unknown").to_string();
    let (_, new) = st.commit(url, Write { branch: target, message: &msg, author: &author, changes: &[], from: Some(&base) }, out);
    Response::json(
        201,
        &json!({
            "commit": { "commit_sha": new, "tree_sha": new, "target_branch": target, "pack_bytes": 0, "blob_count": 0 },
            "result": { "target_branch": target, "branch": target, "old_sha": old, "new_sha": new, "success": true, "status": "ok" },
        }),
    )
}

fn deliver(d: &Delivery, state: &Mutex<State>) {
    let mut result = Err(String::new());
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let t = now_ms() / 1000;
        let sig = fragment_core::webhook::sign(d.body.as_bytes(), &d.secret, t);
        result = http::post(&d.url, &[("content-type", "application/json"), ("x-pierre-event", "push"), ("x-pierre-signature", &sig)], d.body.as_bytes());
        if matches!(result, Ok(200..=299)) {
            break;
        }
    }
    state.lock().expect("fake state lock").deliveries.push((d.url.clone(), result));
}

fn uuid_like(hex40: &str) -> String {
    format!("{}-{}-{}-{}-{}", &hex40[0..8], &hex40[8..12], &hex40[12..16], &hex40[16..20], &hex40[20..32])
}

fn verify_jwt(token: &str, key: &VerifyingKey, org: &str) -> Result<Jwt, String> {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let parts: Vec<&str> = token.split('.').collect();
    let [h, p, s] = parts.as_slice() else { return Err("not a JWT".into()) };
    let header: Value = serde_json::from_slice(&b64.decode(h).map_err(|_| "bad JWT header")?).map_err(|_| "bad JWT header")?;
    if header["alg"] != "ES256" {
        return Err("the JWT is not ES256".into());
    }
    let sig = Signature::from_slice(&b64.decode(s).map_err(|_| "bad JWT signature")?).map_err(|_| "bad JWT signature")?;
    key.verify(format!("{h}.{p}").as_bytes(), &sig).map_err(|_| "the JWT signature does not verify")?;
    let claims: Value = serde_json::from_slice(&b64.decode(p).map_err(|_| "bad JWT claims")?).map_err(|_| "bad JWT claims")?;
    if claims["iss"] != org {
        return Err("the JWT is for another org".into());
    }
    if claims["exp"].as_i64().unwrap_or(0) <= now_ms() / 1000 {
        return Err("Invalid or expired token".into());
    }
    Ok(Jwt {
        repo: claims["repo"].as_str().unwrap_or("").to_string(),
        scopes: claims["scopes"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default(),
    })
}

impl CodeStorage {
    pub fn start(opts: Options) -> anyhow::Result<CodeStorage> {
        let signing = match &opts.org_key_pem {
            Some(pem) => Some(SigningKey::from_pkcs8_pem(pem.replace("\\n", "\n").trim())?),
            None => None,
        };
        let state = match &opts.state_file {
            Some(p) if p.is_file() => serde_json::from_slice(&std::fs::read(p)?)?,
            _ => State::default(),
        };
        // The url is known only once the server has a port; the handler
        // reads it through `Inner`, so build Inner first with the port fixed.
        let listener_port = if opts.port == 0 { free_port()? } else { opts.port };
        let url = format!("http://127.0.0.1:{listener_port}");
        let inner = Arc::new(Inner {
            org: opts.org,
            signing,
            page_size: opts.page_size,
            state_file: opts.state_file,
            host_routes: opts.host_routes,
            url: url.clone(),
            state: Mutex::new(state),
        });
        let handler_inner = Arc::clone(&inner);
        let server = Server::start(listener_port, Arc::new(move |req: &Request| handler_inner.handle(req)))?;
        Ok(CodeStorage { url, inner, _server: server })
    }

    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.inner.state.lock().expect("fake state lock"))
    }

    /// A repo whose url is its name, with `files` on `main` in one commit.
    /// Seeding an existing name resets it (same identity, a new world).
    pub fn seed_repo(&self, name: &str, files: &[(&str, &[u8])]) {
        let mut out = Vec::new();
        self.with(|st| {
            st.repos.insert(name.into(), Repo::new(name, name, &format!("repo_{name}")));
            let changes: Vec<OwnedChange> = files.iter().map(|(p, b)| (p.to_string(), Some(b.to_vec()))).collect();
            st.commit(name, Write { branch: "main", message: "seed", author: "seed", changes: &changes, from: None }, &mut out);
        });
    }

    /// `n` empty repos made after every existing one (a busy org, so a
    /// lookup by name must page).
    pub fn seed_filler(&self, n: usize) {
        self.with(|st| {
            let base = st.repos.values().map(|r| r.created_at_ms).max().unwrap_or(0).max(now_ms());
            for i in 0..n {
                let id = fresh_sha(&mut st.counter, "filler");
                let name = format!("filler-{}", &id[..12]);
                let url = uuid_like(&id);
                let mut repo = Repo::new(&name, &url, &format!("repo_{}", &id[..20]));
                repo.created_at_ms = base + 1 + i as i64;
                st.repos.insert(url, repo);
            }
        });
    }

    /// The url-form identity of a repo named `name`.
    pub fn repo_url(&self, name: &str) -> Option<String> {
        self.with(|st| st.url_of(name))
    }

    pub fn branch(&self, repo: &str, branch: &str) -> Option<String> {
        self.with(|st| st.repo_by(repo)?.branches.get(branch).cloned())
    }

    pub fn branches(&self, repo: &str) -> BTreeMap<String, String> {
        self.with(|st| st.repo_by(repo).map(|r| r.branches.clone()).unwrap_or_default())
    }

    pub fn is_ephemeral(&self, repo: &str, branch: &str) -> bool {
        self.with(|st| st.repo_by(repo).is_some_and(|r| r.ephemeral.contains(branch)))
    }

    pub fn file_at(&self, repo: &str, branch: &str, path: &str) -> Option<Vec<u8>> {
        self.with(|st| {
            let r = st.repo_by(repo)?;
            let entry = r.commits.get(r.branches.get(branch)?)?.tree.get(path)?;
            r.blobs.get(&entry.blob).cloned()
        })
    }

    pub fn paths(&self, repo: &str, branch: &str) -> Vec<String> {
        self.with(|st| {
            st.repo_by(repo)
                .and_then(|r| r.commits.get(r.branches.get(branch)?))
                .map(|c| c.tree.keys().cloned().collect())
                .unwrap_or_default()
        })
    }

    /// A commit from some other writer (no CAS), announced like any other.
    pub fn external_commit(&self, repo: &str, branch: &str, changes: &[Change<'_>], message: &str) -> String {
        let mut out = Vec::new();
        let sha = self.with(|st| {
            let url = st.url_of(repo).expect("external_commit on a known repo");
            let changes: Vec<OwnedChange> = changes.iter().map(|(p, b)| (p.to_string(), b.map(<[u8]>::to_vec))).collect();
            st.commit(&url, Write { branch, message, author: "external", changes: &changes, from: None }, &mut out).1
        });
        for d in &out {
            deliver(d, &self.inner.state);
        }
        sha
    }

    /// Points a branch at a commit (a deploy's ref move), announced.
    pub fn set_branch(&self, repo: &str, branch: &str, sha: &str) {
        let mut out = Vec::new();
        self.with(|st| {
            let url = st.url_of(repo).expect("set_branch on a known repo");
            assert!(st.repos[&url].commits.contains_key(sha), "set_branch to a known commit");
            st.move_branch(&url, branch, sha, &mut out);
            self.inner.persist(st);
        });
        for d in &out {
            deliver(d, &self.inner.state);
        }
    }

    /// Moves a branch without announcing it (a lost webhook).
    pub fn silent_commit(&self, repo: &str, branch: &str, changes: &[Change<'_>], message: &str) -> String {
        self.with(|st| {
            let url = st.url_of(repo).expect("silent_commit on a known repo");
            let changes: Vec<OwnedChange> = changes.iter().map(|(p, b)| (p.to_string(), b.map(<[u8]>::to_vec))).collect();
            st.commit(&url, Write { branch, message, author: "external", changes: &changes, from: None }, &mut Vec::new()).1
        })
    }

    /// Delivers push webhooks for `repo` to `url`, signed with `secret` (the
    /// dashboard registration the real service has).
    pub fn register_webhook(&self, repo: &str, url: &str, secret: &str) {
        self.with(|st| {
            let repo_url = st.url_of(repo).expect("register_webhook on a known repo");
            st.hooks.retain(|h| !(h.repo_url == repo_url && h.url == url));
            st.hooks.push(Hook { url: url.into(), repo_url, secret: secret.into() });
            self.inner.persist(st);
        });
    }

    /// Webhook deliveries so far: (url, status or error).
    pub fn deliveries(&self) -> Vec<(String, Result<u16, String>)> {
        self.with(|st| st.deliveries.clone())
    }

    /// The next commit pack for `repo` first lands these changes on main as
    /// another writer (a writer between the client's head read and its commit).
    pub fn arm_race(&self, repo: &str, changes: &[Change<'_>]) {
        self.with(|st| {
            let url = st.url_of(repo).expect("arm_race on a known repo");
            st.race.insert(url, changes.iter().map(|(p, b)| (p.to_string(), b.map(<[u8]>::to_vec))).collect());
        });
    }

    /// File reads answer 503 until this is cleared.
    pub fn fail_file_reads(&self, failing: bool) {
        self.with(|st| st.reads_failing = failing);
    }

    /// The next `n` commit packs each lose to a competitor commit (409).
    pub fn sabotage_commit_packs(&self, n: u32) {
        self.with(|st| st.sabotage = n);
    }

    /// The next `n` commit packs that land on `repo` lose their answer: the
    /// commit is applied, then the connection closes without a response (a
    /// timeout, or a link that dropped on the way back).
    pub fn drop_commit_answers(&self, repo: &str, n: u32) {
        self.with(|st| {
            let url = st.url_of(repo).expect("drop_commit_answers on a known repo");
            st.unanswered.insert(url, n);
        });
    }

    pub fn commit_pack_count(&self) -> u32 {
        self.with(|st| st.commit_packs)
    }

    /// Requests to `repo` (a name or url) on one route, as `"<method>
    /// <route>"` (`"GET branch"`, `"GET file"`, `"GET files/metadata"`):
    /// every one that named an existing repo, answered or not.
    pub fn requests(&self, repo: &str, route: &str) -> u32 {
        self.with(|st| {
            let Some(url) = st.url_of(repo) else { return 0 };
            st.requests.get(&(url, route.to_string())).copied().unwrap_or(0)
        })
    }

    /// Host `refresh` nudges received (with `host_routes`).
    pub fn refresh_count(&self) -> u32 {
        self.with(|st| st.refreshes)
    }

    /// A token for calling the fake directly.
    pub fn token(&self, repo: &str, scopes: &[&str]) -> String {
        let url = self.repo_url(repo).unwrap_or_else(|| repo.to_string());
        self.inner.mint(&url, scopes, 900)
    }
}

/// A fresh org key (PKCS#8 PEM, P-256), for a dev stack or a test run.
pub fn generate_org_key_pem() -> String {
    use p256::pkcs8::EncodePrivateKey;
    use std::io::Read;
    let mut urandom = std::fs::File::open("/dev/urandom").expect("/dev/urandom");
    loop {
        let mut seed = [0u8; 32];
        urandom.read_exact(&mut seed).expect("read /dev/urandom");
        if let Ok(key) = SigningKey::from_slice(&seed) {
            return key.to_pkcs8_pem(Default::default()).expect("a P-256 key encodes").to_string();
        }
    }
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

mod b64map {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(m: &BTreeMap<String, Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        let enc: BTreeMap<&String, String> = m.iter().map(|(k, v)| (k, base64::engine::general_purpose::STANDARD.encode(v))).collect();
        enc.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Vec<u8>>, D::Error> {
        let enc = BTreeMap::<String, String>::deserialize(d)?;
        enc.into_iter()
            .map(|(k, v)| base64::engine::general_purpose::STANDARD.decode(v).map(|b| (k, b)).map_err(serde::de::Error::custom))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::pkcs8::EncodePrivateKey;

    fn get(url: &str, token: &str) -> (u16, Value) {
        let out = std::process::Command::new("curl")
            .args(["-s", "-w", "\n%{http_code}", "-H", &format!("authorization: Bearer {token}"), url])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n').unwrap();
        (code.parse().unwrap(), serde_json::from_str(body).unwrap_or(Value::Null))
    }

    #[test]
    fn history_merges_and_restores() {
        let cs = CodeStorage::start(Options::default()).unwrap();
        cs.seed_repo("r", &[("a", b"1")]);
        let c1 = cs.branch("r", "main").unwrap();
        let mut out = Vec::new();
        let c2 = cs.external_commit("r", "main", &[("a", Some(b"2"))], "two");
        cs.with(|st| st.move_branch("r", "live", &c1, &mut out));
        // live is behind main: a fast-forward
        cs.with(|st| {
            let repo = &st.repos["r"];
            assert!(repo.is_ancestor(&c1, &c2));
            assert!(repo.first_parent_reaches(&c1, &c2));
        });
        assert_eq!(cs.file_at("r", "main", "a").unwrap(), b"2");
        assert_eq!(cs.file_at("r", "live", "a").unwrap(), b"1");
        assert_eq!(cs.paths("r", "main"), vec!["a".to_string()]);
    }

    #[test]
    fn jwt_scopes_and_repo_claims() {
        let key = SigningKey::from_slice(&[5u8; 32]).unwrap();
        let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
        let cs = CodeStorage::start(Options { org_key_pem: Some(pem), ..Options::default() }).unwrap();
        cs.seed_repo("r", &[("a", b"1")]);
        cs.seed_repo("s", &[]);
        let good = cs.token("r", &["git:read"]);
        let (status, body) = get(&format!("{}/api/repos/r/branch?name=main", cs.url), &good);
        assert_eq!(status, 200, "{body}");
        let (status, _) = get(&format!("{}/api/repos/s/branch?name=main", cs.url), &good);
        assert_eq!(status, 403, "another repo's token");
        let (status, _) = get(&format!("{}/api/repos/r/branch?name=main", cs.url), "garbage");
        assert_eq!(status, 401);
        let write_only = cs.token("r", &["git:write"]);
        let (status, _) = get(&format!("{}/api/repos/r/files/metadata?ref=main", cs.url), &write_only);
        assert_eq!(status, 403, "missing git:read");
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_758_585_600_000), "2025-09-23T00:00:00Z");
    }
}
