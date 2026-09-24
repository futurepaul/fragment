//! code.storage, the fragment file plane: ES256 JWTs signed with the
//! fleet's org key, and the response shapes the cell reads (the documented
//! HTTP API; the fake in `crates/fakes` serves the same shapes).

use std::collections::BTreeMap;

use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde_json::{json, Value};

/// The fleet's code.storage org key (PKCS#8 PEM, P-256).
pub struct OrgKey(SigningKey);

impl OrgKey {
    /// Accepts the PEM with real newlines or with literal `\n` (how a
    /// one-line variable carries it).
    pub fn from_pem(pem: &str) -> Result<OrgKey, String> {
        let pem = pem.replace("\\n", "\n");
        SigningKey::from_pkcs8_pem(pem.trim())
            .map(OrgKey)
            .map_err(|e| format!("the code.storage org key is not a PKCS#8 P-256 PEM: {e}"))
    }

    /// A signed JWT. code.storage requires the `repo` claim on every call,
    /// org-level ones included (where it is the org).
    pub fn token(&self, claims: &Claims<'_>) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = b64.encode(br#"{"alg":"ES256","typ":"JWT"}"#);
        let body = json!({
            "iss": claims.iss,
            "sub": claims.sub,
            "repo": claims.repo,
            "scopes": claims.scopes,
            "iat": claims.iat,
            "exp": claims.exp,
        });
        let payload = b64.encode(body.to_string());
        let signing_input = format!("{header}.{payload}");
        let sig: Signature = self.0.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", b64.encode(sig.to_bytes()))
    }
}

pub struct Claims<'a> {
    /// The org.
    pub iss: &'a str,
    /// Who the token is for: `fragment-runtime`, or `editor:<hex>` for a storage token.
    pub sub: &'a str,
    /// The repo's url-form id, or the org for org-level calls.
    pub repo: &'a str,
    pub scopes: &'a [&'a str],
    pub iat: i64,
    pub exp: i64,
}

/// The cell's own code.storage tokens, per isolate (`cell/src/cs.rs`),
/// keyed by (repo, scopes).
///
/// - Source: `KEYS`' `codestorage/token`, signed with the org key the node
///   read from its environment when it started.
/// - Invalidation: a token is served until `margin_s` before its expiry,
///   and a new one is minted after that. Each insert drops the expired
///   entries, and at `max` entries the one nearest its expiry as well, so
///   the map never holds more than `max`.
/// - Why not keyed by the signing key: the key never reaches the cell, and
///   the node takes a new one only by restarting, which starts new
///   isolates and so empty caches.
/// - A stale read: a token is at most its lifetime less the margin old
///   when served, and valid for the margin after. A token code.storage
///   refuses anyway (the key revoked on its side) fails that call as
///   `upstream_failed`, as a freshly minted one would.
pub struct TokenCache {
    entries: BTreeMap<(String, String), (String, i64)>,
    max: usize,
}

impl TokenCache {
    pub const fn new(max: usize) -> TokenCache {
        assert!(max > 0, "a cache holds at least one token");
        TokenCache { entries: BTreeMap::new(), max }
    }

    fn key(repo: &str, scopes: &[&str]) -> (String, String) {
        (repo.to_string(), scopes.join(","))
    }

    /// A token for (repo, scopes) valid for at least `margin_s` more seconds.
    pub fn get(&self, repo: &str, scopes: &[&str], now_s: i64, margin_s: i64) -> Option<&str> {
        match self.entries.get(&TokenCache::key(repo, scopes)) {
            Some((token, expires_s)) if *expires_s - margin_s > now_s => Some(token),
            _ => None,
        }
    }

