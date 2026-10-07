//! Drafts (docs/api.md, Drafts): a fragment made before anyone has an
//! account, signed by a key no one holds. It spends nothing, so it bills
//! no one: no secrets, no `job.fetch`, no AI steps, no push, no computers,
//! under tight caps (`fragment_proto::limits::DRAFT_*`), and it ends a day
//! after it is made unless it is claimed. Its claim is the login flow: the
//! person signs in, gives its claim code, and approves the key that made
//! it, and in that step it becomes theirs.
//!
//! What a draft is called and who made it come from its key alone, so the
//! same key's create, sent again, finds the same draft: its name (`name`)
//! and its maker's principal (`maker`), an identity the registry never
//! holds (a claim names the person instead).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A draft's label: 12 characters of its key's digest, 60 bits.
pub const LABEL_LEN: usize = 12;
const LABEL_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
/// A claim code: 8 characters of Crockford's base 32 (no I, L, O or U), 40
/// random bits, shown as `XXXX-XXXX`.
pub const CODE_LEN: usize = 8;
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A draft as its fragment keeps it (its `meta` row `draft`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Draft {
    /// The key that made it (64 hex).
    pub key: String,
    /// When it ends unless it is claimed (ms).
    pub until: i64,
    /// Its claim code (`code`).
    pub code: String,
    /// The template it started from, if any.
    pub template: Option<String>,
    /// Who claimed it: from then it is an ordinary fragment of theirs.
    pub claimed_by: Option<String>,
}

fn digest(what: &str, key_hex: &str) -> [u8; 32] {
    Sha256::digest(format!("fragment draft {what}\0{key_hex}").as_bytes()).into()
}

/// 5 bits at a time from the front of `bytes`, as `alphabet`'s characters.
fn base32(bytes: &[u8], alphabet: &[u8; 32], n: usize) -> String {
    assert!(n * 5 <= bytes.len() * 8, "{n} characters take {} bits", n * 5);
    (0..n)
        .map(|i| {
            let bit = i * 5;
            let pair = (u16::from(bytes[bit / 8]) << 8) | u16::from(bytes.get(bit / 8 + 1).copied().unwrap_or(0));
            alphabet[usize::from((pair >> (11 - bit % 8)) & 31)] as char
        })
        .collect()
}

/// The label of the draft `key_hex` makes.
pub fn label(key_hex: &str) -> String {
    assert!(crate::npub::is_hex_key(key_hex), "a draft's key is 64 hex");
    let label = base32(&digest("label", key_hex), LABEL_ALPHABET, LABEL_LEN);
    assert!(fragment_proto::valid_label(&label), "a draft's label is a label: {label}");
    label
}

/// The draft `key_hex` makes: `<label>.draft`.
pub fn name(key_hex: &str) -> String {
    fragment_proto::fragment_name(&label(key_hex), fragment_proto::DRAFT_USERNAME)
}

/// Its maker's principal: `id:` and 32 hex of its key's digest. The
/// registry's identities are random, so none is ever this.
pub fn maker(key_hex: &str) -> String {
    assert!(crate::npub::is_hex_key(key_hex), "a draft's key is 64 hex");
    let d = digest("maker", key_hex);
    crate::npub::identity(d[..16].try_into().expect("16 of 32 bytes"))
}

/// Whether `id`, signing with `key_hex`, is a draft's maker: a key no one
/// holds, which the router names so on a draft's routes alone.
pub fn is_maker(id: &str, key_hex: Option<&str>) -> bool {
    key_hex.is_some_and(|k| crate::npub::is_hex_key(k) && maker(k) == id)
}

/// A claim code from 5 random bytes (the caller's randomness), as kept.
pub fn code(random: [u8; 5]) -> String {
    base32(&random, CODE_ALPHABET, CODE_LEN)
}

/// A kept code as a person reads it: `XXXX-XXXX`.
pub fn shown(code: &str) -> String {
    match code.split_at_checked(CODE_LEN / 2) {
        Some((a, b)) => format!("{a}-{b}"),
        None => code.to_string(),
    }
}

/// Whether `given` (as a person types it: any case, dashes and spaces
/// anywhere) is the code `kept`, compared in constant time.
pub fn code_matches(kept: &str, given: &str) -> bool {
    let given: Vec<u8> = given.bytes().filter(|b| !matches!(b, b'-' | b' ')).map(|b| b.to_ascii_uppercase()).collect();
    kept.len() == CODE_LEN && given.len() == CODE_LEN && kept.bytes().zip(given).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// The address a draft's start counts against: an IPv4 address as it is,
/// an IPv6 address as its /64 (one site's, as an ISP assigns them; an
/// IPv4-mapped one as its IPv4). `None`: not an address.
pub fn address(ip: &str) -> Option<String> {
    match ip.trim().parse::<std::net::IpAddr>().ok()? {
        std::net::IpAddr::V4(v4) => Some(v4.to_string()),
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => Some(v4.to_string()),
            None => {
                let s = v6.segments();
                Some(format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3]))
            }
        },
    }
}

