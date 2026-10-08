//! OAuth 2.1 for MCP clients (docs/api.md, Connected clients): the
//! platform is the authorization server, and a connection a person makes
//! acts as them on one resource (an MCP server of the platform's). The pure
//! parts are here: what a client may register (RFC 7591) or declare in its
//! metadata document (a Client ID Metadata Document), where it may be sent
//! back to, PKCE (S256 only), and the requests' and answers' shapes. The
//! registry keeps the rows (cell/src/registry/oauth.rs); the router asks
//! the network (cell/src/oauth.rs).

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::{Host, Url};

/// An authorization code is good this long after the person said yes …
pub const CODE_TTL_MS: i64 = 5 * 60 * 1000;
/// … an access token this long …
pub const ACCESS_TTL_MS: i64 = 3600 * 1000;
/// … and a refresh token this long, renewed (rotated) at each use: a
/// connection unused for this long ends.
pub const REFRESH_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
/// A person's connections: past this many, the oldest ends.
pub const CONNECTIONS_PER_PERSON_MAX: u64 = 64;
/// A person's codes not yet exchanged: past this many, the oldest goes.
pub const CODES_PER_PERSON_MAX: u64 = 16;
/// Registered clients kept (RFC 7591): past this many, the oldest go. A
/// connection keeps its client's name, so it outlives its registration.
pub const CLIENTS_MAX: u64 = 100_000;
pub const REDIRECT_URIS_MAX: usize = 8;
pub const REDIRECT_URI_MAX_BYTES: usize = 512;
pub const CLIENT_NAME_MAX_CHARS: usize = 80;
/// A client's id when it is its metadata document's URL.
pub const CLIENT_ID_MAX_BYTES: usize = 512;
/// A client's metadata document (the CIMD draft's suggested cap).
pub const CLIENT_DOCUMENT_MAX_BYTES: usize = 5 * 1024;
/// A registration's body, and a token or revocation request's.
pub const REQUEST_MAX_BYTES: usize = 8 * 1024;
pub const STATE_MAX_BYTES: usize = 512;
/// An authorization request's query, all of it: a person signed out comes
/// back to it after signing in, and a way back is at most
/// `site::RETURN_PATH_MAX_BYTES` (its path is `/oauth/authorize?`).
pub const AUTHORIZE_QUERY_MAX_BYTES: usize = crate::site::RETURN_PATH_MAX_BYTES - "/oauth/authorize?".len();

/// An OAuth error code (RFC 6749 5.2, 4.1.2.1; RFC 7591 3.2.2; RFC 8707).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnsupportedGrantType,
    UnsupportedResponseType,
    InvalidTarget,
    AccessDenied,
    InvalidRedirectUri,
    InvalidClientMetadata,
}

impl Error {
    pub fn as_str(self) -> &'static str {
        match self {
            Error::InvalidRequest => "invalid_request",
            Error::InvalidClient => "invalid_client",
            Error::InvalidGrant => "invalid_grant",
            Error::UnsupportedGrantType => "unsupported_grant_type",
            Error::UnsupportedResponseType => "unsupported_response_type",
            Error::InvalidTarget => "invalid_target",
            Error::AccessDenied => "access_denied",
            Error::InvalidRedirectUri => "invalid_redirect_uri",
            Error::InvalidClientMetadata => "invalid_client_metadata",
        }
    }

    /// The status a token, registration, or revocation endpoint answers it with.
    pub fn status(self) -> u16 {
        match self {
            Error::InvalidClient => 401,
            _ => 400,
        }
    }
}

/// A refusal: its code, and why, in words a person reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub error: Error,
    pub why: String,
}

fn refused(error: Error, why: impl Into<String>) -> Refused {
    Refused { error, why: why.into() }
}

/// A client: what it calls itself (shown to the person who connects it;
/// a registered client's is its own word) and where it may be sent back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    pub name: String,
    pub redirect_uris: Vec<String>,
}

