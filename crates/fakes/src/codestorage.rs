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
//!   expected-parent CAS (409 `precondition_failed`). A pack that would
//!   change nothing is refused (412 `precondition_failed`, "no changes to
//!   commit"), as the real service refuses it: no empty commit.
//! - Each commit has its own tree. Merges are git's: nothing when the
//!   target holds the source, a fast-forward when they can, and otherwise
//!   a three-way merge from the merge base, a file whole (409
//!   `merge_conflict` when both sides changed one); the merge preview says
//!   which. Restore commits take an ancestor's tree, and refuse (412) the
//!   tip itself, a commit the branch does not hold, and no change. History
//!   walks first parents.
//! - No branch move is announced: the platform takes no push webhooks
//!   (cell/src/plane.rs), so a writer refreshes the fragment, as the CLI
//!   does.
//! - A repo is deleted (`DELETE /api/repos/{repo}`, `repo:write` on it) as
//!   the service deletes one: its calls answer 404 from then, a delete
//!   again is 409 `repository_deleted`, and it leaves the org's list. Its
//!   name is never made again (409 `repository_deleted`): the service's
//!   docs do not say a deleted name is free, so the fake holds the
//!   platform to never reusing one (`fragment_core::codestorage::repo_name`).
//! - A call can be a round trip away (`set_latency`): the real service
//!   answers a preview in about 100 ms, long enough for a fragment's alarm
//!   to run beside the request that armed it.
//!
//! Test levers are methods on [`CodeStorage`]; none is an HTTP route.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest as _, Sha1};

use crate::http::{Request, Response, Server};
use fragment_core::codestorage::{Claims, OrgKey, CHUNK_MAX};

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
    /// The lifetime of a token the host routes mint (the platform's is 900 s).
    pub token_ttl_s: i64,
}

