//! The cell's code.storage client: the calls the file plane makes, each
//! with a deadline, each authenticated with a short-lived org JWT that
//! names exactly the repo it touches.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use fragment_core::codestorage::{self as core_cs, Claims, OrgKey, TreeEntry};
use fragment_proto::{limits, ErrorCode, StorageToken};
use futures_util::future::{select, Either};
use serde_json::Value;
use worker::{AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, Response};

use crate::config::CodeStorageConfig;
use crate::error::{CellError, CellResult};
use crate::js;

/// A JSON call's deadline, and a streamed read's deadline for headers.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Listing pages per tree read (1000 entries each).
const TREE_PAGES_MAX: usize = 500;
const RUNTIME_SUB: &str = "fragment-runtime";
/// How long the cell's own tokens live, and how long before expiry a cached one is replaced.
const RUNTIME_TOKEN_TTL_S: i64 = 300;
const RUNTIME_TOKEN_MARGIN_S: i64 = 60;

thread_local! {
    /// The parsed org key, per isolate (parsing a PEM costs more than signing).
    static KEY: RefCell<Option<(String, std::rc::Rc<OrgKey>)>> = const { RefCell::new(None) };
    /// The cell's own tokens by (repo, scopes), per isolate.
    static TOKENS: RefCell<HashMap<(String, String), (String, i64)>> = RefCell::new(HashMap::new());
}

pub struct Cs<'a> {
    cfg: &'a CodeStorageConfig,
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

/// A fetch with a deadline. The loser of the race is dropped: a finished
/// fetch clears its timer, and a timed-out fetch is aborted.
pub async fn fetch(req: Request, deadline: Duration) -> CellResult<Response> {
    let ctrl = AbortController::default();
    let signal = ctrl.signal();
    let fetch = Fetch::Request(req);
    let send = fetch.send_with_signal(&signal);
    let timer = Delay::from(deadline);
    futures_util::pin_mut!(send, timer);
    match select(send, timer).await {
        Either::Left((resp, _)) => resp.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("fetch failed: {e}"))),
        Either::Right(_) => {
            ctrl.abort();
            Err(CellError::new(ErrorCode::UpstreamFailed, format!("no answer within {deadline:?}")))
        }
    }
}

impl<'a> Cs<'a> {
    pub fn new(cfg: &'a CodeStorageConfig) -> Cs<'a> {
        Cs { cfg }
    }

    fn key(&self) -> CellResult<std::rc::Rc<OrgKey>> {
        KEY.with(|k| {
            let mut k = k.borrow_mut();
            if let Some((pem, key)) = k.as_ref() {
                if *pem == self.cfg.key_pem {
                    return Ok(key.clone());
                }
            }
            let key = std::rc::Rc::new(OrgKey::from_pem(&self.cfg.key_pem).map_err(CellError::host)?);
            *k = Some((self.cfg.key_pem.clone(), key.clone()));
            Ok(key)
        })
    }

    fn runtime_token(&self, repo: &str, scopes: &[&str]) -> CellResult<String> {
        let now = js::now_ms() / 1000;
        let cache_key = (repo.to_string(), scopes.join(","));
        if let Some(t) = TOKENS.with(|t| t.borrow().get(&cache_key).filter(|(_, exp)| exp - RUNTIME_TOKEN_MARGIN_S > now).map(|(t, _)| t.clone())) {
            return Ok(t);
        }
        let exp = now + RUNTIME_TOKEN_TTL_S;
        let token = self.key()?.token(&Claims { iss: &self.cfg.org, sub: RUNTIME_SUB, repo, scopes, iat: now, exp });
        TOKENS.with(|t| t.borrow_mut().insert(cache_key, (token.clone(), exp)));
        Ok(token)
    }

    fn request(&self, method: Method, path: &str, repo: &str, scopes: &[&str], body: Option<Value>) -> CellResult<Request> {
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {}", self.runtime_token(repo, scopes)?))?;
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
        let req = self.request(method, path, repo, scopes, body)?;
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
        let (status, v) =
            self.json(Method::Post, "/api/repos", &org, &["repo:write"], Some(serde_json::json!({ "id": name, "default_branch": "main" }))).await?;
        let repo_id = match status {
            200 | 201 => core_cs::created_repo_id(&v).ok_or_else(|| upstream("create repo", status, v.to_string().as_bytes()))?,
            409 => {
                let (status, list) = self.json(Method::Get, "/api/repos", &org, &["org:read"], None).await?;
                if status != 200 {
                    return Err(upstream("list repos", status, list.to_string().as_bytes()));
                }
                return core_cs::listed_repo_url(&list, name).ok_or_else(|| upstream("list repos", status, b"the existing repo is not listed"));
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
        let req = self.request(Method::Get, &Cs::file_path(repo, sha, path), repo, &["git:read"], None)?;
        let mut resp = fetch(req, CALL_TIMEOUT).await?;
        match resp.status_code() {
            200 => Ok(resp),
            404 => Err(CellError::new(ErrorCode::NotFound, format!("no file {path}"))),
            s => Err(upstream("stream file", s, &resp.bytes().await.unwrap_or_default())),
        }
    }

    /// A file's git blob identity; `None` when absent.
    pub async fn head(&self, repo: &str, sha: &str, path: &str) -> CellResult<Option<FileHead>> {
        let req = self.request(Method::Head, &Cs::file_path(repo, sha, path), repo, &["git:read"], None)?;
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
        headers.set("authorization", &format!("Bearer {}", self.runtime_token(repo, &["git:write"])?))?;
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

    /// A token for an editor's own client: this repo only, git read and
    /// write, fifteen minutes. The claims are checked after signing.
    pub fn storage_token(&self, repo: &str, principal: &str) -> CellResult<StorageToken> {
        let now = js::now_ms() / 1000;
        let exp = now + limits::STORAGE_TOKEN_TTL_S;
        let scopes = ["git:read", "git:write"];
        let sub = format!("editor:{principal}");
        let token = self.key()?.token(&Claims { iss: &self.cfg.org, sub: &sub, repo, scopes: &scopes, iat: now, exp });
        let claims = token.split('.').nth(1).and_then(|p| {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok()
        });
        let claims: Value = claims.and_then(|c| serde_json::from_slice(&c).ok()).unwrap_or_default();
        if claims["repo"] != repo || claims["scopes"] != serde_json::json!(scopes) || claims["exp"].as_i64().unwrap_or(0) - now > limits::STORAGE_TOKEN_TTL_S {
            return Err(CellError::host("a minted storage token failed its claim check"));
        }
        Ok(StorageToken { token, repo: repo.to_string(), api: self.cfg.api.clone(), expires_at: exp * 1000 })
    }
}
