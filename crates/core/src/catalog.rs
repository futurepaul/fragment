//! A deployment's provider catalog (decisions 22 and 37; docs/computers.md,
//! "Connections and operator keys"): every credential its computers'
//! guests may use, one typed row per provider, from the deployment's
//! configuration (`FRAGMENT_PROVIDERS`). A row says
//!
//! - what the provider is: a `connection` (WorkOS Pipes, acting as the
//!   person), an `operator` key (the operator's, metered at its price), or
//!   an `own` key (the person's own, which they give the platform);
//! - its hosts: the only hosts its credential is ever sent to;
//! - where the credential goes on a request (`Placement`): a header in a
//!   format (`Authorization: Bearer {}`), a query parameter, or a half of
//!   HTTP basic auth;
//! - the environment variables a guest finds its placeholder in, the
//!   names the vendor's own SDK reads where it has one;
//! - an operator key's price per call (`price`, or the price book's list
//!   price for it: `fragment_core::price::DEFAULT_KEYS`);
//! - an own key's sign-in (`oauth`): the person connects it from settings,
//!   through the provider's own page, instead of pasting it (Paul,
//!   2026-10-08; `own_signin`);
//! - an own key's models (`models`): the chat-completions base URL a guest
//!   sends its model calls to, and the few models a person is offered for
//!   an agent (its `agent.json`'s `model`).
//!
//! Adding a provider is a catalog row (and, for a connection, the provider
//! enabled in the WorkOS environment): nothing in code names one.

use serde::{Deserialize, Serialize};

/// The most providers a deployment offers.
pub const PROVIDERS_MAX: usize = 64;
/// The most hosts one provider names.
pub const HOSTS_MAX: usize = 16;
/// The most places one provider's credential may go.
pub const PLACEMENTS_MAX: usize = 4;
/// The most environment variables one provider's placeholder is in.
pub const ENV_MAX: usize = 4;
/// A header's format, at most.
pub const FORMAT_MAX_BYTES: usize = 64;
/// A header's or a query parameter's name, at most.
pub const PLACE_NAME_MAX_BYTES: usize = 64;
/// An environment variable's name, at most.
pub const ENV_NAME_MAX_BYTES: usize = 64;
/// A URL a row names (its sign-in's, its models' base), at most.
pub const URL_MAX_BYTES: usize = 512;
/// The most models one provider offers a person to pick from.
pub const MODELS_OFFERED_MAX: usize = 16;
/// A model's id as its provider names it, at most (`anthropic/claude-…`).
pub const MODEL_ID_MAX_BYTES: usize = 128;
/// A model's name for people, at most.
pub const MODEL_NAME_MAX_BYTES: usize = 64;

/// What a provider's credential is (decisions 22 and 37): a `connection`
/// (WorkOS Pipes, a short-lived token acting as the person), an `operator`
/// key (lent to every agent, each call metered to its owner at the row's
/// price and the margin), or an `own` key (the person's, never metered).
/// The wire type is proto's.
pub use fragment_proto::computer::ProviderKind as Kind;

/// Which half of HTTP basic auth (`user:password`) holds the credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Half {
    User,
    Password,
}

/// Where a provider takes its credential. On the wire (the catalog's
/// JSON): `{"header": "authorization", "format": "Bearer {}"}` (the format
/// is `{}` unless named), `{"query": "key"}`, or `{"basic": "password"}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "PlacementWire", into = "PlacementWire")]
pub enum Placement {
    /// A header (its name lower case), its value the format with the
    /// credential in place of `{}`.
    Header { name: String, format: String },
    /// A query parameter, its value the credential.
    Query { name: String },
    /// `Authorization: Basic`, the credential its user or its password.
    Basic { half: Half },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basic: Option<Half>,
}

impl TryFrom<PlacementWire> for Placement {
    type Error = String;
    fn try_from(w: PlacementWire) -> Result<Placement, String> {
        match (w.header, w.format, w.query, w.basic) {
            (Some(name), format, None, None) => Ok(Placement::Header { name, format: format.unwrap_or_else(|| "{}".into()) }),
            (None, None, Some(name), None) => Ok(Placement::Query { name }),
            (None, None, None, Some(half)) => Ok(Placement::Basic { half }),
            _ => Err("a placement is one of {\"header\", \"format\"?}, {\"query\"} or {\"basic\": \"user\"|\"password\"}".into()),
        }
    }
}