/// Whether a URL is this computer's (`localhost`, `127.0.0.1`, `[::1]`).
pub fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// A redirect URI a client may register: https, or http on this computer
/// (the MCP authorization spec's rule), with no fragment or credentials.
pub fn redirect_uri(raw: &str) -> Result<Url, String> {
    if raw.is_empty() || raw.len() > REDIRECT_URI_MAX_BYTES {
        return Err(format!("a redirect URI is 1 to {REDIRECT_URI_MAX_BYTES} bytes"));
    }
    let url = Url::parse(raw).map_err(|e| format!("{raw:?} is not a URL: {e}"))?;
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{raw:?}: a redirect URI has no fragment and no credentials"));
    }
    match url.scheme() {
        "https" if url.host().is_some() => Ok(url),
        "http" if loopback(&url) => Ok(url),
        _ => Err(format!("{raw:?}: a redirect URI is https, or http on this computer (localhost, 127.0.0.1, [::1])")),
    }
}

/// Whether `asked` is the registered redirect URI: exactly, or, on this
/// computer, on any port (a native client listens on whichever port is
/// free: RFC 8252 7.3).
pub fn redirect_matches(registered: &str, asked: &str) -> bool {
    if registered == asked {
        return true;
    }
    match (Url::parse(registered), Url::parse(asked)) {
        (Ok(r), Ok(a)) => {
            loopback(&r) && loopback(&a) && r.scheme() == a.scheme() && r.host() == a.host() && r.path() == a.path() && r.query() == a.query() && a.fragment().is_none()
        }
        _ => false,
    }
}

/// A client's name as a page shows it: trimmed, at most
/// `CLIENT_NAME_MAX_CHARS`, no control characters.
fn client_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > CLIENT_NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(format!("a client's name is 1 to {CLIENT_NAME_MAX_CHARS} characters, none of them control characters"));
    }
    Ok(name.to_string())
}

/// `redirect_uris` as a registration or a metadata document lists them.
fn redirect_uris(v: &Value) -> Result<Vec<String>, String> {
    let list = v.as_array().ok_or("redirect_uris is a list")?;
    if list.is_empty() || list.len() > REDIRECT_URIS_MAX {
        return Err(format!("redirect_uris lists 1 to {REDIRECT_URIS_MAX}"));
    }
    list.iter().map(|u| u.as_str().ok_or_else(|| "a redirect URI is a string".to_string()).and_then(|u| redirect_uri(u).map(|_| u.to_string()))).collect()
}

/// A dynamic registration's metadata (RFC 7591), as this platform takes
/// it: redirect URIs, a name (else the first redirect URI's host), and a
/// public client of the authorization code grant, whatever else it asks:
/// the answer says what was registered.
pub fn registration(body: &Value) -> Result<Client, Refused> {
    let o = body.as_object().ok_or_else(|| refused(Error::InvalidClientMetadata, "a registration is a JSON object"))?;
    let uris = redirect_uris(o.get("redirect_uris").unwrap_or(&Value::Null)).map_err(|why| refused(Error::InvalidRedirectUri, why))?;
    let listed = |key: &str, wanted: &str| match o.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::Array(a)) => a.iter().any(|v| v == wanted),
        Some(_) => false,
    };
    if !listed("grant_types", "authorization_code") || !listed("response_types", "code") {
        return Err(refused(Error::InvalidClientMetadata, "a client here uses the authorization code grant (response type code)"));
    }
    let host = Url::parse(&uris[0]).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let name = match o.get("client_name") {
        None | Some(Value::Null) => client_name(&host),
        Some(Value::String(s)) => client_name(s),
        Some(_) => Err("client_name is a string".into()),
    }
    .map_err(|why| refused(Error::InvalidClientMetadata, why))?;
    Ok(Client { name, redirect_uris: uris })
}

