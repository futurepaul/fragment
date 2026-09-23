//! Path patterns for file triggers: `*` matches within one path segment,
//! `**` across segments, `?` one character; a pattern ending in `/`
//! matches everything under that folder.

fn matches(p: &[u8], s: &[u8]) -> bool {
    match p {
        [] => s.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && matches(rest, &s[i..])),
        [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| matches(rest, &s[i..])),
        [b'*', rest @ ..] => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| matches(rest, &s[i..])),
        [b'?', rest @ ..] => s.first().is_some_and(|c| *c != b'/') && matches(rest, &s[1..]),
        [c, rest @ ..] => s.first() == Some(c) && matches(rest, &s[1..]),
    }
}

/// Whether `path` (a repo path, no leading `/`) matches `pattern`.
pub fn matches_path(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix('/') {
        Some(folder) => path.len() > folder.len() + 1 && path.starts_with(folder) && path.as_bytes()[folder.len()] == b'/',
        None => matches(pattern.as_bytes(), path.as_bytes()),
    }
}

/// A pattern's shape: non-empty, relative, bounded.
pub fn valid(pattern: &str) -> bool {
    !pattern.is_empty() && pattern.len() <= 200 && !pattern.starts_with('/') && !pattern.contains("//") && pattern.matches("**").count() <= 4
}

#[cfg(test)]
mod tests {
    use super::matches_path as m;

    #[test]
    fn patterns() {
        assert!(m("*.md", "a.md"));
        assert!(!m("*.md", "notes/a.md"), "* stays in a segment");
        assert!(m("**/*.md", "a.md"));
        assert!(m("**/*.md", "notes/deep/a.md"));
        assert!(m("notes/**", "notes/a/b.txt"));
        assert!(!m("notes/**", "notesx/a"));
        assert!(m("notes/", "notes/a/b.txt"));
        assert!(!m("notes/", "notes"));
        assert!(!m("notes/", "notesx/a"));
        assert!(m("data/history.json", "data/history.json"));
        assert!(!m("data/history.json", "data/history.jsonx"));
        assert!(m("data/?.json", "data/a.json"));
        assert!(!m("data/?.json", "data/ab.json"));
        assert!(m("**", "anything/at/all"));
    }
}