impl From<Placement> for PlacementWire {
    fn from(p: Placement) -> PlacementWire {
        match p {
            Placement::Header { name, format } => PlacementWire { header: Some(name), format: Some(format), query: None, basic: None },
            Placement::Query { name } => PlacementWire { header: None, format: None, query: Some(name), basic: None },
            Placement::Basic { half } => PlacementWire { header: None, format: None, query: None, basic: Some(half) },
        }
    }
}

impl Placement {
    /// How a person reads it: `Authorization: Bearer {}`, `?key=`, `basic auth's password`.
    pub fn describe(&self) -> String {
        match self {
            Placement::Header { name, format } => format!("{name}: {format}"),
            Placement::Query { name } => format!("?{name}="),
            Placement::Basic { half: Half::User } => "basic auth's user".into(),
            Placement::Basic { half: Half::Password } => "basic auth's password".into(),
        }
    }
}

/// An operator key's price at list: `micros` per `per` calls (the margin
/// is added by the price book).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub micros: i64,
    pub per: u64,
}

/// An own key the person connects through the provider's own sign-in,
/// never pasted (Paul, 2026-10-08: "I don't want the user to need to use
/// the cli or paste an api key though, they should be able to connect them
/// from settings ideally"): an authorization with PKCE (S256) whose code
/// the platform exchanges for a key the person owns, OpenRouter's shape
/// (`own_signin`). On the wire: `{"authorize", "exchange", "manage"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyOAuth {
    /// Where the person's browser goes; `callback_url`, `code_challenge`,
    /// `code_challenge_method`, `state` and `key_label` are added to its
    /// query.
    pub authorize: String,
    /// Where the code is exchanged for the key: `POST {code,
    /// code_verifier, code_challenge_method}` → `{key}`.
    pub exchange: String,
    /// The provider's page where the person sees and revokes their keys.
    pub manage: String,
}

/// A model a person may pick for an agent: its id as the provider names it,
/// and its name for people.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedModel {
    pub id: String,
    pub name: String,
}

/// The models an own key's provider serves in OpenAI's chat-completions
/// shape: where a guest sends their calls (`base_url`, on one of the row's
/// hosts, so the swap adds the key), and the few a person is offered for
/// an agent (any of the provider's may be named in `agent.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    pub base_url: String,
    pub offer: Vec<OfferedModel>,
}

/// One provider a deployment offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub name: String,
    pub kind: Kind,
    pub hosts: Vec<String>,
    pub placements: Vec<Placement>,
    pub env: Vec<String>,
    /// An operator key's price (its list price per call). Named on an
    /// operator row or taken from the price book's defaults for its name;
    /// never on another kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    /// An own key's sign-in: connected from settings, never pasted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<KeyOAuth>,
    /// An own key's models, which a person may pick for their agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Models>,
}

