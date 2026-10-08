//! Mail the platform sends (docs/api.md, Mail): transactional only (an
//! invite, a seat, a trial), one recipient, plain text. What a message may
//! hold is checked here; the cell sends it (cell/src/mail.rs) through
//! Cloudflare Email Sending's binding, or in dev and the e2e to the fake.

use serde_json::{json, Value};

/// What the platform calls itself to people: the shell's name (its page's
/// title and brand, and its web manifest's name: a test below holds them
/// to this), and what every mail it sends says they were invited to.
pub const PRODUCT: &str = "Finite.Computer";

/// An address, at most (RFC 5321's path limit).
pub const ADDRESS_MAX_BYTES: usize = 320;
/// A subject, at most.
pub const SUBJECT_MAX_CHARS: usize = 200;
/// A message's text, at most: far below the service's 5 MiB, since what
/// the platform sends is a few lines and a link.
pub const TEXT_MAX_BYTES: usize = 16 * 1024;

/// One message to one person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
}

/// Whether `address` is one plain address: `local@domain`, nothing that
/// could name a second recipient or a header (no spaces, commas, angle
/// brackets, or control characters).
pub fn valid_address(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else { return false };
    address.len() <= ADDRESS_MAX_BYTES
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && address.chars().all(|c| !c.is_whitespace() && !c.is_control() && !"<>,;\"()[]\\".contains(c))
}

/// Why `mail` cannot be sent, if it cannot.
pub fn refusal(mail: &Mail) -> Option<String> {
    if !valid_address(&mail.to) {
        return Some(format!("{:?} is not one plain email address", truncated(&mail.to)));
    }
    let subject_chars = mail.subject.chars().count();
    if subject_chars == 0 || subject_chars > SUBJECT_MAX_CHARS || mail.subject.chars().any(char::is_control) {
        return Some(format!("a subject is 1 to {SUBJECT_MAX_CHARS} characters on one line"));
    }
    if mail.text.is_empty() || mail.text.len() > TEXT_MAX_BYTES {
        return Some(format!("a message's text is 1 to {TEXT_MAX_BYTES} bytes"));
    }
    None
}

/// The Email Sending binding's input for `mail` from `from` (the
/// deployment's address), which the fake takes as it is.
pub fn message(from: &str, mail: &Mail) -> Value {
    assert!(refusal(mail).is_none(), "a message is checked before it is built");
    json!({ "to": mail.to, "from": from, "subject": mail.subject, "text": mail.text })
}

/// A subject's longest title and sharer, in characters.
const SUBJECT_PART_MAX: usize = 80;

/// `s` on one line, at most `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let line: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let line = line.trim();
    match line.char_indices().nth(max) {
        Some((cut, _)) => format!("{}\u{2026}", &line[..cut]),
        None => line.to_string(),
    }
}

/// The mail an invite by email sends `to` (docs/cloudflare-v1.md, decision
/// 48): who shares what, whether they may edit it, and its address (`link`),
/// which signs them in. It waits `days`.
pub fn invite(to: &str, from: &str, title: &str, edit: bool, link: &str, days: i64) -> Mail {
    let (title, from) = (one_line(title, SUBJECT_PART_MAX), one_line(from, SUBJECT_PART_MAX));
    let may = if edit { "edit" } else { "view" };
    Mail {
        to: to.to_string(),
        subject: format!("{from} shared \u{201c}{title}\u{201d} with you"),
        text: format!(
            "{from} invited you to {may} \u{201c}{title}\u{201d} on {PRODUCT}.\n\n\
             Open it: {link}\n\n\
             Sign in as {to} to open it. The invite waits {days} days.\n\n\
             If you did not expect this, ignore this mail: nothing happens unless you sign in as {to}.\n"
        ),
    }
}

