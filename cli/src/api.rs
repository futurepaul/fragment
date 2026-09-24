// Thin blocking HTTP client that signs every request with NIP-98.
use crate::auth::Identity;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::time::Duration;

/// An error carrying a stable machine code (surfaced in the `--json`
/// envelope as `error.code`). Display is the plain human message.
#[derive(Debug)]
pub struct CodedError {
    pub code: &'static str,
    pub msg: String,
}
impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}
impl std::error::Error for CodedError {}

/// Map an HTTP status + server summary to a stable error-code string
/// (the only place statuses become codes — the envelope picks it up at
/// the top-level catch via downcast).
pub fn code_for(status: u16, summary: &str) -> &'static str {
    match status {
        401 => "auth_failed",
        403 => "forbidden",
        404 => "not_found",
        // base-rev mismatch and other racing writes are conflicts; the
        // registry's duplicate-name response gets its own sharper code
        409 if summary.contains("name taken") || summary.contains("already exists") => "name_taken",
        409 => "conflict",
        413 => "too_large",
        429 => "rate_limited",
        502..=504 => "unavailable",
        _ => "server_error",
    }
}

/// Hint suffix for 5xx bodies: point agents at the event log; gateway
/// blips get a retry blessing.
fn http_context_suffix(status: u16) -> &'static str {
    if (500..600).contains(&status) {
        match status {
            502 | 503 => " (usually transient; retrying is safe) — see fragment events <name> if it persists",
            _ => " — see fragment events <name> if it persists",
        }
    } else {
        ""
    }
}

/// Every request may take this long, plus the time its body takes at
/// [`UPLOAD_BYTES_PER_S_MIN`]: a fixed total timeout (reqwest's default is
/// 30 s, and it covers the upload) meant no upload over 30 s ever landed.
pub const REQUEST_TIMEOUT_BASE: Duration = Duration::from_secs(30);
/// The slowest link an upload is promised to finish on (256 kbit/s).
pub const UPLOAD_BYTES_PER_S_MIN: u64 = 32 * 1024;
/// How long a connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Attempts per request (the first and two retries), where a retry is allowed.
pub const REQUEST_ATTEMPTS: u32 = 3;

/// The time a request carrying `bytes` (up, or down when the size is
/// known) may take.
pub fn timeout_for(bytes: u64) -> Duration {
    REQUEST_TIMEOUT_BASE + Duration::from_secs(bytes / UPLOAD_BYTES_PER_S_MIN)
}

/// Whether a request that may have reached the server can be sent again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Replay {
    /// GET, HEAD, and a PUT to a content address: sending it twice does
    /// what sending it once does, so any transport failure is retried.
    Safe,
    /// A write that carries its own idempotency id (an operation call):
    /// the server answers a repeat of the id with the first call's answer,
    /// so it is retried like a read. When every try failed and one reached
    /// the server, the write may have been applied: the outcome is unknown,
    /// and a retry with the same id finds out.
    ById,
    /// Everything else is resent only when the connection never opened. A
    /// request that reached the server may have been applied, and sending
    /// it again turned a commit whose answer was lost into a 409 and a
    /// conflict copy of your own bytes, and a create into "name taken" for
    /// your own new fragment.
    ConnectOnly,
}

impl Replay {
    pub fn of(method: &str) -> Replay {
        match method {
            "GET" | "HEAD" => Replay::Safe,
            _ => Replay::ConnectOnly,
        }
    }

    /// Whether this failure of one attempt may be retried.
    pub fn allows_retry(self, connection_opened: bool) -> bool {
        match self {
            Replay::Safe | Replay::ById => true,
            Replay::ConnectOnly => !connection_opened,
        }
    }
}

pub struct Client {
    pub host: String,
    pub id: Identity,
    verbose: bool,
    http: reqwest::blocking::Client,
}

pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).context("response was not JSON")
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    /// The server's explanation: `message` (the Rust cell's
    /// `{error: code, message}`), else `error` (the TypeScript runtime's).
    pub fn err_summary(&self) -> String {
        match self.json() {
            Ok(v) => v["message"].as_str().or_else(|| v["error"].as_str()).unwrap_or("unknown error").to_string(),
            Err(_) => String::from_utf8_lossy(&self.body).chars().take(200).collect(),
        }
    }
}