/// Why a catalog is refused (the deploy checks it first; a node with a
/// bad one refuses its first request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// Not a JSON list of providers.
    Shape(String),
    TooMany(usize),
    /// A provider's name is not `^[a-z0-9-]{1,32}$` (no leading or trailing `-`).
    Name(String),
    /// Two rows name one provider.
    Duplicate(String),
    /// Its hosts are none, too many, or one is not a lower-case host name.
    Hosts { provider: String, why: String },
    /// Its placements are none, too many, conflicting, or one is malformed.
    Placement { provider: String, why: String },
    /// Its environment variables are none, too many, or one is not a name a
    /// guest may be given.
    Env { provider: String, why: String },
    /// Two providers name one environment variable.
    DuplicateEnv(String),
    /// A price on a row that is no operator key, or one out of bounds.
    Price { provider: String, why: String },
    /// An operator key with no price, named or by default.
    NoPrice(String),
    /// A sign-in on a row that is no own key, or one whose URLs are not
    /// https (or a local fleet's loopback).
    OAuth { provider: String, why: String },
    /// Models on a row that is no own key, a base URL not on its hosts, or
    /// an offer that is empty, too long, or names a model badly or twice.
    Models { provider: String, why: String },
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Shape(e) => write!(f, "the providers are a list of {{name, kind, hosts, placements, env, price?}}: {e}"),
            CatalogError::TooMany(n) => write!(f, "at most {PROVIDERS_MAX} providers, not {n}"),
            CatalogError::Name(n) => write!(f, "{n:?} is not a provider's name (^[a-z0-9-]{{1,32}}$)"),
            CatalogError::Duplicate(n) => write!(f, "{n} is named twice"),
            CatalogError::Hosts { provider, why } => write!(f, "{provider}'s hosts: {why}"),
            CatalogError::Placement { provider, why } => write!(f, "{provider}'s placements: {why}"),
            CatalogError::Env { provider, why } => write!(f, "{provider}'s env: {why}"),
            CatalogError::DuplicateEnv(n) => write!(f, "two providers name the environment variable {n}"),
            CatalogError::Price { provider, why } => write!(f, "{provider}'s price: {why}"),
            CatalogError::NoPrice(n) => write!(f, "the operator key {n} has no price: name one (micro-dollars per calls at list), since the price book has none for it"),
            CatalogError::OAuth { provider, why } => write!(f, "{provider}'s oauth: {why}"),
            CatalogError::Models { provider, why } => write!(f, "{provider}'s models: {why}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// A provider's or a key's name: Pipes' slugs (`google-calendar`) and the
/// operator's own (`perplexity`).
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A host a credential is for: a lower-case DNS name with a dot (no port,
/// no wildcard).
pub fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|l| (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-'))
}

/// Headers a credential may never be put in: the hop's own, the body's
/// framing, cookies, and the platform's (`x-fragment-…`).
const RESERVED_HEADERS: [&str; 13] =
    ["connection", "keep-alive", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "host", "content-length", "content-type", "cookie", "set-cookie"];

/// Environment variables a guest image relies on: never a credential's.
const RESERVED_ENV: [&str; 11] = ["PATH", "HOME", "SHELL", "USER", "ENV", "TERM", "PWD", "PYTHONPATH", "PYTHONSTARTUP", "NODE_OPTIONS", "SSL_CERT_FILE"];
const RESERVED_ENV_PREFIXES: [&str; 4] = ["FRAGMENT_", "LD_", "BASH_", "HERMES_"];

fn header_name_ok(name: &str) -> bool {
    (1..=PLACE_NAME_MAX_BYTES).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !RESERVED_HEADERS.contains(&name)
        && !name.starts_with("x-fragment-")
}

fn format_ok(format: &str) -> bool {
    format.len() <= FORMAT_MAX_BYTES && format.bytes().all(|b| (0x20..=0x7e).contains(&b)) && format.matches("{}").count() == 1 && format.matches(['{', '}']).count() == 2
}

fn query_name_ok(name: &str) -> bool {
    (1..=PLACE_NAME_MAX_BYTES).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether `name` is an environment variable a guest may be given a
/// placeholder in: upper case, and none an image relies on.
pub fn env_name_ok(name: &str) -> bool {
    (1..=ENV_NAME_MAX_BYTES).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
        && !RESERVED_ENV.contains(&name)
        && !RESERVED_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn check_placements(p: &Provider) -> Result<(), String> {
    if p.placements.is_empty() || p.placements.len() > PLACEMENTS_MAX {
        return Err(format!("1 to {PLACEMENTS_MAX}, not {}", p.placements.len()));
    }
    let mut headers = vec![];
    let mut queries = vec![];
    let mut basic = false;
    for placement in &p.placements {
        match placement {
            Placement::Header { name, format } => {
                if !header_name_ok(name) {
                    return Err(format!("{name:?} is not a header a credential may go in (lower case; no hop, framing, cookie or x-fragment- header)"));
                }
                if !format_ok(format) {
                    return Err(format!("{format:?} is not a format: printable, at most {FORMAT_MAX_BYTES} bytes, `{{}}` once where the credential goes"));
                }
                headers.push(name.as_str());
            }
            Placement::Query { name } => {
                if !query_name_ok(name) {
                    return Err(format!("{name:?} is not a query parameter's name"));
                }
                queries.push(name.as_str());
            }
            Placement::Basic { .. } => {
                if basic {
                    return Err("basic auth is named twice".into());
                }
                basic = true;
            }
        }
    }
    let mut sorted = headers.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != headers.len() {
        return Err("a header is named twice".into());
    }
    let mut sorted = queries.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != queries.len() {
        return Err("a query parameter is named twice".into());
    }
    if basic && headers.contains(&"authorization") {
        return Err("basic auth is the authorization header: name one or the other".into());
    }
    Ok(())
}

/// Whether `host` is this machine's own loopback, by name or address.
pub fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// A URL a row names, checked: https on a host name (no port), or with
/// `loopback`, a local fleet's fake at `http://127.0.0.1:<port>` (the
/// cell refuses one but on a fleet whose egress may reach local addresses).
/// No user, no fragment. Answers its host.
pub fn row_url(u: &str, loopback: bool) -> Result<String, String> {
    if u.len() > URL_MAX_BYTES {
        return Err(format!("a URL is at most {URL_MAX_BYTES} bytes"));
    }
    let parsed = url::Url::parse(u).map_err(|e| format!("{u:?} is not a URL: {e}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return Err(format!("{u:?} names a user or a fragment"));
    }
    let host = parsed.host_str().unwrap_or_default().to_string();
    match parsed.scheme() {
        "https" if valid_host(&host) && parsed.port().is_none() => Ok(host),
        "http" if loopback && is_loopback(&host) && parsed.port().is_some() => Ok(host),
        _ => Err(format!("{u:?} is not https on a host name{}", if loopback { " (nor a local fleet's http://127.0.0.1:<port>)" } else { "" })),
    }
}

/// Whether `id` is a model's id as a provider names one: 1 to
/// `MODEL_ID_MAX_BYTES` of letters, digits and `.`, `_`, `-`, `:`, `/`, `@`.
pub fn valid_model_id(id: &str) -> bool {
    (1..=MODEL_ID_MAX_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'@'))
}

fn check_oauth(o: &KeyOAuth) -> Result<(), String> {
    row_url(&o.authorize, true).map_err(|e| format!("authorize: {e}"))?;
    row_url(&o.exchange, true).map_err(|e| format!("exchange: {e}"))?;
    row_url(&o.manage, false).map_err(|e| format!("manage: {e}"))?;
    Ok(())
}

fn check_models(m: &Models, hosts: &[String]) -> Result<(), String> {
    let host = row_url(&m.base_url, false).map_err(|e| format!("base_url: {e}"))?;
    if !hosts.contains(&host) {
        return Err(format!("base_url's host {host} is none of its hosts, so its key would never be added"));
    }
    if m.base_url.ends_with('/') || m.base_url.contains('?') {
        return Err("base_url is named without a query or a trailing /: a guest adds /chat/completions".into());
    }
    if m.offer.is_empty() || m.offer.len() > MODELS_OFFERED_MAX {
        return Err(format!("it offers 1 to {MODELS_OFFERED_MAX} models, not {}", m.offer.len()));
    }
    let mut ids = vec![];
    for o in &m.offer {
        if !valid_model_id(&o.id) {
            return Err(format!("{:?} is not a model's id", o.id));
        }
        if !(1..=MODEL_NAME_MAX_BYTES).contains(&o.name.len()) || !o.name.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            return Err(format!("{}'s name is 1 to {MODEL_NAME_MAX_BYTES} printable characters", o.id));
        }
        if ids.contains(&o.id.as_str()) {
            return Err(format!("{} is offered twice", o.id));
        }
        ids.push(o.id.as_str());
    }
    Ok(())
}

/// A deployment's providers, checked: each is offered as its row says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    providers: Vec<Provider>,
}

impl Catalog {
    /// The catalog `json` (`FRAGMENT_PROVIDERS`) names: a list of rows.
    pub fn parse(json: &str) -> Result<Catalog, CatalogError> {
        let rows: Vec<Provider> = serde_json::from_str(json).map_err(|e| CatalogError::Shape(e.to_string()))?;
        Catalog::of(rows)
    }

    /// `rows`, checked, each operator key with its price (its own, or the
    /// price book's default for its name).
    pub fn of(mut rows: Vec<Provider>) -> Result<Catalog, CatalogError> {
        if rows.len() > PROVIDERS_MAX {
            return Err(CatalogError::TooMany(rows.len()));
        }
        let mut names = vec![];
        let mut envs: Vec<String> = vec![];
        for p in &mut rows {
            if !valid_name(&p.name) {
                return Err(CatalogError::Name(p.name.clone()));
            }
            if names.contains(&p.name) {
                return Err(CatalogError::Duplicate(p.name.clone()));
            }
            names.push(p.name.clone());
            let provider = p.name.clone();
            if p.hosts.is_empty() || p.hosts.len() > HOSTS_MAX {
                return Err(CatalogError::Hosts { provider, why: format!("1 to {HOSTS_MAX}, not {}", p.hosts.len()) });
            }
            if let Some(h) = p.hosts.iter().find(|h| !valid_host(h)) {
                return Err(CatalogError::Hosts { provider, why: format!("{h:?} is not a lower-case host name") });
            }
            p.hosts.sort();
            p.hosts.dedup();
            check_placements(p).map_err(|why| CatalogError::Placement { provider: provider.clone(), why })?;
            if p.env.is_empty() || p.env.len() > ENV_MAX {
                return Err(CatalogError::Env { provider, why: format!("1 to {ENV_MAX} names, not {}", p.env.len()) });
            }
            for e in &p.env {
                if !env_name_ok(e) {
                    return Err(CatalogError::Env { provider, why: format!("{e:?} is not one a guest may be given (upper case; none an image relies on, nor FRAGMENT_…)") });
                }
                if envs.contains(e) {
                    return Err(CatalogError::DuplicateEnv(e.clone()));
                }
                envs.push(e.clone());
            }
            match (p.kind, p.price) {
                (Kind::Operator, None) => {
                    let (micros, per) = crate::price::default_key_price(&p.name).ok_or_else(|| CatalogError::NoPrice(p.name.clone()))?;
                    p.price = Some(Price { micros, per });
                }
                (Kind::Operator, Some(price)) => {
                    if !(1..=crate::price::PRICE_MAX).contains(&price.micros) || !(1..=crate::price::QUANTITY_MAX).contains(&price.per) {
                        return Err(CatalogError::Price { provider, why: "at least one micro-dollar per at least one call, within the price book's bounds".into() });
                    }
                }
                (_, Some(_)) => return Err(CatalogError::Price { provider, why: format!("a {} has no price: only an operator key is metered", p.kind.name()) }),
                (_, None) => {}
            }
            if let Some(o) = &p.oauth {
                if p.kind != Kind::Own {
                    return Err(CatalogError::OAuth { provider, why: format!("a {} is not signed in to: only an own key is", p.kind.name()) });
                }
                check_oauth(o).map_err(|why| CatalogError::OAuth { provider: provider.clone(), why })?;
            }
            if let Some(m) = &p.models {
                if p.kind != Kind::Own {
                    return Err(CatalogError::Models { provider, why: format!("a {} offers no models: only an own key does, its owner's credit paying", p.kind.name()) });
                }
                check_models(m, &p.hosts).map_err(|why| CatalogError::Models { provider: provider.clone(), why })?;
            }
        }
        Ok(Catalog { providers: rows })
    }

    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    pub fn get(&self, name: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.name == name)
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Every host any provider names, each once: what the swap intercepts.
    pub fn hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self.providers.iter().flat_map(|p| p.hosts.iter().cloned()).collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// Every environment variable any provider names: what a guest may be
    /// given (so an image can pass them all through, whichever it holds).
    pub fn env_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.iter().flat_map(|p| p.env.iter().cloned()).collect();
        names.sort();
        names
    }

    /// The operator keys' prices, as the price book takes them.
    pub fn key_prices(&self) -> Vec<crate::price::KeyPrice> {
        self.providers
            .iter()
            .filter_map(|p| p.price.filter(|_| p.kind == Kind::Operator).map(|price| crate::price::KeyPrice { key: p.name.clone(), micros: price.micros, per: price.per }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn google() -> serde_json::Value {
        json!({
            "name": "google", "kind": "connection",
            "hosts": ["gmail.googleapis.com", "www.googleapis.com"],
            "placements": [{ "header": "authorization", "format": "Bearer {}" }],
            "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"],
        })
    }

    fn parse(v: serde_json::Value) -> Result<Catalog, CatalogError> {
        Catalog::parse(&v.to_string())
    }

    /// Valid: each kind, each placement, and an operator key's default
    /// price from the price book.
    #[test]
    fn a_catalog_of_every_kind_and_placement() {
        let c = parse(json!([
            google(),
            { "name": "perplexity", "kind": "operator", "hosts": ["api.perplexity.ai"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["PERPLEXITY_API_KEY"] },
            { "name": "google-places", "kind": "operator", "hosts": ["places.googleapis.com"], "placements": [{ "header": "x-goog-api-key" }, { "query": "key" }], "env": ["GOOGLE_PLACES_API_KEY"], "price": { "micros": 1, "per": 1 } },
            { "name": "mail", "kind": "own", "hosts": ["api.mail.test"], "placements": [{ "basic": "password" }], "env": ["MAIL_API_KEY"] },
        ]))
        .unwrap();
        assert_eq!(c.providers().len(), 4);
        assert_eq!(c.get("perplexity").unwrap().price, Some(Price { micros: 5_000_000, per: 1_000 }), "the price book's list price");
        assert_eq!(c.get("google-places").unwrap().price, Some(Price { micros: 1, per: 1 }), "a row's own price wins");
        assert_eq!(c.get("google-places").unwrap().placements[0], Placement::Header { name: "x-goog-api-key".into(), format: "{}".into() }, "a header's format is {{}} unless named");
        assert_eq!(c.get("mail").unwrap().placements[0], Placement::Basic { half: Half::Password });
        assert_eq!(c.hosts(), vec!["api.mail.test", "api.perplexity.ai", "gmail.googleapis.com", "places.googleapis.com", "www.googleapis.com"]);
        assert_eq!(c.env_names(), vec!["GOOGLE_OAUTH_ACCESS_TOKEN", "GOOGLE_PLACES_API_KEY", "MAIL_API_KEY", "PERPLEXITY_API_KEY"]);
        assert_eq!(c.key_prices().iter().map(|k| k.key.as_str()).collect::<Vec<_>>(), vec!["perplexity", "google-places"], "only operator keys are priced");
        // the wire form reads back as it is written
        let again = Catalog::parse(&serde_json::to_string(c.providers()).unwrap()).unwrap();
        assert_eq!(again, c);
        assert!(Catalog::parse("[]").unwrap().is_empty());
    }

    /// Invalid: every rule, each a typed refusal.
    #[test]
    fn a_catalog_that_breaks_a_rule_is_refused() {
        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut g = google();
            f(&mut g);
            parse(json!([g]))
        };
        assert!(matches!(Catalog::parse("{}"), Err(CatalogError::Shape(_))));
        assert!(matches!(with(&|g| g["kind"] = json!("partner")), Err(CatalogError::Shape(_))));
        assert!(matches!(with(&|g| g["extra"] = json!(1)), Err(CatalogError::Shape(_))), "unknown fields are refused");
        assert!(matches!(with(&|g| g["name"] = json!("Google")), Err(CatalogError::Name(_))));
        assert!(matches!(parse(json!([google(), google()])), Err(CatalogError::Duplicate(_))));
        assert!(matches!(with(&|g| g["hosts"] = json!([])), Err(CatalogError::Hosts { .. })));
        assert!(matches!(with(&|g| g["hosts"] = json!(["localhost"])), Err(CatalogError::Hosts { .. })));
        assert!(matches!(with(&|g| g["hosts"] = json!(["*.googleapis.com"])), Err(CatalogError::Hosts { .. })));
        assert!(matches!(with(&|g| g["placements"] = json!([])), Err(CatalogError::Placement { .. })));
        for bad in [
            json!({ "header": "Authorization" }),
            json!({ "header": "host" }),
            json!({ "header": "x-fragment-agent" }),
            json!({ "header": "authorization", "format": "Bearer" }),
            json!({ "header": "authorization", "format": "{} {}" }),
            json!({ "header": "authorization", "format": "Bearer {x}" }),
            json!({ "query": "a b" }),
            json!({ "query": "key", "format": "{}" }),
            json!({ "basic": "both" }),
            json!({}),
        ] {
            let r = with(&|g| g["placements"] = json!([bad.clone()]));
            assert!(matches!(r, Err(CatalogError::Placement { .. }) | Err(CatalogError::Shape(_))), "{bad}: {r:?}");
        }
        assert!(matches!(with(&|g| g["placements"] = json!([{ "basic": "user" }, { "header": "authorization" }])), Err(CatalogError::Placement { .. })));
        assert!(matches!(with(&|g| g["placements"] = json!([{ "query": "k" }, { "query": "k" }])), Err(CatalogError::Placement { .. })));
        assert!(matches!(with(&|g| g["env"] = json!([])), Err(CatalogError::Env { .. })));
        for bad in ["lower", "PATH", "FRAGMENT_API", "LD_PRELOAD", "HERMES_HOME", "1KEY", "A-B"] {
            assert!(matches!(with(&|g| g["env"] = json!([bad])), Err(CatalogError::Env { .. })), "{bad}");
        }
        let twice = json!([google(), { "name": "gmail", "kind": "connection", "hosts": ["gmail.googleapis.com"], "placements": [{ "header": "authorization" }], "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"] }]);
        assert!(matches!(parse(twice), Err(CatalogError::DuplicateEnv(_))));
        assert!(matches!(with(&|g| g["price"] = json!({ "micros": 1, "per": 1 })), Err(CatalogError::Price { .. })), "a connection is not metered");
        let free = json!([{ "name": "nonesuch", "kind": "operator", "hosts": ["a.test"], "placements": [{ "header": "x-api-key" }], "env": ["NONESUCH_KEY"] }]);
        assert!(matches!(parse(free), Err(CatalogError::NoPrice(_))), "an operator key the book prices not needs its own price");
        let zero = json!([{ "name": "k", "kind": "operator", "hosts": ["a.test"], "placements": [{ "header": "x-api-key" }], "env": ["K_KEY"], "price": { "micros": 0, "per": 1 } }]);
        assert!(matches!(parse(zero), Err(CatalogError::Price { .. })));
        let many: Vec<serde_json::Value> = (0..=PROVIDERS_MAX).map(|i| json!({ "name": format!("p{i}"), "kind": "connection", "hosts": ["a.test"], "placements": [{ "header": "x-api-key" }], "env": [format!("P{i}_KEY")] })).collect();
        assert!(matches!(parse(json!(many)), Err(CatalogError::TooMany(_))));
    }

    /// An own key connected through its provider's sign-in, with models: OpenRouter's row.
    pub(crate) fn router() -> serde_json::Value {
        json!({
            "name": "openrouter", "kind": "own", "hosts": ["openrouter.ai"],
            "placements": [{ "header": "authorization", "format": "Bearer {}" }],
            "env": ["OPENROUTER_API_KEY"],
            "oauth": { "authorize": "https://openrouter.ai/auth", "exchange": "https://openrouter.ai/api/v1/auth/keys", "manage": "https://openrouter.ai/settings/keys" },
            "models": { "base_url": "https://openrouter.ai/api/v1", "offer": [{ "id": "anthropic/claude-sonnet-5.5", "name": "Claude Sonnet 5.5" }, { "id": "z-ai/glm-5.3", "name": "GLM-5.3" }] },
        })
    }

    /// Valid: an own key's sign-in and models, read back as written; a
    /// local fleet's fake may be signed in to over loopback http.
    #[test]
    fn an_own_key_signed_in_to_with_models() {
        let c = parse(json!([router()])).unwrap();
        let p = c.get("openrouter").unwrap();
        assert_eq!(p.oauth.as_ref().unwrap().exchange, "https://openrouter.ai/api/v1/auth/keys");
        assert_eq!(p.models.as_ref().unwrap().offer[0], OfferedModel { id: "anthropic/claude-sonnet-5.5".into(), name: "Claude Sonnet 5.5".into() });
        assert_eq!(Catalog::parse(&serde_json::to_string(c.providers()).unwrap()).unwrap(), c, "the wire form reads back");
        let mut local = router();
        local["oauth"]["authorize"] = json!("http://127.0.0.1:4810/auth");
        local["oauth"]["exchange"] = json!("http://localhost:4810/api/v1/auth/keys");
        assert!(parse(json!([local])).is_ok(), "a local fleet's fake");
        assert_eq!(row_url("https://openrouter.ai/auth", false).unwrap(), "openrouter.ai");
    }

    /// Invalid: a sign-in or models on a row that is no own key, a URL that
    /// is not https (or loopback where it may be), a base URL off its hosts,
    /// and an offer that is empty, too long, or names a model badly or twice.
    #[test]
    fn a_sign_in_or_models_that_break_a_rule_are_refused() {
        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut r = router();
            f(&mut r);
            parse(json!([r]))
        };
        assert!(matches!(with(&|r| r["kind"] = json!("connection")), Err(CatalogError::OAuth { .. })));
        assert!(matches!(with(&|r| { r["kind"] = json!("connection"); r.as_object_mut().unwrap().remove("oauth"); }), Err(CatalogError::Models { .. })));
        for bad in ["http://openrouter.ai/auth", "https://openrouter.ai:8443/auth", "https://user@openrouter.ai/auth", "https://openrouter.ai/auth#x", "ftp://openrouter.ai/auth", "http://10.0.0.1:80/auth", "http://127.0.0.1/auth", "not a url"] {
            assert!(matches!(with(&|r| r["oauth"]["authorize"] = json!(bad)), Err(CatalogError::OAuth { .. })), "{bad}");
        }
        assert!(matches!(with(&|r| r["oauth"]["manage"] = json!("http://127.0.0.1:4810/keys")), Err(CatalogError::OAuth { .. })), "a page for people is https");
        assert!(matches!(with(&|r| r["oauth"]["extra"] = json!(1)), Err(CatalogError::Shape(_))));
        assert!(matches!(with(&|r| r["oauth"]["authorize"] = json!(format!("https://openrouter.ai/{}", "a".repeat(URL_MAX_BYTES)))), Err(CatalogError::OAuth { .. })));
        for bad in ["https://api.openrouter.ai/v1", "https://openrouter.ai/api/v1/", "https://openrouter.ai/api/v1?x=1", "http://127.0.0.1:4810/v1"] {
            assert!(matches!(with(&|r| r["models"]["base_url"] = json!(bad)), Err(CatalogError::Models { .. })), "{bad}");
        }
        assert!(matches!(with(&|r| r["models"]["offer"] = json!([])), Err(CatalogError::Models { .. })));
        let many: Vec<serde_json::Value> = (0..=MODELS_OFFERED_MAX).map(|i| json!({ "id": format!("m/{i}"), "name": format!("M{i}") })).collect();
        assert!(matches!(with(&|r| r["models"]["offer"] = json!(many)), Err(CatalogError::Models { .. })));
        for bad in [json!({ "id": "a b", "name": "A" }), json!({ "id": "", "name": "A" }), json!({ "id": "a", "name": "" }), json!({ "id": "a", "name": "tab\there" }), json!({ "id": "x".repeat(MODEL_ID_MAX_BYTES + 1), "name": "X" })] {
            assert!(matches!(with(&|r| r["models"]["offer"] = json!([bad.clone()])), Err(CatalogError::Models { .. })), "{bad}");
        }
        assert!(matches!(with(&|r| r["models"]["offer"] = json!([{ "id": "a", "name": "A" }, { "id": "a", "name": "B" }])), Err(CatalogError::Models { .. })), "named twice");
        assert!(valid_model_id("anthropic/claude-sonnet-5.5") && valid_model_id("deepseek/deepseek-v4.1-flash:floor") && !valid_model_id("a\"b"));
    }

    #[test]
    fn names_and_hosts() {
        for ok in ["github", "google-calendar", "x1"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "GitHub", "a_b", "-a", "a-", &"a".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
        for ok in ["api.github.com", "api.github.test", "a-b.example"] {
            assert!(valid_host(ok), "{ok}");
        }
        for bad in ["localhost", "API.github.com", "*.github.com", "api.github.com:443", "a..b", "-a.b"] {
            assert!(!valid_host(bad), "{bad}");
        }
    }
}
