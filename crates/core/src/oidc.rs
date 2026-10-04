//! Sign-in on OpenID Connect (docs/self-host.md, seam 4): the rules, with
//! no I/O. The cell (`cell/src/oidc.rs`, `registry/signin.rs`) fetches the
//! provider's metadata and keys and asks these functions what they mean.
//!
//! - **Discovery** (OpenID Connect Discovery 1.0): `<issuer>/.well-known/
//!   openid-configuration`, its `issuer` exactly the configured one (4.3),
//!   its endpoints https (http only beside an issuer on loopback: dev).
//! - **The request**: an authorization code with PKCE (S256, RFC 7636), a
//!   state the router binds to the browser, and a nonce the id_token must
//!   carry back.
//! - **The token request**: form-encoded (RFC 6749 4.1.3), the client
//!   authenticated with `client_secret_basic` (2.3.1: id and secret each
//!   form-encoded before base64), `client_secret_post`, or not at all (a
//!   public client, PKCE alone).
//! - **The id_token**: a compact JWS signed RS256 or ES256 by a key in the
//!   provider's JWKS (never a key the token names itself, never `none` or an
//!   HMAC), then `iss`, `aud` (with `azp`), `exp`, `iat` and `nbf` within
//!   `SKEW_S`, `nonce`, and `sub` (OpenID Connect Core 1.0, 3.1.3.7).
//! - **Who it is**: `(issuer URL, sub)`; the claim map says which claims
//!   are the email, the name and the username shown, and an email is never
//!   assumed: the label falls back to `preferred_username`, `upn`, then
//!   `sub`.
//! - **Caches**: the cell keeps the metadata and the JWKS per isolate for
//!   at most `METADATA_TTL_MS`, and fetches the JWKS again for a key it
//!   does not hold at most once per `JWKS_REFETCH_MIN_MS` (`may_refetch`),
//!   so a provider naming keys it never published is not asked again and
//!   again.

use base64::Engine;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The largest id_token read (Entra's, with groups, run to a few KiB).
pub const ID_TOKEN_MAX_BYTES: usize = 16 * 1024;
/// The largest discovery document or JWKS read.
pub const METADATA_MAX_BYTES: usize = 64 * 1024;
/// Keys one JWKS may hold; a larger set is refused whole.
pub const JWKS_KEYS_MAX: usize = 32;
/// The clock skew allowed between the provider and the cell.
pub const SKEW_S: i64 = 120;
/// How old an id_token may be when it arrives: it is minted at the token
/// request, moments before, and a sign-in lasts ten minutes at most.
pub const ID_TOKEN_AGE_MAX_S: i64 = 15 * 60;
/// RSA keys outside these sizes are not used (4096 is `rsa`'s own cap).
pub const RSA_BITS_MIN: usize = 2048;
pub const RSA_BITS_MAX: usize = 4096;
/// A subject's length (OpenID Connect Core 1.0, 2: at most 255 ASCII).
pub const SUBJECT_MAX: usize = 255;
/// An email, name or username kept from a claim (longer ones are dropped).
pub const CLAIM_MAX: usize = 320;
/// Claims a claim map may name for a username.
pub const USERNAME_CLAIMS_MAX: usize = 8;
/// How long the cell keeps the provider's metadata and keys.
pub const METADATA_TTL_MS: i64 = 60 * 60 * 1000;
/// The shortest wait between two fetches of the JWKS for a key it lacks.
/// Only the provider's own token endpoint hands the cell an id_token (for a
/// code and a verifier), so a `kid` the cell lacks is the provider's doing:
/// this bounds a provider misbehaving to four fetches a minute per isolate,
/// and a rotation (Dex signs with a new key the moment it publishes it) is
/// picked up within as long.
pub const JWKS_REFETCH_MIN_MS: i64 = 15 * 1000;
/// The scopes asked for when the deployment names none.
pub const DEFAULT_SCOPES: &str = "openid email profile";
const SCOPES_MAX: usize = 512;
/// A PKCE verifier's length bounds (RFC 7636, 4.1).
const VERIFIER_MIN: usize = 43;
const VERIFIER_MAX: usize = 128;

const _: () = assert!(RSA_BITS_MIN <= RSA_BITS_MAX && SKEW_S > 0 && JWKS_REFETCH_MIN_MS < METADATA_TTL_MS);

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// What a provider, its answer, or a deployment's settings got wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OidcError {
    /// A setting the deployment gave is not usable (it names which, and why).
    Config(String),
    /// The provider's metadata or keys are not what the specs say.
    Provider(String),
    /// The token endpoint refused the code (its `error` and description).
    Refused { status: u16, error: String, description: String },
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OidcError::Config(why) => write!(f, "the sign-in settings: {why}"),
            OidcError::Provider(why) => write!(f, "the sign-in provider: {why}"),
            OidcError::Refused { status, error, description } => write!(f, "the sign-in provider refused the code ({status} {error}): {description}"),
        }
    }
}

fn provider_err(why: impl Into<String>) -> OidcError {
    OidcError::Provider(why.into())
}

/// Whether `host` is this machine's own (a dev provider may be plain http there).
fn loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost")
}

/// An issuer the deployment may name: an https URL with no query or
/// fragment, or http on loopback (a dev provider, the fake). The string is
/// kept as given: `iss` is compared to it exactly.
pub fn check_issuer(issuer: &str) -> Result<(), OidcError> {
    let bad = |why: &str| Err(OidcError::Config(format!("FRAGMENT_OIDC_ISSUER {issuer:?} {why}")));
    let Ok(u) = url::Url::parse(issuer) else { return bad("is not a URL") };
    if u.query().is_some() || u.fragment().is_some() || !u.username().is_empty() || u.password().is_some() {
        return bad("has a query, a fragment, or credentials");
    }
    match (u.scheme(), u.host_str()) {
        ("https", Some(_)) => Ok(()),
        ("http", Some(h)) if loopback(h) => Ok(()),
        _ => bad("is not https (http only on loopback)"),
    }
}

/// Where an issuer's metadata is: its URL less a trailing `/`, then
/// `/.well-known/openid-configuration` (Discovery 1.0, 4).
pub fn discovery_url(issuer: &str) -> String {
    format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'))
}

/// How the client proves itself at the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuth {
    /// HTTP Basic, the id and secret form-encoded first (RFC 6749, 2.3.1).
    Basic,
    /// The id and secret in the form.
    Post,
    /// A public client: its id in the form, PKCE its only proof.
    None,
}

impl ClientAuth {
    /// The deployment's `FRAGMENT_OIDC_AUTH`, by its registered name.
    pub fn parse(s: &str) -> Result<ClientAuth, OidcError> {
        match s {
            "client_secret_basic" => Ok(ClientAuth::Basic),
            "client_secret_post" => Ok(ClientAuth::Post),
            "none" => Ok(ClientAuth::None),
            other => Err(OidcError::Config(format!("FRAGMENT_OIDC_AUTH is client_secret_basic, client_secret_post or none, not {other:?}"))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ClientAuth::Basic => "client_secret_basic",
            ClientAuth::Post => "client_secret_post",
            ClientAuth::None => "none",
        }
    }
}

/// A signature algorithm the cell verifies. Nothing else is: not `none`,
/// not an HMAC (whose key would be the client secret, or a public key
/// misread as one), not a key the token carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Es256,
}

