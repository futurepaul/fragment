// Typed-error HTTP client for the code.storage API (their OpenAPI spec is
// the contract: https://code.storage/docs/openapi.json). Commit-without-a-
// clone: no local .git, no git2 — files go up as NDJSON commit packs with
// expected-parent CAS, come back as listings and raw bytes.
//
// Per the engineering style: no anyhow in here — CsError is matchable at
// the boundary, and every retry/loop has an explicit bound. What goes over
// the wire and what comes back is fragment_core::codestorage's, the same
// encoder and decoders the cell uses; this file is the transport.
use crate::api::{encode_q, timeout_for, Client as HostClient, CodedError, Replay, CONNECT_TIMEOUT, REQUEST_ATTEMPTS};
use fragment_core::codestorage::{self as core_cs, FileChange, TreeEntry};
use fragment_proto::StorageToken;
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
/// files/metadata page size (spec max 1000).
const LIST_PAGE: u64 = 1000;
/// A held token is minted again once it has less than this left (by this
/// machine's clock), so a pass that starts with it finishes with it: the
/// platform's last 15 minutes, a pass takes seconds, a large upload
/// minutes.
pub const TOKEN_REMINT_BEFORE_EXPIRY_MS: i64 = 120_000;
/// A held token is used for at most this long after it was minted. The
/// platform checks the editor's role only when it mints, so this is how
/// long a watcher whose editor was removed can still push; a burst of
/// saves inside it shares one mint.
pub const TOKEN_REUSE_MAX_MS: i64 = 60_000;

#[derive(Debug)]
pub enum CsError {
    /// the fragment host would not mint a token: its refusal, coded
    Host(CodedError),
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
            CsError::Host(e) => write!(f, "storage token: {e}"),
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

/// The fragment host's failure, coded when it is one (a refusal, or a
/// request that never got an answer).
fn host_error(e: anyhow::Error) -> CsError {
    match e.downcast::<CodedError>() {
        Ok(coded) => CsError::Host(coded),
        Err(e) => CsError::Transport(format!("storage token: {e:#}")),
    }
}

/// A token the fragment host minted, and where it works.
struct Minted {
    server: String,
    repo: String,
    token: String,
    expires_at_ms: i64,
}

/// The fragment host mints a short-lived, repo-scoped code.storage JWT
/// (`StorageToken`); the CLI never sees the org key. Its `api` is the
/// spec's server URL (endpoints append /api/repos/...).
fn mint_from_host(host: &HostClient, name: &str, override_url: Option<&str>) -> Result<Minted, CsError> {
    let resp = host.get(&format!("/api/f/{name}/storage-token")).map_err(host_error)?;
    let minted: StorageToken = host.call_as(resp).map_err(host_error)?;
    let server = override_url.unwrap_or(&minted.api).trim_end_matches('/').to_string();
    if server.is_empty() || minted.repo.is_empty() || minted.token.is_empty() {
        return Err(CsError::Malformed(format!("a storage token needs a token, a repo, and an api (repo {:?}, api {server:?})", minted.repo)));
    }
    Ok(Minted { server, repo: minted.repo, token: minted.token, expires_at_ms: minted.expires_at })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
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
    /// the token's `expiresAt` (ms), as the host said
    expires_at_ms: i64,
    /// when this machine minted it (ms, by its clock)
    minted_at_ms: i64,
    http: reqwest::blocking::Client,
}

impl CodeStorage {
    /// Mint a scoped token from the fragment host and build the client.
    /// `override_url` (FRAGMENT_CODESTORAGE_URL / config) replaces the
    /// server the host reports — the backend-swap knob.
    pub fn connect(host: &HostClient, name: &str, override_url: Option<&str>) -> Result<CodeStorage, CsError> {
        let Minted { server, repo, token, expires_at_ms } = mint_from_host(host, name, override_url)?;
        Ok(CodeStorage {
            server,
            repo,
            token,
            expires_at_ms,
            minted_at_ms: now_ms(),
            // every request sets its own total timeout (`timeout_for`)
            http: reqwest::blocking::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .map_err(|e| CsError::Transport(e.to_string()))?,
        })
    }

    /// Whether the token has `TOKEN_REMINT_BEFORE_EXPIRY_MS` left and was
    /// minted less than `TOKEN_REUSE_MAX_MS` ago.
    fn fresh(&self) -> bool {
        let now = now_ms();
        now + TOKEN_REMINT_BEFORE_EXPIRY_MS < self.expires_at_ms && now - self.minted_at_ms < TOKEN_REUSE_MAX_MS
    }

    /// A new token for the same client: its connections stay open.
    fn renew(&mut self, minted: Minted) {
        let Minted { server, repo, token, expires_at_ms } = minted;
        (self.server, self.repo, self.token, self.expires_at_ms, self.minted_at_ms) = (server, repo, token, expires_at_ms, now_ms());
    }

