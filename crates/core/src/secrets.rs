//! Secrets at the egress point (docs/secrets.md); sealing and opening are
//! `seal.rs`.

use fragment_proto::limits;
use url::Url;

/// A host a secret may go to, as a URL names it (`Url::host_str`): a DNS
/// name or an IP literal (`[::1]`), with no scheme, port, path, wildcard,
/// or trailing dot, in lowercase. A host is exact: `api.example.com` is
/// not `example.com`, nor `eu.api.example.com`. `None` when `given` is no
/// such host.
pub fn secret_host(given: &str) -> Option<String> {
    if given.is_empty() || given.len() > limits::SECRET_HOST_MAX_BYTES || given.contains('*') || given.ends_with('.') {
        return None;
    }
    let lower = given.to_ascii_lowercase();
    let url = Url::parse(&format!("https://{lower}/")).ok()?;
    // all of it is the host: a port, a path, credentials, a query, or a
    // name the URL rewrote (unicode to punycode) is not
    let whole = url.port().is_none() && url.path() == "/" && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    if whole && url.host_str() == Some(lower.as_str()) {
        Some(lower)
    } else {
        None
    }
}

/// Whether the secret `name`, which may go to `hosts`, fills a
/// placeholder in a request to `url`: the request's host is one of them,
/// over https. A fleet that allows local egress (dev and the e2e) also
/// fills over http toward a local address, where its fakes listen. A
/// secret that names no host (one set before secrets named them) fills
/// nowhere. The refusal says why.
pub fn may_fill(name: &str, hosts: &[String], url: &Url, allow_local: bool) -> Result<(), String> {
    let host = url.host_str().unwrap_or("").trim_end_matches('.');
    if hosts.is_empty() {
        return Err(format!("secret {name} names no host it may go to; set it again with `fragment secret set <fragment> {name} --to {host}`"));
    }
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("secret {name} may go to {}, not {host}", hosts.join(", ")));
    }
    let https = url.scheme() == "https";
    let local_fake = allow_local && crate::egress::local(url);
    if https || local_fake {
        Ok(())
    } else {
        Err(format!("secret {name} goes over https only, and this request is {}", url.scheme()))
    }
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
            assert_eq!(secret_host(given).as_deref(), Some(kept), "{given}");
        }
        let long = format!("{}.com", "a".repeat(limits::SECRET_HOST_MAX_BYTES));
        for bad in ["", "https://api.example.com", "api.example.com:8443", "api.example.com/v1", "user@api.example.com", "*.example.com", "api.example.com.", "a b.com", "api.example.com?x", "münchen.de", long.as_str()] {
            assert_eq!(secret_host(bad), None, "{bad}");
        }
    }

    /// Goal: a placeholder fills only toward one of its secret's hosts,
    /// over https (the audit's M3). Method: one secret, the request varied
    /// by host, scheme, and whether local egress is allowed (dev's fakes).
    #[test]
    fn a_secret_fills_toward_its_hosts_over_https_only() {
        let api = hosts(&["api.example.com"]);
        assert!(may_fill("K", &api, &url("https://api.example.com/v1"), false).is_ok());
        assert!(may_fill("K", &api, &url("https://api.example.com:8443/v1"), false).is_ok(), "the host is what is named, on any port");
        assert!(may_fill("K", &api, &url("https://API.example.com./v1"), false).is_ok(), "a URL's host is lowercase; a trailing dot is the same name");
        for (to, why) in [
            ("https://evil.example/collect", "not evil.example"),
            ("https://example.com/", "not example.com"),
            ("https://eu.api.example.com/", "not eu.api.example.com"),
            ("http://api.example.com/v1", "https only"),
        ] {
            let refused = may_fill("K", &api, &url(to), false).unwrap_err();
            assert!(refused.contains(why), "{to}: {refused}");
        }
        assert!(may_fill("K", &api, &url("http://api.example.com/v1"), true).is_err(), "local egress is no reason to send a public host cleartext");
        let fake = hosts(&["127.0.0.1"]);
        assert!(may_fill("K", &fake, &url("http://127.0.0.1:9999/data"), true).is_ok(), "dev and the e2e fill toward their fakes");
        assert!(may_fill("K", &fake, &url("http://127.0.0.1:9999/data"), false).is_err());
        let none = may_fill("K", &[], &url("https://api.example.com/"), false).unwrap_err();
        assert!(none.contains("names no host") && none.contains("--to api.example.com"), "{none}");
    }

    #[test]
    fn header_placeholders() {
        assert_eq!(placeholders("Bearer {{API_KEY}}"), vec!["API_KEY"]);
        assert_eq!(placeholders("{{A}}:{{B}}"), vec!["A", "B"]);
        assert!(placeholders("no {{ end").is_empty());
        assert!(placeholders("plain").is_empty());
    }
}
