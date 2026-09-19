// A mock code.storage server for tests: implements the documented HTTP
// shape from their OpenAPI spec — branch heads, metadata listings (cursor
// paginated), raw file reads, NDJSON commit-packs with expected-parent
// rejection (409 precondition_failed), branches/create (incl. ephemeral),
// merge, restore-commit (ancestor-guarded 412), commit listings — plus the
// fragment-host side of the storage-token contract.
//
// Sabotage hooks let tests force CAS conflicts deterministically: with
// `sabotage_commit_packs(n)`, the first n commit-pack calls each apply a
// competitor commit (moving the tip) and then reject ours with 409 —
// exactly the race the CLI's bounded retry loop must survive.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Repo {
    files: HashMap<String, (Vec<u8>, String)>, // (bytes, last_commit_sha)
    commits: Vec<Value>, // {sha, message, parent_shas}
    branches: HashMap<String, String>,
    ephemeral: HashSet<String>,
}

#[derive(Default)]
pub struct MockState {
    repos: HashMap<String, Repo>,
    counter: u64,
    /// first N commit-pack calls get a competitor commit + 409
    sabotage: u32,
    page_size: usize,
    seen_commit_packs: u32,
    api_base: String,
    requests: Vec<String>,
    /// /api/f/{name}/refresh nudges received (the pin-refresh contract)
    refresh_count: u32,
}

pub struct MockServer {
    pub url: String,
    pub state: Arc<Mutex<MockState>>,
    stop: Arc<AtomicBool>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl MockServer {
    /// Start on an ephemeral port; serves until dropped. Also answers the
    /// fragment-host storage-token route, handing out its own URL as the
    /// code.storage server (so `CodeStorage::connect` works against it).
    pub fn start() -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState { page_size: 2, api_base: url.clone(), ..Default::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let state = Arc::clone(&state);
                        std::thread::spawn(move || handle(stream, state));
                    }
                }
            });
        }
        MockServer { url, state, stop }
    }

    /// Seed a repo with files on `main` (one seed commit) — the
    /// pre-existing remote state a folder syncs against.
    pub fn seed_repo(&self, name: &str, files: &[(&str, &[u8])]) {
        let mut st = self.state.lock().unwrap();
        let sha = next_sha(&mut st.counter, "seed");
        let mut repo = Repo {
            files: files.iter().map(|(p, b)| (p.to_string(), (b.to_vec(), sha.clone()))).collect(),
            ..Default::default()
        };
        repo.commits.push(json!({ "sha": sha, "message": "seed", "parent_shas": [] }));
        repo.branches.insert("main".into(), sha);
        st.repos.insert(name.to_string(), repo);
    }

    /// First N commit-pack calls: apply a competitor commit, then 409.
    pub fn sabotage_commit_packs(&self, n: u32) {
        self.state.lock().unwrap().sabotage = n;
    }

    pub fn commit_pack_count(&self) -> u32 {
        self.state.lock().unwrap().seen_commit_packs
    }

    /// pin-refresh nudges received so far
    pub fn refresh_count(&self) -> u32 {
        self.state.lock().unwrap().refresh_count
    }

    /// Peek at a repo's file bytes at the tip of a branch.
    pub fn file_at(&self, repo: &str, branch: &str, path: &str) -> Option<Vec<u8>> {
        let st = self.state.lock().unwrap();
        st.repos.get(repo)?.branches.get(branch)?;
        st.repos.get(repo).unwrap().files.get(path).map(|(b, _)| b.clone())
    }

}

