//! fragment on a home network, run as a company runs an intranet
//! (docs/self-host-lan.md): a delegated subzone (`fragment.home.arpa`, RFC
//! 8375) that a small DNS server on the box answers, forwarding every other
//! name to the router; a private CA made on the box, constrained to that
//! zone, whose root each device installs; a TLS front door before the cell
//! and Dex; and Dex, pinned, as the identity provider.
//!
//! `cargo xtask dev --lan` makes the state (`ca`, `dex`), then runs
//! `fragment-lan serve <config>` (`serve`): the one process that binds the
//! privileged ports (53, 80, 443), so it is the one binary that needs
//! `cap_net_bind_service`.

pub mod ca;
pub mod dex;
pub mod dns;
pub mod door;
pub mod serve;

/// What a zone may be: lower-case DNS labels, at least two, each 1 to 63
/// bytes of letters, digits and inner hyphens, 253 bytes at most.
pub fn valid_zone(zone: &str) -> bool {
    let labels: Vec<&str> = zone.split('.').collect();
    zone.len() <= 253 && labels.len() >= 2 && labels.iter().all(|l| valid_label(l))
}

/// One lower-case DNS label: 1 to 63 bytes of letters, digits and inner
/// hyphens.
pub fn valid_label(l: &str) -> bool {
    (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-')
}

/// A host's name without its port, lower-cased and without a final dot:
/// `Host` as a client sends it.
pub fn host_name(host: &str) -> String {
    let name = match host.strip_prefix('[') {
        // an IPv6 literal keeps its brackets
        Some(rest) => rest.split_once(']').map_or(host, |(inner, _)| &host[..inner.len() + 2]),
        None => match host.rsplit_once(':') {
            Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => host,
        },
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether `name` is the zone or a name under it.
pub fn in_zone(name: &str, zone: &str) -> bool {
    name == zone || name.strip_suffix(zone).is_some_and(|rest| rest.len() > 1 && rest.ends_with('.') && !rest.starts_with('.'))
}

/// The origin a client names for `host` behind the front door's port: no
/// port for 443.
pub fn https_origin(host: &str, port: u16) -> String {
    match port {
        443 => format!("https://{host}"),
        p => format!("https://{host}:{p}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_are_lower_case_dns_names() {
        for ok in ["fragment.home.arpa", "a.b", "x-1.corp.example"] {
            assert!(valid_zone(ok), "{ok}");
        }
        for bad in ["arpa", "Fragment.home.arpa", "-a.b", "a-.b", "a..b", "a.b.", "*.a.b", "a_b.c", ""] {
            assert!(!valid_zone(bad), "{bad}");
        }
        assert!(!valid_zone(&format!("{}.b", "a".repeat(64))));
    }

    #[test]
    fn a_host_loses_its_port_and_case() {
        assert_eq!(host_name("Fragment.Home.Arpa:8443"), "fragment.home.arpa");
        assert_eq!(host_name("dex.fragment.home.arpa"), "dex.fragment.home.arpa");
        assert_eq!(host_name("dex.fragment.home.arpa."), "dex.fragment.home.arpa");
        assert_eq!(host_name("[::1]:443"), "[::1]");
        assert_eq!(host_name("a.b:"), "a.b:");
    }

    #[test]
    fn names_under_the_zone_are_in_it() {
        let zone = "fragment.home.arpa";
        assert!(in_zone("fragment.home.arpa", zone));
        assert!(in_zone("todo--paul.fragment.home.arpa", zone));
        assert!(in_zone("a.b.fragment.home.arpa", zone));
        assert!(!in_zone("evilfragment.home.arpa", zone));
        assert!(!in_zone(".fragment.home.arpa", zone));
        assert!(!in_zone("..fragment.home.arpa", zone));
        assert!(!in_zone("home.arpa", zone));
        assert_eq!(https_origin(zone, 443), "https://fragment.home.arpa");
        assert_eq!(https_origin(zone, 8443), "https://fragment.home.arpa:8443");
    }
}
