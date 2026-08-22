// Thin blocking HTTP client that signs every request with NIP-98.
use crate::auth::Identity;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

pub struct Client {
    pub host: String,
    pub id: Identity,
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
            http: reqwest::blocking::Client::new(),
        }
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
                    let rev = resp
                        .headers()
                        .get("x-fragment-rev")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok());
                    let bytes = resp.bytes().context("reading body")?.to_vec();
                    return Ok(Resp { status, body: bytes, rev });
                }
                Err(e) if e.is_connect() || e.is_request() => {
                    last_err = Some(e);
                    continue; // stale pool / transient network — try again
                }
                Err(e) => return Err(e).context("request failed"),
            }
        }
        let e = last_err.context("request failed (retried)")?;
        let _: reqwest::Error = e;
        unreachable!("retry loop always sets last_err before failing")
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

    /// Full control call with standard error handling: returns parsed JSON or an Err with the server's message.
    pub fn call(&self, resp: Resp) -> Result<Value> {
        if resp.ok() {
            resp.json()
        } else {
            Err(anyhow!("http {}: {}", resp.status, resp.err_summary()))
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
