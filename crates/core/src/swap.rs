//! The computer's swap (decisions 22 and 37; Paul, 2026-10-04;
//! docs/computers.md): a guest finds each credential its agent may use as
//! a placeholder in a standard environment variable
//! (`PERPLEXITY_API_KEY=fck_perplexity_<tag>`), so any SDK or CLI sends it
//! where its provider takes a credential, unmodified, with no header of
//! ours. The intercept swaps in the real credential when the request goes
//! to one of that provider's own hosts, in a place its catalog row names
//! (`catalog::Placement`).
//!
//! - `fcx_<provider>_<tag>`: a connection (WorkOS Pipes, as the person);
//! - `fck_<provider>_<tag>`: a key (the operator's, or the person's own).
//!
//! `<tag>` is a keyed MAC of (computer, agent fragment, provider) under a
//! key only the platform holds (`TagKey`, derived from its host secret).
//! So a placeholder names its agent, and no one can make one: the
//! intercept reads the agent from it, among the agents that run on that
//! computer now (an agent removed has no tags that verify), and a guest
//! that leaks one leaks nothing usable from anywhere but that computer.

use std::collections::BTreeMap;

use base64::Engine;
use fragment_proto::ErrorCode;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::catalog::{Catalog, Half, Kind, Placement};

/// A connection's placeholder begins so, a key's so.
pub const CONNECTION_PREFIX: &str = "fcx_";
pub const KEY_PREFIX: &str = "fck_";
/// A tag is this many bytes of its MAC, as lower-case hex.
pub const TAG_BYTES: usize = 16;
pub const TAG_HEX: usize = 2 * TAG_BYTES;
/// The most placeholders one request carries.
pub const PLACEHOLDERS_MAX: usize = 4;
/// What the tag key is derived from the host secret with.
const TAG_SALT: &[u8] = b"fragment credential tags";
const TAG_INFO: &[u8] = b"v1";

/// The key tags are made with: HKDF-SHA256 of a host secret under a label
/// of its own, so it is never stored, never provisioned apart, and rotates
/// with the host secret.
#[derive(Clone)]
pub struct TagKey([u8; 32]);

impl std::fmt::Debug for TagKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TagKey(…)")
    }
}

impl TagKey {
    pub fn derive(host_secret: &str) -> TagKey {
        assert!(host_secret.len() >= crate::seal::HOST_SECRET_MIN_BYTES, "a host secret is checked before tags are made with it");
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(TAG_SALT), host_secret.as_bytes()).expand(TAG_INFO, &mut key).expect("32 bytes is a valid HKDF-SHA256 length");
        TagKey(key)
    }

    fn mac(&self, computer: &str, agent: &str, provider: &str) -> Hmac<Sha256> {
        // NUL-separated: no name holds one, so no two triples MAC alike
        assert!(![computer, agent, provider].iter().any(|s| s.is_empty() || s.contains('\0')), "a tag names a computer, an agent and a provider");
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC takes a key of any length");
        for part in [computer, agent, provider] {
            mac.update(part.as_bytes());
            mac.update(b"\0");
        }
        mac
    }

    /// The tag of `agent` on `computer` for `provider`.
    pub fn tag(&self, computer: &str, agent: &str, provider: &str) -> String {
        hex::encode(&self.mac(computer, agent, provider).finalize().into_bytes()[..TAG_BYTES])
    }

    /// Whether `tag` is that one (in constant time).
    pub fn verifies(&self, computer: &str, agent: &str, provider: &str, tag: &str) -> bool {
        let Ok(bytes) = hex::decode(tag) else { return false };
        bytes.len() == TAG_BYTES && self.mac(computer, agent, provider).verify_truncated_left(&bytes).is_ok()
    }
}

/// The agent among `agents` (those on `computer` now) whose tag for
/// `provider` this is, under any of `keys` (the current host secret's,
/// then the previous one's while a rotation is under way).
pub fn agent_of<'a>(keys: &[TagKey], computer: &str, agents: &'a [String], provider: &str, tag: &str) -> Option<&'a str> {
    assert!(!keys.is_empty(), "a tag is verified under at least one key");
    agents.iter().find(|a| keys.iter().any(|k| k.verifies(computer, a, provider, tag))).map(String::as_str)
}