impl Alg {
    pub fn parse(s: &str) -> Option<Alg> {
        match s {
            "RS256" => Some(Alg::Rs256),
            "ES256" => Some(Alg::Es256),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Alg::Rs256 => "RS256",
            Alg::Es256 => "ES256",
        }
    }
}

/// A provider's metadata (Discovery 1.0, 3), as much as sign-in reads,
/// checked (`Provider::parse`).
#[derive(Debug, Clone)]
pub struct Provider {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    /// RP-initiated logout's (RP-Initiated Logout 1.0), when it has one.
    pub end_session_endpoint: Option<String>,
    /// The algorithms both it and the cell know, its preference first.
    pub algs: Vec<Alg>,
    /// Its token endpoint's client authentication methods (`None`: it
    /// lists none, so `client_secret_basic`, the default).
    auth_methods: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ProviderDoc {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    id_token_signing_alg_values_supported: Option<Vec<String>>,
    #[serde(default)]
    code_challenge_methods_supported: Option<Vec<String>>,
}

impl Provider {
    /// The metadata `issuer` answered, checked: its `issuer` is exactly
    /// the configured one (Discovery 1.0, 4.3: else another provider could
    /// stand in), its endpoints are https URLs (http only beside a loopback
    /// issuer), it signs id_tokens with an algorithm the cell verifies, and
    /// it does not refuse PKCE's S256.
    pub fn parse(issuer: &str, bytes: &[u8]) -> Result<Provider, OidcError> {
        if bytes.len() > METADATA_MAX_BYTES {
            return Err(provider_err(format!("its metadata is over {METADATA_MAX_BYTES} bytes")));
        }
        let doc: ProviderDoc = serde_json::from_slice(bytes).map_err(|e| provider_err(format!("its metadata is not OpenID Connect discovery JSON: {e}")))?;
        if doc.issuer != issuer {
            return Err(provider_err(format!("its metadata names the issuer {:?}, not {issuer:?} (they must match exactly)", doc.issuer)));
        }
        let dev = url::Url::parse(issuer).ok().is_some_and(|u| u.scheme() == "http");
        let endpoint = |name: &str, raw: &str| -> Result<String, OidcError> {
            let u = url::Url::parse(raw).map_err(|_| provider_err(format!("its {name} {raw:?} is not a URL")))?;
            let ok = match u.scheme() {
                "https" => true,
                "http" => dev && u.host_str().is_some_and(loopback),
                _ => false,
            };
            if !ok || u.fragment().is_some() {
                return Err(provider_err(format!("its {name} {raw:?} is not an https URL")));
            }
            Ok(raw.to_string())
        };
        let algs: Vec<Alg> = match &doc.id_token_signing_alg_values_supported {
            Some(listed) => listed.iter().filter_map(|a| Alg::parse(a)).collect(),
            // REQUIRED in the metadata; RS256 is every provider's default (Core 1.0, 15.1)
            None => vec![Alg::Rs256],
        };
        if algs.is_empty() {
            return Err(provider_err("it signs id_tokens with neither RS256 nor ES256"));
        }
        if doc.code_challenge_methods_supported.as_ref().is_some_and(|m| !m.iter().any(|m| m == "S256")) {
            return Err(provider_err("it lists PKCE methods without S256"));
        }
        Ok(Provider {
            issuer: doc.issuer.clone(),
            authorization_endpoint: endpoint("authorization_endpoint", &doc.authorization_endpoint)?,
            token_endpoint: endpoint("token_endpoint", &doc.token_endpoint)?,
            jwks_uri: endpoint("jwks_uri", &doc.jwks_uri)?,
            end_session_endpoint: doc.end_session_endpoint.as_deref().map(|e| endpoint("end_session_endpoint", e)).transpose()?,
            algs,
            auth_methods: doc.token_endpoint_auth_methods_supported,
        })
    }

    /// How the client authenticates: as configured, else, holding a
    /// secret, the first of Basic and Post the provider lists (Basic when it
    /// lists none, the spec's default), and with no secret, not at all.
    pub fn client_auth(&self, configured: Option<ClientAuth>, has_secret: bool) -> Result<ClientAuth, OidcError> {
        let chosen = match configured {
            Some(c) => c,
            None if !has_secret => ClientAuth::None,
            None => {
                let listed = self.auth_methods.clone().unwrap_or_else(|| vec!["client_secret_basic".into()]);
                [ClientAuth::Basic, ClientAuth::Post]
                    .into_iter()
                    .find(|a| listed.iter().any(|l| l == a.name()))
                    .ok_or_else(|| provider_err(format!("its token endpoint takes none of client_secret_basic, client_secret_post (it lists {listed:?})")))?
            }
        };
        if chosen != ClientAuth::None && !has_secret {
            return Err(OidcError::Config(format!("{} needs the client secret (FRAGMENT_OIDC_CLIENT_SECRET)", chosen.name())));
        }
        Ok(chosen)
    }
}

/// A PKCE verifier as the cell makes one (64 hex: 32 random bytes), or
/// any RFC 7636 verifier.
pub fn valid_verifier(v: &str) -> bool {
    (VERIFIER_MIN..=VERIFIER_MAX).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// PKCE's S256 challenge for a verifier: BASE64URL(SHA-256(verifier)).
pub fn challenge(verifier: &str) -> String {
    assert!(valid_verifier(verifier), "a PKCE verifier is 43 to 128 unreserved characters");
    b64().encode(Sha256::digest(verifier.as_bytes()))
}

/// The scopes a deployment asks for: `FRAGMENT_OIDC_SCOPES`, or
/// `DEFAULT_SCOPES`. `openid` must be among them (else it is OAuth, and no
/// id_token comes back).
pub fn scopes(configured: Option<&str>) -> Result<String, OidcError> {
    let s = configured.unwrap_or(DEFAULT_SCOPES).split_whitespace().collect::<Vec<_>>().join(" ");
    if s.len() > SCOPES_MAX || !s.split(' ').any(|x| x == "openid") || !s.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\' || b == b' ') {
        return Err(OidcError::Config(format!("FRAGMENT_OIDC_SCOPES is space-separated scopes, `openid` among them, at most {SCOPES_MAX} bytes")));
    }
    Ok(s)
}

/// The authorization request (Core 1.0, 3.1.2.1, with PKCE).
pub struct Authorize<'a> {
    pub provider: &'a Provider,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scopes: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    /// PKCE's S256 challenge (`challenge`): the verifier stays with the
    /// registry, which alone exchanges the code.
    pub challenge: &'a str,
    /// Who the person says they are, passed on (Core 1.0's `login_hint`).
    pub login_hint: Option<&'a str>,
}

