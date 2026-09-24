//! Path patterns for file triggers: `*` matches within one path segment,
//! `**` across segments, `?` one character; a pattern ending in `/`
//! matches everything under that folder.
//!
//! The supervisor matches every changed path against every file trigger
//! with no CPU limit of its own, and paths can come from an app's own
//! writes, so matching runs the pattern as a set of states over the path:
//! one pass, at most (pattern bytes + 1) states per path byte, never a
//! backtracking search.

/// One state of a pattern, read left to right.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Byte(u8),
    /// `?`: one byte that is not `/`.
    One,
    /// `*`: any run of bytes within a segment.
    Star,
    /// `**`: any run of bytes.
    Deep,
    /// Reads nothing: either the next state, or skips the next two. `**/`
    /// is this before `**` and `/`: nothing, or any run that ends in `/`.
    Maybe,
}

fn tokens(p: &[u8]) -> Vec<Token> {
    let mut out = Vec::with_capacity(p.len() + p.len() / 3);
    let mut i = 0;
    while i < p.len() {
        match (p[i], p.get(i + 1), p.get(i + 2)) {
            (b'*', Some(b'*'), Some(b'/')) => {
                out.extend([Token::Maybe, Token::Deep, Token::Byte(b'/')]);
                i += 3;
            }
            (b'*', Some(b'*'), _) => {
                out.push(Token::Deep);
                i += 2;
            }
            (b'*', _, _) => {
                out.push(Token::Star);
                i += 1;
            }
            (b'?', _, _) => {
                out.push(Token::One);
                i += 1;
            }
            (c, _, _) => {
                out.push(Token::Byte(c));
                i += 1;
            }
        }
    }
    assert!(out.len() <= p.len(), "every state but a `Maybe` reads a pattern byte of its own");
    out
}

/// Adds the states reachable without reading a byte: a star may match
/// nothing, and a `Maybe` may skip its run. These moves only go forward,
/// so one pass in order is closed.
fn close(t: &[Token], active: &mut [bool]) {
    for j in 0..t.len() {
        if !active[j] {
            continue;
        }
        match t[j] {
            Token::Star | Token::Deep => active[j + 1] = true,
            Token::Maybe => {
                assert!(matches!(t[j + 1..j + 3], [Token::Deep, Token::Byte(b'/')]), "a `Maybe` guards `**/`");
                active[j + 1] = true;
                active[j + 3] = true;
            }
            Token::Byte(_) | Token::One => {}
        }
    }
}

fn matches(p: &[u8], s: &[u8]) -> bool {
    let t = tokens(p);
    let n = t.len();
    // active[j]: the first j tokens match the bytes read so far.
    let mut active = vec![false; n + 1];
    let mut next = vec![false; n + 1];
    active[0] = true;
    close(&t, &mut active);
    // Bounded by the path: each byte is read once.
    for &c in s {
        next.fill(false);
        for j in 0..n {
            if !active[j] {
                continue;
            }
            match t[j] {
                Token::Byte(b) if b == c => next[j + 1] = true,
                Token::Byte(_) => {}
                Token::One if c != b'/' => next[j + 1] = true,
                Token::One => {}
                Token::Star if c != b'/' => next[j] = true,
                Token::Star => {}
                Token::Deep => next[j] = true,
                Token::Maybe => {}
            }
        }
        std::mem::swap(&mut active, &mut next);
        close(&t, &mut active);
        if !active.contains(&true) {
            return false;
        }
    }
    active[n]
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

    /// The backtracking matcher this replaced: the meaning every pattern
    /// keeps, as a reference on inputs small enough for it.
    fn reference(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', b'/', rest @ ..] => (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && reference(rest, &s[i..])),
            [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| reference(rest, &s[i..])),
            [b'*', rest @ ..] => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| reference(rest, &s[i..])),
            [b'?', rest @ ..] => s.first().is_some_and(|c| *c != b'/') && reference(rest, &s[1..]),
            [c, rest @ ..] => s.first() == Some(c) && reference(rest, &s[1..]),
        }
    }

    /// Every string up to `max` bytes over `alphabet`.
    fn all(alphabet: &[u8], max: usize) -> Vec<Vec<u8>> {
        let mut out = vec![vec![]];
        let mut last: Vec<Vec<u8>> = vec![vec![]];
        for _ in 0..max {
            let longer: Vec<Vec<u8>> = last.iter().flat_map(|w| alphabet.iter().map(move |c| [w.as_slice(), &[*c]].concat())).collect();
            out.extend(longer.iter().cloned());
            last = longer;
        }
        out
    }

    #[test]
    fn same_meaning_as_the_backtracking_matcher() {
        let patterns = all(b"a*?/", 5);
        let paths = all(b"ab/", 5);
        let mut matched = 0;
        for p in &patterns {
            for s in &paths {
                let want = reference(p, s);
                assert_eq!(super::matches(p, s), want, "pattern {:?} path {:?}", String::from_utf8_lossy(p), String::from_utf8_lossy(s));
                matched += usize::from(want);
            }
        }
        assert!(matched > 10_000, "the comparison covered matches, not only misses ({matched})");
    }

    #[test]
    fn a_pathological_pattern_is_one_pass() {
        // Thirteen stars against a hundred bytes that almost match: the
        // backtracking matcher tried on the order of 10^15 suffixes here.
        let pattern = "*a".repeat(13) + "b";
        assert!(super::valid(&pattern));
        let path = "a".repeat(100);
        let t0 = std::time::Instant::now();
        assert!(!m(&pattern, &path));
        assert!(m(&pattern, &(path.clone() + "b")));
        let deep = "**/".repeat(4) + "*a*a*a*a*a*a*a*a*b";
        assert!(super::valid(&deep));
        assert!(!m(&deep, &"a/".repeat(50)));
        assert!(t0.elapsed() < std::time::Duration::from_secs(1), "{:?}", t0.elapsed());
    }
}
