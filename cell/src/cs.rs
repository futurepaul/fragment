//! The cell's code.storage client: the calls the file plane makes, each
//! with a deadline, each authenticated with a short-lived org JWT that
//! names exactly the repo it touches. The node's `KEYS` signs the JWTs:
//! the org key never reaches the cell (keys.rs).

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use fragment_core::codestorage::{self as core_cs, TreeEntry};
use fragment_proto::{limits, ErrorCode, StorageToken};
use futures_util::future::{select, Either};
use serde_json::Value;
use worker::{AbortController, Delay, Env, Fetch, Headers, Method, Request, RequestInit, Response};

use crate::config::CodeStorageConfig;
use crate::error::{CellError, CellResult};
use crate::{js, keys};

/// A JSON call's deadline, and a streamed read's deadline for headers.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Listing pages per tree read (1000 entries each).
const TREE_PAGES_MAX: usize = 500;
/// The org's repo list: 100 a page (the service's cap), 10 000 repos at most.
const REPO_LIST_PAGE: usize = 100;
const REPO_LIST_PAGES_MAX: usize = 100;
const RUNTIME_SUB: &str = "fragment-runtime";
/// How long the cell's own tokens live, and how long before expiry a cached one is replaced.
const RUNTIME_TOKEN_TTL_S: i64 = 300;
const RUNTIME_TOKEN_MARGIN_S: i64 = 60;

thread_local! {
    /// The cell's own tokens by (repo, scopes), per isolate.
    static TOKENS: RefCell<HashMap<(String, String), (String, i64)>> = RefCell::new(HashMap::new());
}

pub struct Cs<'a> {
    cfg: &'a CodeStorageConfig,
    env: &'a Env,
}

pub struct FileHead {
    pub size: u64,
    pub blob_sha: String,
    pub last_commit_sha: String,
}

fn upstream(what: &str, status: u16, body: &[u8]) -> CellError {
    let detail: String = String::from_utf8_lossy(body).chars().take(300).collect();
    CellError::new(ErrorCode::UpstreamFailed, format!("code.storage {what}: {status} {detail}"))
}