/// Where the browser goes to sign in.
pub fn authorize_url(a: &Authorize<'_>) -> String {
    assert!(!a.state.is_empty() && !a.nonce.is_empty(), "a sign-in carries a state and a nonce");
    assert!(a.challenge.len() == 43, "an S256 challenge is 43 base64url characters");
    let mut u = url::Url::parse(&a.provider.authorization_endpoint).expect("Provider::parse checked the endpoint");
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", a.client_id)
            .append_pair("redirect_uri", a.redirect_uri)
            .append_pair("scope", a.scopes)
            .append_pair("state", a.state)
            .append_pair("nonce", a.nonce)
            .append_pair("code_challenge", a.challenge)
            .append_pair("code_challenge_method", "S256");
        if let Some(hint) = a.login_hint {
            q.append_pair("login_hint", hint);
        }
    }
    u.into()
}

fn form_enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// A token request, ready to POST as `application/x-www-form-urlencoded`.
#[derive(Debug, PartialEq, Eq)]
pub struct TokenRequest {
    pub body: String,
    /// The `Authorization` header (`client_secret_basic`'s).
    pub authorization: Option<String>,
}

/// The code exchanged (RFC 6749, 4.1.3, with PKCE's verifier).
pub fn token_request(auth: ClientAuth, client_id: &str, secret: Option<&str>, code: &str, redirect_uri: &str, verifier: &str) -> TokenRequest {
    assert!(valid_verifier(verifier), "the verifier is the one the challenge was made from");
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    body.append_pair("grant_type", "authorization_code").append_pair("code", code).append_pair("redirect_uri", redirect_uri).append_pair("code_verifier", verifier);
    let authorization = match (auth, secret) {
        (ClientAuth::Basic, Some(secret)) => Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", form_enc(client_id), form_enc(secret))))),
        (ClientAuth::Post, Some(secret)) => {
            body.append_pair("client_id", client_id).append_pair("client_secret", secret);
            None
        }
        (ClientAuth::None, _) => {
            body.append_pair("client_id", client_id);
            None
        }
        (_, None) => panic!("{} needs a secret (Provider::client_auth checked)", auth.name()),
    };
    TokenRequest { body: body.finish(), authorization }
}

#[derive(Deserialize)]
struct TokenAnswer {
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenRefusal {
    error: Option<String>,
    error_description: Option<String>,
}

/// The id_token in the token endpoint's answer (`status`, `bytes`), or
/// what it refused with.
pub fn id_token_of(status: u16, bytes: &[u8]) -> Result<String, OidcError> {
    if status != 200 {
        let refusal = serde_json::from_slice::<TokenRefusal>(bytes).ok();
        let (error, description) = refusal.map(|r| (r.error.unwrap_or_default(), r.error_description.unwrap_or_default())).unwrap_or_default();
        let cut = |s: String| s.chars().take(200).collect::<String>();
        return Err(OidcError::Refused { status, error: cut(error), description: cut(description) });
    }
    let answer: TokenAnswer = serde_json::from_slice(bytes).map_err(|_| provider_err("its token answer is not JSON"))?;
    answer.id_token.filter(|t| !t.is_empty()).ok_or_else(|| provider_err("its token answer carries no id_token (is `openid` among the scopes?)"))
}

/// RP-initiated logout (RP-Initiated Logout 1.0, 2): the provider's
/// endpoint, with the session's id_token as the hint when the cell kept
/// one, the client, and the way back.
pub fn end_session_url(endpoint: &str, id_token_hint: Option<&str>, client_id: &str, back: &str) -> String {
    let mut u = url::Url::parse(endpoint).expect("Provider::parse checked the endpoint");
    {
        let mut q = u.query_pairs_mut();
        if let Some(hint) = id_token_hint {
            q.append_pair("id_token_hint", hint);
        }
        q.append_pair("client_id", client_id).append_pair("post_logout_redirect_uri", back);
    }
    u.into()
}

// ------------------------------------------------------------------ keys

enum Public {
    Rsa(rsa::pkcs1v15::VerifyingKey<Sha256>, usize),
    Ec(p256::ecdsa::VerifyingKey),
}

/// One signing key of the provider's.
struct Key {
    kid: Option<String>,
    /// The algorithm the key is for, when the JWK names one.
    alg: Option<Alg>,
    public: Public,
}

impl Key {
    fn fits(&self, alg: Alg) -> bool {
        let kind = matches!((&self.public, alg), (Public::Rsa(..), Alg::Rs256) | (Public::Ec(_), Alg::Es256));
        kind && self.alg.is_none_or(|a| a == alg)
    }
}

/// The provider's signing keys (RFC 7517), the ones the cell can use.
pub struct Jwks {
    keys: Vec<Key>,
}

#[derive(Deserialize)]
struct JwkDoc {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default, rename = "use")]
    use_: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

#[derive(Deserialize)]
struct JwksDoc {
    keys: Vec<Value>,
}

/// A JWK the cell can verify with, or `None`: one for encryption, another
/// algorithm or curve, an RSA key outside the sizes, or malformed. A set
/// often holds such keys beside its signing ones.
fn key_of(doc: JwkDoc) -> Option<Key> {
    if doc.use_.as_deref().is_some_and(|u| u != "sig") {
        return None;
    }
    let alg = match doc.alg.as_deref() {
        None => None,
        Some(a) => Some(Alg::parse(a)?),
    };
    let bytes = |s: &Option<String>| s.as_deref().and_then(|v| b64().decode(v).ok());
    let public = match doc.kty.as_str() {
        "RSA" => {
            use rsa::traits::PublicKeyParts;
            let (n, e) = (bytes(&doc.n)?, bytes(&doc.e)?);
            let key = rsa::RsaPublicKey::new_with_max_size(rsa::BigUint::from_bytes_be(&n), rsa::BigUint::from_bytes_be(&e), RSA_BITS_MAX).ok()?;
            let bits = key.n().bits();
            if !(RSA_BITS_MIN..=RSA_BITS_MAX).contains(&bits) {
                return None;
            }
            let size = key.size();
            Public::Rsa(rsa::pkcs1v15::VerifyingKey::<Sha256>::new(key), size)
        }
        "EC" if doc.crv.as_deref() == Some("P-256") => {
            let (x, y) = (bytes(&doc.x)?, bytes(&doc.y)?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let mut sec1 = vec![0x04];
            sec1.extend_from_slice(&x);
            sec1.extend_from_slice(&y);
            Public::Ec(p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1).ok()?)
        }
        _ => return None,
    };
    Some(Key { kid: doc.kid, alg, public })
}

impl Jwks {
    /// The keys at the provider's `jwks_uri`, the usable ones kept. A set
    /// over `JWKS_KEYS_MAX` keys or `METADATA_MAX_BYTES`, or with none the
    /// cell can use, is refused.
    pub fn parse(bytes: &[u8]) -> Result<Jwks, OidcError> {
        if bytes.len() > METADATA_MAX_BYTES {
            return Err(provider_err(format!("its JWKS is over {METADATA_MAX_BYTES} bytes")));
        }
        let doc: JwksDoc = serde_json::from_slice(bytes).map_err(|_| provider_err("its JWKS is not a JSON key set"))?;
        if doc.keys.len() > JWKS_KEYS_MAX {
            return Err(provider_err(format!("its JWKS holds {} keys, over {JWKS_KEYS_MAX}", doc.keys.len())));
        }
        let keys: Vec<Key> = doc.keys.into_iter().filter_map(|k| serde_json::from_value::<JwkDoc>(k).ok()).filter_map(key_of).collect();
        if keys.is_empty() {
            return Err(provider_err("its JWKS holds no RS256 or ES256 signing key"));
        }
        Ok(Jwks { keys })
    }

