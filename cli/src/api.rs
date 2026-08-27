// Thin blocking HTTP client that signs every request with NIP-98.
use crate::auth::Identity;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

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
        409 if summary.contains("name taken") => "name_taken",
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

pub struct Client {
    pub host: String,
    pub id: Identity,
    verbose: bool,
    http: reqwest::blocking::Client,
}

pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
    pub rev: Option<u64>,
}

impl Resp {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).context("response was not JSON")
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn err_summary(&self) -> String {
        match self.json() {
            Ok(v) => v["error"].as_str().unwrap_or("unknown error").to_string(),
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
            http: reqwest::blocking::Client::new(),
        }
    }

    /// Builder-style toggle for `-v`: one stderr line per signed request.
    pub fn with_verbose(mut self) -> Self {
        self.verbose = true;
        self
    }

    fn request(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Result<Resp> {
        let url = format!("{}{}", self.host, path);
        let body = body.unwrap_or_default();
        // connection-level failures are retried a few times: long-lived
        // sync clients hold keep-alive pools that go stale when the host
        // restarts, and without retries a watcher wedges until its process
        // is restarted (observed live on relay-vault)
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
            }
            let auth = self.id.nip98_header(method, &url, &body);
            let t0 = std::time::Instant::now();
            let mut req = match method {
                "GET" => self.http.get(&url),
                "POST" => self.http.post(&url),
                "PUT" => self.http.put(&url),
                "DELETE" => self.http.delete(&url),
                _ => return Err(anyhow!("bad method")),
            };
            req = req.header("authorization", auth);
            if !body.is_empty() {
                req = req.body(body.clone());
            }
            match req.send() {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if self.verbose {
                        eprintln!(
                            "{method} {path} -> {status} ({}ms [retries={attempt}])",
                            t0.elapsed().as_millis()
                        );
                    }
                    let rev = resp
                        .headers()
                        .get("x-fragment-rev")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok());
                    let bytes = resp.bytes().context("reading body")?.to_vec();
                    return Ok(Resp { status, body: bytes, rev });
                }
                Err(e) if e.is_connect() || e.is_request() => {
                    if self.verbose {
                        eprintln!("{method} {path} -> retry after error ({}ms [retries={attempt}])", t0.elapsed().as_millis());
                    }
                    last_err = Some(e);
                    continue; // stale pool / transient network — try again
                }
                Err(e) => return Err(e).context("request failed"),
            }
        }
        // three connect-level failures in a row: surface the last error
        // with its cause — a bare "failed after retries" turned a host
        // dropping large bodies into a silent 90s mystery (and before
        // that, an unreachable!() panicked here; found by restore agents)
        return Err(anyhow::Error::new(CodedError {
            code: "unavailable",
            msg: format!(
                "request failed after retries ({host} unreachable, or it dropped the connection mid-body — check the request size): {err}",
                host = self.host,
                err = last_err.map(|e| e.to_string()).unwrap_or_else(|| "no error recorded".into())
            ),
        }));
    }

    pub fn get(&self, path: &str) -> Result<Resp> {
        self.request("GET", path, None)
    }
    pub fn post_json(&self, path: &str, v: &Value) -> Result<Resp> {
        self.request("POST", path, Some(serde_json::to_vec(v)?))
    }
    pub fn put_bytes(&self, path: &str, bytes: Vec<u8>) -> Result<Resp> {
        self.request("PUT", path, Some(bytes))
    }
    pub fn put_json(&self, path: &str, v: &Value) -> Result<Resp> {
        self.request("PUT", path, Some(serde_json::to_vec(v)?))
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