/// A placeholder's prefix for a provider of `kind`.
pub fn prefix_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Connection => CONNECTION_PREFIX,
        Kind::Operator | Kind::Own => KEY_PREFIX,
    }
}

/// A placeholder, as a guest sends it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Placeholder {
    /// `fcx_` (a connection's) rather than `fck_` (a key's).
    pub connection: bool,
    pub provider: String,
    pub tag: String,
}

impl Placeholder {
    pub fn new(kind: Kind, provider: &str, tag: &str) -> Placeholder {
        assert!(crate::catalog::valid_name(provider) && tag.len() == TAG_HEX, "a placeholder names a provider and a tag");
        Placeholder { connection: kind == Kind::Connection, provider: provider.to_string(), tag: tag.to_string() }
    }

    pub fn text(&self) -> String {
        format!("{}{}_{}", if self.connection { CONNECTION_PREFIX } else { KEY_PREFIX }, self.provider, self.tag)
    }

    /// The placeholder, its tag elided: what a log or a refusal names.
    pub fn named(&self) -> String {
        format!("{}{}_…", if self.connection { CONNECTION_PREFIX } else { KEY_PREFIX }, self.provider)
    }
}

/// Where in a request a placeholder was found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Location {
    /// A header, by its lower-case name.
    Header(String),
    /// A query parameter, by its name.
    Query(String),
    /// A half of `Authorization: Basic`.
    Basic(Half),
    /// The URL's path, where no credential goes.
    Path,
}

impl Location {
    fn describe(&self) -> String {
        match self {
            Location::Header(h) => format!("the {h} header"),
            Location::Query(q) => format!("the query parameter {q}"),
            Location::Basic(Half::User) => "basic auth's user".into(),
            Location::Basic(Half::Password) => "basic auth's password".into(),
            Location::Path => "the path".into(),
        }
    }

    fn allowed_by(&self, p: &Placement) -> bool {
        match (self, p) {
            (Location::Header(h), Placement::Header { name, .. }) => h == name,
            (Location::Query(q), Placement::Query { name }) => q == name,
            (Location::Basic(a), Placement::Basic { half }) => a == half,
            _ => false,
        }
    }
}

/// Why a request's placeholders are refused, typed: each says what to do
/// instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    /// A placeholder's prefix followed by no provider and tag.
    Malformed { at: String },
    /// A provider the deployment does not offer.
    NotOffered { placeholder: String },
    /// A connection's prefix on a key's provider, or the other way.
    WrongKind { placeholder: String, kind: Kind },
    /// Sent to a host that is not its provider's.
    WrongHost { placeholder: String, host: String, hosts: Vec<String> },
    /// In a place its provider does not take its credential.
    WrongPlace { placeholder: String, at: String, places: Vec<String> },
    /// More than one placeholder, or more than the placeholder, in one
    /// place that takes a credential alone.
    NotAlone { at: String },
    /// More than `PLACEHOLDERS_MAX` placeholders.
    TooMany(usize),
}

impl SwapError {
    pub fn code(&self) -> ErrorCode {
        match self {
            // a token is never sent to a host it was not made for
            SwapError::WrongHost { .. } => ErrorCode::Forbidden,
            _ => ErrorCode::InvalidRequest,
        }
    }
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapError::Malformed { at } => write!(f, "a placeholder in {at} is malformed (fcx_<provider>_<tag> or fck_<provider>_<tag>, as its environment variable holds it)"),
            SwapError::NotOffered { placeholder } => write!(f, "{placeholder} names a provider this deployment does not offer"),
            SwapError::WrongKind { placeholder, kind } => write!(f, "{placeholder} has the wrong prefix: the provider is a {}", kind.name()),
            SwapError::WrongHost { placeholder, host, hosts } => write!(f, "{placeholder} is for {}, not {host}", hosts.join(", ")),
            SwapError::WrongPlace { placeholder, at, places } => write!(f, "{placeholder} goes in {}, not {at}", places.join(" or ")),
            SwapError::NotAlone { at } => write!(f, "{at} takes one placeholder, alone"),
            SwapError::TooMany(n) => write!(f, "at most {PLACEHOLDERS_MAX} placeholders a request, not {n}"),
        }
    }
}

impl std::error::Error for SwapError {}

