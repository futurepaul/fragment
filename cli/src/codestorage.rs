// Typed-error HTTP client for the code.storage API (their OpenAPI spec is
// the contract: https://code.storage/docs/openapi.json). Commit-without-a-
// clone: no local .git, no git2 — files go up as NDJSON commit packs with
// expected-parent CAS, come back as listings and raw bytes.
//
// Per the engineering style: no anyhow in here — CsError is matchable at
// the boundary, and every retry/loop has an explicit bound.
use crate::api::{timeout_for, Client as HostClient, Replay, CONNECT_TIMEOUT, REQUEST_ATTEMPTS};
use serde_json::Value;
use std::fmt;

/// Working files live on `main`; `live` is the blessed serve point
/// (ROADMAP wire contract: one repo per fragment, preview = ephemeral ref,
/// promote = move live, rollback = re-point live).
pub const MAIN: &str = "main";
pub const LIVE: &str = "live";

/// CAS retry bound for commit-style writes: conflict -> refetch head,
/// rebuild the diff, retry; explicit error after this many attempts.
pub const MAX_CAS_ATTEMPTS: u32 = 3;
/// Listing pages are capped so a runaway cursor loop fails loudly instead
/// of spinning: 1000 pages x 1000 files/page = 1M files, far past any
/// fragment's shape.
pub const MAX_LIST_PAGES: u32 = 1000;
/// Spec limit: decoded blob_chunk bodies are capped at 4 MiB each; larger
/// files stream as multiple chunks. There is no total-size limit and no
/// LFS path in the HTTP API (flagged vs ROADMAP — see GUIDE/deploy notes).
pub const CHUNK_MAX: usize = 4 * 1024 * 1024;
/// files/metadata page size (spec max 1000).
const LIST_PAGE: u64 = 1000;

#[derive(Debug)]
pub enum CsError {
    /// connection-level failure (after the bounded connect retries)
    Transport(String),
    /// a write reached the server but its answer was lost: it may have
    /// been applied, so the caller re-reads before deciding anything
    OutcomeUnknown(String),
    /// 401/403: token rejected, or a ref policy refused this write
    Auth(String),
    /// 404
    NotFound(String),
    /// 409: expected_target_sha did not match the current tip
    CasRejected { detail: String },
    /// 412: restore no-op / base not an ancestor of the tip
    Precondition(String),
    /// 429 (Retry-After seconds if the server sent one)
    RateLimited(Option<u64>),
    /// any other non-2xx
    Http { status: u16, detail: String },
    /// 2xx body that is not the documented shape
    Malformed(String),
}

impl fmt::Display for CsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CsError::Transport(d) => write!(f, "code.storage unreachable: {d}"),
            CsError::OutcomeUnknown(d) => write!(f, "code.storage may have applied the request, but its answer was lost: {d}"),
            CsError::Auth(d) => write!(f, "code.storage refused the token (auth/ref policy): {d}"),
            CsError::NotFound(d) => write!(f, "not found on code.storage: {d}"),
            CsError::CasRejected { detail } => write!(f, "branch moved under us (expected-parent CAS rejected): {detail}"),
            CsError::Precondition(d) => write!(f, "precondition failed: {d}"),
            CsError::RateLimited(raf) => write!(f, "rate limited{}", raf.map(|s| format!(" (retry after {s}s)")).unwrap_or_default()),
            CsError::Http { status, detail } => write!(f, "http {status} from code.storage: {detail}"),
            CsError::Malformed(d) => write!(f, "code.storage response not in documented shape: {d}"),
        }
    }
}
impl std::error::Error for CsError {}