    /// How many usable keys it holds.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The key a token's header names: by its `kid`, of the algorithm's
    /// kind; without a `kid`, the one key of that kind, if there is one.
    fn select(&self, alg: Alg, kid: Option<&str>) -> Result<&Key, IdTokenError> {
        let mut fitting = self.keys.iter().filter(|k| k.fits(alg));
        match kid {
            Some(kid) => fitting.find(|k| k.kid.as_deref() == Some(kid)).ok_or_else(|| IdTokenError::UnknownKey(Some(kid.to_string()))),
            None => match (fitting.next(), fitting.next()) {
                (Some(k), None) => Ok(k),
                _ => Err(IdTokenError::UnknownKey(None)),
            },
        }
    }
}

/// Whether the cell's keys, fetched at `fetched_at_ms`, may be fetched
/// again for a key they lack (a rotation): at most once a
/// `JWKS_REFETCH_MIN_MS`.
pub fn may_refetch(fetched_at_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(fetched_at_ms) >= JWKS_REFETCH_MIN_MS
}

/// Whether metadata or keys fetched at `fetched_at_ms` are past their time.
pub fn stale(fetched_at_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(fetched_at_ms) >= METADATA_TTL_MS || now_ms < fetched_at_ms
}

// -------------------------------------------------------------- id_token

/// Why an id_token is not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdTokenError {
    TooLarge,
    Malformed(&'static str),
    /// Signed with an algorithm the cell does not take (`none`, an HMAC,
    /// one the provider did not list).
    Alg(String),
    /// Its header names an extension the cell does not understand.
    Critical,
    /// No key in the cell's JWKS fits it (the `kid`, if it names one): the
    /// caller may fetch the JWKS again, once (`may_refetch`).
    UnknownKey(Option<String>),
    Signature,
    Issuer,
    Audience,
    AuthorizedParty,
    Expired,
    NotYetValid,
    TooOld,
    Nonce,
    Subject,
}

impl std::fmt::Display for IdTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdTokenError::TooLarge => write!(f, "the id_token is over {ID_TOKEN_MAX_BYTES} bytes"),
            IdTokenError::Malformed(why) => write!(f, "the id_token is malformed: {why}"),
            IdTokenError::Alg(a) => write!(f, "the id_token is signed {a:?}, which sign-in does not take"),
            IdTokenError::Critical => write!(f, "the id_token's header names an extension sign-in does not understand (crit)"),
            IdTokenError::UnknownKey(Some(kid)) => write!(f, "the id_token is signed by a key ({kid}) the provider's JWKS does not hold"),
            IdTokenError::UnknownKey(None) => write!(f, "the id_token names no key, and the provider's JWKS holds no single one for it"),
            IdTokenError::Signature => write!(f, "the id_token's signature does not verify"),
            IdTokenError::Issuer => write!(f, "the id_token is from another issuer"),
            IdTokenError::Audience => write!(f, "the id_token is for another client"),
            IdTokenError::AuthorizedParty => write!(f, "the id_token was issued to another party (azp)"),
            IdTokenError::Expired => write!(f, "the id_token has expired"),
            IdTokenError::NotYetValid => write!(f, "the id_token is not valid yet (iat or nbf in the future)"),
            IdTokenError::TooOld => write!(f, "the id_token was issued too long ago"),
            IdTokenError::Nonce => write!(f, "the id_token is for another sign-in (its nonce)"),
            IdTokenError::Subject => write!(f, "the id_token names no subject, or one over {SUBJECT_MAX} bytes"),
        }
    }
}

/// What an id_token must say to sign someone in.
pub struct Expect<'a> {
    /// The configured issuer, exactly.
    pub issuer: &'a str,
    pub client_id: &'a str,
    /// The nonce this sign-in sent.
    pub nonce: &'a str,
    pub now_s: i64,
    /// The algorithms the provider lists (`Provider::algs`).
    pub algs: &'a [Alg],
}

/// An id_token that verified: its subject, and every claim.
#[derive(Debug, Clone)]
pub struct Verified {
    pub subject: String,
    pub claims: Map<String, Value>,
}

fn part(s: &str) -> Result<Vec<u8>, IdTokenError> {
    b64().decode(s).map_err(|_| IdTokenError::Malformed("a part is not base64url"))
}

/// A time claim (NumericDate: seconds, maybe fractional), floored.
fn seconds(claims: &Map<String, Value>, name: &str) -> Result<Option<i64>, IdTokenError> {
    match claims.get(name) {
        None => Ok(None),
        Some(v) => v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f.floor() as i64)).map(Some).ok_or(IdTokenError::Malformed("a time claim is not a number")),
    }
}

/// An id_token checked against the provider's keys and this sign-in
/// (Core 1.0, 3.1.3.7): the signature first, by a key of the JWKS only,
/// then each claim.
pub fn verify(token: &str, jwks: &Jwks, expect: &Expect<'_>) -> Result<Verified, IdTokenError> {
    assert!(!expect.issuer.is_empty() && !expect.client_id.is_empty() && !expect.nonce.is_empty(), "a sign-in expects an issuer, a client, and a nonce");
    if token.len() > ID_TOKEN_MAX_BYTES {
        return Err(IdTokenError::TooLarge);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(IdTokenError::Malformed("not three dot-separated parts (a compact JWS)"));
    };
    let header: Map<String, Value> = serde_json::from_slice(&part(h)?).map_err(|_| IdTokenError::Malformed("the header is not a JSON object"))?;
    let alg_name = header.get("alg").and_then(Value::as_str).unwrap_or_default();
    let alg = Alg::parse(alg_name).filter(|a| expect.algs.contains(a)).ok_or_else(|| IdTokenError::Alg(alg_name.chars().take(16).collect()))?;
    if header.contains_key("crit") {
        return Err(IdTokenError::Critical);
    }
    // an access token (RFC 9068) is never taken for an id_token
    if header.get("typ").and_then(Value::as_str).is_some_and(|t| t.eq_ignore_ascii_case("at+jwt") || t.eq_ignore_ascii_case("application/at+jwt")) {
        return Err(IdTokenError::Malformed("it is an access token (typ at+jwt)"));
    }
    let kid = match header.get("kid") {
        None => None,
        Some(Value::String(k)) => Some(k.as_str()),
        Some(_) => return Err(IdTokenError::Malformed("its kid is not a string")),
    };
    let key = jwks.select(alg, kid)?;
    let signed = &token.as_bytes()[..h.len() + 1 + p.len()];
    let sig = part(s)?;
    let good = match &key.public {
        Public::Rsa(vk, size) => {
            use rsa::signature::Verifier;
            sig.len() == *size && rsa::pkcs1v15::Signature::try_from(sig.as_slice()).is_ok_and(|sig| vk.verify(signed, &sig).is_ok())
        }
        Public::Ec(vk) => {
            use p256::ecdsa::signature::Verifier;
            sig.len() == 64 && p256::ecdsa::Signature::from_slice(&sig).is_ok_and(|sig| vk.verify(signed, &sig).is_ok())
        }
    };
    if !good {
        return Err(IdTokenError::Signature);
    }
    let claims: Map<String, Value> = serde_json::from_slice(&part(p)?).map_err(|_| IdTokenError::Malformed("the claims are not a JSON object"))?;
    if claims.get("iss").and_then(Value::as_str) != Some(expect.issuer) {
        return Err(IdTokenError::Issuer);
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().map(|v| v.as_str().ok_or(IdTokenError::Audience)).collect::<Result<_, _>>()?,
        _ => return Err(IdTokenError::Audience),
    };
    if !audiences.contains(&expect.client_id) {
        return Err(IdTokenError::Audience);
    }
    // more audiences than this client: the party it was issued to must be this one
    let azp = claims.get("azp").and_then(Value::as_str);
    if azp.is_some_and(|a| a != expect.client_id) || (audiences.len() > 1 && azp.is_none()) {
        return Err(IdTokenError::AuthorizedParty);
    }
    let now = expect.now_s;
    let exp = seconds(&claims, "exp")?.ok_or(IdTokenError::Malformed("it has no exp"))?;
    if exp.saturating_add(SKEW_S) <= now {
        return Err(IdTokenError::Expired);
    }
    let iat = seconds(&claims, "iat")?.ok_or(IdTokenError::Malformed("it has no iat"))?;
    let nbf = seconds(&claims, "nbf")?;
    if iat > now.saturating_add(SKEW_S) || nbf.is_some_and(|n| n > now.saturating_add(SKEW_S)) {
        return Err(IdTokenError::NotYetValid);
    }
    if iat < now.saturating_sub(ID_TOKEN_AGE_MAX_S + SKEW_S) {
        return Err(IdTokenError::TooOld);
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(expect.nonce) {
        return Err(IdTokenError::Nonce);
    }
    let subject = claims.get("sub").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= SUBJECT_MAX).ok_or(IdTokenError::Subject)?.to_string();
    Ok(Verified { subject, claims })
}