fn encode_q(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn seg(repo: &str) -> String {
    url::form_urlencoded::byte_serialize(repo.as_bytes()).collect::<String>().replace('+', "%20")
}

/// celld rejects a fetch its egress check refused with this prefix
/// (`crates/celld/egress.rs`, `CELLD_EGRESS_PUBLIC_ONLY`): its contract
/// for "this request can never pass".
const EGRESS_REFUSED_PREFIX: &str = "egress refused:";

/// Why a fetch has no response, decided once, here.
#[derive(Debug)]
pub enum FetchError {
    /// The node refused the address: no retry passes.
    Refused(String),
    /// The network failed or the deadline passed: a retry may pass.
    Failed(String),
}

impl From<FetchError> for CellError {
    fn from(e: FetchError) -> CellError {
        match e {
            FetchError::Refused(m) => CellError::new(ErrorCode::Forbidden, m),
            FetchError::Failed(m) => CellError::new(ErrorCode::UpstreamFailed, m),
        }
    }
}

/// A fetch with a deadline. The loser of the race is dropped: a finished
/// fetch clears its timer, and a timed-out fetch is aborted.
pub async fn fetch(req: Request, deadline: Duration) -> Result<Response, FetchError> {
    let ctrl = AbortController::default();
    let signal = ctrl.signal();
    let fetch = Fetch::Request(req);
    let send = fetch.send_with_signal(&signal);
    let timer = Delay::from(deadline);
    futures_util::pin_mut!(send, timer);
    match select(send, timer).await {
        Either::Left((Ok(resp), _)) => Ok(resp),
        Either::Left((Err(e), _)) => {
            // a Worker's fetch rejects with an Error whose message is all it
            // says; celld documents the prefix as the refusal's mark
            let refused = match &e {
                worker::Error::UnknownJsError { message, .. } | worker::Error::JsError(message) => message.starts_with(EGRESS_REFUSED_PREFIX),
                _ => false,
            };
            match refused {
                true => Err(FetchError::Refused(e.to_string())),
                false => Err(FetchError::Failed(format!("fetch failed: {e}"))),
            }
        }
        Either::Right(_) => {
            ctrl.abort();
            Err(FetchError::Failed(format!("no answer within {deadline:?}")))
        }
    }
}

impl<'a> Cs<'a> {
    pub fn new(cfg: &'a CodeStorageConfig, env: &'a Env) -> Cs<'a> {
        Cs { cfg, env }
    }

    async fn runtime_token(&self, repo: &str, scopes: &[&str]) -> CellResult<String> {
        let now = js::now_ms() / 1000;
        let cache_key = (repo.to_string(), scopes.join(","));
        if let Some(t) = TOKENS.with(|t| t.borrow().get(&cache_key).filter(|(_, exp)| exp - RUNTIME_TOKEN_MARGIN_S > now).map(|(t, _)| t.clone())) {
            return Ok(t);
        }
        let (token, expires_ms) = keys::codestorage_token(self.env, repo, RUNTIME_SUB, scopes, RUNTIME_TOKEN_TTL_S).await?;
        TOKENS.with(|t| t.borrow_mut().insert(cache_key, (token.clone(), expires_ms / 1000)));
        Ok(token)
    }

    async fn request(&self, method: Method, path: &str, repo: &str, scopes: &[&str], body: Option<Value>) -> CellResult<Request> {
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {}", self.runtime_token(repo, scopes).await?))?;
        let mut init = RequestInit::new();
        init.with_method(method);
        if let Some(b) = body {
            headers.set("content-type", "application/json")?;
            init.with_body(Some(b.to_string().into()));
        }
        init.with_headers(headers);
        Ok(Request::new_with_init(&format!("{}{path}", self.cfg.api), &init)?)
    }

    /// (status, body) of a call; the body is read within the same deadline.
    async fn call(&self, method: Method, path: &str, repo: &str, scopes: &[&str], body: Option<Value>) -> CellResult<(u16, Vec<u8>)> {
        let req = self.request(method, path, repo, scopes, body).await?;
        let mut resp = fetch(req, CALL_TIMEOUT).await?;
        let status = resp.status_code();
        let bytes = resp.bytes().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("reading {path}: {e}")))?;
        Ok((status, bytes))
    }

    async fn json(&self, method: Method, path: &str, repo: &str, scopes: &[&str], body: Option<Value>) -> CellResult<(u16, Value)> {
        let (status, bytes) = self.call(method, path, repo, scopes, body).await?;
        if !(200..300).contains(&status) {
            return Ok((status, Value::String(String::from_utf8_lossy(&bytes).chars().take(300).collect())));
        }
        let v = serde_json::from_slice(&bytes).map_err(|e| upstream(path, status, format!("not JSON: {e}").as_bytes()))?;
        Ok((status, v))
    }

    /// Creates the fragment's repo, or finds it when it exists (a fragment
    /// deleted and created again keeps its repo). Answers the url-form id.
    pub async fn ensure_repo(&self, name: &str) -> CellResult<String> {
        let org = self.cfg.org.clone();
        // the service names a new repo by the token's repo claim (the body's
        // repo_name must equal it); signing with the org's claim names the
        // repo after the org
        let body = serde_json::json!({ "repo_name": name, "default_branch": "main" });
        let (status, v) = self.json(Method::Post, "/api/repos", name, &["repo:write"], Some(body)).await?;
        let repo_id = match status {
            200 | 201 => core_cs::created_repo_id(&v).ok_or_else(|| upstream("create repo", status, v.to_string().as_bytes()))?,
            // the org lists repos a page at a time (newest first), and a
            // name is not searchable (`q` matches the opaque url form)
            409 => {
                let mut cursor: Option<String> = None;
                for _ in 0..REPO_LIST_PAGES_MAX {
                    let path = match &cursor {
                        None => format!("/api/repos?limit={REPO_LIST_PAGE}"),
                        Some(c) => format!("/api/repos?limit={REPO_LIST_PAGE}&cursor={}", encode_q(c)),
                    };
                    let (status, list) = self.json(Method::Get, &path, &org, &["org:read"], None).await?;
                    if status != 200 {
                        return Err(upstream("list repos", status, list.to_string().as_bytes()));
                    }
                    if let Some(url) = core_cs::listed_repo_url(&list, name) {
                        return Ok(url);
                    }
                    match core_cs::next_repos_cursor(&list) {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                return Err(upstream("list repos", 200, b"the existing repo is not listed"));
            }
            _ => return Err(upstream("create repo", status, v.to_string().as_bytes())),
        };
        let (status, v) = self.json(Method::Get, &format!("/api/repo-urls/{}", seg(&repo_id)), &repo_id, &["org:read"], None).await?;
        if status != 200 {
            return Err(upstream("repo url", status, v.to_string().as_bytes()));
        }
        core_cs::repo_url(&v).ok_or_else(|| upstream("repo url", status, b"no url in the answer"))
    }

    /// A branch's head, or `None` when the branch does not exist.
    pub async fn branch_head(&self, repo: &str, branch: &str) -> CellResult<Option<String>> {
        let path = format!("/api/repos/{}/branch?name={}", seg(repo), encode_q(branch));
        match self.json(Method::Get, &path, repo, &["git:read"], None).await? {
            (200, v) => core_cs::branch_head(&v).map(Some).ok_or_else(|| upstream("branch", 200, v.to_string().as_bytes())),
            (404, _) => Ok(None),
            (status, v) => Err(upstream("branch", status, v.to_string().as_bytes())),
        }
    }

    /// Every blob at a commit (metadata only).
    pub async fn tree(&self, repo: &str, sha: &str) -> CellResult<Vec<TreeEntry>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..TREE_PAGES_MAX {
            let mut path = format!("/api/repos/{}/files/metadata?ref={}&limit=1000", seg(repo), encode_q(sha));
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={}", encode_q(c)));
            }
            let (status, v) = self.json(Method::Get, &path, repo, &["git:read"], None).await?;
            if status != 200 {
                return Err(upstream("tree", status, v.to_string().as_bytes()));
            }
            let (entries, next) = core_cs::tree_page(&v).map_err(|e| upstream("tree", status, e.as_bytes()))?;
            out.extend(entries);
            match next {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
        Err(CellError::new(ErrorCode::UpstreamFailed, format!("the tree at {sha} has more than {TREE_PAGES_MAX} pages")))
    }

    fn file_path(repo: &str, sha: &str, path: &str) -> String {
        format!("/api/repos/{}/file?path={}&ref={}", seg(repo), encode_q(path), encode_q(sha))
    }

    /// A file's bytes, refusing anything over `max`; `None` when absent.
    pub async fn read(&self, repo: &str, sha: &str, path: &str, max: usize) -> CellResult<Option<Vec<u8>>> {
        let (status, bytes) = self.call(Method::Get, &Cs::file_path(repo, sha, path), repo, &["git:read"], None).await?;
        match status {
            200 if bytes.len() > max => Err(CellError::too_large(path, bytes.len(), max)),
            200 => Ok(Some(bytes)),
            404 => Ok(None),
            _ => Err(upstream("read file", status, &bytes)),
        }
    }

    /// A file's bytes as an upstream response to stream through.
    pub async fn stream(&self, repo: &str, sha: &str, path: &str) -> CellResult<Response> {
        let req = self.request(Method::Get, &Cs::file_path(repo, sha, path), repo, &["git:read"], None).await?;
        let mut resp = fetch(req, CALL_TIMEOUT).await?;
        match resp.status_code() {
            200 => Ok(resp),
            404 => Err(CellError::new(ErrorCode::NotFound, format!("no file {path}"))),
            s => Err(upstream("stream file", s, &resp.bytes().await.unwrap_or_default())),
        }
    }

    /// A file's git blob identity; `None` when absent.
    pub async fn head(&self, repo: &str, sha: &str, path: &str) -> CellResult<Option<FileHead>> {
        let req = self.request(Method::Head, &Cs::file_path(repo, sha, path), repo, &["git:read"], None).await?;
        let resp = fetch(req, CALL_TIMEOUT).await?;
        let h = |k: &str| resp.headers().get(k).ok().flatten().unwrap_or_default();
        match resp.status_code() {
            200 => Ok(Some(FileHead {
                size: h("content-length").parse().unwrap_or(0),
                blob_sha: h("x-blob-sha"),
                last_commit_sha: h("x-last-commit-sha"),
            })),
            404 => Ok(None),
            s => Err(upstream("stat file", s, b"")),
        }
    }

    /// Commits an NDJSON pack (`fragment_core::codestorage::commit_pack`).
    /// Answers the new head, or `None` when the branch moved from the head
    /// the pack expected (409: read the head again and rebuild).
    pub async fn commit(&self, repo: &str, pack: String) -> CellResult<Option<String>> {
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {}", self.runtime_token(repo, &["git:write"]).await?))?;
        headers.set("content-type", "application/x-ndjson")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(pack.into()));
        let req = Request::new_with_init(&format!("{}/api/repos/{}/commit-pack", self.cfg.api, seg(repo)), &init)?;
        let mut resp = fetch(req, CALL_TIMEOUT).await?;
        let status = resp.status_code();
        let bytes = resp.bytes().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("reading commit-pack: {e}")))?;
        match status {
            409 => Ok(None),
            200 | 201 => {
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| upstream("commit-pack", status, format!("not JSON: {e}").as_bytes()))?;
                core_cs::committed(&v).map(Some).ok_or_else(|| upstream("commit-pack", status, &bytes))
            }
            _ => Err(upstream("commit-pack", status, &bytes)),
        }
    }

    /// Makes `branch` at `base_ref` (a first `live`): its sha, or `None`
    /// when the branch exists already (made meanwhile: read it again).
    pub async fn create_branch(&self, repo: &str, base_ref: &str, branch: &str) -> CellResult<Option<String>> {
        let body = serde_json::json!({ "base_ref": base_ref, "target_branch": branch, "target_is_ephemeral": false });
        match self.json(Method::Post, &format!("/api/repos/{}/branches/create", seg(repo)), repo, &["git:write"], Some(body)).await? {
            (200 | 201, v) => v["commit_sha"].as_str().filter(|s| !s.is_empty()).map(|s| Some(s.to_string())).ok_or_else(|| upstream("create branch", 200, v.to_string().as_bytes())),
            (409, _) => Ok(None),
            (status, v) => Err(upstream("create branch", status, v.to_string().as_bytes())),
        }
    }

    /// Moves `live` to `main`'s tip (a fast-forward when it can, else a
    /// merge commit), guarded by `expected_live`: the new tip, or `None`
    /// when `live` moved first (read it again).
    pub async fn promote_live(&self, repo: &str, expected_live: &str, message: &str, author: (&str, &str)) -> CellResult<Option<String>> {
        let body = serde_json::json!({
            "target_branch": "live",
            "source_ref": "main",
            "strategy": "ff_prefer",
            "expected_target_sha": expected_live,
            "commit_message": message,
            "author": { "name": author.0, "email": author.1 },
        });
        match self.json(Method::Post, &format!("/api/repos/{}/merge", seg(repo)), repo, &["git:write"], Some(body)).await? {
            (200 | 201, v) => v["target"]["new_sha"].as_str().filter(|s| !s.is_empty()).map(|s| Some(s.to_string())).ok_or_else(|| upstream("merge", 200, v.to_string().as_bytes())),
            (409, _) => Ok(None),
            (status, v) => Err(upstream("merge", status, v.to_string().as_bytes())),
        }
    }

    /// A token for an editor's own client: this repo only, git read and
    /// write, fifteen minutes. The claims are checked after signing.
    pub async fn storage_token(&self, repo: &str, principal: &str) -> CellResult<StorageToken> {
        let scopes = ["git:read", "git:write"];
        let sub = format!("editor:{principal}");
        let (token, expires_ms) = keys::codestorage_token(self.env, repo, &sub, &scopes, limits::STORAGE_TOKEN_TTL_S).await?;
        let exp = expires_ms / 1000;
        let claims = token.split('.').nth(1).and_then(|p| {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok()
        });
        let claims: Value = claims.and_then(|c| serde_json::from_slice(&c).ok()).unwrap_or_default();
        if claims["repo"] != repo || claims["scopes"] != serde_json::json!(scopes) || claims["exp"].as_i64().unwrap_or(0) - claims["iat"].as_i64().unwrap_or(0) > limits::STORAGE_TOKEN_TTL_S {
            return Err(CellError::host("a minted storage token failed its claim check"));
        }
        Ok(StorageToken { token, repo: repo.to_string(), api: self.cfg.api.clone(), expires_at: exp * 1000 })
    }
}