/// Whether an unclaimed draft takes a control request (`method`, and its
/// inner route: `["api", …]`, or `["delete"]`), whoever sends it: its
/// files, deploys, operations, channels and runs, and its delete. Secrets,
/// storage tokens, blobs, subscriptions, the cap and sharing (members,
/// invites, visibility, the links) wait for its claim.
pub fn takes(method: &str, route: &[&str]) -> bool {
    matches!(
        (method, route),
        ("DELETE", ["delete"])
            | ("GET", ["api", "status" | "manifest" | "members" | "files" | "file" | "events" | "channels" | "runs" | "triggers" | "card"])
            | ("GET", ["api", "channels" | "runs", _])
            | ("POST", ["api", "files" | "deploy" | "refresh" | "replay" | "pause" | "inbox"])
            | ("POST", ["api", "ops" | "channels", _])
            | ("PUT", ["api", "channels", _, "draft"])
    )
}

/// A draft's page: `html` with a bar along its foot saying it is a draft,
/// when it ends, and where it is claimed (`claim`, a link without the
/// code: its page asks for it), before `</body>` or at its end.
pub fn with_banner(html: &str, claim: &str, until_ms: i64) -> String {
    const DAY_MS: i64 = 24 * 3600 * 1000;
    let (y, m, d) = crate::cron::civil(until_ms.div_euclid(DAY_MS));
    let minute = until_ms.rem_euclid(DAY_MS) / 60_000;
    let bar = format!(
        r#"<div id="fragment-draft" style="position:fixed;left:0;right:0;bottom:0;z-index:2147483647;margin:0;padding:8px 14px;font:14px/1.4 system-ui,sans-serif;color:#1d2126;background:#fff3c4;border-top:1px solid #e0c96a;text-align:center"><b>Draft</b>: made without an account, and deleted {y}-{m:02}-{d:02} {:02}:{:02} UTC unless it is claimed. <a href="{}" style="color:#2a5bd7">Claim it</a></div>"#,
        minute / 60,
        minute % 60,
        crate::site::html_escape(claim),
    );
    match html.to_ascii_lowercase().rfind("</body>") {
        Some(i) => format!("{}{bar}{}", &html[..i], &html[i..]),
        None => format!("{html}{bar}"),
    }
}

