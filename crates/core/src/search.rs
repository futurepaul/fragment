//! Search over a person's fragments (docs/cloudflare-v1.md, decision 9 and
//! lesson 12; docs/api.md, Search): what of a record a fragment sends its
//! people's lists, and how a person's query becomes an FTS5 match.
//!
//! The platform knows no template, so what counts is a record convention,
//! the chat's (docs/chat-records.md): a record is a message when its body
//! is an object whose `kind` is absent or `"message"`, and its text is the
//! body's `text` when that is a string, and only that. Every other record
//! (a page's own kinds, an agent's steps) is never searched.
//!
//! A query is words, never FTS5 syntax: each word is a quoted phrase
//! (quotes inside doubled, as FTS5 escapes them) matched as a prefix, and
//! a record matches when it has every word. `OR`, `NOT`, `NEAR(…)`, a
//! column filter, `*` and `"` are text like any other.

use fragment_proto::limits;
use serde_json::Value;

/// The text search keeps of a record's body: its `text`, cut to
/// `limits::SEARCH_TEXT_MAX_BYTES` on a character's boundary. `None` for
/// a body that is no message, whose `text` is not a string, or is blank.
pub fn record_text(body: &Value) -> Option<&str> {
    let object = body.as_object()?;
    let is_message = match object.get("kind") {
        None => true,
        Some(kind) => kind.as_str() == Some("message"),
    };
    if !is_message {
        return None;
    }
    let text = object.get("text")?.as_str()?;
    if text.trim().is_empty() {
        return None;
    }
    let kept = cut(text, limits::SEARCH_TEXT_MAX_BYTES);
    assert!(kept.len() <= limits::SEARCH_TEXT_MAX_BYTES, "a search entry's text is bounded");
    Some(kept)
}

/// At most `max_bytes` of `text`, ending on a character's boundary.
pub fn cut(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    // a UTF-8 character is at most 4 bytes: at most 3 steps back
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    assert!(max_bytes - end < 4, "a character boundary is within 4 bytes");
    &text[..end]
}

/// Why a query is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    /// More than `limits::SEARCH_QUERY_MAX_BYTES`.
    TooLong { bytes: usize },
    /// More than `limits::SEARCH_QUERY_WORDS_MAX` words.
    TooManyWords { words: usize },
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::TooLong { bytes } => write!(f, "a query is at most {} bytes (this one is {bytes})", limits::SEARCH_QUERY_MAX_BYTES),
            QueryError::TooManyWords { words } => write!(f, "a query is at most {} words (this one has {words})", limits::SEARCH_QUERY_WORDS_MAX),
        }
    }
}

/// A person's query: its words, lowercased, each with a letter or digit
/// in it (a word of punctuation alone finds nothing, so it is dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    words: Vec<String>,
}

impl Query {
    pub fn parse(q: &str) -> Result<Query, QueryError> {
        if q.len() > limits::SEARCH_QUERY_MAX_BYTES {
            return Err(QueryError::TooLong { bytes: q.len() });
        }
        // control characters split words as spaces do: none reaches FTS5
        let words: Vec<String> =
            q.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| w.chars().any(char::is_alphanumeric)).map(str::to_lowercase).collect();
        if words.len() > limits::SEARCH_QUERY_WORDS_MAX {
            return Err(QueryError::TooManyWords { words: words.len() });
        }
        assert!(words.iter().all(|w| !w.is_empty() && !w.contains(char::is_whitespace)), "a word is one non-empty run of text");
        Ok(Query { words })
    }

    /// No word to look for: the answer is empty.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The FTS5 match: every word, a quoted prefix phrase. `None` for an
    /// empty query (FTS5 refuses an empty match).
    pub fn fts(&self) -> Option<String> {
        if self.words.is_empty() {
            return None;
        }
        let phrases: Vec<String> = self.words.iter().map(|w| format!("\"{}\"*", w.replace('"', "\"\""))).collect();
        let fts = phrases.join(" ");
        // every quote opened is closed: an even count, the escapes included
        assert!(fts.matches('"').count().is_multiple_of(2), "the match's quotes balance");
        Some(fts)
    }

    /// Whether a fragment's title or label (its name before the random
    /// suffix) holds every word, ignoring case. An empty query matches
    /// nothing.
    pub fn names(&self, name: &str, title: Option<&str>) -> bool {
        if self.words.is_empty() {
            return false;
        }
        let label = fragment_proto::split_fragment_name(name).map_or(name, |(label, _)| label);
        let haystack = format!("{} {label}", title.unwrap_or("")).to_lowercase();
        self.words.iter().all(|w| haystack.contains(w.as_str()))
    }
}

/// A hit's snippet, bounded (`limits::SEARCH_SNIPPET_MAX_BYTES`).
pub fn snippet(text: &str) -> &str {
    cut(text, limits::SEARCH_SNIPPET_MAX_BYTES)
}