impl Client {
    pub fn new(host: &str, id: Identity) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            id,
            verbose: false,
            // every request sets its own total timeout (`timeout_for`)
            http: reqwest::blocking::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("the HTTP client builds (its TLS backend is compiled in)"),
        }
    }

    /// Builder-style toggle for `-v`: one stderr line per signed request.
    pub fn with_verbose(mut self) -> Self {
        self.verbose = true;
        self
    }

    fn request(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Result<Resp> {
        let body = body.unwrap_or_default();
        let timeout = timeout_for(body.len() as u64);
        self.send(method, path, body, Replay::of(method), timeout)
    }

    /// One signed request, retried within [`REQUEST_ATTEMPTS`] as `replay`
    /// allows: long-lived sync clients hold keep-alive pools that go stale
    /// when the host restarts, and without retries a watcher wedges until
    /// its process is restarted (observed live on relay-vault).
    fn send(&self, method: &str, path: &str, body: Vec<u8>, replay: Replay, timeout: Duration) -> Result<Resp> {
        let url = format!("{}{}", self.host, path);
        let mut last_err = None;
        // whether any try's connection opened: its request may have landed
        let mut reached = false;
        for attempt in 0..REQUEST_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(300 * attempt as u64));
            }
            let auth = self.id.nip98_header(method, &url, &body);
            let t0 = std::time::Instant::now();
            let mut req = match method {
                "GET" => self.http.get(&url),
                "POST" => self.http.post(&url),
                "PUT" => self.http.put(&url),
                "DELETE" => self.http.delete(&url),
                "HEAD" => self.http.head(&url),
                _ => return Err(anyhow!("bad method")),
            };
            req = req.header("authorization", auth).timeout(timeout);
            if !body.is_empty() {
                req = req.body(body.clone());
            }
            // the body is read here too: an answer cut off mid-body is as
            // lost as one that never came
            let answer = req.send().and_then(|resp| {
                let status = resp.status().as_u16();
                resp.bytes().map(|b| Resp { status, body: b.to_vec() })
            });
            match answer {
                Ok(resp) => {
                    if self.verbose {
                        eprintln!("{method} {path} -> {} ({}ms [retries={attempt}])", resp.status, t0.elapsed().as_millis());
                    }
                    return Ok(resp);
                }
                // a request that could not be built never left
                Err(e) if e.is_builder() => return Err(e).context("building the request"),
                Err(e) if replay.allows_retry(!e.is_connect()) => {
                    if self.verbose {
                        eprintln!("{method} {path} -> retry after error ({}ms [retries={attempt}])", t0.elapsed().as_millis());
                    }
                    reached |= !e.is_connect();
                    last_err = Some(e);
                }
                Err(e) => {
                    // the request may have been applied: say so, and never
                    // send it again blind
                    return Err(anyhow::Error::new(CodedError {
                        code: "outcome_unknown",
                        msg: format!(
                            "{method} {path}: the request may have reached {host}, but its answer was lost ({e}); it may have been applied, so check before repeating it",
                            host = self.host
                        ),
                    }));
                }
            }
        }
        let last = last_err.map(|e| e.to_string()).unwrap_or_else(|| "no error recorded".into());
        if replay == Replay::ById && reached {
            return Err(anyhow::Error::new(CodedError {
                code: "outcome_unknown",
                msg: format!(
                    "{method} {path}: {REQUEST_ATTEMPTS} tries reached {host} or may have, and none got an answer ({last}); it may have been applied",
                    host = self.host
                ),
            }));
        }
        // every attempt failed: surface the last error with its cause — a
        // bare "failed after retries" turned a host dropping large bodies
        // into a silent 90s mystery (and before that, an unreachable!()
        // panicked here; found by restore agents)
        Err(anyhow::Error::new(CodedError {
            code: "unavailable",
            msg: format!(
                "request failed after retries ({host} unreachable, or it dropped the connection mid-body — check the request size): {last}",
                host = self.host,
            ),
        }))
    }

    pub fn get(&self, path: &str) -> Result<Resp> {
        self.request("GET", path, None)
    }
    pub fn post_json(&self, path: &str, v: &Value) -> Result<Resp> {
        self.request("POST", path, Some(serde_json::to_vec(v)?))
    }
    /// A POST whose body carries its idempotency id (an operation call):
    /// retried like a read (`Replay::ById`).
    pub fn post_json_by_id(&self, path: &str, v: &Value) -> Result<Resp> {
        let body = serde_json::to_vec(v)?;
        let timeout = timeout_for(body.len() as u64);
        self.send("POST", path, body, Replay::ById, timeout)
    }
    pub fn put_json(&self, path: &str, v: &Value) -> Result<Resp> {
        self.request("PUT", path, Some(serde_json::to_vec(v)?))
    }
    pub fn put_bytes(&self, path: &str, bytes: Vec<u8>) -> Result<Resp> {
        self.request("PUT", path, Some(bytes))
    }
    /// A PUT to a content address (a blob named by its bytes' hash): safe
    /// to send again whatever became of the first.
    pub fn put_blob(&self, path: &str, bytes: Vec<u8>) -> Result<Resp> {
        let timeout = timeout_for(bytes.len() as u64);
        self.send("PUT", path, bytes, Replay::Safe, timeout)
    }
    /// A GET whose answer is known to be about `bytes` long (a blob), given
    /// the time that takes.
    pub fn get_sized(&self, path: &str, bytes: u64) -> Result<Resp> {
        self.send("GET", path, Vec::new(), Replay::Safe, timeout_for(bytes))
    }
    pub fn head(&self, path: &str) -> Result<Resp> {
        self.request("HEAD", path, None)
    }
    pub fn delete(&self, path: &str) -> Result<Resp> {
        self.request("DELETE", path, None)
    }

    /// Full control call with standard error handling: returns parsed JSON or an Err carrying a
    /// stable machine code (CodedError) plus the server's human-readable message.
    pub fn call(&self, resp: Resp) -> Result<Value> {
        if resp.ok() {
            resp.json()
        } else {
            let summary = resp.err_summary();
            Err(anyhow::Error::new(CodedError {
                code: code_for(resp.status, &summary),
                msg: format!("http {}: {}{}", resp.status, summary, http_context_suffix(resp.status)),
            }))
        }
    }
}

