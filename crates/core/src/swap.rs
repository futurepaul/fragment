//! The computer's swap (decisions 22 and 37, docs/computers.md): a guest
//! names a credential by a placeholder in a request header, and the
//! intercept swaps in the real one when the request goes to one of that
//! credential's own hosts.
//!
//! - `fragment-connection:<provider>`: a connection through WorkOS Pipes
//!   (the agent's owner's account at that provider);
//! - `fragment-key:<name>`: one of the operator's keys (a paid API).
//!
//! The guest holds placeholders only, so a guest that leaks one leaks
//! nothing; a placeholder sent to a host that is not its credential's is
//! refused, so a swapped token never reaches a host it was not made for.

use std::collections::BTreeMap;

const CONNECTION: &str = "fragment-connection:";
const KEY: &str = "fragment-key:";

/// The most providers or keys a deployment names.
pub const CREDENTIALS_MAX: usize = 64;
/// The most hosts one credential names.
pub const HOSTS_MAX: usize = 16;
/// The most placeholders one request carries.
pub const PLACEHOLDERS_MAX: usize = 4;

/// What a placeholder names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Credential {
    Connection(String),
    Key(String),
}

impl Credential {
    pub fn placeholder(&self) -> String {
        match self {
            Credential::Connection(p) => format!("{CONNECTION}{p}"),
            Credential::Key(k) => format!("{KEY}{k}"),
        }
    }
}

/// A provider's or a key's name: Pipes' slugs (`google-calendar`) and
/// the operator's own (`perplexity`).
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A host a credential is for: a lowercase DNS name with a dot (no port,
/// no wildcard).
pub fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|l| {
            (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-')
        })
}

/// The placeholders in one header value, in order of appearance: each is
/// its prefix and a name, ended by anything a name cannot hold. `Err`
/// when a prefix is followed by no valid name (a malformed placeholder is
/// refused rather than sent on as it is).
pub fn placeholders(value: &str) -> Result<Vec<Credential>, String> {
    let mut found = vec![];
    let mut rest = value;
    loop {
        let next = [(CONNECTION, true), (KEY, false)].into_iter().filter_map(|(p, c)| rest.find(p).map(|i| (i, p, c))).min_by_key(|(i, _, _)| *i);
        let Some((i, prefix, connection)) = next else { break };
        let after = &rest[i + prefix.len()..];
        let len = after.bytes().take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-').count();
        let name = &after[..len];
        if !valid_name(name) {
            return Err(format!("{prefix} names no provider or key"));
        }
        found.push(if connection { Credential::Connection(name.into()) } else { Credential::Key(name.into()) });
        rest = &after[len..];
    }
    Ok(found)
}

/// `value` with each placeholder replaced by its credential's secret.
/// Every placeholder in it must be in `resolved`.
pub fn swapped(value: &str, resolved: &BTreeMap<Credential, String>) -> String {
    let mut out = value.to_string();
    for (credential, secret) in resolved {
        assert!(!secret.is_empty(), "a resolved credential is never empty");
        // a name ends at a byte no name holds, so `fragment-key:p` inside
        // `fragment-key:pq` is left alone
        let placeholder = credential.placeholder();
        let mut next = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&placeholder) {
            let end = i + placeholder.len();
            let whole = rest.as_bytes().get(end).is_none_or(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-'));
            next.push_str(&rest[..i]);
            next.push_str(if whole { secret } else { &placeholder });
            rest = &rest[end..];
        }
        next.push_str(rest);
        out = next;
    }
    out
}

/// A deployment's credentials and their hosts: `{"<name>": ["<host>", …]}`
/// (`FRAGMENT_CONNECTIONS`, `FRAGMENT_OPERATOR_KEYS`).
pub fn parse_hosts(json: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let map: BTreeMap<String, Vec<String>> = serde_json::from_str(json).map_err(|e| format!("not {{\"<name>\": [\"<host>\", …]}}: {e}"))?;
    if map.len() > CREDENTIALS_MAX {
        return Err(format!("at most {CREDENTIALS_MAX} names"));
    }
    for (name, hosts) in &map {
        if !valid_name(name) {
            return Err(format!("{name:?} is not a name (^[a-z0-9-]{{1,32}}$)"));
        }
        if hosts.is_empty() || hosts.len() > HOSTS_MAX {
            return Err(format!("{name}: 1 to {HOSTS_MAX} hosts"));
        }
        if let Some(h) = hosts.iter().find(|h| !valid_host(h)) {
            return Err(format!("{name}: {h:?} is not a lowercase host name"));
        }
    }
    Ok(map)
}

/// Every host any credential names, each once.
pub fn all_hosts<'a>(maps: impl IntoIterator<Item = &'a BTreeMap<String, Vec<String>>>) -> Vec<String> {
    let mut hosts: Vec<String> = maps.into_iter().flat_map(|m| m.values().flatten().cloned()).collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// The worker secret that holds operator key `name` (`perplexity` →
/// `FRAGMENT_KEY_PERPLEXITY`).
pub fn key_secret_name(name: &str) -> String {
    assert!(valid_name(name), "a key's name is checked before its secret is named");
    format!("FRAGMENT_KEY_{}", name.to_ascii_uppercase().replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_in_a_header() {
        assert_eq!(placeholders("Bearer fragment-connection:github").unwrap(), vec![Credential::Connection("github".into())]);
        assert_eq!(placeholders("fragment-key:perplexity").unwrap(), vec![Credential::Key("perplexity".into())]);
        assert_eq!(
            placeholders("a fragment-key:x b fragment-connection:google-calendar,c").unwrap(),
            vec![Credential::Key("x".into()), Credential::Connection("google-calendar".into())]
        );
        assert_eq!(placeholders("Bearer sk-real").unwrap(), vec![]);
        // a prefix with no name is refused, not passed on
        for bad in ["Bearer fragment-connection:", "fragment-key:UPPER", "fragment-connection:-x", "fragment-key: x"] {
            assert!(placeholders(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_swap_replaces_whole_placeholders_only() {
        let resolved = BTreeMap::from([(Credential::Connection("github".into()), "tok".to_string()), (Credential::Key("p".into()), "k1".to_string())]);
        assert_eq!(swapped("Bearer fragment-connection:github", &resolved), "Bearer tok");
        assert_eq!(swapped("fragment-key:p fragment-key:p", &resolved), "k1 k1");
        // `fragment-key:p` inside `fragment-key:pq` is not it
        assert_eq!(swapped("fragment-key:pq", &resolved), "fragment-key:pq");
        assert_eq!(swapped("nothing here", &resolved), "nothing here");
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

    #[test]
    fn a_deployments_hosts() {
        let m = parse_hosts(r#"{"github": ["api.github.com", "uploads.github.com"], "notion": ["api.notion.com"]}"#).unwrap();
        assert_eq!(m["github"].len(), 2);
        let k = parse_hosts(r#"{"perplexity": ["api.perplexity.ai"], "gh2": ["api.github.com"]}"#).unwrap();
        assert_eq!(all_hosts([&m, &k]), vec!["api.github.com", "api.notion.com", "api.perplexity.ai", "uploads.github.com"]);
        for bad in [r#"{"GitHub": ["api.github.com"]}"#, r#"{"github": []}"#, r#"{"github": ["localhost"]}"#, r#"["api.github.com"]"#] {
            assert!(parse_hosts(bad).is_err(), "{bad}");
        }
        assert_eq!(key_secret_name("google-places"), "FRAGMENT_KEY_GOOGLE_PLACES");
    }
}
