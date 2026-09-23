//! Where a job's `fetch` may go. Jobs are author code on a shared fleet,
//! so the platform refuses addresses on the fleet's own networks (loopback,
//! private, link-local, Fly's `.internal` and `.flycast` names) unless the
//! fleet allows local egress (dev and e2e, which call local fakes). The
//! check is on the URL: a public name that resolves to a private address
//! is the fleet's outbound firewall's to stop (docs/technical-debt-ledger.md).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

fn private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT
}

fn private_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    if let Some(v4) = ip.to_ipv4_mapped() {
        return private_v4(v4);
    }
    ip.is_loopback() || ip.is_unspecified() || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00 // unique local (Fly's 6PN is fdaa::/16)
        || (first & 0xffc0) == 0xfe80 // link-local
}

const LOCAL_SUFFIXES: [&str; 5] = ["localhost", "internal", "local", "flycast", "localdomain"];

/// The URL a job may fetch, or why not.
pub fn check(raw: &str, allow_local: bool) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("not a URL: {e}"))?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(format!("{}: only http and https", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in the URL are refused; send them in a header".into());
    }
    let local = match url.host() {
        None => return Err("the URL has no host".into()),
        Some(Host::Ipv4(ip)) => private_v4(ip),
        Some(Host::Ipv6(ip)) => private_v6(ip),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            // a bare name (no dot) resolves inside whatever network the node is on
            !d.contains('.') || LOCAL_SUFFIXES.iter().any(|s| d == *s || d.ends_with(&format!(".{s}"))) || d.parse::<IpAddr>().is_ok()
        }
    };
    if local && !allow_local {
        return Err(format!("{} is on a private network; jobs reach public addresses only", url.host_str().unwrap_or("")));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn public_only() {
        for ok in ["https://openrouter.ai/api/v1/chat", "http://example.com:8080/x?y=1", "https://1.1.1.1/", "https://[2606:4700::1111]/"] {
            assert!(check(ok, false).is_ok(), "{ok}");
        }
        for bad in [
            "ftp://example.com/", "file:///etc/passwd", "https://user:pw@example.com/", "http://localhost:8790/", "http://127.0.0.1/",
            "http://10.0.0.1/", "http://192.168.1.1/", "http://172.16.0.1/", "http://169.254.169.254/latest", "http://[::1]/",
            "http://[fdaa:0:1::3]:8080/", "http://[fe80::1]/", "http://[::ffff:127.0.0.1]/", "http://0.0.0.0/", "http://100.64.0.1/",
            "http://my-app.internal/", "http://my-app.flycast/", "http://a.fragment.localhost:8790/", "http://intranet/", "not a url",
        ] {
            assert!(check(bad, false).is_err(), "{bad}");
        }
        assert!(check("http://127.0.0.1:9999/", true).is_ok(), "dev and e2e allow local egress");
    }
}