// ------------------------------------------------------------- the person

/// Which claims say who the person is (`FRAGMENT_OIDC_CLAIMS`, JSON; each
/// field optional): `email` (default `email`), `name` (default `name`), and
/// `username`, the claims tried in order for a username (default
/// `preferred_username`, then `upn`: Entra's and ADFS's). None is assumed
/// present: a person is their `sub`, and the label shown falls back from
/// the email to the username to the `sub`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimMap {
    #[serde(default = "ClaimMap::default_email")]
    pub email: String,
    #[serde(default = "ClaimMap::default_name")]
    pub name: String,
    #[serde(default = "ClaimMap::default_username")]
    pub username: Vec<String>,
}

impl Default for ClaimMap {
    fn default() -> ClaimMap {
        ClaimMap { email: ClaimMap::default_email(), name: ClaimMap::default_name(), username: ClaimMap::default_username() }
    }
}

/// What sign-in keeps of a person's claims: attributes, refreshed at each
/// sign-in and never matched (the person is `(issuer, subject)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
    /// The first username claim present, else the subject: what names
    /// them where there is no email.
    pub handle: String,
}

impl Person {
    /// The label a page shows: the email, else the handle.
    pub fn label(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.handle)
    }
}

impl ClaimMap {
    fn default_email() -> String {
        "email".into()
    }

    fn default_name() -> String {
        "name".into()
    }

    fn default_username() -> Vec<String> {
        vec!["preferred_username".into(), "upn".into()]
    }

    /// `FRAGMENT_OIDC_CLAIMS`, or the defaults.
    pub fn parse(configured: Option<&str>) -> Result<ClaimMap, OidcError> {
        let map = match configured {
            None => ClaimMap::default(),
            Some(json) => serde_json::from_str::<ClaimMap>(json)
                .map_err(|e| OidcError::Config(format!("FRAGMENT_OIDC_CLAIMS is {{\"email\": <claim>, \"name\": <claim>, \"username\": [<claim>, ...]}}: {e}")))?,
        };
        let named = |c: &str| !c.is_empty() && c.len() <= 64;
        if !named(&map.email) || !named(&map.name) || map.username.len() > USERNAME_CLAIMS_MAX || !map.username.iter().all(|c| named(c)) {
            return Err(OidcError::Config(format!("FRAGMENT_OIDC_CLAIMS names claims of 1 to 64 bytes, at most {USERNAME_CLAIMS_MAX} for a username")));
        }
        Ok(map)
    }

    /// The person a verified id_token names, as this map reads it.
    pub fn person(&self, v: &Verified) -> Person {
        let text = |claim: &str| v.claims.get(claim).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty() && s.len() <= CLAIM_MAX).map(str::to_string);
        let handle = self.username.iter().find_map(|c| text(c)).unwrap_or_else(|| v.subject.clone());
        Person { subject: v.subject.clone(), email: text(&self.email), name: text(&self.name), handle }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::traits::PublicKeyParts;
    use serde_json::json;
    use std::sync::OnceLock;

    const ISSUER: &str = "https://idp.example/realms/corp";
    const CLIENT: &str = "fragment";
    const NONCE: &str = "n-0123456789";
    const NOW: i64 = 1_790_000_000;