    /// url-form repo identity this client is scoped to — the world a sync
    /// journal binds itself to
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// repo name URL-encoded as one path segment (spec: `a/b` -> `a%2Fb`)
    fn repo_seg(&self) -> String {
        encode_q(&self.repo).replace('/', "%2F")
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
                // a request that could not be built never left
                Err(e) if e.is_builder() => return Err(CsError::Malformed(format!("building {method} {path}: {e}"))),
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

    /// A 2xx answer's JSON (after `check`).
    fn json(body: &[u8]) -> Result<Value, CsError> {
        serde_json::from_slice(body).map_err(|e| CsError::Malformed(e.to_string()))
    }

    /// Branch head SHA; Ok(None) when the branch does not exist (404).
    pub fn branch_head(&self, branch: &str) -> Result<Option<String>, CsError> {
        let (status, body) = self.req("GET", &format!("/branch?name={}", encode_q(branch)), None, None)?;
        if status == 404 {
            return Ok(None);
        }
        self.check(status, &body)?;
        let v = Self::json(&body)?;
        let sha = core_cs::branch_head(&v).ok_or_else(|| CsError::Malformed(format!("a branch answer without a head sha: {v}")))?;
        Ok(Some(sha))
    }

    /// Full recursive file listing at a ref, cursor-paginated (bounded).
    /// Every entry names its path, size, and last commit, or the listing
    /// fails: a missing commit read as "" made every file look changed.
    pub fn list_files(&self, git_ref: &str) -> Result<Vec<TreeEntry>, CsError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut path = format!("/files/metadata?ref={}&limit={LIST_PAGE}", encode_q(git_ref));
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={}", encode_q(c)));
            }
            let (status, body) = self.req("GET", &path, None, None)?;
            self.check(status, &body)?;
            let (entries, next) = core_cs::tree_page(&Self::json(&body)?).map_err(CsError::Malformed)?;
            out.extend(entries);
            match next {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
        Err(CsError::Malformed(format!("listing exceeded {MAX_LIST_PAGES} pages")))
    }

    /// Raw file bytes at a ref.
    pub fn read_file(&self, path: &str, git_ref: &str) -> Result<Vec<u8>, CsError> {
        let (status, body) = self.req("GET", &format!("/file?path={}&ref={}", encode_q(path), encode_q(git_ref)), None, None)?;
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
        let files: Vec<FileChange> = changes
            .iter()
            .map(|change| match change {
                Change::Upsert { path, bytes } => FileChange::Upsert { path, bytes },
                Change::Delete { path } => FileChange::Delete { path },
            })
            .collect();
        let pack = core_cs::commit_pack(MAIN, expected, message, (&author.name, &author.email), &files);
        let (status, rbody) = self.req("POST", "/commit-pack", Some(pack.into_bytes()), Some("application/x-ndjson"))?;
        self.check(status, &rbody)?;
        let v = Self::json(&rbody)?;
        core_cs::committed(&v).ok_or_else(|| CsError::Malformed(format!("commit-pack result not ok: {v}")))
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
        let v = Self::json(&rbody)?;
        sha_at(&v["commit_sha"]).ok_or_else(|| CsError::Malformed(format!("branches/create without a commit sha: {v}")))
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
        let v = Self::json(&rbody)?;
        sha_at(&v["target"]["new_sha"]).ok_or_else(|| CsError::Malformed(format!("merge without a target sha: {v}")))
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
        // a restore answers as a commit pack does
        let v = Self::json(&rbody)?;
        core_cs::committed(&v).ok_or_else(|| CsError::Malformed(format!("restore result not ok: {v}")))
    }

    /// Commit history of a ref, newest first (deploy history on `live`).
    pub fn list_commits(&self, git_ref: &str, limit: u64) -> Result<Vec<CommitInfo>, CsError> {
        let (status, body) = self.req("GET", &format!("/commits?ref={}&limit={}", encode_q(git_ref), limit), None, None)?;
        self.check(status, &body)?;
        let v = Self::json(&body)?;
        let commits = v["commits"].as_array().ok_or_else(|| CsError::Malformed(format!("a commit list without commits: {v}")))?;
        let mut out = Vec::with_capacity(commits.len());
        for c in commits {
            // a deploy is named by its sha (rollback's target): one without is no deploy
            let sha = sha_at(&c["sha"]).ok_or_else(|| CsError::Malformed(format!("a commit without a sha: {c}")))?;
            let text = |k: &str| c[k].as_str().unwrap_or_default().to_string();
            out.push(CommitInfo { sha, message: text("message"), author: text("author_name"), date: text("date") });
        }
        Ok(out)
    }
}

/// One fragment's code.storage client, held for a watcher's life: one HTTP
/// client, so its connections stay open across passes, and one token,
/// minted again a minute after it was (`TOKEN_REUSE_MAX_MS`), when it
/// nears its `expiresAt`, or after code.storage refused it. (A one-shot
/// command connects once: `CodeStorage::connect`.)
pub struct Held {
    name: String,
    override_url: Option<String>,
    client: Option<CodeStorage>,
}

impl Held {
    pub fn new(name: &str, override_url: Option<&str>) -> Held {
        Held { name: name.to_string(), override_url: override_url.map(str::to_string), client: None }
    }

