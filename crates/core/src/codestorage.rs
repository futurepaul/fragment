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

/// Hex digits of the owner's digest a repo's name carries: 48 bits, so two
/// identities that ever hold one username share a repo name once in 2^48.
pub const REPO_OWNER_HEX: usize = 12;

/// A new fragment's code.storage repo name: the one place it is derived.
/// The cell's create names the repo with it once and keeps the repo it
/// made (`Created.repo`, the url-form id every later call uses); nothing
/// rebuilds the name.
///
/// `<prefix><label>--<username>--<owner>`: the deployment's prefix (a
/// branch's `<branch>--`), the fragment's flat name, and the first 12 hex
/// digits of a digest of its owner's identity. The owner is in it so a
/// username another identity holds later (a wiped person's, or one an
/// operator released) never finds the repo its earlier holder made under
/// the same label: a repo a wipe deleted is never made again, nor another
/// person's files read. The same owner making a deleted name again finds
/// its repo, as before. `None`: not a fragment's name, or not an identity.
pub fn repo_name(prefix: &str, fragment: &str, owner: &str) -> Option<String> {
    assert!(prefix.is_empty() || prefix.ends_with("--"), "a deployment's repo prefix is empty or ends in --: {prefix:?}");
    let flat = fragment_proto::flat_name(fragment)?;
    if !crate::npub::is_identity(owner) {
        return None;
    }
    let digest = <sha2::Sha256 as sha2::Digest>::digest(format!("fragment repo owner\0{owner}").as_bytes());
    let name = format!("{prefix}{flat}--{}", &hex::encode(digest)[..REPO_OWNER_HEX]);
    assert!(name.starts_with(prefix) && name.ends_with(&hex::encode(digest)[..REPO_OWNER_HEX]), "the name is the prefix, the flat name, and the owner's digits");
    Some(name)
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
/// metadata first, then each upsert's content as base64 chunks, each
/// stream ending with an `eof` chunk. A delete names an id but streams
/// nothing, as code.storage's SDKs send it: the service refuses a chunk
/// no upsert names ("unexpected content_id"). `expected` is the branch
/// head the commit must land on (the service answers 409 when it moved).
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
            FileChange::Delete { path } => files.push(serde_json::json!({ "path": path, "operation": "delete", "content_id": id })),
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

/// The one NDJSON line of `POST /api/repos/<repo>/restore-commit`: a
/// commit on `branch` (whose tip must be `expected`) with the tree of
/// `base`, an ancestor of that tip.
pub fn restore_commit(branch: &str, base: &str, expected: &str, message: &str, author: (&str, &str)) -> String {
    let meta = json!({
        "target_branch": branch,
        "base_ref": base,
        "expected_target_sha": expected,
        "commit_message": message,
        "author": { "name": author.0, "email": author.1 },
    });
    format!("{}\n", json!({ "metadata": meta }))
}

/// The query of `GET /api/repos/<repo>/merge/preview` for a deploy: main
/// merged into live.
pub const DEPLOY_PREVIEW_QUERY: &str = "source_branch=main&target_branch=live";

/// How a deploy makes `live` serve `main`'s tip, from a preview of
/// merging main into live. code.storage's merge is git's three-way merge,
/// so once a rollback's restore commit is on `live`, merging main into it
/// keeps what the rollback reverted (and conflicts where main changed a
/// file it reverted), and when main is unchanged since, does nothing. So a
/// deploy merges only a `live` that holds nothing main lacks, or whose
/// files are their merge base's; restore commits, which take a tree whole,
/// do the rest. Each step is guarded by the `live` it read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Promotion {
    /// `live` is at main's tip.
    Current { live: String },
    /// `live` is an ancestor of main: the merge fast-forwards it.
    FastForward { live: String },
    /// main's tip is an ancestor of `live` (a rollback, and main unchanged
    /// since the deploy before it): a restore commit of main's tip.
    Restore { live: String, main: String },
    /// They diverged (a rollback, then main moved): a restore commit of
    /// their merge base, then the merge, which takes main's side of every
    /// path since live's files are the base's. A deploy after such a deploy
    /// is one too, its restore a no-op (412).
    RestoreThenMerge { live: String, base: String },
}

/// `GET /api/repos/<repo>/merge/preview?source_branch=main&target_branch=live`
/// → the deploy's steps; `None` for an answer that is not one.
pub fn promotion(v: &Value) -> Option<Promotion> {
    let sha = |k: &str| v[k].as_str().filter(|s| is_sha(s)).map(str::to_string);
    let (live, main) = (sha("target_tip_sha")?, sha("source_tip_sha")?);
    match v["result"].as_str()? {
        _ if live == main => Some(Promotion::Current { live }),
        "fast_forward" => Some(Promotion::FastForward { live }),
        "no_op" => Some(Promotion::Restore { live, main }),
        "merge_commit" => Some(Promotion::RestoreThenMerge { live, base: sha("merge_base_sha")? }),
        _ => None,
    }
}

