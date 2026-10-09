//! An own key connected through its provider's sign-in (Paul, 2026-10-08:
//! "I don't want the user to need to use the cli or paste an api key
//! though, they should be able to connect them from settings ideally";
//! docs/computers.md, "Connections and operator keys"). Pure: the cell's
//! connections.rs and the person's Computer DO make the calls.
//!
//! The shape is OpenRouter's PKCE key exchange
//! (openrouter.ai/docs/guides/overview/auth/oauth), named by a catalog
//! row's `oauth` (`catalog::KeyOAuth`):
//!
//! 1. `POST /api/connections/{provider}/authorize`: the person's computer
//!    makes a nonce and a verifier, keeps them for `SIGNIN_TTL_MS` (at most
//!    `SIGNINS_PENDING_MAX` at once), and the person's browser is sent to
//!    the provider's `authorize` with the callback, the verifier's S256
//!    challenge, the state (`<computer's 24 hex>.<nonce>`) and a key label.
//! 2. The provider asks the person, makes a key that is theirs, and sends
//!    the browser back to `GET /api/connections/{provider}/callback?code=…
//!    &state=…`. The state names the computer and the sign-in it made, once:
//!    no session is needed, and a state replayed, expired or made for
//!    another provider is refused, nothing exchanged.
//! 3. The platform exchanges the code with the verifier (`POST exchange
//!    {code, code_verifier, code_challenge_method}` → `{key}`) and the
//!    computer seals the key as `PUT …/key` does. Nothing gives it back.

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::catalog::KeyOAuth;

/// A sign-in is finished within this, or not at all (OpenRouter's codes
/// live 10 minutes too).
pub const SIGNIN_TTL_MS: i64 = 10 * 60_000;
/// Sign-ins one computer keeps under way at once: a person starts one or
/// two; more are refused until the oldest end or expire.
pub const SIGNINS_PENDING_MAX: usize = 8;
/// Random bytes in a nonce, and in a verifier (43 characters as base64url:
/// RFC 7636's least).
pub const NONCE_BYTES: usize = 32;
pub const VERIFIER_BYTES: usize = 32;
/// The label the provider gives the key it makes (the person sees it in
/// its list of keys).
pub const KEY_LABEL: &str = "Fragment";
/// A code the provider sends back, at most.
pub const CODE_MAX_BYTES: usize = 512;

/// `bytes` as base64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 challenge of `verifier` (RFC 7636 4.2).
pub fn challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// What a computer keeps a sign-in under: the nonce's hash, so its storage
/// holds nothing a callback could be forged from.
pub fn nonce_key(nonce: &str) -> String {
    hex::encode(Sha256::digest(nonce.as_bytes()))
}

/// Whether `s` is a nonce or a verifier as made here: 43 base64url characters.
pub fn valid_token(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The state sent with an authorization: the computer it is for and its
/// nonce.
pub fn state(computer: &str, nonce: &str) -> String {
    assert!(fragment_proto::computer::valid_computer_id(computer) && valid_token(nonce), "a state is made from a computer's id and a nonce made here");
    format!("{}.{nonce}", &computer["computer:".len()..])
}

/// A state back from a provider: `(computer, nonce)`, or none if it is not
/// one `state` made.
pub fn parse_state(state: &str) -> Option<(String, String)> {
    let (hex, nonce) = state.split_once('.')?;
    let computer = format!("computer:{hex}");
    (fragment_proto::computer::valid_computer_id(&computer) && valid_token(nonce)).then(|| (computer, nonce.to_string()))
}

/// Whether `code` is one a provider may send back: 1 to `CODE_MAX_BYTES`
/// printable characters, no space.
pub fn valid_code(code: &str) -> bool {
    (1..=CODE_MAX_BYTES).contains(&code.len()) && code.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Where the person's browser goes: the row's `authorize`, its query kept,
/// with the callback, the challenge, its method, the state and the key's
/// label added.
pub fn authorize_url(o: &KeyOAuth, callback: &str, challenge: &str, state: &str) -> String {
    let mut u = url::Url::parse(&o.authorize).expect("a catalog's authorize is checked as a URL");
    u.query_pairs_mut()
        .append_pair("callback_url", callback)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("key_label", KEY_LABEL);
    u.to_string()
}

/// The exchange's body.
pub fn exchange_body(code: &str, verifier: &str) -> Value {
    json!({ "code": code, "code_verifier": verifier, "code_challenge_method": "S256" })
}

/// Why an exchange gave no key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    /// The provider refused the code (a 4xx): expired, spent, or not made
    /// with this verifier. The person signs in again.
    Refused(u16, String),
    /// The provider failed, or answered no key.
    Failed(String),
}

impl ExchangeError {
    pub fn message(&self) -> String {
        match self {
            ExchangeError::Refused(status, why) => format!("the provider refused the sign-in's code ({status}{}): connect again", if why.is_empty() { String::new() } else { format!(": {why}") }),
            ExchangeError::Failed(why) => format!("the provider gave no key: {why}"),
        }
    }
}

/// The key an exchange answered, checked as `PUT …/key` checks one.
pub fn key_of(status: u16, answer: &Value) -> Result<String, ExchangeError> {
    let said = |v: &Value| ["error", "message"].iter().find_map(|k| v[k].as_str().or(v["error"][k].as_str())).unwrap_or_default().chars().take(200).collect::<String>();
    match status {
        200 | 201 => {}
        400..=499 => return Err(ExchangeError::Refused(status, said(answer))),
        _ => return Err(ExchangeError::Failed(format!("status {status}"))),
    }
    let key = answer["key"].as_str().map(str::trim).unwrap_or_default();
    if key.is_empty() || key.len() > OWN_KEY_MAX_BYTES || !key.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(ExchangeError::Failed("its answer holds no key that is a printable token".into()));
    }
    Ok(key.to_string())
}