fn token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// The placeholders in `text`, each with where it starts and ends. A
/// placeholder begins with its prefix where no token byte comes before it
/// (so one inside a longer token, a JWT's say, is none), then a provider's
/// name and `_`, and ends where no token byte comes after it. One so begun
/// whose tag is not 32 lower-case hex is malformed (cut short, or run on):
/// refused, never sent on as it is.
fn tokens(text: &str, at: &Location) -> Result<Vec<(usize, usize, Placeholder)>, SwapError> {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    // bounded by the text: each pass moves past a match or ends
    while let Some(off) = text[i..].find("fc") {
        let start = i + off;
        i = start + 2;
        let rest = &text[start..];
        let connection = rest.starts_with(CONNECTION_PREFIX);
        if !(connection || rest.starts_with(KEY_PREFIX)) || (start > 0 && token_byte(bytes[start - 1])) {
            continue;
        }
        let body = &rest[CONNECTION_PREFIX.len()..];
        let name_len = body.bytes().take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-').count();
        let provider = &body[..name_len];
        let after = &body[name_len..];
        // a candidate is a prefix, a provider's name and `_`; past that, a
        // tag that is not one is a placeholder cut short
        if !crate::catalog::valid_name(provider) || !after.starts_with('_') {
            continue;
        }
        let malformed = || SwapError::Malformed { at: at.describe() };
        let tag_len = after[1..].bytes().take_while(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()).count();
        let end = start + CONNECTION_PREFIX.len() + name_len + 1 + tag_len;
        if tag_len != TAG_HEX || bytes.get(end).is_some_and(|b| token_byte(*b)) {
            return Err(malformed());
        }
        out.push((start, end, Placeholder { connection, provider: provider.to_string(), tag: after[1..1 + tag_len].to_string() }));
        i = end;
    }
    Ok(out)
}

/// `Authorization: Basic`'s `(user, password)`, when the value is one.
fn basic_of(value: &str) -> Option<(String, String)> {
    let (scheme, rest) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(rest.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

/// A query string's pieces: each raw `name=value` and its decoded pair.
fn query_pieces(query: &str) -> Vec<(&str, String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|piece| {
            let (k, v) = url::form_urlencoded::parse(piece.as_bytes()).next().map(|(k, v)| (k.into_owned(), v.into_owned())).unwrap_or_default();
            (piece, k, v)
        })
        .collect()
}

/// One placeholder found, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    placeholder: Placeholder,
    at: Location,
    /// The place holds the placeholder alone (a header may hold a scheme
    /// word beside it: `Bearer fck_…`).
    alone: bool,
}

/// A request as the swap reads it: its headers (names lower case), its
/// raw query string and its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub headers: Vec<(String, String)>,
    pub query: Option<String>,
}

/// A request's swap, planned: the placeholders it carries, each checked
/// against the catalog (its provider offered, its kind's prefix, the host
/// its provider's, the place one its provider takes), the distinct ones
/// in `wanted`, in the order they appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    found: Vec<Found>,
    pub wanted: Vec<Placeholder>,
}