/// deterministic, unique fake 40-hex SHAs: sha256(tag#counter), first 20
/// bytes (a plain counter formatting collides once counters share prefixes)
fn next_sha(counter: &mut u64, tag: &str) -> String {
    use sha2::{Digest, Sha256};
    *counter += 1;
    let d = Sha256::digest(format!("{tag}#{counter}").as_bytes());
    let mut out = String::new();
    for b in d.iter().take(20) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = find_subslice(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?.to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut content_length = 0usize;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body: Vec<u8> = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    let (path, query) = split_query(&target);
    Some(Req { method, path, query, body })
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn split_query(target: &str) -> (String, HashMap<String, String>) {
    let (p, q) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let mut map = HashMap::new();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(url_decode(k), url_decode(v));
    }
    (p, map)
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8], ct: &str) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        412 => "Precondition Failed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn problem(status: u16, detail: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "type": "about:blank", "title": "error", "status": status, "detail": detail, "error": detail })).unwrap()
}

fn handle(mut stream: TcpStream, state: Arc<Mutex<MockState>>) {
    let Some(req) = read_request(&mut stream) else { return };
    {
        let mut st = state.lock().unwrap();
        st.requests.push(format!("{} {}", req.method, req.path));
    }

    // fragment-host side: storage-token mint (the real endpoint is
    // NIP-98-gated; the mock ignores auth)
    if req.method == "GET" && req.path.starts_with("/api/f/") && req.path.ends_with("/storage-token") {
        let name = req.path.trim_start_matches("/api/f/").trim_end_matches("/storage-token").to_string();
        let api = state.lock().unwrap().api_base.clone();
        let body = serde_json::to_vec(&json!({ "token": "test-jwt", "repo": name, "api": api })).unwrap();
        write_response(&mut stream, 200, &body, "application/json");
        return;
    }

    // fragment-host side: the pin-refresh nudge landing syncs/deployed
    // fire after a commit (the real endpoint is NIP-98 editor+)
    if req.method == "POST" && req.path.starts_with("/api/f/") && req.path.ends_with("/refresh") {
        state.lock().unwrap().refresh_count += 1;
        let body = serde_json::to_vec(&json!({ "ok": true })).unwrap();
        write_response(&mut stream, 200, &body, "application/json");
        return;
    }

    // code.storage side: /api/repos/{repo}/{op...}
    let rest = match req.path.strip_prefix("/api/repos/") {
        Some(r) => r,
        None => {
            write_response(&mut stream, 404, &problem(404, "unknown path"), "application/json");
            return;
        }
    };
    let (repo_name, op) = match rest.split_once('/') {
        Some((r, o)) => (url_decode(r), format!("/{o}")),
        None => {
            write_response(&mut stream, 404, &problem(404, "unknown path"), "application/json");
            return;
        }
    };

    let (status, out, ct) = {
        let mut st = state.lock().unwrap();
        route(&mut st, &repo_name, &req.method, &op, &req.query, &req.body)
    };
    write_response(&mut stream, status, &out, ct);
}

fn repo_mut<'a>(st: &'a mut MockState, repo: &str) -> &'a mut Repo {
    st.repos.entry(repo.to_string()).or_default()
}