/// A chat's preview in a person's list, of its newest message's text: its
/// first line with words, bounded (`limits::LISTED_PREVIEW_MAX_BYTES`).
pub fn preview(text: &str) -> &str {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    cut(line, limits::LISTED_PREVIEW_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Goal: only a message's `text` is searched (docs/chat-records.md's
    /// convention). Method: each kind of chat record, and bodies that are
    /// not messages or have no text.
    #[test]
    fn a_record_is_searched_by_its_message_text_only() {
        assert_eq!(record_text(&json!({ "text": "water the tomatoes" })), Some("water the tomatoes"));
        assert_eq!(record_text(&json!({ "text": "echo: hi", "turn": "abc" })), Some("echo: hi"));
        assert_eq!(record_text(&json!({ "kind": "message", "text": "hi" })), Some("hi"));
        assert_eq!(record_text(&json!({ "kind": "stop", "turn": "abc" })), None);
        assert_eq!(record_text(&json!({ "kind": "turn.step", "text": "the model's words" })), None);
        assert_eq!(record_text(&json!({ "kind": "turn.prompt", "text": "Run `rm`?" })), None);
        assert_eq!(record_text(&json!({ "kind": 3, "text": "hi" })), None);
        assert_eq!(record_text(&json!("a bare string")), None);
        assert_eq!(record_text(&json!({ "text": 7 })), None);
        assert_eq!(record_text(&json!({ "text": "  \n " })), None);
        assert_eq!(record_text(&json!({ "body": { "text": "nested" } })), None);
    }

    /// Goal: a long message keeps its first 4 KiB, never splitting a
    /// character. Method: text of 3-byte characters across the limit.
    #[test]
    fn a_long_text_is_cut_on_a_character() {
        let long = "€".repeat(limits::SEARCH_TEXT_MAX_BYTES);
        let body = json!({ "text": long });
        let kept = record_text(&body).unwrap();
        assert!(kept.len() <= limits::SEARCH_TEXT_MAX_BYTES && kept.len() > limits::SEARCH_TEXT_MAX_BYTES - 3);
        assert!(kept.chars().all(|c| c == '€'));
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abc", 2), "ab");
        assert_eq!(cut("é", 1), "");
        assert_eq!(snippet(&"x".repeat(1000)).len(), limits::SEARCH_SNIPPET_MAX_BYTES);
    }

    /// Goal: a chat's row shows one bounded line of its newest message.
    /// Method: blank lines first, CRLF, and a long line of 3-byte characters.
    #[test]
    fn a_preview_is_the_first_line_with_words() {
        assert_eq!(preview("Water the tomatoes\nevery Tuesday"), "Water the tomatoes");
        assert_eq!(preview("\n  \r\n  hello there \r\nmore"), "hello there");
        let euros = "€".repeat(limits::LISTED_PREVIEW_MAX_BYTES);
        let long = preview(&euros);
        assert!(long.len() <= limits::LISTED_PREVIEW_MAX_BYTES && long.len() > limits::LISTED_PREVIEW_MAX_BYTES - 3);
        assert_eq!(preview(" \n "), "");
    }

    /// Goal: no query is FTS5 syntax (no injection). Method: every
    /// operator FTS5 has, each a quoted prefix phrase in the match.
    #[test]
    fn operators_are_words() {
        let fts = |q: &str| Query::parse(q).unwrap().fts();
        assert_eq!(fts("Tomatoes").as_deref(), Some(r#""tomatoes"*"#));
        assert_eq!(fts("tomatoes OR zucchini").as_deref(), Some(r#""tomatoes"* "or"* "zucchini"*"#));
        assert_eq!(fts("NOT water").as_deref(), Some(r#""not"* "water"*"#));
        assert_eq!(fts("\"tomat").as_deref(), Some(r#""""tomat"*"#));
        assert_eq!(fts("a\"b").as_deref(), Some(r#""a""b"*"#));
        assert_eq!(fts("text:water").as_deref(), Some(r#""text:water"*"#));
        assert_eq!(fts("NEAR(a b)").as_deref(), Some(r#""near(a"* "b)"*"#));
        assert_eq!(fts("{text} : ^water").as_deref(), Some(r#""{text}"* "^water"*"#));
        assert_eq!(fts("a\0b\tc").as_deref(), Some(r#""a"* "b"* "c"*"#));
        // punctuation alone looks for nothing
        assert_eq!(fts("* \" ( ) -"), None);
        assert_eq!(fts(""), None);
        assert!(Query::parse("   ").unwrap().is_empty());
    }

    /// Goal: a query is bounded. Method: one byte and one word past each
    /// limit, and each limit itself.
    #[test]
    fn a_query_is_bounded() {
        let at = "x".repeat(limits::SEARCH_QUERY_MAX_BYTES);
        assert!(Query::parse(&at).is_ok());
        assert_eq!(Query::parse(&format!("{at}x")), Err(QueryError::TooLong { bytes: limits::SEARCH_QUERY_MAX_BYTES + 1 }));
        let words = |n: usize| vec!["w"; n].join(" ");
        assert!(Query::parse(&words(limits::SEARCH_QUERY_WORDS_MAX)).is_ok());
        assert_eq!(Query::parse(&words(limits::SEARCH_QUERY_WORDS_MAX + 1)), Err(QueryError::TooManyWords { words: limits::SEARCH_QUERY_WORDS_MAX + 1 }));
        // punctuation is not a word, so it does not count toward the limit
        assert!(Query::parse(&format!("{} * * *", words(limits::SEARCH_QUERY_WORDS_MAX))).is_ok());
    }

    /// Goal: a fragment's title or label match every word, not its name's
    /// random suffix. Method: names and titles against queries.
    #[test]
    fn names_match_title_or_label() {
        let q = |q: &str| Query::parse(q).unwrap();
        assert!(q("garden").names("garden-chat--k3x9", None));
        assert!(q("Garden Crew").names("group--k3x9", Some("The garden crew")));
        assert!(q("crew grou").names("group--k3x9", Some("The garden crew")));
        assert!(!q("garden zucchini").names("group--k3x9", Some("The garden crew")));
        assert!(!q("k3x9").names("group--k3x9", Some("The garden crew")));
        assert!(!q("").names("group--k3x9", Some("anything")));
    }
}