impl Plan {
    /// Reads `headers`, `query` and `path` of a request to `host`. A
    /// request with no placeholder plans nothing, and goes on as it came.
    pub fn of(headers: &[(String, String)], query: Option<&str>, path: &str, catalog: &Catalog, host: &str) -> Result<Plan, SwapError> {
        let mut found = vec![];
        let mut add = |text: &str, at: Location| -> Result<(), SwapError> {
            let t = tokens(text, &at)?;
            let alone = t.len() == 1 && t[0].0 == 0 && t[0].1 == text.len();
            for (_, _, placeholder) in t {
                found.push(Found { placeholder, at: at.clone(), alone });
            }
            Ok(())
        };
        for (name, value) in headers {
            assert_eq!(name, &name.to_ascii_lowercase(), "header names are read lower case");
            match (name.as_str(), basic_of(value)) {
                ("authorization", Some((user, password))) => {
                    add(&user, Location::Basic(Half::User))?;
                    add(&password, Location::Basic(Half::Password))?;
                }
                _ => add(value, Location::Header(name.clone()))?,
            }
        }
        for (_, name, value) in query_pieces(query.unwrap_or_default()) {
            add(&value, Location::Query(name))?;
        }
        add(path, Location::Path)?;
        let mut wanted: Vec<Placeholder> = vec![];
        let mut places: Vec<&Location> = vec![];
        for f in &found {
            let text = f.placeholder.named();
            let p = catalog.get(&f.placeholder.provider).ok_or_else(|| SwapError::NotOffered { placeholder: text.clone() })?;
            if (p.kind == Kind::Connection) != f.placeholder.connection {
                return Err(SwapError::WrongKind { placeholder: text, kind: p.kind });
            }
            if !p.hosts.iter().any(|h| h == host) {
                return Err(SwapError::WrongHost { placeholder: text, host: host.to_string(), hosts: p.hosts.clone() });
            }
            if !p.placements.iter().any(|pl| f.at.allowed_by(pl)) {
                return Err(SwapError::WrongPlace { placeholder: text, at: f.at.describe(), places: p.placements.iter().map(Placement::describe).collect() });
            }
            // a query parameter and a half of basic auth are the credential
            // alone; a header is its format, which may hold more
            let alone_needed = !matches!(f.at, Location::Header(_));
            if places.contains(&&f.at) || (alone_needed && !f.alone) {
                return Err(SwapError::NotAlone { at: f.at.describe() });
            }
            places.push(&f.at);
            if !wanted.contains(&f.placeholder) {
                wanted.push(f.placeholder.clone());
            }
        }
        if wanted.len() > PLACEHOLDERS_MAX {
            return Err(SwapError::TooMany(wanted.len()));
        }
        Ok(Plan { found, wanted })
    }

    pub fn is_empty(&self) -> bool {
        self.wanted.is_empty()
    }

