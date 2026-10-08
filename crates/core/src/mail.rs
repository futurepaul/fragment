//! Mail the platform sends (docs/api.md, Mail): transactional only (an
//! invite, a seat, a trial), one recipient, plain text. What a message may
//! hold is checked here; the cell sends it (cell/src/mail.rs) through
//! Cloudflare Email Sending's binding, or in dev and the e2e to the fake.

use serde_json::{json, Value};

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
}