/// A client id that is a URL to its metadata document (CIMD): https with
/// a path, or, on a fleet that allows local egress (dev and the e2e), http
/// on this computer. Any other id is a registered client's.
pub fn client_id_url(raw: &str, allow_local: bool) -> Option<Url> {
    if raw.len() > CLIENT_ID_MAX_BYTES {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let fits = match url.scheme() {
        "https" => url.host().is_some(),
        "http" => allow_local && loopback(&url),
        _ => false,
    };
    (fits && url.path() != "/" && url.fragment().is_none() && url.username().is_empty() && url.password().is_none()).then_some(url)
}

/// A client's metadata document, fetched from `client_id`: JSON naming
/// that same URL, its name, and its redirect URIs (the CIMD draft; the MCP
/// authorization spec requires the name).
pub fn client_document(client_id: &str, body: &[u8]) -> Result<Client, Refused> {
    let bad = |why: String| refused(Error::InvalidClient, format!("{client_id}: {why}"));
    if body.len() > CLIENT_DOCUMENT_MAX_BYTES {
        return Err(bad(format!("a client's metadata document is at most {CLIENT_DOCUMENT_MAX_BYTES} bytes")));
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| bad(format!("not JSON: {e}")))?;
    if v["client_id"].as_str() != Some(client_id) {
        return Err(bad("its client_id is not its own URL".into()));
    }
    let name = v["client_name"].as_str().ok_or_else(|| bad("it names no client_name".into())).and_then(|n| client_name(n).map_err(bad))?;
    let redirect_uris = redirect_uris(&v["redirect_uris"]).map_err(bad)?;
    Ok(Client { name, redirect_uris })
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// An S256 code challenge: 43 characters of unpadded base64url.
pub fn valid_challenge(c: &str) -> bool {
    c.len() == 43 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A code verifier (RFC 7636 4.1): 43 to 128 unreserved characters.
pub fn valid_verifier(v: &str) -> bool {
    (43..=128).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// The S256 challenge of a verifier.
pub fn challenge_of(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// Whether `verifier` is the one `challenge` was made from, compared in
/// constant time.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let made = challenge_of(verifier);
    valid_verifier(verifier) && made.len() == challenge.len() && made.bytes().zip(challenge.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A request's parameter, sent at most once (RFC 6749 3.1).
pub fn one<'a>(pairs: &'a [(String, String)], key: &str) -> Result<Option<&'a str>, Refused> {
    let mut found = pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let first = found.next();
    if found.next().is_some() {
        return Err(refused(Error::InvalidRequest, format!("{key} is sent once")));
    }
    Ok(first.filter(|v| !v.is_empty()))
}

fn required<'a>(pairs: &'a [(String, String)], key: &str) -> Result<&'a str, Refused> {
    one(pairs, key)?.ok_or_else(|| refused(Error::InvalidRequest, format!("{key} is required")))
}

/// An authorization request's asks, past its client and redirect URI
/// (checked first: a refusal of those is shown, never sent back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub challenge: String,
    /// The resource indicator (RFC 8707): the MCP server the connection is for.
    pub resource: String,
    pub state: Option<String>,
}

/// The rest of an authorization request: the code flow, PKCE with S256,
/// one resource, and a bounded state. `scope` is not read: a connection
/// acts on its resource as its person may.
pub fn asked(pairs: &[(String, String)]) -> Result<Asked, Refused> {
    if required(pairs, "response_type")? != "code" {
        return Err(refused(Error::UnsupportedResponseType, "response_type is code"));
    }
    let challenge = required(pairs, "code_challenge")?;
    if one(pairs, "code_challenge_method")? != Some("S256") || !valid_challenge(challenge) {
        return Err(refused(Error::InvalidRequest, "PKCE is required: code_challenge_method S256, and its 43-character code_challenge"));
    }
    let resource = one(pairs, "resource")?.ok_or_else(|| refused(Error::InvalidTarget, "resource names the MCP server the connection is for (RFC 8707)"))?;
    let state = one(pairs, "state")?;
    if state.is_some_and(|s| s.len() > STATE_MAX_BYTES) {
        return Err(refused(Error::InvalidRequest, format!("state is at most {STATE_MAX_BYTES} bytes")));
    }
    Ok(Asked { challenge: challenge.to_string(), resource: resource.to_string(), state: state.map(str::to_string) })
}

/// A resource indicator, canonical: an absolute URL without a query or
/// fragment, its scheme and host lowercase, and no trailing slash but the
/// root's. `None` is no resource.
pub fn resource(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw).ok()?;
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() || url.host().is_none() {
        return None;
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Some(url)
}

/// `redirect_uri` with `params` added to its query, as an authorization
/// response is sent back.
pub fn redirect_with(redirect_uri: &str, params: &[(&str, &str)]) -> String {
    let mut url = Url::parse(redirect_uri).expect("a redirect URI was checked when it was registered");
    url.query_pairs_mut().extend_pairs(params);
    url.into()
}

/// A token request (RFC 6749 4.1.3, 6), from a public client: its
/// `client_id` in the body, no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Grant {
    Code { code: String, client_id: String, redirect_uri: String, verifier: String, resource: Option<String> },
    Refresh { refresh: String, client_id: String, resource: Option<String> },
}