/// Why a draft's start is refused: the address's starts today, and the
/// deployment's, at their caps (`limits::DRAFTS_PER_*`).
pub fn start_refused(by_address: u64, in_all: u64) -> Option<String> {
    use fragment_proto::limits::{DRAFTS_PER_ADDRESS_PER_DAY, DRAFTS_PER_DAY};
    if by_address >= DRAFTS_PER_ADDRESS_PER_DAY {
        return Some(format!("this address started {DRAFTS_PER_ADDRESS_PER_DAY} drafts in the last day; sign in to make fragments of your own"));
    }
    if in_all >= DRAFTS_PER_DAY {
        return Some("the platform takes no more drafts today; sign in to make fragments of your own".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> String {
        hex::encode([n; 32])
    }

    /// Valid and replayed: the same key makes the same name and maker every
    /// time; another key, others.
    #[test]
    fn a_key_names_its_draft_and_its_maker() {
        let (a, b) = (key(1), key(2));
        assert_eq!(name(&a), name(&a));
        assert_eq!(maker(&a), maker(&a));
        assert_ne!(name(&a), name(&b));
        assert_ne!(maker(&a), maker(&b));
        assert!(fragment_proto::is_draft_name(&name(&a)), "{}", name(&a));
        assert_eq!(label(&a).len(), LABEL_LEN);
        assert!(crate::npub::is_identity(&maker(&a)));
        assert!(is_maker(&maker(&a), Some(&a)));
        assert!(!is_maker(&maker(&a), Some(&b)), "another key's maker is no one's");
        assert!(!is_maker(&maker(&a), None), "a session is no draft's maker");
        assert!(!is_maker("id:00000000000000000000000000000000", Some(&a)));
    }

    #[test]
    fn base32_takes_five_bits_a_character() {
        assert_eq!(base32(&[0; 5], CODE_ALPHABET, 8), "00000000");
        assert_eq!(base32(&[0xff; 5], CODE_ALPHABET, 8), "ZZZZZZZZ");
        // 00001 00010 00011 00100 00101 00110 00111 01000
        assert_eq!(base32(&[0x08, 0x86, 0x42, 0x98, 0xe8], CODE_ALPHABET, 8), "12345678");
        assert_eq!(base32(&[0x08, 0x86, 0x42, 0x98, 0xe8], LABEL_ALPHABET, 8), "bcdefghi");
    }

    /// A code is typed as a person types it, and nothing else is it.
    #[test]
    fn a_code_matches_as_it_is_typed() {
        let kept = code([0x08, 0x86, 0x42, 0x98, 0xe8]);
        assert_eq!(kept, "12345678");
        assert_eq!(shown(&kept), "1234-5678");
        for typed in ["12345678", "1234-5678", " 1234 5678 "] {
            assert!(code_matches(&kept, typed), "{typed:?}");
        }
        let lower = code([0xa5; 5]);
        assert!(code_matches(&lower, &shown(&lower).to_ascii_lowercase()));
        for not in ["", "1234567", "123456789", "1234-5679", "12345678x"] {
            assert!(!code_matches(&kept, not), "{not:?}");
        }
        assert!(!code_matches("", ""), "no kept code matches anything");
    }

    #[test]
    fn an_address_is_an_ipv4_or_an_ipv6_64() {
        assert_eq!(address("203.0.113.9").as_deref(), Some("203.0.113.9"));
        assert_eq!(address(" 203.0.113.9 ").as_deref(), Some("203.0.113.9"));
        assert_eq!(address("2001:db8:1:2:3:4:5:6").as_deref(), Some("2001:db8:1:2::/64"));
        assert_eq!(address("2001:db8:1:2::ffff").as_deref(), address("2001:db8:1:2:aaaa::1").as_deref(), "one /64 is one address");
        assert_eq!(address("::ffff:203.0.113.9").as_deref(), Some("203.0.113.9"));
        for not in ["", "unknown", "203.0.113", "2001:db8::g", "203.0.113.9:80"] {
            assert_eq!(address(not), None, "{not:?}");
        }
    }

    /// What a draft takes: its own loop (files, deploys, operations,
    /// channels, runs, its delete), and nothing that spends, holds a secret,
    /// reaches out, or shares.
    #[test]
    fn a_draft_takes_its_own_loop_alone() {
        for (method, route) in [
            ("GET", &["api", "status"][..]),
            ("GET", &["api", "events"]),
            ("GET", &["api", "channels", "todo"]),
            ("POST", &["api", "files"]),
            ("POST", &["api", "deploy"]),
            ("POST", &["api", "ops", "add"]),
            ("POST", &["api", "channels", "chat"]),
            ("POST", &["api", "inbox"]),
            ("PUT", &["api", "channels", "chat", "draft"]),
            ("DELETE", &["delete"]),
        ] {
            assert!(takes(method, route), "{method} {route:?}");
        }
        for (method, route) in [
            ("PUT", &["api", "secrets", "KEY"][..]),
            ("GET", &["api", "secrets"]),
            ("GET", &["api", "storage-token"]),
            ("PUT", &["api", "blobs", "ab"]),
            ("GET", &["api", "blobs", "ab"]),
            ("POST", &["api", "subscriptions"]),
            ("PUT", &["api", "members", "id:x"]),
            ("DELETE", &["api", "members", "me"]),
            ("POST", &["api", "invites"]),
            ("PUT", &["api", "visibility"]),
            ("POST", &["api", "rotate"]),
            ("PUT", &["api", "cap"]),
            ("POST", &["api", "join"]),
            ("DELETE", &["api", "ops", "add"]),
            ("POST", &["api", "ops", "add", "more"]),
        ] {
            assert!(!takes(method, route), "{method} {route:?}");
        }
    }

    #[test]
    fn starts_stop_at_their_caps() {
        use fragment_proto::limits::{DRAFTS_PER_ADDRESS_PER_DAY, DRAFTS_PER_DAY};
        assert_eq!(start_refused(0, 0), None);
        assert_eq!(start_refused(DRAFTS_PER_ADDRESS_PER_DAY - 1, DRAFTS_PER_DAY - 1), None);
        assert!(start_refused(DRAFTS_PER_ADDRESS_PER_DAY, 0).is_some_and(|m| m.contains("this address")));
        assert!(start_refused(0, DRAFTS_PER_DAY).is_some_and(|m| m.contains("no more drafts")));
    }

    /// Every page of a draft says so, with its end and its claim link, its
    /// own markup kept around the bar.
    #[test]
    fn a_draft_page_says_it_is_one() {
        // 2026-10-08 14:03 UTC
        let until = 1_791_468_180_000;
        let page = with_banner("<html><body><h1>Hi</h1></BODY></html>", "https://p/claim/a.draft?x=\"1\"", until);
        assert!(page.starts_with("<html><body><h1>Hi</h1><div id=\"fragment-draft\""), "{page}");
        assert!(page.ends_with("</div></BODY></html>"), "{page}");
        assert!(page.contains("deleted 2026-10-08 14:03 UTC unless it is claimed"), "{page}");
        assert!(page.contains("href=\"https://p/claim/a.draft?x=&quot;1&quot;\""), "the link is escaped: {page}");
        let bare = with_banner("<p>no body", "https://p/claim/a.draft", until);
        assert!(bare.starts_with("<p>no body<div id=\"fragment-draft\""), "{bare}");
    }

    #[test]
    fn a_kept_draft_reads_back() {
        let d = Draft { key: key(3), until: 7, code: code([1; 5]), template: Some("todo".into()), claimed_by: None };
        let text = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Draft>(&text).unwrap(), d);
        assert!(serde_json::from_str::<Draft>(r#"{"key":"k","until":1,"code":"c","template":null,"claimedBy":null,"x":1}"#).is_err(), "unknown fields are refused");
    }
}