    /// The request with each placeholder's credential in its place: a
    /// header is its placement's format around the secret, a query
    /// parameter the secret (percent-encoded), a half of basic auth the
    /// secret (the other half as it came). Everything else goes as it came.
    /// Every wanted placeholder is in `secrets`, each a printable token.
    pub fn apply(&self, headers: &[(String, String)], query: Option<&str>, catalog: &Catalog, secrets: &BTreeMap<Placeholder, String>) -> Outgoing {
        for w in &self.wanted {
            let s = secrets.get(w).expect("every wanted placeholder is resolved before the swap");
            assert!(!s.is_empty() && s.bytes().all(|b| (0x21..=0x7e).contains(&b)), "a credential is a printable token (checked as it is resolved)");
        }
        let at = |loc: &Location| self.found.iter().find(|f| &f.at == loc);
        let format_of = |f: &Found| {
            let p = catalog.get(&f.placeholder.provider).expect("a planned placeholder's provider is offered");
            p.placements.iter().find_map(|pl| match pl {
                Placement::Header { name, format } if Location::Header(name.clone()) == f.at => Some(format.clone()),
                _ => None,
            })
        };
        let mut out = Vec::with_capacity(headers.len());
        for (name, value) in headers {
            let basic = (name == "authorization").then(|| basic_of(value)).flatten();
            let swapped = match basic {
                Some((user, password)) => {
                    let user_f = at(&Location::Basic(Half::User));
                    let pass_f = at(&Location::Basic(Half::Password));
                    if user_f.is_none() && pass_f.is_none() {
                        value.clone()
                    } else {
                        let user = user_f.map_or(user, |f| secrets[&f.placeholder].clone());
                        let password = pass_f.map_or(password, |f| secrets[&f.placeholder].clone());
                        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
                    }
                }
                None => match at(&Location::Header(name.clone())) {
                    Some(f) => format_of(f).expect("a header's placeholder is planned only where its provider takes it").replacen("{}", &secrets[&f.placeholder], 1),
                    None => value.clone(),
                },
            };
            out.push((name.clone(), swapped));
        }
        let query = query.map(|q| {
            query_pieces(q)
                .into_iter()
                .map(|(raw, name, _)| match at(&Location::Query(name.clone())) {
                    Some(f) => {
                        let key: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
                        let value: String = url::form_urlencoded::byte_serialize(secrets[&f.placeholder].as_bytes()).collect();
                        format!("{key}={value}")
                    }
                    None => raw.to_string(),
                })
                .collect::<Vec<_>>()
                .join("&")
        });
        Outgoing { headers: out, query }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const COMPUTER: &str = "computer:0123456789abcdef01234567";
    const SECRET: &str = "a host secret of thirty-two bytes or more, for tests";

    fn key() -> TagKey {
        TagKey::derive(SECRET)
    }

    fn catalog() -> Catalog {
        Catalog::parse(
            &json!([
                { "name": "google", "kind": "connection", "hosts": ["gmail.googleapis.com", "www.googleapis.com"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"] },
                { "name": "perplexity", "kind": "operator", "hosts": ["api.perplexity.ai"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["PERPLEXITY_API_KEY"] },
                { "name": "google-places", "kind": "operator", "hosts": ["places.googleapis.com"], "placements": [{ "header": "x-goog-api-key" }, { "query": "key" }], "env": ["GOOGLE_PLACES_API_KEY"] },
                { "name": "mail", "kind": "own", "hosts": ["api.mail.test"], "placements": [{ "basic": "password" }], "env": ["MAIL_API_KEY"] },
            ])
            .to_string(),
        )
        .unwrap()
    }

    fn ph(kind: Kind, provider: &str, agent: &str) -> Placeholder {
        Placeholder::new(kind, provider, &key().tag(COMPUTER, agent, provider))
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn secrets(pairs: &[(&Placeholder, &str)]) -> BTreeMap<Placeholder, String> {
        pairs.iter().map(|(p, s)| ((*p).clone(), s.to_string())).collect()
    }

    /// Valid: a tag names its agent on its computer for its provider, and
    /// reads back as that agent among the computer's.
    #[test]
    fn a_tag_names_its_agent() {
        let agents = vec!["juniper.paul".to_string(), "maple.paul".to_string()];
        let t = key().tag(COMPUTER, "juniper.paul", "google");
        assert_eq!(t.len(), TAG_HEX);
        assert_eq!(t, key().tag(COMPUTER, "juniper.paul", "google"), "deterministic: a placeholder is stable across reads and restarts");
        assert!(key().verifies(COMPUTER, "juniper.paul", "google", &t));
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &t), Some("juniper.paul"));
        let m = key().tag(COMPUTER, "maple.paul", "google");
        assert_ne!(m, t);
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &m), Some("maple.paul"), "each agent's own");
        let p = ph(Kind::Operator, "perplexity", "juniper.paul");
        assert!(p.text().starts_with("fck_perplexity_") && p.text().len() == "fck_perplexity_".len() + TAG_HEX);
        assert!(ph(Kind::Connection, "google", "juniper.paul").text().starts_with("fcx_google_"));
        assert_eq!(format!("{:?}", key()), "TagKey(…)", "the key is never printed");
    }

    /// Invalid: a forged tag, another computer's agent's, another
    /// provider's, and one under another host secret verify as no agent.
    #[test]
    fn a_forged_or_anothers_tag_names_no_agent() {
        let agents = vec!["juniper.paul".to_string()];
        let forged = "0".repeat(TAG_HEX);
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &forged), None);
        let elsewhere = key().tag("computer:ffffffffffffffffffffffff", "juniper.paul", "google");
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &elsewhere), None, "the same agent name on another computer");
        let skyler = key().tag(COMPUTER, "juniper.skyler", "google");
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &skyler), None, "an agent not on this computer");
        let other = key().tag(COMPUTER, "juniper.paul", "perplexity");
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &other), None, "another provider's tag");
        let rekeyed = TagKey::derive("another host secret of thirty-two bytes, for tests").tag(COMPUTER, "juniper.paul", "google");
        assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", &rekeyed), None);
        for bad in ["", "zz", &"A".repeat(TAG_HEX), &"0".repeat(TAG_HEX - 2)] {
            assert_eq!(agent_of(&[key()], COMPUTER, &agents, "google", bad), None, "{bad}");
        }
    }

    /// Revoked: an agent no longer on the computer has no tag that
    /// verifies; a rotation keeps the previous secret's tags until it ends.
    #[test]
    fn removing_an_agent_revokes_its_tags() {
        let t = key().tag(COMPUTER, "juniper.paul", "google");
        let before = vec!["juniper.paul".to_string(), "maple.paul".to_string()];
        let after = vec!["maple.paul".to_string()];
        assert_eq!(agent_of(&[key()], COMPUTER, &before, "google", &t), Some("juniper.paul"));
        assert_eq!(agent_of(&[key()], COMPUTER, &after, "google", &t), None);
        let next = TagKey::derive("the next host secret, thirty-two bytes or more");
        assert_eq!(agent_of(&[next.clone(), key()], COMPUTER, &before, "google", &t), Some("juniper.paul"), "during a rotation");
        assert_eq!(agent_of(&[next], COMPUTER, &before, "google", &t), None, "after it");
    }

    /// Header, valid: the placeholder's header becomes its format around
    /// the secret, whatever scheme word the guest wrote; other headers go
    /// as they came.
    #[test]
    fn a_header_placement() {
        let c = catalog();
        let p = ph(Kind::Operator, "perplexity", "juniper.paul");
        let headers = h(&[("authorization", &format!("Bearer {}", p.text())), ("accept", "application/json")]);
        let plan = Plan::of(&headers, None, "/search", &c, "api.perplexity.ai").unwrap();
        assert_eq!(plan.wanted, vec![p.clone()]);
        let out = plan.apply(&headers, None, &c, &secrets(&[(&p, "pplx-real")]));
        assert_eq!(out.headers, h(&[("authorization", "Bearer pplx-real"), ("accept", "application/json")]));
        let bare = h(&[("authorization", &p.text())]);
        let out = Plan::of(&bare, None, "/", &c, "api.perplexity.ai").unwrap().apply(&bare, None, &c, &secrets(&[(&p, "pplx-real")]));
        assert_eq!(out.headers, h(&[("authorization", "Bearer pplx-real")]), "the format is the catalog's");
        let gp = ph(Kind::Operator, "google-places", "juniper.paul");
        let headers = h(&[("x-goog-api-key", &gp.text())]);
        let out = Plan::of(&headers, None, "/v1/places:searchText", &c, "places.googleapis.com").unwrap().apply(&headers, None, &c, &secrets(&[(&gp, "AIzaReal")]));
        assert_eq!(out.headers, h(&[("x-goog-api-key", "AIzaReal")]));
        // nothing to swap: as it came
        let plain = h(&[("authorization", "Bearer sk-own"), ("x-request-id", "a.fck_x")]);
        let plan = Plan::of(&plain, Some("q=1"), "/", &c, "api.perplexity.ai").unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.apply(&plain, Some("q=1"), &c, &BTreeMap::new()), Outgoing { headers: plain.clone(), query: Some("q=1".into()) });
        // a JWT holding `fck_` inside a token is no placeholder
        let jwt = h(&[("authorization", "Bearer eyJhbGciOi.xfck_perplexity_abc.sig")]);
        assert!(Plan::of(&jwt, None, "/", &c, "api.perplexity.ai").unwrap().is_empty());
    }

    /// Header, invalid: a malformed placeholder, another provider's host,
    /// the wrong header, the wrong prefix, two in one header, an offer
    /// the deployment has not.
    #[test]
    fn a_header_placement_refused() {
        let c = catalog();
        let p = ph(Kind::Operator, "perplexity", "juniper.paul");
        let plan = |headers: Vec<(String, String)>, host: &str| Plan::of(&headers, None, "/", &c, host);
        let short = &p.text()[..p.text().len() - 1];
        let long = format!("{}0", p.text());
        let run_on = format!("{}Z", p.text());
        for bad in ["Bearer fck_perplexity_", "Bearer fck_perplexity_abc", short, &long, &run_on] {
            assert!(matches!(plan(h(&[("authorization", bad)]), "api.perplexity.ai"), Err(SwapError::Malformed { .. })), "{bad}");
        }
        // no provider's name and `_` after the prefix: no placeholder at all
        for none in ["fck_Perplexity_00000000000000000000000000000000", "fcx__00000000000000000000000000000000", "a.fck_x.b"] {
            assert!(plan(h(&[("x-request-id", none)]), "api.perplexity.ai").unwrap().is_empty(), "{none}");
        }
        let e = plan(h(&[("authorization", &format!("Bearer {}", p.text()))]), "www.googleapis.com").unwrap_err();
        assert!(matches!(e, SwapError::WrongHost { .. }) && e.code() == ErrorCode::Forbidden, "{e}");
        assert!(e.to_string().contains("is for api.perplexity.ai, not www.googleapis.com") && !e.to_string().contains(&p.tag), "a refusal never names the tag: {e}");
        let e = plan(h(&[("x-api-key", &p.text())]), "api.perplexity.ai").unwrap_err();
        assert!(matches!(e, SwapError::WrongPlace { .. }) && e.to_string().contains("goes in authorization: Bearer {}"), "{e}");
        let wrong = Placeholder { connection: true, ..p.clone() };
        assert!(matches!(plan(h(&[("authorization", &wrong.text())]), "api.perplexity.ai"), Err(SwapError::WrongKind { .. })));
        let twice = format!("{} {}", p.text(), p.text());
        assert!(matches!(plan(h(&[("authorization", &twice)]), "api.perplexity.ai"), Err(SwapError::NotAlone { .. })));
        let notion = Placeholder::new(Kind::Connection, "notion", &"0".repeat(TAG_HEX));
        assert!(matches!(plan(h(&[("authorization", &notion.text())]), "api.notion.com"), Err(SwapError::NotOffered { .. })));
        // inside a path's token it is no placeholder: sent as it came, which
        // carries nothing (a key in a URL is a place the catalog cannot name)
        assert!(Plan::of(&[], None, &format!("/bot{}/send", p.text()), &c, "api.perplexity.ai").unwrap().is_empty());
        let path = Plan::of(&[], None, &format!("/{}/send", p.text()), &c, "api.perplexity.ai");
        assert!(matches!(path, Err(SwapError::WrongPlace { .. })), "no credential goes in a path: {path:?}");
    }

    /// Query, valid and invalid: the parameter's value is the secret,
    /// percent-encoded, the rest of the query as it came; the placeholder
    /// must be the value alone, in a parameter its provider takes.
    #[test]
    fn a_query_placement() {
        let c = catalog();
        let gp = ph(Kind::Operator, "google-places", "juniper.paul");
        let q = format!("input=caf%C3%A9+near+me&key={}&fields=name", gp.text());
        let plan = Plan::of(&[], Some(&q), "/maps/api/place", &c, "places.googleapis.com").unwrap();
        let out = plan.apply(&[], Some(&q), &c, &secrets(&[(&gp, "AIza/real+key")]));
        assert_eq!(out.query.as_deref(), Some("input=caf%C3%A9+near+me&key=AIza%2Freal%2Bkey&fields=name"));
        let other = format!("q={}", gp.text());
        assert!(matches!(Plan::of(&[], Some(&other), "/", &c, "places.googleapis.com"), Err(SwapError::WrongPlace { .. })));
        let prefixed = format!("key=x{}", gp.text());
        assert!(Plan::of(&[], Some(&prefixed), "/", &c, "places.googleapis.com").unwrap().is_empty(), "inside a token: none");
        let spaced = format!("key=my+{}", gp.text());
        assert!(matches!(Plan::of(&[], Some(&spaced), "/", &c, "places.googleapis.com"), Err(SwapError::NotAlone { .. })));
        let both = format!("key={}&key={}", gp.text(), gp.text());
        assert!(matches!(Plan::of(&[], Some(&both), "/", &c, "places.googleapis.com"), Err(SwapError::NotAlone { .. })));
    }

    /// Basic auth, valid and invalid: the half its provider names is the
    /// secret, the other as it came, encoded again.
    #[test]
    fn a_basic_placement() {
        let c = catalog();
        let mail = ph(Kind::Own, "mail", "juniper.paul");
        let enc = |s: &str| format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(s));
        let headers = h(&[("authorization", &enc(&format!("api:{}", mail.text())))]);
        let plan = Plan::of(&headers, None, "/v3/send", &c, "api.mail.test").unwrap();
        assert_eq!(plan.wanted, vec![mail.clone()]);
        let out = plan.apply(&headers, None, &c, &secrets(&[(&mail, "key-real")]));
        assert_eq!(out.headers, h(&[("authorization", &enc("api:key-real"))]));
        let user = h(&[("authorization", &enc(&format!("{}:", mail.text())))]);
        assert!(matches!(Plan::of(&user, None, "/", &c, "api.mail.test"), Err(SwapError::WrongPlace { .. })), "the user half is not the one it takes");
        let not_basic = h(&[("authorization", &format!("Bearer {}", mail.text()))]);
        assert!(matches!(Plan::of(&not_basic, None, "/", &c, "api.mail.test"), Err(SwapError::WrongPlace { .. })));
        let plain = h(&[("authorization", &enc("api:own-key"))]);
        assert!(Plan::of(&plain, None, "/", &c, "api.mail.test").unwrap().is_empty());
    }

    /// Replay: planning and applying the same request twice gives the same
    /// request; a swapped request carries no placeholder, so it plans
    /// nothing again.
    #[test]
    fn a_swap_replayed_is_the_same_swap() {
        let c = catalog();
        let gp = ph(Kind::Operator, "google-places", "juniper.paul");
        let mail = ph(Kind::Own, "mail", "juniper.paul");
        let headers = h(&[("x-goog-api-key", &gp.text())]);
        let q = format!("key={}", gp.text());
        let s = secrets(&[(&gp, "AIzaReal")]);
        let once = Plan::of(&headers, Some(&q), "/", &c, "places.googleapis.com").unwrap();
        assert_eq!(once.wanted, vec![gp.clone()], "one placeholder in two places is wanted once");
        let first = once.apply(&headers, Some(&q), &c, &s);
        let again = Plan::of(&headers, Some(&q), "/", &c, "places.googleapis.com").unwrap().apply(&headers, Some(&q), &c, &s);
        assert_eq!(first, again);
        assert!(Plan::of(&first.headers, first.query.as_deref(), "/", &c, "places.googleapis.com").unwrap().is_empty());
        let enc = |s: &str| format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(s));
        let basic = h(&[("authorization", &enc(&format!("api:{}", mail.text())))]);
        let out = Plan::of(&basic, None, "/", &c, "api.mail.test").unwrap().apply(&basic, None, &c, &secrets(&[(&mail, "key-real")]));
        assert!(Plan::of(&out.headers, None, "/", &c, "api.mail.test").unwrap().is_empty());
    }

    #[test]
    fn at_most_four_placeholders_a_request() {
        let c = Catalog::parse(
            &json!([{ "name": "p", "kind": "operator", "hosts": ["a.test"], "placements": [{ "header": "x-a" }, { "header": "x-b" }, { "header": "x-c" }, { "header": "x-d" }], "env": ["P_KEY"], "price": { "micros": 1, "per": 1 } }, { "name": "q", "kind": "operator", "hosts": ["a.test"], "placements": [{ "header": "x-e" }], "env": ["Q_KEY"], "price": { "micros": 1, "per": 1 } }]).to_string(),
        )
        .unwrap();
        let agents = ["a.paul", "b.paul", "c.paul", "d.paul", "e.paul"];
        let mut headers = vec![];
        for (i, header) in ["x-a", "x-b", "x-c", "x-d"].iter().enumerate() {
            headers.push((header.to_string(), ph(Kind::Operator, "p", agents[i]).text()));
        }
        assert_eq!(Plan::of(&headers, None, "/", &c, "a.test").unwrap().wanted.len(), 4);
        headers.push(("x-e".into(), ph(Kind::Operator, "q", agents[4]).text()));
        assert_eq!(Plan::of(&headers, None, "/", &c, "a.test"), Err(SwapError::TooMany(5)));
    }

    #[test]
    #[should_panic(expected = "every wanted placeholder is resolved")]
    fn applying_an_unresolved_placeholder_is_a_bug() {
        let c = catalog();
        let p = ph(Kind::Operator, "perplexity", "juniper.paul");
        let headers = h(&[("authorization", &p.text())]);
        Plan::of(&headers, None, "/", &c, "api.perplexity.ai").unwrap().apply(&headers, None, &c, &BTreeMap::new());
    }

    #[test]
    #[should_panic(expected = "a credential is a printable token")]
    fn a_secret_with_a_line_break_is_a_bug() {
        let c = catalog();
        let p = ph(Kind::Operator, "perplexity", "juniper.paul");
        let headers = h(&[("authorization", &p.text())]);
        Plan::of(&headers, None, "/", &c, "api.perplexity.ai").unwrap().apply(&headers, None, &c, &secrets(&[(&p, "a\r\nx-evil: 1")]));
    }
}