    pub fn insert(&mut self, repo: &str, scopes: &[&str], token: String, expires_s: i64, now_s: i64) {
        self.entries.retain(|_, (_, exp)| *exp > now_s);
        let key = TokenCache::key(repo, scopes);
        if self.entries.len() >= self.max && !self.entries.contains_key(&key) {
            let nearest = self.entries.iter().min_by_key(|(_, (_, exp))| *exp).map(|(k, _)| k.clone()).expect("a full cache has an entry");
            self.entries.remove(&nearest);
        }
        self.entries.insert(key, (token, expires_s));
        assert!(self.entries.len() <= self.max, "the cache is bounded");
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The API base when the fleet does not override it.
pub fn default_api(org: &str) -> String {
    format!("https://api.{org}.code.storage")
}

pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `GET /api/repos/{repo}/branch?name=` → the head sha.
pub fn branch_head(v: &Value) -> Option<String> {
    let sha = v["branch"]["head_sha"].as_str()?;
    is_sha(sha).then(|| sha.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub size: u64,
    pub mode: String,
    pub last_commit_sha: String,
}

/// `GET /api/repos/{repo}/files/metadata` → one page of blobs and the next cursor.
pub fn tree_page(v: &Value) -> Result<(Vec<TreeEntry>, Option<String>), String> {
    let files = v["files"].as_array().ok_or("a tree listing has no files array")?;
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        if f["type"].as_str().is_some_and(|t| t != "blob") {
            continue;
        }
        let (Some(path), Some(size), Some(last)) = (f["path"].as_str(), f["size"].as_u64(), f["last_commit_sha"].as_str()) else {
            return Err(format!("a tree entry is missing path, size, or last_commit_sha: {f}"));
        };
        out.push(TreeEntry {
            path: path.to_string(),
            size,
            mode: f["mode"].as_str().unwrap_or("100644").to_string(),
            last_commit_sha: last.to_string(),
        });
    }
    // A page that has more but names no cursor would end the listing
    // early, and a missing file reads as a deleted one: fail instead.
    let next = match (v["has_more"].as_bool(), v["next_cursor"].as_str()) {
        (Some(true), Some(c)) if !c.is_empty() => Some(c.to_string()),
        (Some(true), _) => return Err(format!("a tree listing has more pages but names no cursor: {}", v["next_cursor"])),
        _ => None,
    };
    Ok((out, next))
}

/// `POST /api/repos` → the new repo's id.
pub fn created_repo_id(v: &Value) -> Option<String> {
    v["repo_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// `GET /api/repo-urls/{id}` → the url-form identity every repo call uses.
pub fn repo_url(v: &Value) -> Option<String> {
    v["url"].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// `GET /api/repos` → the url-form identity of the repo named `name`.
pub fn listed_repo_url(v: &Value, name: &str) -> Option<String> {
    v["repos"].as_array()?.iter().find(|r| r["repo_name"] == name).and_then(repo_url)
}

/// `GET /api/repos` → the cursor of the next page, when there is one.
pub fn next_repos_cursor(v: &Value) -> Option<String> {
    (v["has_more"] == true).then(|| v["next_cursor"].as_str().map(str::to_string)).flatten().filter(|c| !c.is_empty())
}

/// One change in a commit pack.
pub enum FileChange<'a> {
    Upsert { path: &'a str, bytes: &'a [u8] },
    Delete { path: &'a str },
}

/// Decoded bytes per `blob_chunk` line (the service's limit).
pub const CHUNK_MAX: usize = 4 * 1024 * 1024;

/// An NDJSON commit pack for `POST /api/repos/<repo>/commit-pack`:
/// metadata first, then each file's content as base64 chunks, each stream
/// ending with an `eof` chunk. `expected` is the branch head the commit
/// must land on (the service answers 409 when it moved).
pub fn commit_pack(branch: &str, expected: Option<&str>, message: &str, author: (&str, &str), changes: &[FileChange]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut files = Vec::with_capacity(changes.len());
    let mut chunks = String::new();
    let mut chunk = |id: &str, data: &[u8], eof: bool| {
        chunks.push_str(&serde_json::json!({ "blob_chunk": { "content_id": id, "data": b64.encode(data), "eof": eof } }).to_string());
        chunks.push('\n');
    };
    for (i, change) in changes.iter().enumerate() {
        let id = format!("c{i}");
        match change {
            FileChange::Upsert { path, bytes } => {
                files.push(serde_json::json!({ "path": path, "operation": "upsert", "content_id": id, "mode": "100644" }));
                let pieces: Vec<&[u8]> = bytes.chunks(CHUNK_MAX).collect();
                if pieces.is_empty() {
                    chunk(&id, b"", true);
                }
                for (j, piece) in pieces.iter().enumerate() {
                    chunk(&id, piece, j + 1 == pieces.len());
                }
            }
            FileChange::Delete { path } => {
                files.push(serde_json::json!({ "path": path, "operation": "delete", "content_id": id, "mode": "100644" }));
                chunk(&id, b"", true);
            }
        }
    }
    let mut meta = serde_json::json!({
        "target_branch": branch,
        "commit_message": message,
        "author": { "name": author.0, "email": author.1 },
        "files": files,
    });
    if let Some(sha) = expected {
        meta["expected_target_sha"] = Value::String(sha.to_string());
    }
    format!("{}\n{chunks}", serde_json::json!({ "metadata": meta }))
}

/// The new head from a commit pack's answer.
pub fn committed(v: &Value) -> Option<String> {
    (v["result"]["success"] == true).then(|| v["result"]["new_sha"].as_str().map(str::to_string)).flatten().filter(|s| is_sha(s))
}

#[cfg(test)]
mod tests {
    #[test]
    fn repo_list_pages() {
        use super::{listed_repo_url, next_repos_cursor};
        let page = serde_json::json!({ "repos": [{ "repo_name": "a", "url": "u-a" }], "has_more": true, "next_cursor": "c2" });
        assert_eq!(listed_repo_url(&page, "a").as_deref(), Some("u-a"));
        assert_eq!(listed_repo_url(&page, "b"), None);
        assert_eq!(next_repos_cursor(&page).as_deref(), Some("c2"));
        assert_eq!(next_repos_cursor(&serde_json::json!({ "repos": [], "has_more": false, "next_cursor": "c3" })), None);
        assert_eq!(next_repos_cursor(&serde_json::json!({ "repos": [], "has_more": true })), None);
    }

    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;
    use p256::pkcs8::EncodePrivateKey;

    fn key_pem() -> String {
        let sk = SigningKey::from_slice(&[9u8; 32]).unwrap();
        sk.to_pkcs8_pem(Default::default()).unwrap().to_string()
    }

    #[test]
    fn tokens_verify_and_carry_claims() {
        let pem = key_pem();
        let key = OrgKey::from_pem(&pem.replace('\n', "\\n")).unwrap();
        let t = key.token(&Claims { iss: "org", sub: "editor:ab", repo: "r-1", scopes: &["git:read", "git:write"], iat: 100, exp: 1000 });
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let claims: Value = serde_json::from_slice(&b64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({"iss":"org","sub":"editor:ab","repo":"r-1","scopes":["git:read","git:write"],"iat":100,"exp":1000}));
        let sig = Signature::from_slice(&b64.decode(parts[2]).unwrap()).unwrap();
        let vk = VerifyingKey::from(&SigningKey::from_pkcs8_pem(&pem).unwrap());
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
        assert!(OrgKey::from_pem("not a key").is_err());
    }

    #[test]
    fn the_token_cache_serves_fresh_tokens_and_stays_bounded() {
        let mut cache = TokenCache::new(3);
        cache.insert("r1", &["git:read"], "t1".into(), 1_000, 0);
        assert_eq!(cache.get("r1", &["git:read"], 900, 60), Some("t1"));
        assert_eq!(cache.get("r1", &["git:read"], 940, 60), None, "within the margin of its expiry");
        assert_eq!(cache.get("r1", &["git:write"], 0, 60), None, "keyed by its scopes");
        cache.insert("r2", &["git:read"], "t2".into(), 500, 0);
        cache.insert("r3", &["git:read"], "t3".into(), 2_000, 0);
        assert_eq!(cache.len(), 3);
        // full: the entry nearest its expiry goes
        cache.insert("r4", &["git:read"], "t4".into(), 3_000, 0);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get("r2", &["git:read"], 0, 60), None);
        assert_eq!(cache.get("r1", &["git:read"], 0, 60), Some("t1"));
        // replacing a present key evicts nothing
        cache.insert("r4", &["git:read"], "t4b".into(), 3_100, 0);
        assert_eq!((cache.len(), cache.get("r4", &["git:read"], 0, 60)), (3, Some("t4b")));
        // an insert drops every expired entry first
        cache.insert("r5", &["git:read"], "t5".into(), 5_000, 2_500);
        assert_eq!(cache.len(), 2, "r1 (1000) and r3 (2000) expired by 2500");
        assert_eq!(cache.get("r4", &["git:read"], 2_500, 60), Some("t4b"));
    }

    #[test]
    fn response_shapes() {
        let sha = "a".repeat(40);
        assert_eq!(branch_head(&json!({"branch": {"head_sha": sha}})), Some(sha.clone()));
        assert_eq!(branch_head(&json!({"branch": {"head_sha": "short"}})), None);
        let page = json!({"files": [
            {"path": "a.md", "size": 3, "mode": "100644", "type": "blob", "last_commit_sha": sha},
            {"path": "dir", "size": 0, "type": "tree", "last_commit_sha": sha}
        ], "has_more": true, "next_cursor": "c2"});
        let (entries, next) = tree_page(&page).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "a.md");
        assert_eq!(next.as_deref(), Some("c2"));
        assert!(tree_page(&json!({"files": [{"path": "x"}]})).is_err());
        // a size or last commit that is missing is refused, never read as 0 or ""
        assert!(tree_page(&json!({"files": [{"path": "x", "size": 1, "type": "blob"}]})).is_err());
        assert!(tree_page(&json!({"files": [{"path": "x", "last_commit_sha": sha, "type": "blob"}]})).is_err());
        // the last page ends the listing; a page with more and no cursor fails
        assert_eq!(tree_page(&json!({"files": [], "has_more": false})).unwrap().1, None);
        assert!(tree_page(&json!({"files": [], "has_more": true})).is_err());
        assert!(tree_page(&json!({"files": [], "has_more": true, "next_cursor": ""})).is_err());
        assert_eq!(listed_repo_url(&json!({"repos": [{"repo_name": "a", "url": "u-a"}, {"repo_name": "b", "url": "u-b"}]}), "b").as_deref(), Some("u-b"));
    }
}
