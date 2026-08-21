//! Blocking HTTP client that NIP-98-signs every control request.

use crate::auth::{self, Identity};
use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub struct Client {
    pub host: String,
    http: HttpClient,
    /// None = unsigned (used for token-only endpoints like inbox).
    id: Option<Identity>,
}

/// Minimal percent-encoding for query values (everything outside unreserved).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    pub fn new(host: &str, id: Option<Identity>) -> Result<Self> {
        let http = HttpClient::builder()
            .user_agent(concat!("fragment-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Client {
            host: host.trim_end_matches('/').to_string(),
            http,
            id,
        })
    }

    /// Absolute URL for a path (with optional query) — also the NIP-98 `u` tag.
    pub fn url(&self, path_and_query: &str) -> String {
        format!("{}{}", self.host, path_and_query)
    }

    /// Send a signed (or unsigned, when constructed without an identity) request.
    pub fn call(&self, method: &str, path_and_query: &str, body: Option<Vec<u8>>) -> Result<Response> {
        let url = self.url(path_and_query);
        let method = method.to_ascii_uppercase();
        let mut req = self
            .http
            .request(reqwest::Method::from_bytes(method.as_bytes())?, &url);
        if let Some(id) = &self.id {
            let header = auth::nip98_auth_header(id, &method, &url, body.as_deref(), auth::unix_now())?;
            req = req.header(AUTHORIZATION, header);
        }
        if let Some(b) = &body {
            req = req.header(CONTENT_TYPE, "application/octet-stream").body(b.clone());
        }
        req.send().with_context(|| format!("{method} {url} failed"))
    }

    pub fn call_json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
        let bytes = body.map(serde_json::to_vec).transpose()?;
        self.call(method, path, bytes)
    }

    /// Expect 2xx, parse JSON body.
    pub fn json<T: DeserializeOwned>(&self, method: &str, path: &str, body: Option<&Value>) -> Result<T> {
        let resp = self.call_json(method, path, body)?;
        expect_ok(resp, method, path)?.json().context("invalid JSON response")
    }

    /// Expect 2xx, return raw bytes + headers.
    pub fn bytes(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Result<(Vec<u8>, reqwest::header::HeaderMap)> {
        let resp = self.call(method, path, body)?;
        let resp = expect_ok(resp, method, path)?;
        let headers = resp.headers().clone();
        let bytes = resp.bytes()?.to_vec();
        Ok((bytes, headers))
    }
}

/// Map non-2xx to an error containing status + body; pass through 409 unchanged
/// (sync needs the `{currentRev}` body).
pub fn expect_ok(resp: Response, method: &str, path: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().unwrap_or_default();
    let short: String = body.chars().take(500).collect();
    if status == StatusCode::UNAUTHORIZED {
        return Err(anyhow!("{method} {path}: 401 unauthorized (bad or missing NIP-98 signature — run `fragment login`?)"));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(anyhow!("{method} {path}: 403 forbidden (your npub lacks the role for this fragment)"));
    }
    Err(anyhow!("{method} {path}: {status} {short}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding() {
        assert_eq!(url_encode("notes/today.md"), "notes%2Ftoday.md");
        assert_eq!(url_encode("a b&c.txt"), "a%20b%26c.txt");
        assert_eq!(url_encode("plain-1_ok~x.md"), "plain-1_ok~x.md");
    }
}