fn route(
    st: &mut MockState,
    repo: &str,
    method: &str,
    op: &str,
    query: &HashMap<String, String>,
    body: &[u8],
) -> (u16, Vec<u8>, &'static str) {
    let no_repo = !st.repos.contains_key(repo);
    match (method, op) {
        ("GET", "/branch") => {
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let name = query.get("name").cloned().unwrap_or_default();
            match st.repos[repo].branches.get(&name).cloned() {
                Some(s) => (
                    200,
                    serde_json::to_vec(&json!({ "branch": { "name": name, "head_sha": s, "created_at": "2026-01-01T00:00:00Z" } })).unwrap(),
                    "application/json",
                ),
                None => (404, problem(404, "branch not found"), "application/json"),
            }
        }
        ("GET", "/files/metadata") => {
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let git_ref = query.get("ref").cloned().unwrap_or_else(|| "main".into());
            let Some(_tip) = st.repos[repo].branches.get(&git_ref).cloned() else {
                return (404, problem(404, "ref not found"), "application/json");
            };
            // The mock's files map is the tip state (changes apply
            // immediately), which is what listing at a branch head means.
            let mut files: Vec<(String, u64, String)> = st.repos[repo]
                .files
                .iter()
                .map(|(p, (b, last))| (p.clone(), b.len() as u64, last.clone()))
                .collect();
            files.sort();
            let page = st.page_size.max(1);
            let cursor = query.get("cursor").cloned().unwrap_or_default();
            let start = if cursor.is_empty() {
                0
            } else {
                files.iter().position(|(p, _, _)| p.as_str() >= cursor.as_str()).unwrap_or(files.len())
            };
            let end = (start + page).min(files.len());
            let has_more = end < files.len();
            let entries: Vec<Value> = files[start..end]
                .iter()
                .map(|(p, size, last)| json!({ "path": p, "mode": "100644", "type": "blob", "size": size, "last_commit_sha": last }))
                .collect();
            let mut resp = json!({ "files": entries, "commits": {}, "ref": git_ref, "has_more": has_more });
            if has_more {
                resp["next_cursor"] = json!(files[end].0.clone());
            }
            (200, serde_json::to_vec(&resp).unwrap(), "application/json")
        }
        ("GET", "/file") => {
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let path = query.get("path").cloned().unwrap_or_default();
            match st.repos[repo].files.get(&path) {
                Some((bytes, _)) => (200, bytes.clone(), "application/octet-stream"),
                None => (404, problem(404, "file not found"), "application/json"),
            }
        }
        ("POST", "/commit-pack") => {
            st.seen_commit_packs += 1;
            // sabotage: a competitor commit lands first, then our pack is
            // rejected — the expected-parent race, deterministically
            if st.sabotage > 0 {
                st.sabotage -= 1;
                apply_pack(st, repo, &competitor_pack(st.counter));
                return (
                    409,
                    serde_json::to_vec(&json!({ "result": { "target_branch": "main", "branch": "main", "old_sha": "", "new_sha": "", "success": false, "status": "precondition_failed", "message": "expected branch head did not match current tip" } })).unwrap(),
                    "application/json",
                );
            }
            let text = String::from_utf8_lossy(body).to_string();
            let pack = match parse_pack(&text) {
                Some(p) => p,
                None => return (400, problem(400, "malformed NDJSON"), "application/json"),
            };
            let current = st.repos.get(repo).and_then(|r| r.branches.get("main").cloned());
            if let Some(exp) = &pack.expected {
                if current.clone().unwrap_or_default() != *exp {
                    return (
                        409,
                        serde_json::to_vec(&json!({ "result": { "target_branch": "main", "branch": "main", "old_sha": "", "new_sha": "", "success": false, "status": "precondition_failed", "message": "expected branch head did not match current tip" } })).unwrap(),
                        "application/json",
                    );
                }
            }
            let new_tip = apply_pack(st, repo, &pack);
            let old = current.unwrap_or_else(|| "0".repeat(40));
            let resp = json!({
                "commit": { "commit_sha": new_tip, "tree_sha": new_tip, "target_branch": "main", "pack_bytes": 0, "blob_count": pack.files.len() },
                "result": { "target_branch": "main", "branch": "main", "old_sha": old, "new_sha": new_tip, "success": true, "status": "ok" },
            });
            (201, serde_json::to_vec(&resp).unwrap(), "application/json")
        }
        ("POST", "/branches/create") => {
            let Ok(v) = serde_json::from_slice::<Value>(body) else {
                return (400, problem(400, "bad json"), "application/json");
            };
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let base_ref = v["base_ref"].as_str().unwrap_or("main");
            let target = v["target_branch"].as_str().unwrap_or_default().to_string();
            let ephemeral = v["target_is_ephemeral"].as_bool().unwrap_or(false);
            // resolve base_ref like the real API: branch name, a sha some
            // branch points at, or a bare commit sha
            let sha = st.repos[repo]
                .branches
                .get(base_ref)
                .cloned()
                .or_else(|| st.repos[repo].branches.values().find(|v| *v == base_ref).cloned())
                .or_else(|| {
                    st.repos[repo]
                        .commits
                        .iter()
                        .find(|c| c["sha"].as_str() == Some(base_ref))
                        .and_then(|c| c["sha"].as_str().map(|s| s.to_string()))
                });
            let Some(sha) = sha else {
                return (404, problem(404, "base ref not found"), "application/json");
            };
            let r = repo_mut(st, repo);
            if ephemeral {
                r.ephemeral.insert(target.clone());
            }
            r.branches.insert(target.clone(), sha.clone());
            (
                200,
                serde_json::to_vec(&json!({ "message": "branch created", "target_branch": target, "target_is_ephemeral": ephemeral, "commit_sha": sha })).unwrap(),
                "application/json",
            )
        }
        ("POST", "/merge") => {
            let Ok(v) = serde_json::from_slice::<Value>(body) else {
                return (400, problem(400, "bad json"), "application/json");
            };
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let target = v["target_branch"].as_str().unwrap_or_default().to_string();
            let source_ref = v["source_ref"].as_str().unwrap_or_default().to_string();
            let expected = v["expected_target_sha"].as_str().map(|s| s.to_string());
            let cur = st.repos[repo].branches.get(&target).cloned().unwrap_or_default();
            if let Some(exp) = expected {
                if cur != exp {
                    return (
                        409,
                        serde_json::to_vec(&json!({ "error": "target moved", "conflict_type": "target_moved" })).unwrap(),
                        "application/json",
                    );
                }
            }
            let Some(source_sha) = st.repos[repo].branches.get(&source_ref).cloned() else {
                return (404, problem(404, "source ref not found"), "application/json");
            };
            // mock: moving the target to the source tip (fast-forward in
            // the common case; a diverged-merge commit shape is not needed
            // for the CLI flow tests)
            repo_mut(st, repo).branches.insert(target.clone(), source_sha.clone());
            let resp = json!({
                "result": "fast_forward",
                "commit_sha": source_sha,
                "tree_sha": source_sha,
                "merge_base_sha": cur,
                "promoted_commits": 1,
                "source": { "ref": source_ref, "branch": source_ref, "ephemeral": false, "sha": source_sha },
                "target": { "branch": target, "old_sha": cur, "new_sha": source_sha },
            });
            (200, serde_json::to_vec(&resp).unwrap(), "application/json")
        }
        ("POST", "/restore-commit") => {
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let text = String::from_utf8_lossy(body).to_string();
            let Some(v) = text.lines().next().and_then(|l| serde_json::from_str::<Value>(l).ok()) else {
                return (400, problem(400, "malformed NDJSON"), "application/json");
            };
            let meta = &v["metadata"];
            let base_ref = meta["base_ref"].as_str().unwrap_or_default().to_string();
            let expected = meta["expected_target_sha"].as_str().unwrap_or_default().to_string();
            let cur = st.repos[repo].branches.get("live").cloned().unwrap_or_default();
            if cur != expected {
                return (409, problem(409, "expected live tip mismatch"), "application/json");
            }
            // ancestor guard: base must be a commit in the repo's history
            let in_history = st.repos[repo].commits.iter().any(|c| c["sha"].as_str() == Some(base_ref.as_str()));
            if !in_history || base_ref == cur {
                return (
                    412,
                    serde_json::to_vec(&json!({ "result": { "target_branch": "live", "branch": "live", "old_sha": cur, "new_sha": "", "success": false, "status": "precondition_failed", "message": "base_ref not an ancestor of the current tip" } })).unwrap(),
                    "application/json",
                );
            }
            let new_tip = next_sha(&mut st.counter, "restore");
            let r = repo_mut(st, repo);
            r.commits.push(json!({ "sha": new_tip, "message": "restore", "parent_shas": [cur] }));
            r.branches.insert("live".into(), new_tip.clone());
            let resp = json!({
                "commit": { "commit_sha": new_tip, "tree_sha": base_ref, "target_branch": "live", "pack_bytes": 0, "blob_count": 0 },
                "result": { "target_branch": "live", "branch": "live", "old_sha": cur, "new_sha": new_tip, "success": true, "status": "ok" },
            });
            (201, serde_json::to_vec(&resp).unwrap(), "application/json")
        }
        ("GET", "/commits") => {
            if no_repo {
                return (404, problem(404, "repo not found"), "application/json");
            }
            let git_ref = query.get("ref").cloned().unwrap_or_else(|| "main".into());
            let Some(tip) = st.repos[repo].branches.get(&git_ref).cloned() else {
                return (404, problem(404, "ref not found"), "application/json");
            };
            // walk parents from the tip (bounded: test repos are tiny)
            let mut ordered: Vec<Value> = Vec::new();
            let mut frontier = vec![tip];
            let mut seen: HashSet<String> = HashSet::new();
            while let Some(sha) = frontier.pop() {
                if !seen.insert(sha.clone()) {
                    continue;
                }
                if let Some(c) = st.repos[repo].commits.iter().find(|c| c["sha"].as_str() == Some(sha.as_str())).cloned() {
                    for p in c["parent_shas"].as_array().cloned().unwrap_or_default() {
                        if let Some(ps) = p.as_str() {
                            frontier.push(ps.to_string());
                        }
                    }
                    let mut out = c.clone();
                    out["author_name"] = json!("tester");
                    out["author_email"] = json!("t@example.com");
                    out["date"] = json!("2026-01-01T00:00:00Z");
                    ordered.push(out);
                }
            }
            (200, serde_json::to_vec(&json!({ "commits": ordered, "has_more": false })).unwrap(), "application/json")
        }
        _ => (404, problem(404, "no such operation"), "application/json"),
    }
}