/// An own key, at most (a provider's API key is far shorter).
pub const OWN_KEY_MAX_BYTES: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    const COMPUTER: &str = "computer:0123456789abcdef01234567";

    fn token(c: char) -> String {
        std::iter::repeat_n(c, 43).collect()
    }

    /// The challenge is RFC 7636's (its appendix B's example).
    #[test]
    fn the_challenge_is_s256() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(nonce_key("a").len(), 64);
    }

    /// Valid: a state names its computer and nonce and reads back. Invalid:
    /// anything else, which names no sign-in.
    #[test]
    fn a_state_names_its_computer_and_nonce() {
        let s = state(COMPUTER, &token('a'));
        assert_eq!(s, format!("0123456789abcdef01234567.{}", token('a')));
        assert_eq!(parse_state(&s), Some((COMPUTER.to_string(), token('a'))));
        for bad in ["", ".", "0123456789abcdef01234567", &format!("0123456789abcdef0123456.{}", token('a')), &format!("0123456789abcdef01234567.{}", "a".repeat(42)), &format!("0123456789ABCDEF01234567.{}", token('a')), &format!("0123456789abcdef01234567.{}!", &token('a')[1..])] {
            assert_eq!(parse_state(bad), None, "{bad}");
        }
        assert!(valid_code("abc-123") && !valid_code("") && !valid_code("a b") && !valid_code(&"a".repeat(CODE_MAX_BYTES + 1)));
    }

    /// The authorization keeps the row's own query and adds the five.
    #[test]
    fn the_authorization_names_the_callback_challenge_and_state() {
        let o = KeyOAuth { authorize: "https://openrouter.ai/auth?ref=x".into(), exchange: "https://openrouter.ai/api/v1/auth/keys".into(), manage: "https://openrouter.ai/settings/keys".into() };
        let u = url::Url::parse(&authorize_url(&o, "https://fragment.test/api/connections/openrouter/callback", "ch", "st")).unwrap();
        let q: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let want = [("ref", "x"), ("callback_url", "https://fragment.test/api/connections/openrouter/callback"), ("code_challenge", "ch"), ("code_challenge_method", "S256"), ("state", "st"), ("key_label", KEY_LABEL)];
        assert_eq!(q, want.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>());
        assert_eq!((u.host_str(), u.path()), (Some("openrouter.ai"), "/auth"));
        assert_eq!(exchange_body("c", "v"), json!({ "code": "c", "code_verifier": "v", "code_challenge_method": "S256" }));
    }

    /// Valid: a 200 with a key. Invalid: a refusal (the person connects
    /// again), a failure, and an answer with no key that could be sent.
    #[test]
    fn an_exchange_answers_a_key_or_why_not() {
        assert_eq!(key_of(200, &json!({ "key": "sk-or-v1-abc" })), Ok("sk-or-v1-abc".into()));
        assert_eq!(key_of(403, &json!({ "error": { "message": "Invalid code or code_verifier" } })), Err(ExchangeError::Refused(403, "Invalid code or code_verifier".into())));
        assert!(key_of(403, &Value::Null).unwrap_err().message().contains("connect again"));
        assert!(matches!(key_of(502, &Value::Null), Err(ExchangeError::Failed(_))));
        for bad in [json!({}), json!({ "key": "" }), json!({ "key": "two words" }), json!({ "key": 5 }), json!({ "key": "k".repeat(OWN_KEY_MAX_BYTES + 1) })] {
            assert!(matches!(key_of(200, &bad), Err(ExchangeError::Failed(_))), "{bad}");
        }
    }
}