    /// One RSA key per test run: making one is the slow part.
    fn rsa_key() -> &'static rsa::RsaPrivateKey {
        static KEY: OnceLock<rsa::RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).expect("an RSA key"))
    }

    fn ec_key() -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("an EC key")
    }

    fn rsa_jwk(kid: &str, key: &rsa::RsaPrivateKey) -> Value {
        let public = key.to_public_key();
        json!({ "kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256", "n": b64().encode(public.n().to_bytes_be()), "e": b64().encode(public.e().to_bytes_be()) })
    }

    fn ec_jwk(kid: &str, key: &p256::ecdsa::SigningKey) -> Value {
        let point = key.verifying_key().to_encoded_point(false);
        json!({ "kty": "EC", "kid": kid, "crv": "P-256", "x": b64().encode(point.x().unwrap()), "y": b64().encode(point.y().unwrap()) })
    }

    fn jwks() -> Jwks {
        Jwks::parse(json!({ "keys": [rsa_jwk("r1", rsa_key()), ec_jwk("e1", &ec_key())] }).to_string().as_bytes()).expect("a JWKS")
    }

    fn claims() -> Value {
        json!({ "iss": ISSUER, "aud": CLIENT, "sub": "248289761001", "exp": NOW + 300, "iat": NOW, "nonce": NONCE, "email": "jane@corp.example", "name": "Jane Doe" })
    }

    fn sign(alg: &str, kid: Option<&str>, claims: &Value) -> String {
        let mut header = json!({ "alg": alg, "typ": "JWT" });
        if let Some(k) = kid {
            header["kid"] = json!(k);
        }
        let input = format!("{}.{}", b64().encode(header.to_string()), b64().encode(claims.to_string()));
        let sig = match alg {
            "RS256" => SigningKey::<Sha256>::new(rsa_key().clone()).sign(input.as_bytes()).to_vec(),
            "ES256" => {
                let s: p256::ecdsa::Signature = ec_key().sign(input.as_bytes());
                s.to_bytes().to_vec()
            }
            _ => vec![],
        };
        format!("{input}.{}", b64().encode(sig))
    }

    fn expect() -> Expect<'static> {
        Expect { issuer: ISSUER, client_id: CLIENT, nonce: NONCE, now_s: NOW, algs: &[Alg::Rs256, Alg::Es256] }
    }

    // Goal: a well-formed id_token signed by a key the provider published
    // signs its subject in, with RS256 and with ES256, by kid or as the
    // only key of its kind.
    #[test]
    fn valid_tokens_verify() {
        let set = jwks();
        for (alg, kid) in [("RS256", Some("r1")), ("ES256", Some("e1")), ("RS256", None), ("ES256", None)] {
            let v = verify(&sign(alg, kid, &claims()), &set, &expect()).unwrap_or_else(|e| panic!("{alg} {kid:?}: {e}"));
            assert_eq!(v.subject, "248289761001");
        }
        // an audience list that names this client, with azp saying it is the party
        let mut c = claims();
        c["aud"] = json!([CLIENT, "another"]);
        c["azp"] = json!(CLIENT);
        assert!(verify(&sign("RS256", Some("r1"), &c), &set, &expect()).is_ok());
        // skew either way, within bounds; fractional times floor
        c = claims();
        c["exp"] = json!(NOW - SKEW_S + 1);
        c["iat"] = json!((NOW + SKEW_S) as f64 - 0.5);
        assert!(verify(&sign("RS256", Some("r1"), &c), &set, &expect()).is_ok());
    }

    // Goal: a token whose signature does not verify is refused, whatever
    // it claims. Method: a flipped byte in the signature, in the claims
    // after signing, a signature by another key under the published kid,
    // and an RSA signature cut short.
    #[test]
    fn bad_signatures_are_refused() {
        let set = jwks();
        let good = sign("RS256", Some("r1"), &claims());
        let (input, sig) = good.rsplit_once('.').unwrap();
        let mut bytes = b64().decode(sig).unwrap();
        bytes[10] ^= 1;
        assert_eq!(verify(&format!("{input}.{}", b64().encode(&bytes)), &set, &expect()).unwrap_err(), IdTokenError::Signature);
        let mut forged = claims();
        forged["sub"] = json!("admin");
        let (h, _) = input.split_once('.').unwrap();
        assert_eq!(verify(&format!("{h}.{}.{sig}", b64().encode(forged.to_string())), &set, &expect()).unwrap_err(), IdTokenError::Signature);
        assert_eq!(verify(&format!("{input}.{}", b64().encode(&bytes[1..])), &set, &expect()).unwrap_err(), IdTokenError::Signature);
        // another EC key signs under the published EC kid
        let other = p256::ecdsa::SigningKey::from_slice(&[9u8; 32]).unwrap();
        let header = b64().encode(json!({ "alg": "ES256", "kid": "e1" }).to_string());
        let input = format!("{header}.{}", b64().encode(claims().to_string()));
        let s: p256::ecdsa::Signature = other.sign(input.as_bytes());
        assert_eq!(verify(&format!("{input}.{}", b64().encode(s.to_bytes())), &set, &expect()).unwrap_err(), IdTokenError::Signature);
    }

    // Goal: the algorithm is the cell's choice, never the token's. Method:
    // `none` with no signature, HS256 keyed with the client secret and with
    // the RSA key's public bytes (the classic confusion), an alg the
    // provider did not list, and RS256 naming the EC key's kid.
    #[test]
    fn algorithm_confusion_is_refused() {
        use hmac::{Hmac, Mac};
        let set = jwks();
        let unsigned = format!("{}.{}.", b64().encode(json!({ "alg": "none" }).to_string()), b64().encode(claims().to_string()));
        assert_eq!(verify(&unsigned, &set, &expect()).unwrap_err(), IdTokenError::Alg("none".into()));
        let public = rsa_key().to_public_key().n().to_bytes_be();
        for secret in [b"the-client-secret".to_vec(), public] {
            let input = format!("{}.{}", b64().encode(json!({ "alg": "HS256", "kid": "r1" }).to_string()), b64().encode(claims().to_string()));
            let mut mac = Hmac::<Sha256>::new_from_slice(&secret).unwrap();
            mac.update(input.as_bytes());
            let token = format!("{input}.{}", b64().encode(mac.finalize().into_bytes()));
            assert_eq!(verify(&token, &set, &expect()).unwrap_err(), IdTokenError::Alg("HS256".into()));
        }
        let only_rs = Expect { algs: &[Alg::Rs256], ..expect() };
        assert_eq!(verify(&sign("ES256", Some("e1"), &claims()), &set, &only_rs).unwrap_err(), IdTokenError::Alg("ES256".into()));
        let crossed = sign("RS256", Some("e1"), &claims());
        assert_eq!(verify(&crossed, &set, &expect()).unwrap_err(), IdTokenError::UnknownKey(Some("e1".into())));
        let mut header = json!({ "alg": "RS256", "kid": "r1", "crit": ["exp"] });
        let input = format!("{}.{}", b64().encode(header.to_string()), b64().encode(claims().to_string()));
        let sig = SigningKey::<Sha256>::new(rsa_key().clone()).sign(input.as_bytes()).to_vec();
        assert_eq!(verify(&format!("{input}.{}", b64().encode(sig)), &set, &expect()).unwrap_err(), IdTokenError::Critical);
        header = json!({ "alg": "RS256", "kid": "r1", "typ": "at+jwt" });
        let input = format!("{}.{}", b64().encode(header.to_string()), b64().encode(claims().to_string()));
        let sig = SigningKey::<Sha256>::new(rsa_key().clone()).sign(input.as_bytes()).to_vec();
        assert!(matches!(verify(&format!("{input}.{}", b64().encode(sig)), &set, &expect()), Err(IdTokenError::Malformed(_))));
    }

    // Goal: each claim the spec requires is checked, each refusal named.
    // Method: one claim wrong at a time on an otherwise good token.
    #[test]
    fn wrong_claims_are_refused() {
        let set = jwks();
        let with = |k: &str, v: Value| {
            let mut c = claims();
            if v.is_null() {
                c.as_object_mut().unwrap().remove(k);
            } else {
                c[k] = v;
            }
            verify(&sign("RS256", Some("r1"), &c), &set, &expect()).unwrap_err()
        };
        assert_eq!(with("iss", json!("https://idp.example/realms/other")), IdTokenError::Issuer);
        assert_eq!(with("iss", json!(format!("{ISSUER}/"))), IdTokenError::Issuer);
        assert_eq!(with("aud", json!("another-client")), IdTokenError::Audience);
        assert_eq!(with("aud", json!([CLIENT, "another"])), IdTokenError::AuthorizedParty);
        assert_eq!(with("azp", json!("another")), IdTokenError::AuthorizedParty);
        assert_eq!(with("exp", json!(NOW - SKEW_S)), IdTokenError::Expired);
        assert_eq!(with("iat", json!(NOW + SKEW_S + 1)), IdTokenError::NotYetValid);
        assert_eq!(with("nbf", json!(NOW + SKEW_S + 1)), IdTokenError::NotYetValid);
        assert_eq!(with("iat", json!(NOW - ID_TOKEN_AGE_MAX_S - SKEW_S - 1)), IdTokenError::TooOld);
        assert_eq!(with("nonce", json!("another-sign-in")), IdTokenError::Nonce);
        assert_eq!(with("nonce", Value::Null), IdTokenError::Nonce);
        assert_eq!(with("sub", json!("")), IdTokenError::Subject);
        assert_eq!(with("sub", json!("x".repeat(SUBJECT_MAX + 1))), IdTokenError::Subject);
        assert!(matches!(with("exp", Value::Null), IdTokenError::Malformed(_)));
        assert!(matches!(with("iat", json!("yesterday")), IdTokenError::Malformed(_)));
    }

    // Goal: a token is never trusted for more than one sign-in. Method: the
    // same id_token, good for the sign-in whose nonce it carries, is
    // replayed into the next sign-in (a new nonce), and kept until it is
    // past its time.
    #[test]
    fn a_replayed_token_signs_no_one_in() {
        let set = jwks();
        let token = sign("RS256", Some("r1"), &claims());
        assert!(verify(&token, &set, &expect()).is_ok());
        let next = Expect { nonce: "n-the-next-sign-in", ..expect() };
        assert_eq!(verify(&token, &set, &next).unwrap_err(), IdTokenError::Nonce);
        let later = Expect { now_s: NOW + 300 + SKEW_S, ..expect() };
        assert_eq!(verify(&token, &set, &later).unwrap_err(), IdTokenError::Expired);
    }

    // Goal: a token signed by a key the cell has not seen asks for a fresh
    // JWKS, at most once a cooldown, and a fresh set with the new key
    // verifies it (the provider rotated). Method: a set without the kid,
    // then a set with it; the refetch clock.
    #[test]
    fn a_rotated_key_is_fetched_once_then_trusted() {
        let token = sign("RS256", Some("r2"), &claims());
        let old = jwks();
        assert_eq!(verify(&token, &old, &expect()).unwrap_err(), IdTokenError::UnknownKey(Some("r2".into())));
        let fetched_at = 1_000_000;
        assert!(!may_refetch(fetched_at, fetched_at + JWKS_REFETCH_MIN_MS - 1), "not again within the cooldown");
        assert!(may_refetch(fetched_at, fetched_at + JWKS_REFETCH_MIN_MS));
        let rotated = Jwks::parse(json!({ "keys": [rsa_jwk("r2", rsa_key())] }).to_string().as_bytes()).unwrap();
        assert!(verify(&token, &rotated, &expect()).is_ok());
        // a cell that restarted holds no keys: whatever it fetched before is past its time
        assert!(stale(fetched_at, fetched_at + METADATA_TTL_MS));
        assert!(!stale(fetched_at, fetched_at + METADATA_TTL_MS - 1));
        assert!(stale(fetched_at, fetched_at - 1), "a clock that went back fetches again");
    }

    // Goal: a JWKS keeps the keys it can use and refuses sets it cannot.
    // Method: encryption keys, a short RSA key, another curve, an unknown
    // alg and junk beside a good key; an empty and an oversized set.
    #[test]
    fn jwks_keeps_only_usable_keys() {
        let small = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 1024).unwrap();
        let set = Jwks::parse(
            json!({ "keys": [
                rsa_jwk("good", rsa_key()),
                { "kty": "RSA", "kid": "enc", "use": "enc", "n": "AQAB", "e": "AQAB" },
                rsa_jwk("small", &small),
                { "kty": "EC", "kid": "p384", "crv": "P-384", "x": "AA", "y": "AA" },
                { "kty": "RSA", "kid": "ps", "alg": "PS256", "n": "AQAB", "e": "AQAB" },
                { "kty": "OKP", "kid": "ed", "crv": "Ed25519", "x": "AA" },
                "junk"
            ] })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(verify(&sign("RS256", None, &claims()), &set, &expect()).map(|v| v.subject), Ok("248289761001".into()));
        assert!(Jwks::parse(br#"{"keys": []}"#).is_err());
        let many: Vec<Value> = (0..=JWKS_KEYS_MAX).map(|i| ec_jwk(&format!("k{i}"), &ec_key())).collect();
        assert!(Jwks::parse(json!({ "keys": many }).to_string().as_bytes()).is_err());
        // two keys of a kind and a token naming none: which one is not guessed
        let two = Jwks::parse(json!({ "keys": [ec_jwk("a", &ec_key()), ec_jwk("b", &ec_key())] }).to_string().as_bytes()).unwrap();
        assert_eq!(verify(&sign("ES256", None, &claims()), &two, &expect()).unwrap_err(), IdTokenError::UnknownKey(None));
    }

    // Goal: malformed tokens are refused before any key is chosen.
    #[test]
    fn malformed_tokens_are_refused() {
        let set = jwks();
        for t in ["", "a.b", "a.b.c.d", "!!.e30.e30", "e30.e30.e30"] {
            assert!(verify(t, &set, &expect()).is_err(), "{t:?}");
        }
        assert_eq!(verify(&"a".repeat(ID_TOKEN_MAX_BYTES + 1), &set, &expect()).unwrap_err(), IdTokenError::TooLarge);
    }

    fn provider_doc(issuer: &str) -> Value {
        json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/auth"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/keys"),
            "end_session_endpoint": format!("{issuer}/logout"),
            "id_token_signing_alg_values_supported": ["RS256", "HS256"],
            "code_challenge_methods_supported": ["plain", "S256"],
        })
    }

    // Goal: discovery holds the provider to its issuer and to https, and
    // reads what sign-in needs. Method: a good document; then each fault.
    #[test]
    fn discovery_is_checked() {
        let p = Provider::parse(ISSUER, provider_doc(ISSUER).to_string().as_bytes()).unwrap();
        assert_eq!(p.token_endpoint, format!("{ISSUER}/token"));
        assert_eq!(p.algs, vec![Alg::Rs256], "HS256 is listed and never taken");
        assert_eq!(discovery_url("https://idp.example/"), "https://idp.example/.well-known/openid-configuration");
        assert_eq!(discovery_url(ISSUER), format!("{ISSUER}/.well-known/openid-configuration"));
        let fault = |k: &str, v: Value| {
            let mut d = provider_doc(ISSUER);
            d[k] = v;
            Provider::parse(ISSUER, d.to_string().as_bytes()).unwrap_err()
        };
        assert!(matches!(fault("issuer", json!("https://evil.example")), OidcError::Provider(_)));
        assert!(matches!(fault("issuer", json!(format!("{ISSUER}/"))), OidcError::Provider(_)));
        assert!(matches!(fault("token_endpoint", json!("http://idp.example/token")), OidcError::Provider(_)));
        assert!(matches!(fault("jwks_uri", json!("not a url")), OidcError::Provider(_)));
        assert!(matches!(fault("id_token_signing_alg_values_supported", json!(["HS256", "none"])), OidcError::Provider(_)));
        assert!(matches!(fault("code_challenge_methods_supported", json!(["plain"])), OidcError::Provider(_)));
        assert!(Provider::parse(ISSUER, &vec![b' '; METADATA_MAX_BYTES + 1]).is_err());
        // a dev provider on loopback may be plain http, and only there
        let dev = "http://127.0.0.1:5556/dex";
        assert!(check_issuer(dev).is_ok() && Provider::parse(dev, provider_doc(dev).to_string().as_bytes()).is_ok());
        let mut d = provider_doc(dev);
        d["token_endpoint"] = json!("http://10.0.0.5/token");
        assert!(Provider::parse(dev, d.to_string().as_bytes()).is_err());
        for bad in ["http://idp.example", "https://idp.example?x=1", "https://u:p@idp.example", "ftp://idp.example", "idp.example"] {
            assert!(check_issuer(bad).is_err(), "{bad}");
        }
    }

    // Goal: the client authenticates as configured, else as the provider
    // lists, and never sends a secret it does not have. Method: each method
    // listed or not; RFC 6749 2.3.1's encoding of an id and secret with
    // reserved characters.
    #[test]
    fn the_token_request_is_form_encoded_and_authenticated() {
        let mut d = provider_doc(ISSUER);
        let p = Provider::parse(ISSUER, d.to_string().as_bytes()).unwrap();
        assert_eq!(p.client_auth(None, true), Ok(ClientAuth::Basic), "unlisted: Basic, the default");
        assert_eq!(p.client_auth(None, false), Ok(ClientAuth::None));
        assert!(matches!(p.client_auth(Some(ClientAuth::Post), false), Err(OidcError::Config(_))));
        d["token_endpoint_auth_methods_supported"] = json!(["private_key_jwt", "client_secret_post"]);
        let p = Provider::parse(ISSUER, d.to_string().as_bytes()).unwrap();
        assert_eq!(p.client_auth(None, true), Ok(ClientAuth::Post));
        d["token_endpoint_auth_methods_supported"] = json!(["private_key_jwt"]);
        assert!(Provider::parse(ISSUER, d.to_string().as_bytes()).unwrap().client_auth(None, true).is_err());
        let verifier = "a".repeat(64);
        let basic = token_request(ClientAuth::Basic, "my app", Some("s3cr:t/+="), "the code", "https://fragment.example/auth/callback", &verifier);
        let decoded = base64::engine::general_purpose::STANDARD.decode(basic.authorization.unwrap().strip_prefix("Basic ").unwrap()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "my+app:s3cr%3At%2F%2B%3D");
        assert_eq!(basic.body, format!("grant_type=authorization_code&code=the+code&redirect_uri=https%3A%2F%2Ffragment.example%2Fauth%2Fcallback&code_verifier={verifier}"));
        let post = token_request(ClientAuth::Post, "app", Some("s&t"), "c", "https://f/cb", &verifier);
        assert!(post.authorization.is_none() && post.body.ends_with("&client_id=app&client_secret=s%26t"));
        let public = token_request(ClientAuth::None, "app", None, "c", "https://f/cb", &verifier);
        assert!(public.authorization.is_none() && public.body.ends_with("&client_id=app") && !public.body.contains("secret"));
        assert_eq!(id_token_of(200, br#"{"id_token":"x.y.z","access_token":"a"}"#), Ok("x.y.z".into()));
        assert!(matches!(id_token_of(200, br#"{"access_token":"a"}"#), Err(OidcError::Provider(_))));
        assert_eq!(
            id_token_of(400, br#"{"error":"invalid_grant","error_description":"code used"}"#),
            Err(OidcError::Refused { status: 400, error: "invalid_grant".into(), description: "code used".into() })
        );
    }

    // Goal: the authorization request carries PKCE's S256 challenge (RFC
    // 7636 appendix B's vector), the state, the nonce, and the hint, and
    // logout carries the hint and the way back.
    #[test]
    fn the_request_carries_pkce_state_and_nonce() {
        assert_eq!(b64().encode(Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert!(!valid_verifier(&"a".repeat(42)) && !valid_verifier(&"a".repeat(129)) && !valid_verifier(&format!("{}+", "a".repeat(43))));
        let p = Provider::parse(ISSUER, provider_doc(ISSUER).to_string().as_bytes()).unwrap();
        let verifier = "0123456789abcdef".repeat(4);
        let to = authorize_url(&Authorize {
            provider: &p,
            client_id: CLIENT,
            redirect_uri: "https://fragment.example/auth/callback",
            scopes: "openid email",
            state: "s1",
            nonce: NONCE,
            challenge: &challenge(&verifier),
            login_hint: Some("jane@corp.example"),
        });
        let u = url::Url::parse(&to).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge"], challenge(&verifier));
        assert_eq!((q["code_challenge_method"].as_str(), q["state"].as_str(), q["nonce"].as_str(), q["scope"].as_str()), ("S256", "s1", NONCE, "openid email"));
        assert_eq!((q["response_type"].as_str(), q["login_hint"].as_str()), ("code", "jane@corp.example"));
        assert!(!to.contains(&verifier), "the verifier stays with the cell");
        let out = end_session_url(&p.end_session_endpoint.unwrap(), Some("x.y.z"), CLIENT, "https://fragment.example/");
        assert_eq!(out, format!("{ISSUER}/logout?id_token_hint=x.y.z&client_id=fragment&post_logout_redirect_uri=https%3A%2F%2Ffragment.example%2F"));
        assert_eq!(scopes(None), Ok(DEFAULT_SCOPES.into()));
        assert_eq!(scopes(Some("  openid   groups ")), Ok("openid groups".into()));
        assert!(scopes(Some("email profile")).is_err(), "without openid no id_token comes back");
    }

    // Goal: no email is assumed. Method: the label falls back from the
    // email to preferred_username to upn to sub; a deployment's map reads
    // other claims (ADFS's unique_name); a bad map is refused.
    #[test]
    fn the_claim_map_falls_back_without_an_email() {
        let map = ClaimMap::default();
        let person = |c: Value| map.person(&Verified { subject: "s-42".into(), claims: c.as_object().unwrap().clone() });
        let p = person(json!({ "email": "jane@corp.example", "preferred_username": "jane", "name": "Jane" }));
        assert_eq!((p.label(), p.handle.as_str(), p.name.as_deref()), ("jane@corp.example", "jane", Some("Jane")));
        assert_eq!(person(json!({ "preferred_username": "jane", "upn": "jane@corp.local" })).label(), "jane");
        assert_eq!(person(json!({ "upn": "jane@corp.local" })).label(), "jane@corp.local");
        assert_eq!(person(json!({ "email": "", "preferred_username": 7 })).label(), "s-42");
        assert_eq!(person(json!({ "email": "x".repeat(CLAIM_MAX + 1) })).email, None);
        let adfs = ClaimMap::parse(Some(r#"{"username": ["unique_name"], "name": "given_name"}"#)).unwrap();
        let p = adfs.person(&Verified { subject: "s".into(), claims: json!({ "unique_name": "CORP\\jane", "given_name": "Jane" }).as_object().unwrap().clone() });
        assert_eq!((p.handle.as_str(), p.email, p.name.as_deref()), ("CORP\\jane", None, Some("Jane")));
        assert!(ClaimMap::parse(Some(r#"{"emails": "mail"}"#)).is_err(), "an unknown field is a typo, not ignored");
        assert!(ClaimMap::parse(Some(r#"{"email": ""}"#)).is_err());
        assert!(ClaimMap::parse(Some("not json")).is_err());
    }
}
