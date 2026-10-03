//! Frames (docs/api.md, Frame sessions; docs/fragment-boats.md, design
//! C): the platform's own page (the shell) shows fragments, each on an
//! origin of its own, and a computer's ports in frames, signed in as the
//! person looking at it. These are the pure decisions under that: which
//! navigation may mint a frame's sign-in, which pages may show a frame's
//! answer, and what may be named there as an origin.

/// Whether `s` is an origin as a browser names one in `Origin`: `http` or
/// `https`, a host, a port only when it is not the scheme's default, and
/// nothing else (no user, no path, not even `/`, no query). Only such a
/// string goes into a `frame-ancestors` source or a `postMessage` target:
/// anything else could widen what it names (a path, a wildcard, a second
/// source after a space or a `;`).
pub fn is_origin(s: &str) -> bool {
    let Ok(url) = url::Url::parse(s) else { return false };
    let web = matches!(url.scheme(), "http" | "https");
    // A URL's host may hold what a CSP source reads as more than a name
    // (`*` is no forbidden host code point), so a domain is held to the
    // letters, digits, dots and dashes hostnames are made of.
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'),
        Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => true,
        None => false,
    };
    // the serialization is the browser's own form: a trailing slash, an
    // upper-case host, a default port, or a user each make it differ
    web && host && url.origin().ascii_serialization() == s
}

/// Whether a request is a frame of a page on its own origin, by the Fetch
/// Metadata it carries (which no page's script sets): a frame's navigation
/// (`Sec-Fetch-Dest: iframe` or `frame`, `Sec-Fetch-Mode: navigate`) that
/// a page on the same origin started (`Sec-Fetch-Site: same-origin`, which
/// a redirect keeps only when every hop to it was same-origin too). On the
/// platform's origin that page is the platform's own, and only such a
/// request mints a frame's sign-in (`/auth/frame`). A page on any other
/// origin, a fragment's (its author's code) included, sends `same-site` or
/// `cross-site`; a top-level visit, a fetch, an image, an `object` or an
/// `embed` another dest or mode; a browser from before 2023 none at all.
pub fn platform_frame(dest: &str, mode: &str, site: &str) -> bool {
    let framed = matches!(dest, "iframe" | "frame");
    framed && mode == "navigate" && site == "same-origin"
}

/// Whom a fragment's answer to a frame's navigation was made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framed {
    /// A live frame session the platform's mint made: the person, in a
    /// frame of the platform's page.
    Session,
    /// The fragment's own page framing it (`same-origin`), as whoever its
    /// cookies there name.
    OwnPage,
    /// Any other page's frame: no cookie of the person's counts there, so
    /// the answer is the one a stranger opening the URL gets.
    Stranger,
}

/// The `frame-ancestors` of a fragment's answer to a frame's navigation:
/// the pages that may show it.
///
/// - A frame session's answer shows only in the platform's page, the one
///   its mint was for: before the Public Suffix List lists the fragments'
///   domain, every fragment shares the frame cookie's partition, so
///   another fragment framing this one (in the shell, or anywhere) sends
///   it too, and only this header keeps the answer out of that frame.
/// - The fragment's own page framing itself keeps to this origin's pages.
/// - A stranger's answer may show in this origin's pages and the
///   platform's (the shell shows a public fragment to someone signed out).
///   It acts for no one, and no other page may lay it under a click.
pub fn ancestors(framed: Framed, platform: &str) -> String {
    assert!(is_origin(platform), "the platform is named by its origin: {platform:?}");
    let named = match framed {
        Framed::Session => platform.to_string(),
        Framed::OwnPage => "'self'".to_string(),
        Framed::Stranger => format!("'self' {platform}"),
    };
    assert!(!named.contains(';') && !named.contains(','), "one directive's sources");
    named
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: only a browser's own form of an origin is one, since it goes
    /// into a header that decides who may frame a signed-in page. Method:
    /// the platform's forms on fragment.club and on the e2e's nodes, then
    /// every near miss that would widen or bend the source.
    #[test]
    fn origins() {
        for valid in ["https://fragment.club", "http://127.0.0.1:8790", "http://fragment.localhost:8790", "https://a--b.fragment.boats", "http://[::1]:8790"] {
            assert!(is_origin(valid), "{valid}");
        }
        for invalid in [
            "",
            "'self'",
            "*",
            "https://fragment.club/",
            "https://fragment.club/chat",
            "https://fragment.club?x=1",
            "https://fragment.club#x",
            "https://FRAGMENT.club",
            "https://fragment.club:443",
            "http://127.0.0.1:80",
            "https://user@fragment.club",
            "https://fragment.club https://evil.example",
            "https://fragment.club; script-src *",
            "https://*.fragment.boats",
            "fragment.club",
            "//fragment.club",
            "ws://fragment.club",
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
        ] {
            assert!(!is_origin(invalid), "{invalid:?}");
        }
    }

    /// Goal: a frame of a page on the platform's own origin, and nothing
    /// else, may mint. Method: the one shape that passes, then each header
    /// changed to every value a browser sends instead (and none).
    #[test]
    fn only_a_frame_of_the_platforms_own_page_mints() {
        assert!(platform_frame("iframe", "navigate", "same-origin"));
        assert!(platform_frame("frame", "navigate", "same-origin"));
        for dest in ["document", "object", "embed", "empty", "image", "script", "style", "worker", "fencedframe", ""] {
            assert!(!platform_frame(dest, "navigate", "same-origin"), "dest {dest:?}");
        }
        for mode in ["cors", "no-cors", "same-origin", "websocket", ""] {
            assert!(!platform_frame("iframe", mode, "same-origin"), "mode {mode:?}");
        }
        for site in ["same-site", "cross-site", "none", ""] {
            assert!(!platform_frame("iframe", "navigate", site), "site {site:?}");
        }
        assert!(!platform_frame("IFRAME", "navigate", "same-origin"), "browsers send these in lower case");
    }

    /// Goal: a frame's answer names the platform's exact origin or this
    /// origin's own pages, and a signed-in one the platform alone. Method:
    /// each case for the platform on fragment.club and on a dev node.
    #[test]
    fn frame_ancestors() {
        for platform in ["https://fragment.club", "http://127.0.0.1:8790"] {
            assert_eq!(ancestors(Framed::Session, platform), platform);
            assert_eq!(ancestors(Framed::OwnPage, platform), "'self'");
            assert_eq!(ancestors(Framed::Stranger, platform), format!("'self' {platform}"));
        }
    }

    #[test]
    #[should_panic(expected = "the platform is named by its origin")]
    fn a_platform_that_is_no_origin_is_never_named() {
        ancestors(Framed::Stranger, "https://fragment.club https://evil.example");
    }
}
