//! Secrets at the egress point (docs/secrets.md); sealing and opening are
//! `seal.rs`.

use fragment_proto::limits;
use url::Url;

/// A host refused for a secret: not a host as a URL names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotHost(pub String);

impl std::fmt::Display for NotHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} is not a host as a URL names it (api.example.com: no scheme, port, path, or wildcard)", self.0)
    }
}

/// A host a secret may go to, as a URL names it (`Url::host_str`): a DNS
/// name or an IP literal (`[::1]`), with no scheme, port, path, wildcard,
/// or trailing dot, in lowercase. A host is exact: `api.example.com` is
/// not `example.com`, nor `eu.api.example.com`; any port of it is it.
pub fn secret_host(given: &str) -> Result<String, NotHost> {
    let refused = || NotHost(given.to_string());
    if given.is_empty() || given.len() > limits::SECRET_HOST_MAX_BYTES || given.contains('*') || given.ends_with('.') {
        return Err(refused());
    }
    let lower = given.to_ascii_lowercase();
    let url = Url::parse(&format!("https://{lower}/")).map_err(|_| refused())?;
    // all of it is the host: a port, a path, credentials, a query, or a
    // name the URL rewrote (unicode to punycode) is not
    let whole = url.port().is_none() && url.path() == "/" && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    if whole && url.host_str() == Some(lower.as_str()) {
        Ok(lower)
    } else {
        Err(refused())
    }
}

/// Why a placeholder is not filled. No refusal names a host: a secret's
/// hosts are its editors' to know (`GET secrets`), while a run's error is
/// every viewer's, and a hint naming the request's host would invite
/// pasting the very host a job chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfilled {
    /// The secret names no host: set before secrets named them.
    NoHosts,
    /// The request's host is none of the secret's.
    OtherHost,
    /// One of its hosts, but not over https.
    Cleartext,
}

impl Unfilled {
    pub fn message(self, name: &str) -> String {
        match self {
            Unfilled::NoHosts => format!("secret {name} names no host it may go to; set it again with `fragment secret set <fragment> {name} --to <the API's host>`"),
            Unfilled::OtherHost => format!("secret {name} may not go to this request's host: a secret goes only to the hosts it was set for"),
            Unfilled::Cleartext => format!("secret {name} goes over https only"),
        }
    }
}

/// Whether a secret that may go to `hosts` fills a placeholder in a
/// request to `url`: the request's host is one of them (on any port),
/// over https. A fleet that allows local egress (dev and the e2e) also
/// fills over http toward a local address, where its fakes listen.
pub fn may_fill(hosts: &[String], url: &Url, allow_local: bool) -> Result<(), Unfilled> {
    let host = url.host_str().unwrap_or("").trim_end_matches('.');
    if hosts.is_empty() {
        return Err(Unfilled::NoHosts);
    }
    if !hosts.iter().any(|h| h == host) {
        return Err(Unfilled::OtherHost);
    }
    let https = url.scheme() == "https";
    let local_fake = allow_local && crate::egress::local(url);
    if https || local_fake {
        Ok(())
    } else {
        Err(Unfilled::Cleartext)
    }
}

/// Headers that would send a request somewhere its URL does not say (a
/// proxy's or a virtual host's): a request carrying a secret sets none,
/// so its URL's host, the one `may_fill` checked, is where it goes.
pub const ROUTING_HEADERS: [&str; 3] = ["host", "forwarded", "x-forwarded-host"];

/// The first of `names` that is a routing header (`ROUTING_HEADERS`), in
/// any case.
pub fn routing_header<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names.into_iter().find(|n| ROUTING_HEADERS.iter().any(|r| n.eq_ignore_ascii_case(r)))
}