/// The message of a deploy's first step after a rollback (`RestoreThenMerge`),
/// as `drafts` lists it beside the deploy's own: `base` is a sha.
pub fn back_to_base_message(deploy: &str, base: &str) -> String {
    format!("{deploy} (first, live back to {}: the last of main it holds)", &base[..12.min(base.len())])
}

/// A merge's 409 that is a conflict, not a target that moved (the service
/// answers 409 for both, told apart by `conflict_type`): a retry cannot
/// pass.
pub fn merge_conflicted(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|v| v["conflict_type"] == "merge_conflict" || v["code"] == "merge_conflict")
}

/// What `DELETE /api/repos/{repo}` answered (the service deletes softly,
/// then cleans up its storage after).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoDeleted {
    /// Deleted by this call.
    Now,
    /// Gone before it: deleted already (409 `repository_deleted`), or
    /// never there (404, 410). A replay of a delete lands here.
    Already,
    /// The repo may still be there, and a retry may pass: a timeout, a
    /// rate limit, a 5xx, or a 409 that is not a delete's (one thawing).
    Transient,
    /// The service refused the call (another 4xx): the same call cannot
    /// pass until something else changes (its key, its scopes).
    Refused,
}

pub fn repo_deleted(status: u16, body: &[u8]) -> RepoDeleted {
    match status {
        200..=299 => RepoDeleted::Now,
        404 | 410 => RepoDeleted::Already,
        409 if serde_json::from_slice::<Value>(body).is_ok_and(|v| v["code"] == "repository_deleted") => RepoDeleted::Already,
        408 | 409 | 425 | 429 | 500..=599 => RepoDeleted::Transient,
        400..=499 => RepoDeleted::Refused,
        _ => RepoDeleted::Transient,
    }
}

/// A commit pack's 412 that says it would change nothing: the branch's
/// tip holds its files already (a retry of a pack that landed, or a
/// write of what is there). The service makes no empty commit.
pub fn nothing_to_commit(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|v| v["result"]["status"] == "precondition_failed" && v["result"]["message"] == "no changes to commit")
}