impl Default for Options {
    fn default() -> Options {
        Options { org: "fragment-dev".into(), org_key_pem: None, page_size: 1000, state_file: None, port: 0, host_routes: false, token_ttl_s: 900 }
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
    commits: BTreeMap<String, Commit>,
    #[serde(with = "b64map")]
    blobs: BTreeMap<String, Vec<u8>>,
}

/// One file change, as the levers take them: `None` deletes.
pub type Change<'a> = (&'a str, Option<&'a [u8]>);
type OwnedChange = (String, Option<Vec<u8>>);

/// A commit's files, by path.
type Tree = BTreeMap<String, Entry>;

/// A commit to write: `changes` on top of the branch's tip, or, with
/// `from`, on top of the tree given with that commit as a second parent
/// (a merge commit and its merged tree, or a restore commit and the tree
/// of the commit it restores).
struct Write<'a> {
    branch: &'a str,
    message: &'a str,
    author: &'a str,
    changes: &'a [OwnedChange],
    from: Option<(&'a str, Tree)>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    repos: BTreeMap<String, Repo>,
    /// The repos deleted, by url, with their names: gone from `repos`.
    #[serde(default)]
    deleted: BTreeMap<String, String>,
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
    /// File reads answer 503 while set (an outage).
    #[serde(skip)]
    reads_failing: bool,
    /// Repo deletes still to answer 503 (`fail_repo_deletes`).
    #[serde(skip)]
    deletes_failing: u32,
    /// Repo deletes answer 403 while set (`refuse_repo_deletes`).
    #[serde(skip)]
    deletes_refused: bool,
    /// Requests answered, by (the bearer token's subject, the repo url a
    /// repo route names or "", route): what a test counts, by repo
    /// (`requests`) or by caller (`take_requests`). Bounded by subjects
    /// times repos times routes.
    #[serde(skip)]
    requests: BTreeMap<(String, String, String), u32>,
    /// Unsigned tokens minted so far (`fake-token-<n>`), and the first
    /// serial still honored (`revoke_tokens`).
    #[serde(skip)]
    tokens_minted: u64,
    #[serde(skip)]
    tokens_revoked_below: u64,
}

struct Inner {
    org: String,
    signing: Option<SigningKey>,
    page_size: usize,
    state_file: Option<PathBuf>,
    host_routes: bool,
    token_ttl_s: i64,
    url: String,
    state: Mutex<State>,
    /// How long every call waits before it is served (`set_latency`).
    latency_ms: AtomicU64,
}

pub struct CodeStorage {
    pub url: String,
    inner: Arc<Inner>,
    _server: Server,
}

pub(crate) fn now_ms() -> i64 {
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
pub(crate) fn iso(ms: i64) -> String {
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

/// A commit pack code.storage refuses as malformed, in its shape.
/// The service's answer about a repo deleted before (a delete again, or
/// its name made again).
fn deleted_repo() -> Response {
    Response::json(
        409,
        &json!({ "code": "repository_deleted", "detail": "repository already deleted", "error": "repository already deleted", "status": 409, "title": "Conflict", "type": "about:blank" }),
    )
}

fn invalid(message: &str) -> Response {
    Response::json(400, &json!({ "commit": null, "result": { "success": false, "status": "invalid", "message": message } }))
}

/// A commit pack that would change nothing, refused as code.storage
/// refuses it (its answer, read from a preview, 2026-10-05).
fn nothing_to_commit() -> Response {
    Response::json(
        412,
        &json!({
            "commit": null,
            "result": { "target_branch": "", "branch": "", "old_sha": "", "new_sha": "", "success": false,
                        "status": "precondition_failed", "message": "no changes to commit" },
        }),
    )
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

    /// Every commit reachable from `heads`, the heads included.
    fn reachable(&self, heads: impl IntoIterator<Item = String>) -> BTreeSet<String> {
        let mut stack: Vec<String> = heads.into_iter().collect();
        let mut seen = BTreeSet::new();
        while let Some(sha) = stack.pop() {
            if seen.insert(sha.clone()) {
                if let Some(c) = self.commits.get(&sha) {
                    stack.extend(c.parents.iter().cloned());
                }
            }
        }
        seen
    }

    /// Is `ancestor` reachable from `head` by any parents (`head` itself
    /// included), as git's `merge-base --is-ancestor`?
    fn is_ancestor(&self, ancestor: &str, head: &str) -> bool {
        self.reachable([head.to_string()]).contains(ancestor)
    }

    /// git's merge base of two commits: a common ancestor that no other
    /// common ancestor descends from (`None` for unrelated histories).
    fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        let of_a = self.reachable([a.to_string()]);
        let common: BTreeSet<String> = self.reachable([b.to_string()]).into_iter().filter(|c| of_a.contains(c)).collect();
        let below = self.reachable(common.iter().flat_map(|c| self.commits[c].parents.clone()));
        common.into_iter().find(|c| !below.contains(c))
    }

    /// git's three-way merge of `ours` and `theirs` from `base`, by path: a
    /// side that changed a path from the base wins it, and both changing it
    /// differently is a conflict (git would also try to merge the two
    /// files' lines; the fake takes a file whole). The paths that conflict
    /// when any do.
    fn merge_trees(&self, base: &str, ours: &str, theirs: &str) -> Result<Tree, Vec<String>> {
        let (base, ours, theirs) = (&self.commits[base].tree, &self.commits[ours].tree, &self.commits[theirs].tree);
        let blob = |t: &Tree, p: &str| t.get(p).map(|e| e.blob.clone());
        let mut merged = Tree::new();
        let mut conflicts = Vec::new();
        let paths: BTreeSet<&String> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
        for path in paths {
            let (b, o, t) = (blob(base, path), blob(ours, path), blob(theirs, path));
            let take = if o == t || b == t {
                ours.get(path)
            } else if b == o {
                theirs.get(path)
            } else {
                conflicts.push(path.clone());
                continue;
            };
            if let Some(entry) = take {
                merged.insert(path.clone(), entry.clone());
            }
        }
        if conflicts.is_empty() { Ok(merged) } else { Err(conflicts) }
    }

    /// Whether `changes` on `branch`'s tip leave its files as they are.
    fn changes_nothing(&self, branch: &str, changes: &[OwnedChange]) -> bool {
        let tree = self.branches.get(branch).and_then(|b| self.commits.get(b)).map(|c| &c.tree);
        changes.iter().all(|(path, bytes)| {
            let at = tree.and_then(|t| t.get(path));
            match bytes {
                None => at.is_none(),
                Some(b) => at.is_some_and(|e| e.blob == blob_sha(b)),
            }
        })
    }

    /// The same files, by bytes (a restore that would change nothing).
    fn same_files(&self, a: &str, b: &str) -> bool {
        let blobs = |sha: &str| self.commits[sha].tree.iter().map(|(p, e)| (p.clone(), e.blob.clone())).collect::<Vec<_>>();
        blobs(a) == blobs(b)
    }
}

impl State {
    fn repo_by(&self, name_or_url: &str) -> Option<&Repo> {
        self.repos.get(name_or_url).or_else(|| self.repos.values().find(|r| r.name == name_or_url))
    }

    fn url_of(&self, name_or_url: &str) -> Option<String> {
        self.repo_by(name_or_url).map(|r| r.url.clone())
    }

    /// Writes a commit; returns (old, new).
    fn commit(&mut self, url: &str, w: Write<'_>) -> (String, String) {
        let Write { branch, message, author, changes, from } = w;
        let tag = format!("{url}|{branch}|{message}");
        let sha = fresh_sha(&mut self.counter, &tag);
        let repo = self.repos.get_mut(url).expect("commit on a known repo");
        let old = repo.branches.get(branch).cloned();
        let (extra_parent, mut tree) = match from {
            Some((parent, tree)) => (Some(parent), tree),
            None => (None, old.as_ref().and_then(|b| repo.commits.get(b)).map(|c| c.tree.clone()).unwrap_or_default()),
        };
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
        (old.unwrap_or_else(|| ZERO.to_string()), sha)
    }

    fn move_branch(&mut self, url: &str, branch: &str, to: &str) -> String {
        let repo = self.repos.get_mut(url).expect("move on a known repo");
        repo.branches.insert(branch.into(), to.into()).unwrap_or_else(|| ZERO.to_string())
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

    fn handle(&self, req: &Request) -> Response {
        // served as it arrives at a service that far away
        let latency = self.latency_ms.load(Ordering::Relaxed);
        if latency > 0 {
            std::thread::sleep(std::time::Duration::from_millis(latency));
        }
        let mut st = self.state.lock().expect("fake state lock");
        *st.requests.entry((bearer_subject(req), repo_of(req), route_key(req))).or_default() += 1;
        let resp = self.route(&mut st, req);
        if req.method != "GET" && req.method != "HEAD" {
            self.persist(&st);
        }
        resp
    }

    fn route(&self, st: &mut State, req: &Request) -> Response {
        let path = req.path.as_str();
        let m = req.method.as_str();
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
                let token = self.mint(st, &url, &["git:read", "git:write"], self.token_ttl_s);
                // the host's whole answer (fragment_proto::StorageToken)
                return Response::json(200, &json!({ "token": token, "repo": url, "api": self.url, "expiresAt": now_ms() + self.token_ttl_s * 1000 }));
            }
            if path.starts_with("/api/f/") && path.ends_with("/refresh") && m == "POST" {
                st.refreshes += 1;
                return Response::json(200, &json!({ "ok": true }));
            }
            // the host's deploy (`POST /api/f/{name}/deploy`): live serves
            // main's tip, as the cell's `go_live` leaves it; its steps after
            // a rollback are the cell's, proven by the e2e's deploy lane
            if let Some(name) = path.strip_prefix("/api/f/").and_then(|p| p.strip_suffix("/deploy")).filter(|_| m == "POST") {
                let Some(url) = st.url_of(name) else { return problem(404, &format!("no fragment {name}")) };
                let Some(main) = st.repos[&url].branches.get("main").cloned() else { return problem(400, "nothing to deploy: main has no commits") };
                let live = match st.repos[&url].branches.get("live").cloned() {
                    Some(live) if live == main => live,
                    Some(live) if !st.repos[&url].is_ancestor(&live, &main) => {
                        let tree = st.repos[&url].commits[&main].tree.clone();
                        st.commit(&url, Write { branch: "live", message: "deploy", author: "host", changes: &[], from: Some((&main, tree)) }).1
                    }
                    _ => {
                        st.move_branch(&url, "live", &main);
                        main
                    }
                };
                return Response::json(200, &json!({ "live": live }));
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
            if st.deleted.values().any(|n| n == name) {
                return deleted_repo();
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
            // most 100, an opaque cursor
            let limit = req.query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20usize).clamp(1, 100);
            let start: usize = match req.query.get("cursor") {
                None => 0,
                Some(c) => match c.strip_prefix("page-").and_then(|n| n.parse().ok()) {
                    Some(n) => n,
                    None => return problem(400, "invalid cursor"),
                },
            };
            let mut all: Vec<&Repo> = st.repos.values().collect();
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
        // a repo's delete: `/api/repos/{repo}`, nothing after it
        if let Some(url) = path.strip_prefix("/api/repos/").filter(|u| !u.contains('/') && m == "DELETE") {
            if let Err(r) = self.authorize(req, "repo:write", Some(url)) {
                return r;
            }
            if st.deleted.contains_key(url) {
                return deleted_repo();
            }
            if st.deletes_refused {
                return problem(403, "repo deletes are refused (the fake's lever: a key without the right)");
            }
            if st.deletes_failing > 0 {
                st.deletes_failing -= 1;
                return problem(503, "repo deletes are unavailable (the fake's outage lever)");
            }
            let Some(repo) = st.repos.remove(url) else { return problem(404, "repository not found") };
            st.deleted.insert(url.to_string(), repo.name.clone());
            let message = format!("Repository {} deletion initiated. Physical storage cleanup will complete asynchronously.", repo.name);
            return Response::json(200, &json!({ "message": message, "repo_name": repo.name, "repo_id": repo.repo_id }));
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
        let scope = if m == "POST" { "git:write" } else { "git:read" };
        if let Err(r) = self.authorize(req, scope, Some(url)) {
            return r;
        }
        if unsigned_serial(req).is_some_and(|n| n < st.tokens_revoked_below) {
            return problem(401, "Invalid or expired token");
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
            ("GET", "merge/preview") => merge_preview(&st.repos[&url], req),
            ("POST", "commit-pack") => self.commit_pack(st, &url, req),
            ("POST", "branches/create") => branch_create(st, &url, req),
            ("POST", "merge") => merge(st, &url, req),
            ("POST", "restore-commit") => restore(st, &url, req),
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

    fn commit_pack(&self, st: &mut State, url: &str, req: &Request) -> Response {
        st.commit_packs += 1;
        if st.sabotage > 0 {
            st.sabotage -= 1;
            let n = st.counter;
            let competitor = [("competitor.txt".to_string(), Some(format!("competitor {n}").into_bytes()))];
            let (_, tip) = st.commit(url, Write { branch: "main", message: "competitor", author: "competitor", changes: &competitor, from: None });
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
        // as code.storage checks: every upsert names a content_id, and a
        // chunk streams only for one (a delete's id streams nothing)
        let files = meta["files"].as_array().cloned().unwrap_or_default();
        let mut streams = BTreeSet::new();
        for f in files.iter().filter(|f| f["operation"] != "delete") {
            let Some(id) = f["content_id"].as_str().filter(|id| !id.is_empty()) else { return invalid(&format!("missing content_id for {}", f["path"])) };
            streams.insert(id);
        }
        let mut chunks: BTreeMap<String, (Vec<u8>, bool)> = BTreeMap::new();
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else { return problem(400, "a line is not JSON") };
            let Some(id) = v["blob_chunk"]["content_id"].as_str() else { return problem(400, "a line is not a blob_chunk") };
            if !streams.contains(id) {
                return invalid(&format!("unexpected content_id {id:?}"));
            }
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
        for f in &files {
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
        if let Some(exp) = meta["expected_target_sha"].as_str() {
            let exp = if exp.is_empty() { ZERO } else { exp };
            if exp != current {
                return cas_failed(branch, &current);
            }
        }
        if st.repos[url].changes_nothing(branch, &changes) {
            return nothing_to_commit();
        }
        let author = meta["author"]["name"].as_str().unwrap_or("unknown");
        let message = meta["commit_message"].as_str().unwrap_or("");
        let (old, new) = st.commit(url, Write { branch, message, author, changes: &changes, from: None });
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

    fn mint(&self, st: &mut State, repo: &str, scopes: &[&str], ttl_s: i64) -> String {
        match &self.signing {
            None => {
                st.tokens_minted += 1;
                format!("{UNSIGNED_TOKEN}{}", st.tokens_minted - 1)
            }
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
    Response::bytes(200, "application/octet-stream", bytes)
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

fn branch_create(st: &mut State, url: &str, req: &Request) -> Response {
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
    st.move_branch(url, target, &base);
    Response::json(
        201,
        &json!({ "message": "branch created", "target_branch": target, "target_is_ephemeral": ephemeral, "commit_sha": base }),
    )
}

/// What merging `source` into `target` does, as code.storage's merge and
/// its preview see it: nothing when the target holds the source already,
/// a fast-forward when the source holds the target, else git's three-way
/// merge from their merge base (`Repo::merge_trees`). `None` for
/// unrelated histories.
enum Merging {
    NoOp,
    FastForward,
    Merge { base: String, merged: Result<Tree, Vec<String>> },
}

impl Repo {
    fn merging(&self, target: &str, source: &str) -> Option<Merging> {
        if self.is_ancestor(source, target) {
            return Some(Merging::NoOp);
        }
        if self.is_ancestor(target, source) {
            return Some(Merging::FastForward);
        }
        let base = self.merge_base(target, source)?;
        let merged = self.merge_trees(&base, target, source);
        Some(Merging::Merge { base, merged })
    }
}

/// A merge that conflicts, in the service's shape: its 409 is also a
/// stale `expected_target_sha`'s, told apart by `conflict_type`.
fn merge_conflict(base: &str, paths: &[String]) -> Response {
    Response::json(409, &json!({ "error": "merge conflict", "conflict_type": "merge_conflict", "conflict_paths": paths, "merge_base_sha": base }))
}

fn merge(st: &mut State, url: &str, req: &Request) -> Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let target = body["target_branch"].as_str().unwrap_or("");
    let Some(source) = st.repos[url].resolve(body["source_ref"].as_str().unwrap_or("")) else { return problem(404, "source_ref not found") };
    let Some(old) = st.repos[url].branches.get(target).cloned() else { return problem(404, &format!("branch not found: {target}")) };
    if let Some(exp) = body["expected_target_sha"].as_str() {
        if exp != old {
            return cas_failed(target, &old);
        }
    }
    let (new, strategy) = match st.repos[url].merging(&old, &source) {
        None => return problem(400, "refusing to merge unrelated histories"),
        Some(Merging::NoOp) => (old.clone(), "no_op"),
        Some(Merging::FastForward) => {
            st.move_branch(url, target, &source);
            (source, "ff")
        }
        Some(Merging::Merge { base, merged: Err(paths) }) => return merge_conflict(&base, &paths),
        Some(Merging::Merge { merged: Ok(tree), .. }) => {
            let msg = body["commit_message"].as_str().unwrap_or("merge").to_string();
            let author = body["author"]["name"].as_str().unwrap_or("unknown").to_string();
            let (_, new) = st.commit(url, Write { branch: target, message: &msg, author: &author, changes: &[], from: Some((&source, tree)) });
            (new, "merge_commit")
        }
    };
    Response::json(
        200,
        &json!({ "target": { "branch": target, "old_sha": old, "new_sha": new, "strategy": strategy }, "result": { "success": true, "status": "ok" } }),
    )
}

/// `GET merge/preview?source_branch=&target_branch=`: what that merge
/// would do, changing nothing.
fn merge_preview(repo: &Repo, req: &Request) -> Response {
    let named = |k: &str| req.query.get(k).map(String::as_str).unwrap_or("");
    let (source_branch, target_branch) = (named("source_branch"), named("target_branch"));
    let (Some(source), Some(target)) = (repo.branches.get(source_branch), repo.branches.get(target_branch)) else { return problem(404, "branch not found") };
    let (result, base, conflicts) = match repo.merging(target, source) {
        None => return problem(400, "refusing to merge unrelated histories"),
        Some(Merging::NoOp) => ("no_op", source.clone(), vec![]),
        Some(Merging::FastForward) => ("fast_forward", target.clone(), vec![]),
        Some(Merging::Merge { base, merged }) => ("merge_commit", base, merged.err().unwrap_or_default()),
    };
    Response::json(
        200,
        &json!({
            "status": if conflicts.is_empty() { "clean" } else { "conflicted" },
            "result": result,
            "source_branch": source_branch,
            "target_branch": target_branch,
            "source_tip_sha": source,
            "target_tip_sha": target,
            "merge_base_sha": base,
            "conflict_paths": conflicts,
            "conflicts": [],
        }),
    )
}

fn restore(st: &mut State, url: &str, req: &Request) -> Response {
    let text = String::from_utf8_lossy(&req.body);
    let Some(Ok(first)) = text.lines().find(|l| !l.trim().is_empty()).map(serde_json::from_str::<Value>) else { return problem(400, "a bad metadata line") };
    let meta = &first["metadata"];
    let target = meta["target_branch"].as_str().unwrap_or("");
    let Some(old) = st.repos[url].branches.get(target).cloned() else { return problem(404, &format!("branch not found: {target}")) };
    // as the service refuses a restore that cannot move the branch: to its
    // own tip, to a commit it does not hold (an unknown one included), or
    // to the files it has
    let repo = &st.repos[url];
    let base = repo.resolve(meta["base_ref"].as_str().unwrap_or("")).unwrap_or_default();
    if base == old {
        return problem(412, "the branch already points at base_ref");
    }
    if !repo.is_ancestor(&base, &old) {
        return problem(412, "base_ref is not an ancestor of the target's tip");
    }
    if repo.same_files(&base, &old) {
        return problem(412, "the restore would produce no change");
    }
    if let Some(exp) = meta["expected_target_sha"].as_str() {
        if exp != old {
            return cas_failed(target, &old);
        }
    }
    let tree = repo.commits[&base].tree.clone();
    let msg = meta["commit_message"].as_str().unwrap_or("restore").to_string();
    let author = meta["author"]["name"].as_str().unwrap_or("unknown").to_string();
    let (_, new) = st.commit(url, Write { branch: target, message: &msg, author: &author, changes: &[], from: Some((&base, tree)) });
    Response::json(
        201,
        &json!({
            "commit": { "commit_sha": new, "tree_sha": new, "target_branch": target, "pack_bytes": 0, "blob_count": 0 },
            "result": { "target_branch": target, "branch": target, "old_sha": old, "new_sha": new, "success": true, "status": "ok" },
        }),
    )
}

/// What a request is counted under: its method and route (`GET branch`,
/// `POST commit-pack`, `GET storage-token`), not its repo or query.
fn route_key(req: &Request) -> String {
    let path = req.path.as_str();
    let route = match (path.strip_prefix("/api/repos/"), path.strip_prefix("/api/f/")) {
        (Some(repo_scoped), _) => repo_scoped.split_once('/').map_or(repo_scoped, |(_, op)| op),
        (None, Some(host)) => host.rsplit_once('/').map_or(host, |(_, op)| op),
        (None, None) => path,
    };
    format!("{} {route}", req.method)
}

/// The repo url a repo route names (`/api/repos/<url>/<op>`), or "".
fn repo_of(req: &Request) -> String {
    req.path.strip_prefix("/api/repos/").and_then(|p| p.split_once('/')).map_or_else(String::new, |(url, _)| url.to_string())
}

/// The subject (`sub`) of a request's bearer JWT, read without verifying it
/// (counting is not authorizing); "" for none, or an unsigned token.
fn bearer_subject(req: &Request) -> String {
    let payload = req.header("authorization").and_then(|h| h.strip_prefix("Bearer ")).and_then(|t| t.split('.').nth(1));
    let claims = payload.and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    claims.and_then(|c| c["sub"].as_str().map(str::to_string)).unwrap_or_default()
}

/// Tokens the fake mints when it signs nothing: this prefix and a serial.
const UNSIGNED_TOKEN: &str = "fake-token-";

/// An unsigned token's serial, if the request carries one.
fn unsigned_serial(req: &Request) -> Option<u64> {
    req.header("authorization")?.strip_prefix("Bearer ")?.strip_prefix(UNSIGNED_TOKEN)?.parse().ok()
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
        // reads it through `Inner`, so bind first (port 0 picks a free one)
        // and build Inner with the port the listener holds.
        let listener = std::net::TcpListener::bind(("127.0.0.1", opts.port))?;
        let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let inner = Arc::new(Inner {
            org: opts.org,
            signing,
            page_size: opts.page_size,
            state_file: opts.state_file,
            host_routes: opts.host_routes,
            token_ttl_s: opts.token_ttl_s,
            url: url.clone(),
            state: Mutex::new(state),
            latency_ms: AtomicU64::new(0),
        });
        let handler_inner = Arc::clone(&inner);
        let server = Server::serve(listener, Arc::new(move |req: &Request| handler_inner.handle(req)))?;
        Ok(CodeStorage { url, inner, _server: server })
    }

    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.inner.state.lock().expect("fake state lock"))
    }

    /// A repo whose url is its name, with `files` on `main` in one commit.
    /// Seeding an existing name resets it (same identity, a new world).
    pub fn seed_repo(&self, name: &str, files: &[(&str, &[u8])]) {
        self.with(|st| {
            st.repos.insert(name.into(), Repo::new(name, name, &format!("repo_{name}")));
            let changes: Vec<OwnedChange> = files.iter().map(|(p, b)| (p.to_string(), Some(b.to_vec()))).collect();
            st.commit(name, Write { branch: "main", message: "seed", author: "seed", changes: &changes, from: None });
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

    /// The name of the repo whose url-form identity is `url` (one deleted
    /// too).
    pub fn repo_name(&self, url: &str) -> Option<String> {
        self.with(|st| st.repos.get(url).map(|r| r.name.clone()).or_else(|| st.deleted.get(url).cloned()))
    }

    /// The next `n` repo deletes answer 503 (an outage the platform retries).
    pub fn fail_repo_deletes(&self, n: u32) {
        self.with(|st| st.deletes_failing = n);
    }

    /// While `on`, every repo delete answers 403: a refusal no retry passes
    /// until something changes (a key given the right).
    pub fn refuse_repo_deletes(&self, on: bool) {
        self.with(|st| st.deletes_refused = on);
    }

    /// Whether the repo `url` was deleted (`DELETE /api/repos/{repo}`).
    pub fn repo_deleted(&self, url: &str) -> bool {
        self.with(|st| st.deleted.contains_key(url))
    }

    pub fn branch(&self, repo: &str, branch: &str) -> Option<String> {
        self.with(|st| st.repo_by(repo)?.branches.get(branch).cloned())
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

    /// A commit from some other writer (no CAS), a git push with a storage
    /// token: no one is told, until the writer refreshes the fragment.
    pub fn external_commit(&self, repo: &str, branch: &str, changes: &[Change<'_>], message: &str) -> String {
        self.with(|st| {
            let url = st.url_of(repo).expect("external_commit on a known repo");
            let changes: Vec<OwnedChange> = changes.iter().map(|(p, b)| (p.to_string(), b.map(<[u8]>::to_vec))).collect();
            st.commit(&url, Write { branch, message, author: "external", changes: &changes, from: None }).1
        })
    }

    /// Points a branch at a commit (a deploy's ref move).
    pub fn set_branch(&self, repo: &str, branch: &str, sha: &str) {
        self.with(|st| {
            let url = st.url_of(repo).expect("set_branch on a known repo");
            assert!(st.repos[&url].commits.contains_key(sha), "set_branch to a known commit");
            st.move_branch(&url, branch, sha);
            self.inner.persist(st);
        });
    }

    /// Every call waits this long before it is served, as a call to the
    /// real service travels (zero: at once, the default).
    pub fn set_latency(&self, latency: std::time::Duration) {
        self.inner.latency_ms.store(latency.as_millis() as u64, Ordering::Relaxed);
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
    /// <route>"` (`"GET branch"`, `"GET file"`, `"GET files/metadata"`),
    /// from every caller, answered or not. A check counts by difference;
    /// `take_requests` resets these counts too.
    pub fn requests(&self, repo: &str, route: &str) -> u32 {
        self.with(|st| {
            let Some(url) = st.url_of(repo) else { return 0 };
            st.requests.iter().filter(|((_, r, rt), _)| *r == url && rt == route).map(|(_, n)| n).sum()
        })
    }

    /// Host `refresh` nudges received (with `host_routes`).
    pub fn refresh_count(&self) -> u32 {
        self.with(|st| st.refreshes)
    }

    /// The requests answered since the last take, by route (`GET branch`,
    /// `GET files/metadata`, `GET file`, `POST commit-pack`, and with
    /// `host_routes` `GET storage-token` and `POST refresh`), from callers
    /// whose token's subject starts with `subject`: "" for every caller;
    /// an editor's client is `editor:<id>`, the cell's `fragment-runtime`.
    /// Taking resets every count, the other subjects' too.
    pub fn take_requests(&self, subject: &str) -> BTreeMap<String, u32> {
        self.with(|st| {
            let mut out = BTreeMap::new();
            for ((sub, _, route), n) in std::mem::take(&mut st.requests) {
                if sub.starts_with(subject) {
                    *out.entry(route).or_default() += n;
                }
            }
            out
        })
    }

    /// Every token minted so far is refused from now on (401, as an expired
    /// or revoked one is); tokens minted later pass. Unsigned tokens only:
    /// a fake with an org key checks real expiry instead.
    pub fn revoke_tokens(&self) {
        assert!(self.inner.signing.is_none(), "revoke_tokens works on the unsigned tokens of a fake without an org key");
        self.with(|st| st.tokens_revoked_below = st.tokens_minted);
    }

    /// A token for calling the fake directly.
    pub fn token(&self, repo: &str, scopes: &[&str]) -> String {
        let url = self.repo_url(repo).unwrap_or_else(|| repo.to_string());
        self.with(|st| self.inner.mint(st, &url, scopes, 900))
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
    use crate::http;
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

    /// Goal: the counters a CLI test pins its requests with count what was
    /// asked, by route and by caller. Method: two callers (the org key signs
    /// both, as the subjects `fake` and another) ask known routes.
    #[test]
    fn requests_are_counted_by_route_and_subject() {
        let key = SigningKey::from_slice(&[5u8; 32]).unwrap();
        let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
        let cs = CodeStorage::start(Options { org_key_pem: Some(pem), ..Options::default() }).unwrap();
        cs.seed_repo("r", &[("a", b"1")]);
        let token = cs.token("r", &["git:read"]);
        for _ in 0..2 {
            assert_eq!(get(&format!("{}/api/repos/r/branch?name=main", cs.url), &token).0, 200);
        }
        assert_eq!(get(&format!("{}/api/repos/r/files/metadata?ref=main", cs.url), &token).0, 200);
        assert_eq!(get(&format!("{}/api/repos/r/branch?name=nope", cs.url), "").0, 401);
        let mine = cs.take_requests("fake");
        assert_eq!(mine, BTreeMap::from([("GET branch".to_string(), 2), ("GET files/metadata".to_string(), 1)]));
        assert!(cs.take_requests("").is_empty(), "taking resets every count");
        assert_eq!(get(&format!("{}/api/repos/r/branch?name=main", cs.url), &token).0, 200);
        assert_eq!(cs.take_requests(""), BTreeMap::from([("GET branch".to_string(), 1)]));
    }

    /// Goal: a revoked token is refused and a later one is not, as a
    /// client that mints again after a refusal needs. Method: unsigned
    /// tokens before and after the lever.
    #[test]
    fn revoked_tokens_are_refused_and_new_ones_pass() {
        let cs = CodeStorage::start(Options::default()).unwrap();
        cs.seed_repo("r", &[("a", b"1")]);
        let old = cs.token("r", &["git:read"]);
        assert_eq!(get(&format!("{}/api/repos/r/branch?name=main", cs.url), &old).0, 200);
        cs.revoke_tokens();
        assert_eq!(get(&format!("{}/api/repos/r/branch?name=main", cs.url), &old).0, 401);
        let new = cs.token("r", &["git:read"]);
        assert_ne!(new, old);
        assert_eq!(get(&format!("{}/api/repos/r/branch?name=main", cs.url), &new).0, 200);
    }

    fn send(method: &str, url: &str, token: &str, body: Option<&str>) -> (u16, Value) {
        let mut args = vec!["-s", "-w", "\n%{http_code}", "-X", method, "-H"];
        let auth = format!("authorization: Bearer {token}");
        args.push(&auth);
        if let Some(b) = body {
            args.extend(["-H", "content-type: application/json", "--data", b]);
        }
        args.push(url);
        let out = std::process::Command::new("curl").args(&args).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n').unwrap();
        (code.parse().unwrap(), serde_json::from_str(body).unwrap_or(Value::Null))
    }

    /// Goal: a repo is deleted as the service deletes one: its calls are
    /// 404 from then, a delete again is 409 `repository_deleted` (a wipe run
    /// again reads it as done), it leaves the org's list, its name is never
    /// made again, and only `repo:write` on it deletes it. Method: a repo
    /// made through the API, deleted with the wrong token, then the right
    /// one, twice, then made again.
    #[test]
    fn a_deleted_repo_is_gone_and_its_name_never_made_again() {
        let key = SigningKey::from_slice(&[5u8; 32]).unwrap();
        let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
        let cs = CodeStorage::start(Options { org_key_pem: Some(pem), ..Options::default() }).unwrap();
        let made = send("POST", &format!("{}/api/repos", cs.url), &cs.token("todo--paul--abc", &["repo:write"]), Some(r#"{"repo_name":"todo--paul--abc"}"#));
        assert_eq!(made.0, 201, "{}", made.1);
        let url = cs.repo_url("todo--paul--abc").expect("its url");
        cs.seed_filler(1);
        let delete = |token: &str| send("DELETE", &format!("{}/api/repos/{url}", cs.url), token, None);
        assert_eq!(delete(&cs.token(&url, &["git:write"])).0, 403, "git:write deletes no repo");
        assert!(!cs.repo_deleted(&url));
        let gone = delete(&cs.token(&url, &["repo:write"]));
        assert_eq!(gone.0, 200, "{}", gone.1);
        assert_eq!(gone.1["repo_name"], "todo--paul--abc");
        assert!(cs.repo_deleted(&url));
        assert_eq!(cs.repo_name(&url).as_deref(), Some("todo--paul--abc"), "a deleted repo's name is known");
        let again = delete(&cs.token(&url, &["repo:write"]));
        assert_eq!((again.0, again.1["code"].as_str()), (409, Some("repository_deleted")), "a delete again");
        assert_eq!(get(&format!("{}/api/repos/{url}/branch?name=main", cs.url), &cs.token(&url, &["git:read"])).0, 404, "its calls are 404");
        let listed = get(&format!("{}/api/repos?limit=100", cs.url), &cs.token("fragment-dev", &["org:read"])).1;
        assert!(listed["repos"].as_array().unwrap().iter().all(|r| r["repo_name"] != "todo--paul--abc"), "{listed}");
        let remade = send("POST", &format!("{}/api/repos", cs.url), &cs.token("todo--paul--abc", &["repo:write"]), Some(r#"{"repo_name":"todo--paul--abc"}"#));
        assert_eq!((remade.0, remade.1["code"].as_str()), (409, Some("repository_deleted")), "its name is never made again");
    }

    /// Goal: a commit pack that changes nothing is refused as the real
    /// service refuses it (412, "no changes to commit"), never an empty
    /// commit. Method: packs on a seeded repo that write the bytes already
    /// there, delete what is absent, and then change a file.
    #[test]
    fn a_pack_that_changes_nothing_is_refused() {
        use fragment_core::codestorage::{commit_pack, FileChange};
        let cs = CodeStorage::start(Options::default()).unwrap();
        cs.seed_repo("r", &[("a", b"1")]);
        let head = cs.branch("r", "main").unwrap();
        let post = |changes: &[FileChange]| {
            let pack = commit_pack("main", Some(&head), "m", ("t", "t@e2e.test"), changes);
            http::post(&format!("{}/api/repos/r/commit-pack", cs.url), &[("content-type", "application/x-ndjson")], pack.as_bytes()).unwrap()
        };
        assert_eq!(post(&[FileChange::Upsert { path: "a", bytes: b"1" }]), 412, "the same bytes");
        assert_eq!(post(&[FileChange::Delete { path: "nope" }]), 412, "a file that is not there");
        assert_eq!(cs.branch("r", "main").as_deref(), Some(head.as_str()), "no commit was made");
        assert_eq!(post(&[FileChange::Upsert { path: "a", bytes: b"1" }, FileChange::Upsert { path: "b", bytes: b"2" }]), 201, "one file changed");
        assert_ne!(cs.branch("r", "main").as_deref(), Some(head.as_str()));
    }
}