pub fn encode_q(s: &str) -> String {
    // minimal percent-encoding for query values we control (paths, names)
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use fragment_fakes::http::{Response as FakeResponse, Server};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A host that reads each request, counts it, and closes the
    /// connection without answering (an answer lost on its way back).
    fn silent_host() -> (Server, Arc<AtomicU32>, Client) {
        let seen = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&seen);
        let server = Server::start(
            0,
            Arc::new(move |_: &fragment_fakes::http::Request| {
                counter.fetch_add(1, Ordering::SeqCst);
                FakeResponse::unanswered()
            }),
        )
        .expect("start the silent host");
        let client = Client::new(&server.url, Identity::from_secret([7u8; 32]));
        (server, seen, client)
    }

    /// The failure's stable code (a request that got an answer fails the test).
    fn code_of(r: Result<Resp>) -> &'static str {
        match r {
            Ok(resp) => panic!("expected a failure, got http {}", resp.status),
            Err(e) => e.downcast_ref::<CodedError>().map(|c| c.code).unwrap_or("uncoded"),
        }
    }

    /// Goal: a write that reached the host is never sent again blind.
    /// Method: the host swallows every answer; a POST and a DELETE each
    /// arrive once and fail as outcome_unknown, where the old client sent
    /// them three times.
    #[test]
    fn a_write_whose_answer_is_lost_is_sent_once() {
        let (_server, seen, c) = silent_host();
        assert_eq!(code_of(c.post_json("/api/fragments", &serde_json::json!({ "name": "x" }))), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), 1, "the POST arrived once");
        assert_eq!(code_of(c.delete("/api/f/x")), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), 2, "the DELETE arrived once");
        assert_eq!(code_of(c.put_bytes("/api/f/x/secrets/K", b"v".to_vec())), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), 3, "a PUT that is not content-addressed arrived once");
    }

    /// The flip side: reads and content-addressed uploads are safe to
    /// repeat, so they are, within REQUEST_ATTEMPTS.
    #[test]
    fn reads_and_blob_uploads_are_retried() {
        let (_server, seen, c) = silent_host();
        assert_eq!(code_of(c.get("/api/f/x/status")), "unavailable");
        assert_eq!(seen.load(Ordering::SeqCst), REQUEST_ATTEMPTS);
        assert_eq!(code_of(c.put_blob("/api/f/x/blobs/abc", vec![1, 2, 3])), "unavailable");
        assert_eq!(seen.load(Ordering::SeqCst), 2 * REQUEST_ATTEMPTS);
    }

    /// Goal: an operation call, which carries its idempotency id, is
    /// retried like a read, and a failure after it may have landed says the
    /// outcome is unknown. Method: the host swallows every answer; the call
    /// arrives REQUEST_ATTEMPTS times (a plain POST arrives once), and with
    /// no host at all it is merely unavailable.
    #[test]
    fn an_op_call_is_retried_by_its_id() {
        let (_server, seen, c) = silent_host();
        let call = serde_json::json!({ "id": "cli-1", "input": {} });
        assert_eq!(code_of(c.post_json_by_id("/api/f/x/ops/add", &call)), "outcome_unknown");
        assert_eq!(seen.load(Ordering::SeqCst), REQUEST_ATTEMPTS, "each try arrived");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let nowhere = Client::new(&format!("http://127.0.0.1:{port}"), Identity::from_secret([7u8; 32]));
        assert_eq!(code_of(nowhere.post_json_by_id("/api/f/x/ops/add", &call)), "unavailable", "no try reached a host");
    }

    /// A connection that never opened carried nothing, so even a POST is
    /// retried, and the failure is plain unavailability.
    #[test]
    fn a_write_that_never_connected_is_unavailable() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = Client::new(&format!("http://127.0.0.1:{port}"), Identity::from_secret([7u8; 32]));
        assert_eq!(code_of(c.post_json("/api/fragments", &serde_json::json!({ "name": "x" }))), "unavailable");
    }

    #[test]
    fn the_timeout_grows_with_the_upload() {
        assert_eq!(timeout_for(0), REQUEST_TIMEOUT_BASE);
        assert_eq!(timeout_for(UPLOAD_BYTES_PER_S_MIN * 90), REQUEST_TIMEOUT_BASE + Duration::from_secs(90));
        // the largest blob gets hours, not 30 seconds
        assert!(timeout_for(256 * 1024 * 1024) > Duration::from_secs(2 * 3600));
    }

    #[test]
    fn code_for_maps_statuses() {
        assert_eq!(code_for(401, ""), "auth_failed");
        assert_eq!(code_for(409, "name taken"), "name_taken");
        assert_eq!(code_for(409, "base_rev mismatch"), "conflict");
        assert_eq!(code_for(503, ""), "unavailable");
    }
}