#[cfg(test)]
mod tests {
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
        assert_eq!(cache.entries.len(), 3);
        // full: the entry nearest its expiry goes
        cache.insert("r4", &["git:read"], "t4".into(), 3_000, 0);
        assert_eq!(cache.entries.len(), 3);
        assert_eq!(cache.get("r2", &["git:read"], 0, 60), None);
        assert_eq!(cache.get("r1", &["git:read"], 0, 60), Some("t1"));
        // replacing a present key evicts nothing
        cache.insert("r4", &["git:read"], "t4b".into(), 3_100, 0);
        assert_eq!((cache.entries.len(), cache.get("r4", &["git:read"], 0, 60)), (3, Some("t4b")));
        // an insert drops every expired entry first
        cache.insert("r5", &["git:read"], "t5".into(), 5_000, 2_500);
        assert_eq!(cache.entries.len(), 2, "r1 (1000) and r3 (2000) expired by 2500");
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
        let repos = json!({"repos": [{"repo_name": "a", "url": "u-a"}, {"repo_name": "b", "url": "u-b"}], "has_more": true, "next_cursor": "c2"});
        assert_eq!((listed_repo_url(&repos, "b").as_deref(), listed_repo_url(&repos, "c")), (Some("u-b"), None));
        assert_eq!(next_repos_cursor(&repos).as_deref(), Some("c2"));
        assert_eq!(next_repos_cursor(&json!({ "repos": [], "has_more": false, "next_cursor": "c3" })), None);
        assert_eq!(next_repos_cursor(&json!({ "repos": [], "has_more": true })), None);
    }

    /// Goal: a new fragment's repo name carries its owner, so another
    /// identity under the same username and label never names the same
    /// repo, while the same owner making the name again does (a delete
    /// keeps the repo). Method: names derived for one owner, another, and
    /// the inputs the derivation refuses.
    #[test]
    fn a_repo_name_is_its_owners() {
        let (paul, fresh) = (crate::npub::identity_of(&"a".repeat(64)), crate::npub::identity_of(&"b".repeat(64)));
        let first = repo_name("e2e--", "todo.paul", &paul).unwrap();
        assert!(first.starts_with("e2e--todo--paul--"), "{first}");
        assert_eq!(first.len(), "e2e--todo--paul--".len() + REPO_OWNER_HEX, "{first}");
        assert!(first["e2e--todo--paul--".len()..].bytes().all(|b| b.is_ascii_hexdigit()), "{first}");
        // the same owner, again (a delete, then a create): the same repo
        assert_eq!(repo_name("e2e--", "todo.paul", &paul).as_deref(), Some(first.as_str()));
        // another identity under the same username (a wipe freed it): another repo
        let theirs = repo_name("e2e--", "todo.paul", &fresh).unwrap();
        assert_ne!(theirs, first);
        assert!(theirs.starts_with("e2e--todo--paul--"), "{theirs}");
        // production's has no prefix; another label is another repo
        assert!(repo_name("", "todo.paul", &paul).unwrap().starts_with("todo--paul--"));
        assert_ne!(repo_name("", "notes.paul", &paul), repo_name("", "todo.paul", &paul));
        // not a fragment's name, or not an identity: no name
        for (fragment, owner) in [("todo", paul.as_str()), ("Todo.paul", &paul), ("todo.paul", "id:short"), ("todo.paul", "npub1x"), ("todo.paul", &"a".repeat(64))] {
            assert_eq!(repo_name("", fragment, owner), None, "{fragment} {owner}");
        }
    }

    /// Goal: a repo's delete is done once the service says it is gone,
    /// however often it is asked (a wipe run again asks again), and only
    /// then. Method: the service's documented answers, and others.
    #[test]
    fn a_repo_delete_is_done_once_it_is_gone() {
        assert_eq!(repo_deleted(200, br#"{"message":"deletion initiated","repo_name":"x"}"#), RepoDeleted::Now);
        assert_eq!(repo_deleted(404, b""), RepoDeleted::Already, "never there");
        assert_eq!(repo_deleted(410, b""), RepoDeleted::Already, "gone");
        let deleted = br#"{"code":"repository_deleted","detail":"repository already deleted","status":409}"#;
        assert_eq!(repo_deleted(409, deleted), RepoDeleted::Already, "a replay");
        assert_eq!(repo_deleted(409, br#"{"code":"repository_thawing"}"#), RepoDeleted::Transient, "a 409 that is not a delete's");
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert_eq!(repo_deleted(status, b"{}"), RepoDeleted::Transient, "{status}: a retry may pass");
        }
        for status in [400, 401, 403, 405, 422] {
            assert_eq!(repo_deleted(status, b"{}"), RepoDeleted::Refused, "{status}: the same call cannot pass");
        }
    }

    /// Goal: a deploy's steps follow the preview, and an answer that names
    /// no tips, an unknown result, or a diverged live without its merge
    /// base is refused, never read as a fast-forward. Method: the service's
    /// documented answers, and broken ones.
    #[test]
    fn a_deploy_follows_the_merge_preview() {
        let (live, main, base) = ("1".repeat(40), "2".repeat(40), "3".repeat(40));
        let preview = |result: &str| {
            json!({ "status": "clean", "result": result, "source_branch": "main", "target_branch": "live",
                    "source_tip_sha": main, "target_tip_sha": live, "merge_base_sha": base, "conflict_paths": [], "conflicts": [] })
        };
        assert_eq!(promotion(&preview("fast_forward")), Some(Promotion::FastForward { live: live.clone() }));
        assert_eq!(promotion(&preview("no_op")), Some(Promotion::Restore { live: live.clone(), main: main.clone() }));
        assert_eq!(promotion(&preview("merge_commit")), Some(Promotion::RestoreThenMerge { live: live.clone(), base: base.clone() }));
        let mut same = preview("no_op");
        same["source_tip_sha"] = json!(live);
        assert_eq!(promotion(&same), Some(Promotion::Current { live: live.clone() }));
        assert_eq!(promotion(&preview("unknown")), None);
        let mut baseless = preview("merge_commit");
        baseless["merge_base_sha"] = json!("");
        assert_eq!(promotion(&baseless), None);
        let mut tipless = preview("fast_forward");
        tipless["target_tip_sha"] = json!("short");
        assert_eq!(promotion(&tipless), None);

        assert!(merge_conflicted(br#"{"error":"merge conflict","conflict_type":"merge_conflict","conflict_paths":["a"]}"#));
        assert!(merge_conflicted(br#"{"type":"about:blank","title":"Conflict","status":409,"code":"merge_conflict"}"#));
        assert!(!merge_conflicted(br#"{"result":{"status":"precondition_failed"}}"#), "a stale expected sha is not a conflict");
        assert!(!merge_conflicted(b"not json"));

        // the service's answer, read from a preview (2026-10-05)
        let unchanged = br#"{"commit":null,"result":{"target_branch":"","branch":"","old_sha":"","new_sha":"","success":false,"status":"precondition_failed","message":"no changes to commit"}}"#;
        assert!(nothing_to_commit(unchanged));
        assert!(!nothing_to_commit(br#"{"result":{"status":"precondition_failed","message":"expected branch head did not match current tip"}}"#), "a stale expected sha is no such answer");
        assert!(!nothing_to_commit(b"not json"));

        let line = restore_commit("live", &base, &live, "deploy x", ("a", "a@x"));
        assert!(line.ends_with('\n') && line.lines().count() == 1, "{line:?}");
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!((v["metadata"]["base_ref"].as_str(), v["metadata"]["expected_target_sha"].as_str()), (Some(base.as_str()), Some(live.as_str())));
    }
}