/// A token request's form, read.
pub fn token_request(pairs: &[(String, String)]) -> Result<Grant, Refused> {
    let client_id = required(pairs, "client_id").map_err(|_| refused(Error::InvalidClient, "client_id is required: a client here is public, and names itself"))?.to_string();
    let resource = one(pairs, "resource")?.map(str::to_string);
    match required(pairs, "grant_type")? {
        "authorization_code" => {
            let verifier = required(pairs, "code_verifier")?;
            if !valid_verifier(verifier) {
                return Err(refused(Error::InvalidRequest, "code_verifier is 43 to 128 unreserved characters"));
            }
            Ok(Grant::Code {
                code: required(pairs, "code")?.to_string(),
                client_id,
                redirect_uri: required(pairs, "redirect_uri")?.to_string(),
                verifier: verifier.to_string(),
                resource,
            })
        }
        "refresh_token" => Ok(Grant::Refresh { refresh: required(pairs, "refresh_token")?.to_string(), client_id, resource }),
        other => Err(refused(Error::UnsupportedGrantType, format!("{other:?}: the grants are authorization_code and refresh_token"))),
    }
}

/// A token answer (RFC 6749 5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: String,
}

impl Tokens {
    pub fn bearer(access_token: String, refresh_token: String) -> Tokens {
        Tokens { access_token, token_type: "Bearer".into(), expires_in: ACCESS_TTL_MS / 1000, refresh_token }
    }
}

