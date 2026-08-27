// Thin blocking HTTP client that signs every request with NIP-98.
use crate::auth::Identity;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::io::Read;

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
}

/// A streaming response from [`Client::get_stream`]: status known up front,
/// body read lazily (pulls stream through tmp files instead of buffering).
pub struct StreamGet {
    pub status: u16,
    pub resp: reqwest::blocking::Response,
}

impl StreamGet {
    /// etag of a hashed/immutable asset, if the server sent one
    #[allow(dead_code)] // public helper; used by tests and future hashed reads
    pub fn etag(&self) -> Option<String> {
        self.resp.headers().get("etag")?.to_str().ok().map(|s| s.to_string())
    }
    /// Source mtime for pull materialization ("preserving mtime"): tier
    /// responses carry Last-Modified; parse it best-effort (IMF-fixdate).
    pub fn last_modified(&self) -> Option<std::time::SystemTime> {
        let v = self.resp.headers().get("last-modified")?.to_str().ok()?;
        parse_http_date(v)
    }
    pub fn into_read(self) -> impl std::io::Read {
        self.resp
    }
    pub fn err_summary(&mut self) -> String {
        let mut buf = String::new();
        let _ = self.resp.read_to_string(&mut buf);
        buf.chars().take(200).collect()
    }
}

/// Minimal IMF-fixdate parser ("Sun, 06 Nov 1994 08:49:37 GMT"); everything
/// else parses as None and callers skip the mtime preservation. Days-in-month
/// table with leap-year correction is enough for RFC 1123 dates.
fn parse_http_date(s: &str) -> Option<std::time::SystemTime> {
    use std::time::{Duration, UNIX_EPOCH};
    let parts: Vec<&str> = s.split_whitespace().collect();
    // [day-name,] dd Mon yyyy HH:MM:SS GMT — some servers omit the weekday
    let idx = |i: usize| parts.get(i).copied().unwrap_or_default();
    // IMF-fixdate "Sun, 06 Nov 1994 08:49:37 GMT" splits into 6 tokens;
    // asctime "Sun Nov  6 08:49:37 1994" into 5 (year last!)
    let (day, mon_name, year, hms) = match parts.len() {
        6 => (idx(1), idx(2), idx(3), idx(4)),
        5 => (idx(2), idx(1), idx(4), idx(3)),
        _ => return None,
    };
    let day: i64 = day.trim_end_matches(',').parse().ok()?;
    let month = match mon_name.get(..3)?.to_ascii_lowercase().as_str() {
        "jan" => 1u64,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i64 = year.parse().ok()?;
    let mut hmss = hms.split(':');
    let hh: i64 = hmss.next()?.parse().ok()?;
    let mm: i64 = hmss.next()?.parse().ok()?;
    let ss: i64 = hmss.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // days since civil epoch via Howard Hinnant's algorithm
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe: i64 = y - era * 400;
    let mp: i64 = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day - 1;
    const SHIFT: i64 = 719_468;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - SHIFT;
    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss;
    Some(UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64))
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
        self.request_ct(method, path, body, None)
    }

    /// request() with an explicit content-type (the ref-form commit REQUIRES
    /// application/json on the wire for the runtime to select ref parsing).
    fn request_ct(&self, method: &str, path: &str, body: Option<Vec<u8>>, content_type: Option<&str>) -> Result<Resp> {
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
            if let Some(ct) = content_type {
                req = req.header("content-type", ct);
            }
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
                    let bytes = resp.bytes().context("reading body")?.to_vec();
                    return Ok(Resp { status, body: bytes });
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
        self.request_ct("PUT", path, Some(serde_json::to_vec(v)?), Some("application/json"))
    }
    /// Ref-form file commit (docs/blob-tier.md two-tier wire form): the row
    /// points at bytes presumed already in the tier:
    /// `content-type: application/json`, body {"ref":{"sha256","size","mime"}}.
    /// Revision/notify semantics stay bound to this row commit.
    pub fn put_ref(&self, path_qs: &str, sha256: &str, size: u64, mime: &str) -> Result<Resp> {
        let body = serde_json::json!({ "ref": { "sha256": sha256, "size": size, "mime": mime } });
        self.request_ct(
            "PUT",
            path_qs,
            Some(serde_json::to_vec(&body)?),
            Some("application/json"),
        )
    }
    pub fn delete(&self, path: &str) -> Result<Resp> {
        self.request("DELETE", path, None)
    }

    /// Streaming GET for materialization/pull: follows redirects (reqwest's
    /// default policy applies — the new public-read wire form answers with a
    /// 302 to `${BLOBSD_PUBLIC_URL}/${sha}`), and hands back status + response
    /// so callers can stream the body without buffering it whole first.
    pub fn get_stream(&self, path: &str) -> Result<StreamGet> {
        let url = format!("{}{}", self.host, path);
        // connection-level retry parity with request(): stale keep-alive
        // pools must not wedge pulls behind long-lived watchers
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
            }
            let auth = self.id.nip98_header("GET", &url, b"");
            let result = self.http.get(&url).header("authorization", auth).send();
            match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if self.verbose {
                        eprintln!("GET {path} -> {status} (stream [retries={attempt}])");
                    }
                    return Ok(StreamGet { status, resp });
                }
                Err(e) if e.is_connect() || e.is_request() => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e).context("request failed"),
            }
        }
        Err(anyhow!(
            "request failed after retries ({host} unreachable): {err}",
            host = self.host,
            err = last_err.map(|e| e.to_string()).unwrap_or_else(|| "no error recorded".into())
        ))
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
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn http_date_parses_imf_fixdate() {
        let t = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(t.duration_since(UNIX_EPOCH).unwrap(), Duration::from_secs(784111777));
    }

    #[test]
    fn http_date_malformed_is_none() {
        assert!(parse_http_date("not a date").is_none());
        assert!(parse_http_date("").is_none());
        // hour out of range, day out of range
        assert!(parse_http_date("Sun, 06 Nov 1994 99:49:37 GMT").is_none());
        assert!(parse_http_date("Sun, 40 Nov 1994 08:49:37 GMT").is_none());
    }
}