    /// The client, with a token that has time left: the first call
    /// connects, and a later one mints again only when it must.
    pub fn get(&mut self, host: &HostClient) -> Result<&CodeStorage, CsError> {
        if !self.client.as_ref().is_some_and(CodeStorage::fresh) {
            match self.client.as_mut() {
                Some(client) => client.renew(mint_from_host(host, &self.name, self.override_url.as_deref())?),
                None => self.client = Some(CodeStorage::connect(host, &self.name, self.override_url.as_deref())?),
            }
        }
        // a token minted just now is used whatever it has left: a host whose
        // tokens are shorter than the margin gets a mint a pass, as before
        Ok(self.client.as_ref().expect("connected above"))
    }

    /// code.storage refused the token (its clock and ours disagree, or it
    /// was revoked): the next `get` mints another.
    pub fn refused(&mut self) {
        if let Some(client) = self.client.as_mut() {
            client.expires_at_ms = 0;
        }
    }
}

/// A commit sha the service named, checked (40 lowercase hex).
fn sha_at(v: &Value) -> Option<String> {
    v.as_str().filter(|s| core_cs::is_sha(s)).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;
    use crate::mockcs::MockServer;
    use fragment_core::codestorage::CHUNK_MAX;

    fn cs_for(mock: &MockServer, repo: &str) -> CodeStorage {
        let host = crate::api::Client::new(&mock.url, auth::fixed(7));
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
        assert!(listed.iter().all(|f| fragment_core::codestorage::is_sha(&f.last_commit_sha)), "every entry names its commit");
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

    /// Goal: a held client mints once while its token lasts, and again
    /// when the token nears its expiry or code.storage refuses it.
    /// Method: count the fake's storage-token route across gets, with the
    /// platform's lifetime, after a revocation, and with a lifetime shorter
    /// than the margin.
    #[test]
    fn a_held_client_mints_again_only_near_expiry_or_when_refused() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"1")]);
        let host = crate::api::Client::new(&mock.url, auth::fixed(7));
        let mut held = Held::new("t", None);
        for _ in 0..3 {
            assert!(held.get(&host).unwrap().branch_head(MAIN).unwrap().is_some());
        }
        assert_eq!(mock.take_requests("").get("GET storage-token"), Some(&1), "one token for three passes");

        mock.revoke_tokens();
        let err = held.get(&host).unwrap().branch_head(MAIN).unwrap_err();
        assert!(matches!(err, CsError::Auth(_)), "got: {err}");
        held.refused();
        assert!(held.get(&host).unwrap().branch_head(MAIN).unwrap().is_some(), "a new token after the refusal");
        assert_eq!(mock.take_requests("").get("GET storage-token"), Some(&1));

        let short = MockServer::with_token_ttl(TOKEN_REMINT_BEFORE_EXPIRY_MS / 1000 / 2);
        short.seed_repo("t", &[("a", b"1")]);
        let host = crate::api::Client::new(&short.url, auth::fixed(7));
        let mut held = Held::new("t", None);
        assert!(held.get(&host).unwrap().branch_head(MAIN).unwrap().is_some());
        assert!(held.get(&host).unwrap().branch_head(MAIN).unwrap().is_some());
        assert_eq!(short.take_requests("").get("GET storage-token"), Some(&2), "a token inside the margin is minted again");

        // a token a minute old is minted again, though it has long left:
        // a removed editor's watcher pushes for at most that long
        let host = crate::api::Client::new(&mock.url, auth::fixed(7));
        let mut held = Held::new("t", None);
        assert!(held.get(&host).unwrap().branch_head(MAIN).unwrap().is_some());
        held.client.as_mut().expect("connected").minted_at_ms -= TOKEN_REUSE_MAX_MS;
        assert!(held.get(&host).unwrap().branch_head(MAIN).unwrap().is_some());
        assert_eq!(mock.take_requests("").get("GET storage-token"), Some(&2), "a token past its reuse is minted again");
    }

    #[test]
    fn storage_token_override_wins() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a", b"1")]);
        let host = crate::api::Client::new(&mock.url, auth::fixed(7));
        let cs = CodeStorage::connect(&host, "t", Some(&mock.url)).unwrap();
        assert!(cs.branch_head(MAIN).unwrap().is_some());
    }
}