/// The fragment host mints a short-lived, repo-scoped code.storage JWT;
/// the CLI never sees the org key. Response contract (runtime side):
/// {"token": ..., "repo": ..., "api": "https://api.<cluster>.code.storage"}
/// where `api` is the spec's server URL (endpoints append /api/repos/...).
fn mint_from_host(host: &HostClient, name: &str, override_url: Option<&str>) -> Result<CodeStorage, CsError> {
    let resp = host
        .get(&format!("/api/f/{name}/storage-token"))
        .map_err(|e| CsError::Auth(format!("storage-token request failed: {e:#}")))?;
    let v: Value = host
        .call(resp)
        .map_err(|e| CsError::Auth(format!("storage-token fetch failed: {e:#}")))?;
    let server = override_url
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            v["api"].as_str().unwrap_or_default().trim_end_matches('/').to_string()
        });
    let repo = v["repo"].as_str().unwrap_or_default().to_string();
    let token = v["token"].as_str().unwrap_or_default().to_string();
    if server.is_empty() || repo.is_empty() || token.is_empty() {
        return Err(CsError::Malformed(format!(
            "storage-token response needs non-empty token/repo/api, got: {v}"
        )));
    }
    Ok(CodeStorage {
        server,
        repo,
        token,
        // every request sets its own total timeout (`timeout_for`)
        http: reqwest::blocking::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| CsError::Transport(e.to_string()))?,
    })
}

#[derive(Clone)]
pub struct RemoteFile {
    pub path: String,
    pub size: u64,
    pub last_commit_sha: String,
}

#[derive(serde::Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub date: String,
}

/// One staged file operation for the commit builder.
pub enum Change {
    Upsert { path: String, bytes: Vec<u8> },
    Delete { path: String },
}

pub struct Author {
    pub name: String,
    pub email: String,
}

impl Author {
    /// The CLI's git identity: the writer's npub prefix, machine replyable.
    pub fn writer(writer_id: &str) -> Author {
        Author { name: format!("fragment/{writer_id}"), email: format!("{writer_id}@users.fragment") }
    }
}

pub struct CodeStorage {
    server: String,
    repo: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl CodeStorage {
    /// Mint a scoped token from the fragment host and build the client.
    /// `override_url` (FRAGMENT_CODESTORAGE_URL / config) replaces the
    /// server the host reports — the backend-swap knob.
    pub fn connect(host: &HostClient, name: &str, override_url: Option<&str>) -> Result<CodeStorage, CsError> {
        mint_from_host(host, name, override_url)
    }

    /// url-form repo identity this client is scoped to — the world a sync
    /// journal binds itself to
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// repo name URL-encoded as one path segment (spec: `a/b` -> `a%2Fb`)
    fn repo_seg(&self) -> String {
        let mut out = String::new();
        for b in self.repo.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    /// One request against the spec server: its status and body. A
    /// transport failure is retried within `REQUEST_ATTEMPTS` as
    /// `Replay::of(method)` allows (a stale keep-alive pool must not wedge a
    /// watcher, and a POST that may have landed is never sent again blind);
    /// payload-level failures NEVER retry here — CAS semantics belong to
    /// the callers' bounded retry loops.
    fn req(&self, method: &str, path: &str, body: Option<Vec<u8>>, content_type: Option<&str>) -> Result<(u16, Vec<u8>), CsError> {
        let url = format!("{}/api/repos/{}{}", self.server, self.repo_seg(), path);
        let body = body.unwrap_or_default();
        let replay = Replay::of(method);
        let timeout = timeout_for(body.len() as u64);
        let mut last_err: Option<String> = None;
        for attempt in 0..REQUEST_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
            }
            let mut r = match method {
                "GET" => self.http.get(&url),
                "HEAD" => self.http.head(&url),
                "POST" => self.http.post(&url),
                _ => return Err(CsError::Malformed(format!("bad method {method}"))),
            };
            r = r.bearer_auth(&self.token).timeout(timeout);
            if let Some(ct) = content_type {
                r = r.header("content-type", ct);
            }
            if !body.is_empty() {
                r = r.body(body.clone());
            }
            // the body is read here too: an answer cut off mid-body is as
            // lost as one that never came
            let answer = r.send().and_then(|resp| {
                let status = resp.status().as_u16();
                resp.bytes().map(|b| (status, b.to_vec()))
            });
            match answer {
                Ok(answer) => return Ok(answer),
                Err(e) if replay.allows_retry(!e.is_connect()) => last_err = Some(e.to_string()),
                Err(e) => return Err(CsError::OutcomeUnknown(format!("{method} {path}: {e}"))),
            }
        }
        Err(CsError::Transport(last_err.unwrap_or_else(|| "no error recorded".into())))
    }

