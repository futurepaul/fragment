//! code.storage push webhooks: `X-Pierre-Signature: t=<unix>,sha256=<hex>`
//! where the hex is HMAC-SHA256(secret, "<t>.<body>"). The cell only
//! verifies; the code.storage fake signs (crates/fakes).

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

    /// A delivery signed outside this code: the hex is HMAC-SHA256 of
    /// `1790000000.<BODY>` under the secret `s3cret`, as OpenSSL computes
    /// it (`printf '%s' "1790000000.$BODY" | openssl dgst -sha256 -hmac
    /// s3cret`). crates/fakes signs to the same answer.
    const BODY: &[u8] = br#"{"ref":"refs/heads/main","before":"0","after":"1"}"#;
    const SIGNED: &str = "t=1790000000,sha256=4d38e01ea6039c2094e98395081b85c0efd6af8ae2740ebfab403ded420cb4b8";
    const T: i64 = 1_790_000_000;

    /// Goal: a delivery signed as code.storage signs one verifies, and one
    /// edit to its body, secret, timestamp, or header does not. Method:
    /// the known answer above, then one edit at a time; the window's edges
    /// with the signature intact, so time is the only fault.
    #[test]
    fn a_known_signature_verifies() {
        assert_eq!(verify(BODY, SIGNED, "s3cret", T + 10, 300), Ok(()));
        assert_eq!(verify(BODY, &format!("  {SIGNED}\n"), "s3cret", T, 300), Ok(()), "the header is trimmed");
        assert!(verify(&[BODY, b" "].concat(), SIGNED, "s3cret", T, 300).is_err(), "another body");
        assert!(verify(BODY, SIGNED, "s3cret ", T, 300).is_err(), "another secret");
        let moved = SIGNED.replace("t=1790000000", "t=1790000001");
        assert!(verify(BODY, &moved, "s3cret", T, 300).is_err(), "the timestamp is signed too");
        assert!(verify(BODY, &SIGNED.replace("sha256=4d", "sha256=4e"), "s3cret", T, 300).is_err(), "another signature");
        assert!(verify(BODY, SIGNED.split_once(',').unwrap().1, "s3cret", T, 300).is_err(), "no timestamp");
        assert!(verify(BODY, &SIGNED.replace("sha256=4d", "sha256=zz"), "s3cret", T, 300).is_err(), "not hex");
        // the window: its edges are inside it, a second past either is not
        assert_eq!(verify(BODY, SIGNED, "s3cret", T + 300, 300), Ok(()));
        assert_eq!(verify(BODY, SIGNED, "s3cret", T - 300, 300), Ok(()));
        assert!(verify(BODY, SIGNED, "s3cret", T + 301, 300).is_err());
        assert!(verify(BODY, SIGNED, "s3cret", T - 301, 300).is_err());
    }

    #[test]
    fn pushes() {
        let body = serde_json::json!({"ref": "refs/heads/live", "before": "a", "after": "b"});
        assert_eq!(parse_push("push", &body).unwrap().branch, "live");
        assert_eq!(parse_push("sync", &body), None);
        assert_eq!(parse_push("push", &serde_json::json!({"ref": "refs/tags/v1"})), None);
    }
}