/// The authorization server's metadata (RFC 8414) for an issuer (the
/// platform's origin, which is where its endpoints are).
pub fn metadata(issuer: &str) -> Value {
    serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(q: &[(&str, &str)]) -> Vec<(String, String)> {
        q.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn redirect_uris_are_https_or_this_computers() {
        for good in ["https://claude.ai/api/mcp/auth_callback", "http://localhost:3118/callback", "http://127.0.0.1/cb", "http://[::1]:9/cb", "https://x.example/cb?a=1"] {
            assert!(redirect_uri(good).is_ok(), "{good}");
        }
        for bad in ["http://claude.ai/cb", "cursor://anysphere/cb", "https://x.example/cb#f", "https://u:p@x.example/cb", "", "not a url", "javascript:alert(1)"] {
            assert!(redirect_uri(bad).is_err(), "{bad}");
        }
        assert!(redirect_uri(&format!("https://x.example/{}", "a".repeat(REDIRECT_URI_MAX_BYTES))).is_err(), "too long");
    }

    #[test]
    fn a_loopback_redirect_matches_on_any_port() {
        assert!(redirect_matches("https://claude.ai/api/mcp/auth_callback", "https://claude.ai/api/mcp/auth_callback"));
        assert!(redirect_matches("http://localhost/callback", "http://localhost:3118/callback"));
        assert!(redirect_matches("http://127.0.0.1:1/callback", "http://127.0.0.1:55001/callback"));
        assert!(!redirect_matches("http://localhost/callback", "http://127.0.0.1:3118/callback"), "another host");
        assert!(!redirect_matches("http://localhost/callback", "http://localhost:3118/other"), "another path");
        assert!(!redirect_matches("https://claude.ai/cb", "https://claude.ai:8443/cb"), "a port matters off this computer");
        assert!(!redirect_matches("https://claude.ai/cb", "https://evil.example/cb"));
    }

    #[test]
    fn a_registration_takes_a_public_code_client() {
        let c = registration(&json!({ "client_name": " Claude ", "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "token_endpoint_auth_method": "client_secret_post" })).unwrap();
        assert_eq!(c, Client { name: "Claude".into(), redirect_uris: vec!["https://claude.ai/api/mcp/auth_callback".into()] });
        let unnamed = registration(&json!({ "redirect_uris": ["http://localhost:9/cb"], "grant_types": ["authorization_code", "refresh_token"] })).unwrap();
        assert_eq!(unnamed.name, "localhost", "no name: its redirect URI's host");
        let error = |v: Value| registration(&v).unwrap_err().error;
        assert_eq!(error(json!({ "client_name": "x" })), Error::InvalidRedirectUri);
        assert_eq!(error(json!({ "redirect_uris": [] })), Error::InvalidRedirectUri);
        assert_eq!(error(json!({ "redirect_uris": ["http://evil.example/cb"] })), Error::InvalidRedirectUri);
        assert_eq!(error(json!({ "redirect_uris": vec!["https://x.example/cb"; REDIRECT_URIS_MAX + 1] })), Error::InvalidRedirectUri);
        assert_eq!(error(json!({ "redirect_uris": ["https://x.example/cb"], "grant_types": ["client_credentials"] })), Error::InvalidClientMetadata);
        assert_eq!(error(json!({ "redirect_uris": ["https://x.example/cb"], "client_name": "a\u{7}b" })), Error::InvalidClientMetadata);
        assert_eq!(error(json!({ "redirect_uris": ["https://x.example/cb"], "client_name": "x".repeat(CLIENT_NAME_MAX_CHARS + 1) })), Error::InvalidClientMetadata);
        assert_eq!(error(json!(["https://x.example/cb"])), Error::InvalidClientMetadata);
    }

    #[test]
    fn a_metadata_document_names_its_own_url() {
        let id = "https://claude.ai/oauth/claude-code-client-metadata";
        assert!(client_id_url(id, false).is_some());
        assert!(client_id_url("https://claude.ai/", false).is_none(), "no path");
        assert!(client_id_url("http://127.0.0.1:9/client.json", false).is_none(), "http only where local egress is allowed");
        assert!(client_id_url("http://127.0.0.1:9/client.json", true).is_some());
        assert!(client_id_url("http://example.com/client.json", true).is_none(), "http only on this computer");
        assert!(client_id_url("4a1c0f", false).is_none(), "a registered client's id");
        let doc = |v: Value| v.to_string().into_bytes();
        let ok = client_document(id, &doc(json!({ "client_id": id, "client_name": "Claude Code", "redirect_uris": ["http://localhost/callback", "http://127.0.0.1/callback"] }))).unwrap();
        assert_eq!(ok.name, "Claude Code");
        assert_eq!(ok.redirect_uris.len(), 2);
        let error = |b: &[u8]| client_document(id, b).unwrap_err().error;
        assert_eq!(error(&doc(json!({ "client_id": "https://evil.example/c", "client_name": "x", "redirect_uris": ["https://x.example/cb"] }))), Error::InvalidClient);
        assert_eq!(error(&doc(json!({ "client_id": id, "redirect_uris": ["https://x.example/cb"] }))), Error::InvalidClient);
        assert_eq!(error(&doc(json!({ "client_id": id, "client_name": "x", "redirect_uris": ["http://x.example/cb"] }))), Error::InvalidClient);
        assert_eq!(error(b"<html>"), Error::InvalidClient);
        assert_eq!(error(&vec![b' '; CLIENT_DOCUMENT_MAX_BYTES + 1]), Error::InvalidClient);
    }

    #[test]
    fn pkce_is_s256() {
        // RFC 7636 appendix B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(challenge_of(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert!(pkce_matches(verifier, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_matches(verifier, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cN"));
        assert!(!pkce_matches("short", &challenge_of("short")), "a verifier too short never matches");
        assert!(valid_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM") && !valid_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-c="));
        assert!(!valid_verifier(&"a".repeat(129)) && valid_verifier(&"a".repeat(128)));
    }

    #[test]
    fn an_authorization_request_asks_for_one_resource_with_pkce() {
        let base = [("response_type", "code"), ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"), ("code_challenge_method", "S256"), ("resource", "https://todo--p.fragment.boats/__mcp"), ("state", "s1")];
        let a = asked(&pairs(&base)).unwrap();
        assert_eq!((a.resource.as_str(), a.state.as_deref()), ("https://todo--p.fragment.boats/__mcp", Some("s1")));
        let without = |key: &str| asked(&pairs(&base.iter().copied().filter(|(k, _)| *k != key).collect::<Vec<_>>())).unwrap_err().error;
        assert_eq!(without("code_challenge"), Error::InvalidRequest);
        assert_eq!(without("code_challenge_method"), Error::InvalidRequest, "plain PKCE is refused");
        assert_eq!(without("resource"), Error::InvalidTarget);
        assert_eq!(without("response_type"), Error::InvalidRequest);
        assert!(asked(&pairs(&[&base[..], &[("state", "s2")]].concat())).is_err(), "a parameter sent twice");
        let mut token = pairs(&base);
        token[0].1 = "token".into();
        assert_eq!(asked(&token).unwrap_err().error, Error::UnsupportedResponseType);
        let mut long = pairs(&base);
        long[4].1 = "s".repeat(STATE_MAX_BYTES + 1);
        assert_eq!(asked(&long).unwrap_err().error, Error::InvalidRequest);
    }

    #[test]
    fn a_resource_is_canonical() {
        assert_eq!(resource("HTTPS://Todo--P.Fragment.Boats/__mcp/").unwrap().as_str(), "https://todo--p.fragment.boats/__mcp");
        assert_eq!(resource("https://fragment.club/mcp").unwrap().as_str(), "https://fragment.club/mcp");
        for bad in ["https://fragment.club/mcp?x=1", "https://fragment.club/mcp#f", "mcp", "https://u@fragment.club/mcp"] {
            assert!(resource(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_token_request_is_a_code_or_a_refresh() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let code = token_request(&pairs(&[("grant_type", "authorization_code"), ("code", "c"), ("client_id", "x"), ("redirect_uri", "https://x.example/cb"), ("code_verifier", verifier)])).unwrap();
        assert!(matches!(code, Grant::Code { resource: None, .. }));
        let refresh = token_request(&pairs(&[("grant_type", "refresh_token"), ("refresh_token", "r"), ("client_id", "x"), ("resource", "https://fragment.club/mcp")])).unwrap();
        assert!(matches!(refresh, Grant::Refresh { resource: Some(_), .. }));
        assert_eq!(token_request(&pairs(&[("grant_type", "refresh_token"), ("refresh_token", "r")])).unwrap_err().error, Error::InvalidClient);
        assert_eq!(token_request(&pairs(&[("grant_type", "client_credentials"), ("client_id", "x")])).unwrap_err().error, Error::UnsupportedGrantType);
        assert_eq!(token_request(&pairs(&[("grant_type", "authorization_code"), ("code", "c"), ("client_id", "x"), ("redirect_uri", "u"), ("code_verifier", "short")])).unwrap_err().error, Error::InvalidRequest);
    }

    #[test]
    fn an_answer_goes_back_on_the_redirect_uris_query() {
        assert_eq!(redirect_with("https://x.example/cb?a=1", &[("code", "c d"), ("state", "s")]), "https://x.example/cb?a=1&code=c+d&state=s");
        let m = metadata("https://fragment.club");
        assert_eq!(m["token_endpoint"], "https://fragment.club/oauth/token");
        assert_eq!(m["code_challenge_methods_supported"], json!(["S256"]));
        assert!(AUTHORIZE_QUERY_MAX_BYTES + "/oauth/authorize?".len() <= crate::site::RETURN_PATH_MAX_BYTES);
    }
}