    /// Non-2xx -> typed error; problem+json `detail`/`error` preferred.
    fn check(&self, status: u16, body: &[u8]) -> Result<(), CsError> {
        if (200..300).contains(&status) {
            return Ok(());
        }
        let detail = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| {
                v["detail"].as_str().or_else(|| v["error"].as_str()).map(|s| s.to_string())
            })
            .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(200).collect());
        match status {
            401 | 403 => Err(CsError::Auth(detail)),
            404 => Err(CsError::NotFound(detail)),
            409 => Err(CsError::CasRejected { detail }),
            412 => Err(CsError::Precondition(detail)),
            429 => Err(CsError::RateLimited(None)),
            _ => Err(CsError::Http { status, detail }),
        }
    }

    /// Branch head SHA; Ok(None) when the branch does not exist (404).
    pub fn branch_head(&self, branch: &str) -> Result<Option<String>, CsError> {
        let (status, body) = self.req("GET", &format!("/branch?name={}", q(branch)), None, None)?;
        if status == 404 {
            return Ok(None);
        }
        self.check(status, &body)?;
        let v: Value = serde_json::from_slice(&body).map_err(|e| CsError::Malformed(e.to_string()))?;
        let sha = v["branch"]["head_sha"].as_str().unwrap_or_default().to_string();
        if sha.is_empty() {
            return Err(CsError::Malformed(format!("branch response missing head_sha: {v}")));
        }
        Ok(Some(sha))
    }

    /// Full recursive file listing at a ref, cursor-paginated (bounded).
    pub fn list_files(&self, git_ref: &str) -> Result<Vec<RemoteFile>, CsError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut path = format!("/files/metadata?ref={}&limit={LIST_PAGE}", q(git_ref));
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={}", q(c)));
            }
            let (status, body) = self.req("GET", &path, None, None)?;
            self.check(status, &body)?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| CsError::Malformed(e.to_string()))?;
            let files = v["files"].as_array().cloned().unwrap_or_default();
            for f in files {
                let path = f["path"].as_str().unwrap_or_default().to_string();
                if path.is_empty() {
                    return Err(CsError::Malformed("file entry missing path".into()));
                }
                out.push(RemoteFile {
                    path,
                    size: f["size"].as_u64().unwrap_or(0),
                    last_commit_sha: f["last_commit_sha"].as_str().unwrap_or_default().to_string(),
                });
            }
            if !v["has_more"].as_bool().unwrap_or(false) {
                return Ok(out);
            }
            cursor = Some(v["next_cursor"].as_str().unwrap_or_default().to_string());
        }
        Err(CsError::Malformed(format!("listing exceeded {MAX_LIST_PAGES} pages")))
    }

    /// Raw file bytes at a ref.
    pub fn read_file(&self, path: &str, git_ref: &str) -> Result<Vec<u8>, CsError> {
        let (status, body) = self.req("GET", &format!("/file?path={}&ref={}", q(path), q(git_ref)), None, None)?;
        self.check(status, &body)?;
        Ok(body)
    }

    /// Build and send one NDJSON commit pack. `expected` is the
    /// expected-parent SHA (CAS); None lets the server create the branch.
    /// Returns the new branch tip.
    pub fn commit(
        &self,
        expected: Option<&str>,
        message: &str,
        author: &Author,
        changes: &[Change],
    ) -> Result<String, CsError> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut files_meta: Vec<Value> = Vec::new();
        let mut chunks: Vec<String> = Vec::new();
        for (i, ch) in changes.iter().enumerate() {
            let id = format!("blob-{i}");
            match ch {
                Change::Upsert { path, bytes } => {
                    files_meta.push(json_f(path, "upsert", &id));
                    // spec: decoded chunks cap at 4 MiB; each content stream
                    // must END with an eof:true chunk (live-verified: the
                    // real service reports "incomplete content stream" for
                    // a final chunk sent with eof:false)
                    let pieces: Vec<&[u8]> = bytes.chunks(CHUNK_MAX.max(1)).collect();
                    for (i, piece) in pieces.iter().enumerate() {
                        let last = i + 1 == pieces.len();
                        chunks.push(line(&id, &b64.encode(piece), last));
                    }
                    if bytes.is_empty() {
                        chunks.push(line(&id, "", true)); // empty file: one eof chunk
                    }
                }
                Change::Delete { path } => {
                    files_meta.push(json_f(path, "delete", &id));
                    // spec example: deletes still send one empty eof chunk
                    chunks.push(line(&id, "", true));
                }
            }
        }
        let mut meta = serde_json::json!({
            "target_branch": MAIN,
            "commit_message": message,
            "author": { "name": author.name, "email": author.email },
            "files": files_meta,
        });
        if let Some(sha) = expected {
            meta["expected_target_sha"] = Value::String(sha.to_string());
        }
        let mut body = serde_json::to_string(&serde_json::json!({ "metadata": meta })).unwrap();
        body.push('\n');
        for c in chunks {
            body.push_str(&c);
        }
        let (status, rbody) = self.req("POST", "/commit-pack", Some(body.into_bytes()), Some("application/x-ndjson"))?;
        self.check(status, &rbody)?;
        let v: Value = serde_json::from_slice(&rbody).map_err(|e| CsError::Malformed(e.to_string()))?;
        let tip = v["result"]["new_sha"].as_str().unwrap_or_default().to_string();
        if tip.is_empty() || !v["result"]["success"].as_bool().unwrap_or(false) {
            return Err(CsError::Malformed(format!("commit-pack result not ok: {v}")));
        }
        Ok(tip)
    }

    /// Create a branch (or ephemeral ref) at a base ref. Returns its SHA.
    pub fn create_branch(&self, base_ref: &str, target_branch: &str, ephemeral: bool) -> Result<String, CsError> {
        let body = serde_json::json!({
            "base_ref": base_ref,
            "target_branch": target_branch,
            "target_is_ephemeral": ephemeral,
        });
        let (status, rbody) = self.req("POST", "/branches/create", Some(body.to_string().into_bytes()), Some("application/json"))?;
        self.check(status, &rbody)?;
        let v: Value = serde_json::from_slice(&rbody).map_err(|e| CsError::Malformed(e.to_string()))?;
        let sha = v["commit_sha"].as_str().unwrap_or_default().to_string();
        if sha.is_empty() {
            return Err(CsError::Malformed(format!("branches/create missing commit_sha: {v}")));
        }
        Ok(sha)
    }

    /// Move `live` to `main`'s current tip: ff when possible, else a merge
    /// commit (post-rollback deploys). `expected_live` is the CAS tip guard.
    /// Returns the new live tip.
    pub fn promote_live(&self, expected_live: &str, message: &str, author: &Author) -> Result<String, CsError> {
        let body = serde_json::json!({
            "target_branch": LIVE,
            "source_ref": MAIN,
            "strategy": "ff_prefer",
            "expected_target_sha": expected_live,
            "commit_message": message,
            "author": { "name": author.name, "email": author.email },
        });
        let (status, rbody) = self.req("POST", "/merge", Some(body.to_string().into_bytes()), Some("application/json"))?;
        self.check(status, &rbody)?;
        let v: Value = serde_json::from_slice(&rbody).map_err(|e| CsError::Malformed(e.to_string()))?;
        let tip = v["target"]["new_sha"].as_str().unwrap_or_default().to_string();
        if tip.is_empty() {
            return Err(CsError::Malformed(format!("merge response missing target.new_sha: {v}")));
        }
        Ok(tip)
    }

    /// Rollback: append a restore commit on `live` whose tree matches
    /// `base_ref` (must be an ancestor of the current tip, else 412).
    pub fn restore_live(&self, base_ref: &str, expected_live: &str, message: &str, author: &Author) -> Result<String, CsError> {
        let meta = serde_json::json!({
            "metadata": {
                "target_branch": LIVE,
                "base_ref": base_ref,
                "expected_target_sha": expected_live,
                "commit_message": message,
                "author": { "name": author.name, "email": author.email },
            }
        });
        let body = format!("{meta}\n");
        let (status, rbody) = self.req("POST", "/restore-commit", Some(body.into_bytes()), Some("application/x-ndjson"))?;
        self.check(status, &rbody)?;
        let v: Value = serde_json::from_slice(&rbody).map_err(|e| CsError::Malformed(e.to_string()))?;
        let tip = v["result"]["new_sha"].as_str().unwrap_or_default().to_string();
        if tip.is_empty() || !v["result"]["success"].as_bool().unwrap_or(false) {
            return Err(CsError::Malformed(format!("restore result not ok: {v}")));
        }
        Ok(tip)
    }

    /// Commit history of a ref, newest first (deploy history on `live`).
    pub fn list_commits(&self, git_ref: &str, limit: u64) -> Result<Vec<CommitInfo>, CsError> {
        let (status, body) = self.req("GET", &format!("/commits?ref={}&limit={}", q(git_ref), limit), None, None)?;
        self.check(status, &body)?;
        let v: Value = serde_json::from_slice(&body).map_err(|e| CsError::Malformed(e.to_string()))?;
        let mut out = Vec::new();
        for c in v["commits"].as_array().cloned().unwrap_or_default() {
            out.push(CommitInfo {
                sha: c["sha"].as_str().unwrap_or_default().to_string(),
                message: c["message"].as_str().unwrap_or_default().to_string(),
                author: c["author_name"].as_str().unwrap_or_default().to_string(),
                date: c["date"].as_str().unwrap_or_default().to_string(),
            });
        }
        Ok(out)
    }
}

