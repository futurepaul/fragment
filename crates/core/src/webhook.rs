//! code.storage push webhooks: `X-Pierre-Signature: t=<unix>,sha256=<hex>`
//! where the hex is HMAC-SHA256(secret, "<t>.<body>").

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

/// Checks a delivery's signature and freshness.
pub fn verify(body: &[u8], header: &str, secret: &str, now_s: i64, window_s: i64) -> Result<(), String> {
    let header = header.trim();
    let (t, mac) = header
        .strip_prefix("t=")
        .and_then(|rest| rest.split_once(",sha256="))
        .ok_or("the signature header is not t=<unix>,sha256=<hex>")?;
    let t: i64 = t.parse().map_err(|_| "the signature timestamp is not a number")?;
    if (now_s - t).abs() > window_s {
        return Err(format!("the delivery is {} s from now; the window is {window_s} s", now_s - t));
    }
    let expected = hex::decode(mac).map_err(|_| "the signature is not hex")?;
    let mut h = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    h.update(t.to_string().as_bytes());
    h.update(b".");
    h.update(body);
    h.verify_slice(&expected).map_err(|_| "the signature does not match".to_string())
}

/// Signs a delivery (the fake's side, and tests).
pub fn sign(body: &[u8], secret: &str, t: i64) -> String {
    let mut h = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    h.update(t.to_string().as_bytes());
    h.update(b".");
    h.update(body);
    format!("t={t},sha256={}", hex::encode(h.finalize().into_bytes()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    pub branch: String,
    pub before: String,
    pub after: String,
}

/// A branch push, or `None` for any other event (ignored, not refused).
pub fn parse_push(event: &str, body: &Value) -> Option<Push> {
    if event != "push" {
        return None;
    }
    let branch = body["ref"].as_str()?.strip_prefix("refs/heads/")?;
    Some(Push {
        branch: branch.to_string(),
        before: body["before"].as_str().unwrap_or("").to_string(),
        after: body["after"].as_str().unwrap_or("").to_string(),
    })
}

/// What makes two deliveries the same delivery (redeliveries are acked,
/// not interpreted again).
pub fn dedupe_key(event: &str, body: &Value) -> String {
    let s = |k: &str| body[k].as_str().unwrap_or("").to_string();
    format!(
        "{event}|{}|{}|{}|{}|{}",
        body["repository"]["url"].as_str().unwrap_or(""),
        s("ref"),
        s("before"),
        s("after"),
        s("pushed_at")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify() {
        let h = sign(b"{}", "s3cret", 1000);
        assert!(verify(b"{}", &h, "s3cret", 1010, 300).is_ok());
        assert!(verify(b"{} ", &h, "s3cret", 1010, 300).is_err());
        assert!(verify(b"{}", &h, "other", 1010, 300).is_err());
        assert!(verify(b"{}", &h, "s3cret", 1400, 300).unwrap_err().contains("window"));
        assert!(verify(b"{}", "sha256=00", "s3cret", 1000, 300).is_err());
    }

    #[test]
    fn pushes() {
        let body = serde_json::json!({"ref": "refs/heads/live", "before": "a", "after": "b"});
        assert_eq!(parse_push("push", &body).unwrap().branch, "live");
        assert_eq!(parse_push("sync", &body), None);
        assert_eq!(parse_push("push", &serde_json::json!({"ref": "refs/tags/v1"})), None);
    }
}