fn truncated(s: &str) -> String {
    s.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(to: &str, subject: &str, text: &str) -> Mail {
        Mail { to: to.into(), subject: subject.into(), text: text.into() }
    }

    /// Goal: a message goes to one plain address, with a one-line subject
    /// and a bounded text; anything that could name a second recipient or
    /// inject a header is refused. Method: valid and invalid of each.
    #[test]
    fn what_a_message_may_hold() {
        assert_eq!(refusal(&mail("bob@example.com", "Paul shared todo with you", "Open it: https://x")), None);
        for to in ["", "bob", "@example.com", "bob@", "bob@example", "bob@.com", "bob@example.", "bob@a@b.com", "bob @example.com", "bob@example.com, eve@example.com", "Bob <bob@example.com>", "bob@example.com\r\nBcc: eve@example.com", &format!("{}@example.com", "b".repeat(320))] {
            assert!(refusal(&mail(to, "s", "t")).is_some(), "{to:?}");
        }
        for subject in ["", "two\nlines", &"s".repeat(SUBJECT_MAX_CHARS + 1)] {
            assert!(refusal(&mail("bob@example.com", subject, "t")).is_some(), "{subject:?}");
        }
        assert!(refusal(&mail("bob@example.com", &"s".repeat(SUBJECT_MAX_CHARS), "t")).is_none());
        assert!(refusal(&mail("bob@example.com", "s", "")).is_some());
        assert!(refusal(&mail("bob@example.com", "s", &"t".repeat(TEXT_MAX_BYTES + 1))).is_some());
        assert_eq!(
            message("fragment <mail@fragment.club>", &mail("bob@example.com", "s", "t")),
            json!({ "to": "bob@example.com", "from": "fragment <mail@fragment.club>", "subject": "s", "text": "t" })
        );
    }

    /// Goal: an invite's mail says who shares what, as which role, and
    /// where, and stays a message the service takes whatever the title
    /// holds. Method: a title with a newline, and a title and sharer far
    /// past a subject's length.
    #[test]
    fn an_invite_names_its_sharer_fragment_and_role_on_one_line() {
        let m = invite("bea@example.com", "ann@example.com", "Garden\nplans", true, "https://garden--k3x9.fragment.boats/", 30);
        assert_eq!(m.subject, "ann@example.com shared \u{201c}Garden plans\u{201d} with you");
        assert!(m.text.contains("invited you to edit \u{201c}Garden plans\u{201d} on Finite.Computer.\n"), "{}", m.text);
        assert!(m.text.contains("Open it: https://garden--k3x9.fragment.boats/\n"), "{}", m.text);
        assert!(m.text.contains("Sign in as bea@example.com to open it. The invite waits 30 days."), "{}", m.text);
        assert_eq!(refusal(&m), None);
        let long = "x".repeat(500);
        let m = invite("bea@example.com", &long, &long, false, "https://a--k3x9.fragment.boats/", 30);
        assert_eq!(refusal(&m), None, "{}", m.subject);
        assert!(m.text.contains("invited you to view"));
    }

    /// Goal: a person reads one name for the platform, in its mail and in
    /// the shell it sends them to. Method: the shell's page and web
    /// manifest, as the cell serves them, name `PRODUCT`.
    #[test]
    fn the_mail_names_the_platform_as_the_shell_does() {
        let page = include_str!("../../../cell/shell/index.html");
        assert!(page.contains(&format!("<title>{PRODUCT}</title>")), "the shell's page is titled {PRODUCT}");
        assert!(page.contains(&format!("id=\"brand\">{PRODUCT}</div>")), "the shell's brand is {PRODUCT}");
        let manifest: Value = serde_json::from_str(include_str!("../../../cell/shell/manifest.webmanifest")).expect("the web manifest is JSON");
        assert_eq!(manifest["name"], PRODUCT);
    }

    #[test]
    fn one_line_cuts_by_characters() {
        assert_eq!(one_line("  a\tb\n", 10), "a b");
        assert_eq!(one_line("\u{e9}\u{e9}\u{e9}", 2), "\u{e9}\u{e9}\u{2026}");
        assert_eq!(one_line("abc", 3), "abc");
    }
}