fn json_f(path: &str, op: &str, id: &str) -> Value {
    serde_json::json!({ "path": path, "operation": op, "content_id": id, "mode": "100644" })
}

fn line(id: &str, data_b64: &str, eof: bool) -> String {
    let l = serde_json::json!({ "blob_chunk": { "content_id": id, "data": data_b64, "eof": eof } });
    format!("{l}\n")
}

fn q(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;
    use crate::mockcs::MockServer;

    fn cs_for(mock: &MockServer, repo: &str) -> CodeStorage {
        let host = crate::api::Client::new(&mock.url, auth::Identity::from_secret([7u8; 32]));
        CodeStorage::connect(&host, repo, None).expect("connect")
    }

    fn author() -> Author {
        Author::writer("deadbeef")
    }

    fn upsert(path: &str, bytes: &[u8]) -> Change {
        Change::Upsert { path: path.into(), bytes: bytes.to_vec() }
    }

    /// The idempotency triple on the raw commit path, per the engineering
    /// style: success, replay-same-parent, conflicting-parent.
    #[test]
    fn commit_triple_success_replay_conflict() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("base.txt", b"base")]);
        let cs = cs_for(&mock, "t");
        let head0 = cs.branch_head(MAIN).unwrap().unwrap();

        // 1. success: expected parent matches -> new tip, old content kept
        let tip1 = cs.commit(Some(&head0), "one", &author(), &[upsert("a.txt", b"A")]).unwrap();
        assert_ne!(tip1, head0);
        assert_eq!(cs.read_file("base.txt", MAIN).unwrap(), b"base");

        // 2. replay-same-parent: retrying with the SAME expected parent
        //    after our own commit moved the tip is a 409 — the server saw
        //    a different current tip (our own commit). A duplicate commit
        //    is impossible; the caller converges by re-diffing.
        let err = cs.commit(Some(&head0), "one-again", &author(), &[upsert("a.txt", b"A")]).unwrap_err();
        assert!(matches!(err, CsError::CasRejected { .. }), "got: {err}");
        assert_eq!(cs.branch_head(MAIN).unwrap().unwrap(), tip1, "replay committed nothing");

        // 3. conflicting-parent: a competitor's tip makes our stale
        //    expected parent wrong — same rejection, distinct cause
        let tip2 = cs.commit(Some(&tip1), "competitor", &author(), &[upsert("b.txt", b"B")]).unwrap();
        let err = cs.commit(Some(&tip1), "ours", &author(), &[upsert("a.txt", b"A2")]).unwrap_err();
        assert!(matches!(err, CsError::CasRejected { .. }), "got: {err}");
        assert_eq!(cs.branch_head(MAIN).unwrap().unwrap(), tip2);

        // 4. landing on the fresh tip works (the bounded-retry behavior)
        let tip3 = cs.commit(Some(&tip2), "ours", &author(), &[upsert("a.txt", b"A2")]).unwrap();
        assert_eq!(cs.read_file("a.txt", MAIN).unwrap(), b"A2");
        assert_eq!(cs.read_file("b.txt", MAIN).unwrap(), b"B");
        assert_ne!(tip3, tip2);
    }

    /// Goal: a commit pack that landed but lost its answer is not sent
    /// again. Method: the fake applies the next pack and drops its answer;
    /// the client reports an unknown outcome after exactly one pack, and
    /// the commit is on the branch.
    #[test]
    fn a_commit_whose_answer_is_lost_is_not_resent() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("base.txt", b"base")]);
        let cs = cs_for(&mock, "t");
        let head0 = cs.branch_head(MAIN).unwrap().unwrap();
        mock.drop_commit_answers("t", 1);
        let err = cs.commit(Some(&head0), "one", &author(), &[upsert("a.txt", b"A")]).unwrap_err();
        assert!(matches!(err, CsError::OutcomeUnknown(_)), "got: {err}");
        assert_eq!(mock.commit_pack_count(), 1, "sent once");
        assert_ne!(cs.branch_head(MAIN).unwrap().unwrap(), head0, "and it landed");
        assert_eq!(cs.read_file("a.txt", MAIN).unwrap(), b"A");
    }

    #[test]
    fn commit_deletes_and_oversized_chunking() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("small.txt", b"s")]);
        let cs = cs_for(&mock, "t");
        let head = cs.branch_head(MAIN).unwrap().unwrap();
        // a body bigger than one 4 MiB chunk exercises the chunker
        let big = vec![7u8; CHUNK_MAX + 1024];
        let tip = cs
            .commit(Some(&head), "big", &author(), &[upsert("huge.bin", &big), Change::Delete { path: "small.txt".into() }])
            .unwrap();
        assert!(!tip.is_empty());
        assert_eq!(cs.read_file("huge.bin", MAIN).unwrap().len(), CHUNK_MAX + 1024);
        assert!(cs.read_file("small.txt", MAIN).is_err(), "deleted remotely");
    }

    #[test]
    fn list_files_paginates() {
        let mock = MockServer::start(); // page_size = 2 in the mock
        let files: Vec<(String, Vec<u8>)> = (0..5).map(|i| (format!("f{i}.txt"), vec![7u8; i + 1])).collect();
        let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        mock.seed_repo("t", &refs);
        let cs = cs_for(&mock, "t");
        let listed = cs.list_files(MAIN).unwrap();
        let mut paths: Vec<&str> = listed.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["f0.txt", "f1.txt", "f2.txt", "f3.txt", "f4.txt"]);
        assert_eq!(listed.iter().find(|f| f.path == "f3.txt").unwrap().size, 4);
    }

    #[test]
    fn branch_head_missing_is_none() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"1")]);
        let cs = cs_for(&mock, "t");
        assert!(cs.branch_head(MAIN).unwrap().is_some());
        assert!(cs.branch_head("nope").unwrap().is_none());
    }

    #[test]
    fn first_commit_creates_branch_without_expected() {
        let mock = MockServer::start();
        let cs = cs_for(&mock, "fresh"); // a repo with no branches at all
        assert!(cs.branch_head(MAIN).unwrap().is_none());
        let tip = cs.commit(None, "root", &author(), &[upsert("root.txt", b"r")]).unwrap();
        assert_eq!(cs.branch_head(MAIN).unwrap().unwrap(), tip);
        assert_eq!(cs.read_file("root.txt", MAIN).unwrap(), b"r");
    }

    #[test]
    fn promote_and_rollback_flow() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("v1.txt", b"1")]);
        let cs = cs_for(&mock, "t");
        // first deploy: create live at main's tip
        let tip1 = cs.branch_head(MAIN).unwrap().unwrap();
        let live1 = cs.create_branch(&tip1, LIVE, false).unwrap();
        assert_eq!(live1, tip1);
        // second deploy: main moves, live fast-forwards via merge (CAS-guarded)
        let tip2 = cs.commit(Some(&tip1), "v2", &author(), &[upsert("v2.txt", b"2")]).unwrap();
        let live2 = cs.promote_live(&live1, "deploy v2", &author()).unwrap();
        assert_eq!(live2, tip2);
        // preview: ephemeral ref at main's tip
        let p = cs.create_branch(&tip2, "preview/abc123", true).unwrap();
        assert_eq!(p, tip2);
        // rollback: restore live to tip1 (an ancestor) with CAS on live
        let restored = cs.restore_live(&tip1, &live2, "rollback", &author()).unwrap();
        assert_ne!(restored, live2);
        assert_eq!(cs.branch_head(LIVE).unwrap().unwrap(), restored);
        // rollback to something not in history -> 412
        let fake = "f".repeat(40);
        let err = cs.restore_live(&fake, &restored, "bad", &author()).unwrap_err();
        assert!(matches!(err, CsError::Precondition(_)), "got: {err}");
        // deploy history: live's commits, newest first
        let history = cs.list_commits(LIVE, 10).unwrap();
        assert!(!history.is_empty());
        assert_eq!(history[0].sha, restored);
    }

    #[test]
    fn promote_with_stale_expected_live_is_cas_rejected() {
        // deploy's bounded retry loop keys off this rejection: live moved
        // since the SHA we pinned -> 409 target_moved, nothing applied
        let mock = MockServer::start();
        mock.seed_repo("t", &[("v1.txt", b"1")]);
        let cs = cs_for(&mock, "t");
        let tip1 = cs.branch_head(MAIN).unwrap().unwrap();
        cs.create_branch(&tip1, LIVE, false).unwrap();
        let tip2 = cs.commit(Some(&tip1), "v2", &author(), &[upsert("v2.txt", b"2")]).unwrap();
        // someone already moved live to tip2; we pinned tip1 (stale)
        cs.promote_live(&tip1, "first deploy", &author()).unwrap();
        let err = cs.promote_live(&tip1, "second deploy", &author()).unwrap_err();
        assert!(matches!(err, CsError::CasRejected { .. }), "got: {err}");
        assert_eq!(cs.branch_head(LIVE).unwrap().unwrap(), tip2);
    }

    #[test]
    fn storage_token_override_wins() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"1")]);
        let host = crate::api::Client::new(&mock.url, auth::Identity::from_secret([7u8; 32]));
        let cs = CodeStorage::connect(&host, "t", Some(&mock.url)).unwrap();
        assert!(cs.branch_head(MAIN).unwrap().is_some());
    }
}