struct Pack {
    expected: Option<String>,
    files: Vec<(String, String, Vec<u8>)>, // (path, op, bytes)
}

fn parse_pack(text: &str) -> Option<Pack> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let meta_line = lines.next()?;
    let meta: Value = serde_json::from_str(meta_line).ok()?;
    let meta = meta.get("metadata")?;
    let expected = meta["expected_target_sha"].as_str().map(|s| s.to_string());
    let mut chunks: HashMap<String, Vec<u8>> = HashMap::new();
    for l in lines {
        let v: Value = serde_json::from_str(l).ok()?;
        let c = v.get("blob_chunk")?;
        let id = c["content_id"].as_str()?.to_string();
        let data = c["data"].as_str().unwrap_or("");
        let bytes = if data.is_empty() { Vec::new() } else { b64.decode(data).ok()? };
        chunks.entry(id).or_default().extend(bytes);
    }
    let mut files = Vec::new();
    for f in meta["files"].as_array()? {
        let path = f["path"].as_str()?.to_string();
        let op = f["operation"].as_str()?.to_string();
        let id = f["content_id"].as_str().unwrap_or_default().to_string();
        let bytes = chunks.get(&id).cloned().unwrap_or_default();
        files.push((path, op.to_string(), bytes));
    }
    Some(Pack { expected, files })
}

fn competitor_pack(n: u64) -> Pack {
    Pack {
        expected: None,
        files: vec![("competitor.txt".into(), "upsert".into(), format!("competitor {n}").into_bytes())],
    }
}

fn apply_pack(st: &mut MockState, repo: &str, pack: &Pack) -> String {
    let repo_entry = st.repos.entry(repo.to_string()).or_default();
    for (path, op, _bytes) in &pack.files {
        if op == "delete" {
            repo_entry.files.remove(path);
        }
    }
    let old = repo_entry.branches.get("main").cloned();
    let tip = next_sha(&mut st.counter, "commit");
    let parents: Vec<String> = old.into_iter().collect();
    repo_entry.commits.push(json!({ "sha": tip, "message": "commit", "parent_shas": parents }));
    for (path, op, bytes) in &pack.files {
        if op != "delete" {
            // per-file last_commit_sha: untouched files keep theirs
            repo_entry.files.insert(path.clone(), (bytes.clone(), tip.clone()));
        }
    }
    repo_entry.branches.insert("main".into(), tip.clone());
    tip
}