/// The secret names a header value refers to as `{{NAME}}` (a job's
/// fetch; the cell substitutes them at its egress point).
pub fn placeholders(v: &str) -> Vec<&str> {
    let mut names = vec![];
    let mut rest = v;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start + 2..].find("}}") else { break };
        names.push(&rest[start + 2..start + 2 + end]);
        rest = &rest[start + 2 + end + 2..];
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(of: &[&str]) -> Vec<String> {
        of.iter().map(|h| h.to_string()).collect()
    }

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    /// Goal: a host a secret names is a URL's host and nothing more.
    /// Method: the shapes people type, kept or refused.
    #[test]
    fn a_secret_names_exact_hosts() {
        for (given, kept) in [("api.example.com", "api.example.com"), ("API.Example.com", "api.example.com"), ("1.1.1.1", "1.1.1.1"), ("[2606:4700::1111]", "[2606:4700::1111]"), ("127.0.0.1", "127.0.0.1")] {
            assert_eq!(secret_host(given).as_deref(), Ok(kept), "{given}");
        }
        let long = format!("{}.com", "a".repeat(limits::SECRET_HOST_MAX_BYTES));
        for bad in ["", "https://api.example.com", "api.example.com:8443", "api.example.com/v1", "user@api.example.com", "*.example.com", "api.example.com.", "a b.com", "api.example.com?x", "münchen.de", long.as_str()] {
            assert_eq!(secret_host(bad), Err(NotHost(bad.to_string())), "{bad}");
        }
    }

    /// Goal: a placeholder fills only toward one of its secret's hosts,
    /// over https (the audit's M3), and no refusal names a host. Method:
    /// one secret, the request varied by host, scheme, and whether local
    /// egress is allowed (dev's fakes).
    #[test]
    fn a_secret_fills_toward_its_hosts_over_https_only() {
        let api = hosts(&["api.example.com"]);
        assert_eq!(may_fill(&api, &url("https://api.example.com/v1"), false), Ok(()));
        assert_eq!(may_fill(&api, &url("https://api.example.com:8443/v1"), false), Ok(()), "the host is what is named, on any port");
        assert_eq!(may_fill(&api, &url("https://API.example.com./v1"), false), Ok(()), "a URL's host is lowercase; a trailing dot is the same name");
        for to in ["https://evil.example/collect", "https://example.com/", "https://eu.api.example.com/"] {
            assert_eq!(may_fill(&api, &url(to), false), Err(Unfilled::OtherHost), "{to}");
        }
        assert_eq!(may_fill(&api, &url("http://api.example.com/v1"), false), Err(Unfilled::Cleartext));
        assert_eq!(may_fill(&api, &url("http://api.example.com/v1"), true), Err(Unfilled::Cleartext), "local egress is no reason to send a public host cleartext");
        let fake = hosts(&["127.0.0.1"]);
        assert_eq!(may_fill(&fake, &url("http://127.0.0.1:9999/data"), true), Ok(()), "dev and the e2e fill toward their fakes");
        assert_eq!(may_fill(&fake, &url("http://127.0.0.1:9999/data"), false), Err(Unfilled::Cleartext));
        assert_eq!(may_fill(&[], &url("https://api.example.com/"), false), Err(Unfilled::NoHosts));
        for why in [Unfilled::NoHosts, Unfilled::OtherHost, Unfilled::Cleartext] {
            let said = why.message("K");
            assert!(said.contains("secret K") && !said.contains("example"), "{said}");
        }
        assert!(Unfilled::NoHosts.message("K").contains("--to <the API's host>"));
    }

    /// Goal: a request carrying a secret names no other host in a header.
    /// Method: each routing header, in any case, among ordinary ones.
    #[test]
    fn a_secrets_request_sets_no_routing_header() {
        assert_eq!(routing_header(["authorization", "content-type"]), None);
        assert_eq!(routing_header(["authorization", "Host"]), Some("Host"));
        assert_eq!(routing_header(["X-Forwarded-Host"]), Some("X-Forwarded-Host"));
        assert_eq!(routing_header(["forwarded", "accept"]), Some("forwarded"));
        assert_eq!(routing_header(["x-forwarded-for", "hostname"]), None);
    }

    #[test]
    fn header_placeholders() {
        assert_eq!(placeholders("Bearer {{API_KEY}}"), vec!["API_KEY"]);
        assert_eq!(placeholders("{{A}}:{{B}}"), vec!["A", "B"]);
        assert!(placeholders("no {{ end").is_empty());
        assert!(placeholders("plain").is_empty());
    }
}
