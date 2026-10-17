//! `cargo xtask check`'s HTML-sink pass: no first-party JavaScript turns a
//! string into markup or code. A page's text (a file, a message, a model's
//! words) becomes DOM through the DOM's constructors and setters only
//! (cell/markdown.mjs renders markdown so; an icon is built of its shapes),
//! so no text can add an element, an attribute or a script to a fragment's
//! origin. The notes viewer once put `marked`'s output in `innerHTML`, and
//! every note's `<img onerror>` ran as its viewers (the 2026-10-16 audit's
//! H2); this pass makes the sink itself the error, wherever it is written.
//!
//! Which source: every file `js_syntax` checks (first-party `.js`, `.mjs`
//! and `.cjs`, less its `VENDORED`), and each inline `<script>` of a `.html`
//! git tracks or would add. A refused name (`REFUSED`) found anywhere in
//! one fails the check, its file and line named: in code, in a string (so
//! `el["innerHTML"]` is found too) and in a comment (so no exception hides
//! in one, and the words never appear). A name is found where it starts a
//! word, and ends one when it ends in a letter (`retrieval(` is no `eval(`).
//!
//! Besides the names, a few shapes are sinks only by what follows them
//! (`SHAPED`): an `on…` attribute set by `setAttribute`, a timer handed a
//! string, a `<script>` the page makes. `eval` and `Function` are found
//! with space before their `(` too, and `srcdoc` in any case.
//!
//! An exception is a place that must turn a string into code or markup:
//! `ALLOWED`, each with why. Like `js_syntax`'s lists, an entry that no
//! longer matches fails the check, so a stale one never waves a new use
//! through.
//!
//! What a text scan cannot see, and review must: a name built at run time
//! (`el["inner" + "HTML"]`, `Reflect.set`, `document[name]`), a sink taken
//! into a variable first (`const write = document.write`), a URL of
//! `javascript:` set on a link or `location` (the renderer's links are held
//! to safe schemes, and a page's own must be), code loaded from a string's
//! URL (`import()` or a `Worker` of a `data:` or `blob:` URL), and the text
//! of a `<script>` the page did not make here.

use std::path::Path;

use anyhow::{ensure, Context, Result};

/// The names that turn a string into markup or code, each with what it
/// does. A name ending in `(` is found with space before its `(` too; one
/// in lower case (an attribute's) is found in any case.
pub const REFUSED: [(&str, &str); 12] = [
    ("innerHTML", "parses a string as markup"),
    ("outerHTML", "parses a string as markup"),
    ("insertAdjacentHTML", "parses a string as markup"),
    ("document.write", "parses a string into the document"),
    ("document.writeln", "parses a string into the document"),
    ("srcdoc", "makes a frame's document of a string"),
    ("DOMParser", "parses a string as markup (its event handlers run once inserted)"),
    ("createContextualFragment", "parses a string as markup"),
    ("setHTMLUnsafe", "parses a string as markup"),
    ("parseHTMLUnsafe", "parses a string as markup"),
    ("eval(", "runs a string as code"),
    ("Function(", "runs a string as code"),
];

/// A test of the text after a name's `(` and any space.
pub type Follows = fn(&str) -> bool;

/// The shapes that are sinks by what follows a name: `(name, after, what)`.
pub const SHAPED: [(&str, Follows, &str); 4] = [
    ("setAttribute", event_attribute, "sets an event handler (on…) from a string"),
    ("setTimeout", string_first, "runs a string as code"),
    ("setInterval", string_first, "runs a string as code"),
    ("createElement", script_element, "makes a <script>, whose text runs"),
];

// `"on…` or `'on…`: an attribute that is an event handler.
fn event_attribute(after: &str) -> bool {
    let mut chars = after.chars();
    matches!(chars.next(), Some('"' | '\'' | '`')) && chars.as_str().get(..2).is_some_and(|on| on.eq_ignore_ascii_case("on"))
}

// a string literal first: the timer runs it as code
fn string_first(after: &str) -> bool {
    after.starts_with(['"', '\'', '`'])
}

// `"script"`, in any case and quote
fn script_element(after: &str) -> bool {
    let mut chars = after.chars();
    matches!(chars.next(), Some('"' | '\'' | '`')) && chars.as_str().get(..7).is_some_and(|s| s[..6].eq_ignore_ascii_case("script") && s[6..].starts_with(['"', '\'', '`']))
}

/// The places that must, each `(path, name, why)`.
pub const ALLOWED: [(&str, &str, &str); 2] = [
    ("crates/e2e/fixtures/probe.mjs", "eval(", "the e2e's probe that a fragment's Worker refuses code from strings (cell/platform.mjs)"),
    ("crates/e2e/fixtures/probe.mjs", "Function(", "the same probe, by the Function constructor"),
];

/// More inline scripts than this in one page means the scan is reading
/// something that is not one.
const SCRIPTS_PER_PAGE_MAX: usize = 64;

/// A refused name found: where, and which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    pub path: String,
    pub line: usize,
    pub name: &'static str,
}

/// A piece of first-party JavaScript: a file, or a page's inline script,
/// with the line of its file it starts on.
pub struct Source {
    pub path: String,
    pub text: String,
    pub first_line: usize,
}

/// The repo's first-party JavaScript: each file `js_syntax` checks, and
/// each inline script of its pages.
pub fn sources(root: &Path) -> Result<Vec<Source>> {
    let listed = crate::js_syntax::listed(root)?;
    let mut out = vec![];
    for (path, _) in crate::js_syntax::select(listed.iter().cloned())? {
        let text = std::fs::read_to_string(root.join(&path)).with_context(|| format!("read {path}"))?;
        out.push(Source { path, text, first_line: 1 });
    }
    for path in listed.iter().filter(|p| p.ends_with(".html") && !crate::js_syntax::vendored(p)) {
        let text = std::fs::read_to_string(root.join(path)).with_context(|| format!("read {path}"))?;
        out.extend(inline_scripts(path, &text)?);
    }
    ensure!(out.iter().any(|s| s.path == "cell/markdown.mjs"), "the sources hold no cell/markdown.mjs: the walk is broken");
    Ok(out)
}

/// A page's inline scripts (a `<script>` with no `src`), each with the line
/// it starts on.
pub fn inline_scripts(path: &str, html: &str) -> Result<Vec<Source>> {
    // ASCII lowercase keeps byte offsets: they index `html` as they index `lower`
    let lower = html.to_ascii_lowercase();
    let mut out = vec![];
    let mut at = 0;
    // each pass moves `at` past a script's end, or stops
    while let Some(open) = lower[at..].find("<script").map(|i| at + i) {
        let tag_end = lower[open..].find('>').map(|i| open + i + 1).with_context(|| format!("{path}: a <script> tag that never closes"))?;
        let body_end = lower[tag_end..].find("</script").map(|i| tag_end + i).with_context(|| format!("{path}: a <script> with no </script>"))?;
        if !has_attribute(&lower[open..tag_end], "src") {
            let first_line = html[..tag_end].matches('\n').count() + 1;
            out.push(Source { path: path.to_string(), text: html[tag_end..body_end].to_string(), first_line });
        }
        ensure!(out.len() <= SCRIPTS_PER_PAGE_MAX, "{path}: more than {SCRIPTS_PER_PAGE_MAX} inline scripts");
        at = body_end;
    }
    Ok(out)
}

// Whether a tag (lower case) has the attribute `name`: preceded by space,
// followed by `=` or the tag's end (so `data-src` is no `src`).
fn has_attribute(tag: &str, name: &str) -> bool {
    tag.match_indices(name).any(|(i, _)| {
        let before = tag[..i].chars().next_back().is_some_and(|c| c.is_ascii_whitespace());
        let after = after_space(&tag[i + name.len()..]);
        before && (after.starts_with('=') || after.starts_with('>') || after.starts_with('/'))
    })
}

/// Every refused name and shape in `source`, by line.
pub fn sinks(source: &Source) -> Vec<Sink> {
    let mut found = vec![];
    for (n, line) in source.text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        for (name, _) in REFUSED {
            let call = name.strip_suffix('(');
            let word = call.unwrap_or(name);
            // a lower-case name is an attribute's, which HTML reads in any case
            let (hay, needle) = if word.bytes().all(|b| !b.is_ascii_uppercase()) { (lower.as_str(), word) } else { (line, word) };
            let hit = match call {
                Some(_) => words(hay, needle).any(|i| after_space(&hay[i + needle.len()..]).starts_with('(')),
                None => words(hay, needle).next().is_some(),
            };
            if hit {
                found.push(Sink { path: source.path.clone(), line: source.first_line + n, name });
            }
        }
        for (name, follows, _) in SHAPED {
            let shaped = words(line, name).any(|i| {
                let rest = after_space(&line[i + name.len()..]);
                rest.strip_prefix('(').is_some_and(|args| follows(after_space(args)))
            });
            if shaped {
                found.push(Sink { path: source.path.clone(), line: source.first_line + n, name });
            }
        }
    }
    found
}

// Where `word` stands as a word in `line`: no identifier character before
// it, nor after it when it ends in one. Bounded by the line: each match
// moves past the one before.
fn words<'a>(line: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    line.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        let starts = line[..i].chars().next_back().is_none_or(|c| !ident(c));
        let ends = !word.ends_with(ident) || line[i + word.len()..].chars().next().is_none_or(|c| !ident(c));
        starts && ends
    })
}

fn after_space(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

/// Fails on every refused name `allowed` does not name, and on an
/// `allowed` entry that names nothing found.
pub fn check(sources: &[Source], allowed: &[(&str, &str, &str)]) -> Result<()> {
    let mut used = vec![false; allowed.len()];
    let mut refused = vec![];
    for sink in sources.iter().flat_map(sinks) {
        match allowed.iter().position(|(path, name, _)| *path == sink.path && *name == sink.name) {
            Some(i) => used[i] = true,
            None => refused.push(sink),
        }
    }
    let why = |name: &str| REFUSED.iter().find(|(n, _)| *n == name).map(|(_, why)| *why).or_else(|| SHAPED.iter().find(|(n, _, _)| *n == name).map(|(_, _, why)| *why)).unwrap_or("");
    let listed: Vec<String> = refused.iter().map(|s| format!("{}:{}: {} ({})", s.path, s.line, s.name, why(s.name))).collect();
    ensure!(
        listed.is_empty(),
        "first-party JavaScript turns a string into markup or code (xtask/src/html_sinks.rs), {} place(s); build the DOM instead (cell/markdown.mjs for markdown, an icon's shapes for an icon):\n{}",
        listed.len(),
        listed.join("\n")
    );
    for (i, (path, name, why)) in allowed.iter().enumerate() {
        ensure!(used[i], "html_sinks::ALLOWED names {name} in {path} ({why}), and it is not there: remove the entry");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_devstack as devstack;

    fn source(path: &str, text: &str) -> Source {
        Source { path: path.to_string(), text: text.to_string(), first_line: 1 }
    }

    /// Goal: a planted sink fails the check, named by its file, line and
    /// name. Method: each refused name in code, in a string and in a
    /// comment, beside clean code and names that only contain one.
    #[test]
    fn a_planted_sink_fails_the_check_naming_its_line() {
        let clean = source(
            "site/clean.js",
            "const p = document.createElement(\"p\");\np.textContent = text;\nretrieval(1);\nconst innerHTMLish = 1;\nisFunction(x);\nsetTimeout(() => tick(), 10);\np.setAttribute(\"aria-label\", text);\nconst f = function (x) { return x; };\ndocument.createElement(\"scripts-list\");\n",
        );
        check(&[source("site/clean.js", &clean.text)], &[]).expect("clean code passes");
        assert!(sinks(&clean).is_empty(), "{:?}", sinks(&clean));
        let planted = [
            ("a.js", "ok();\nnode.innerHTML = text;\n", 2, "innerHTML"),
            ("b.mjs", "node[\"outerHTML\"] = text;\n", 1, "outerHTML"),
            ("c.js", "x.insertAdjacentHTML(\"beforeend\", text);\n", 1, "insertAdjacentHTML"),
            ("d.js", "\n\ndocument.write(text);\n", 3, "document.write"),
            ("e.js", "document.writeln(text);\n", 1, "document.writeln"),
            ("f.js", "frame.srcdoc = page;\n", 1, "srcdoc"),
            ("g.js", "new DOMParser().parseFromString(text, \"text/html\");\n", 1, "DOMParser"),
            ("h.js", "range.createContextualFragment(text);\n", 1, "createContextualFragment"),
            ("i.js", "el.setHTMLUnsafe(text);\n", 1, "setHTMLUnsafe"),
            ("j.js", "eval(text);\n", 1, "eval("),
            ("k.js", "const run = new Function(\"return 1\");\n", 1, "Function("),
            ("l.js", "// one must never write el.innerHTML here\n", 1, "innerHTML"),
            ("m.js", "eval (text);\n", 1, "eval("),
            ("n.js", "frame.setAttribute(\"srcDoc\", page);\n", 1, "srcdoc"),
            ("o.js", "Document.parseHTMLUnsafe(text);\n", 1, "parseHTMLUnsafe"),
            ("p.js", "el.setAttribute(\"onclick\", code);\n", 1, "setAttribute"),
            ("q.js", "el.setAttribute( 'OnLoad', code);\n", 1, "setAttribute"),
            ("r.js", "setTimeout(\"tick()\", 10);\n", 1, "setTimeout"),
            ("s.js", "setInterval (`tick()`, 10);\n", 1, "setInterval"),
            ("t.js", "const s = document.createElement(\"SCRIPT\");\n", 1, "createElement"),
        ];
        for (path, text, line, name) in planted {
            let s = source(path, text);
            assert_eq!(sinks(&s), [Sink { path: path.into(), line, name }], "{path}");
            let err = check(&[source("site/clean.js", &clean.text), s], &[]).expect_err(path).to_string();
            assert!(err.contains(&format!("{path}:{line}: {name}")), "{err}");
            assert!(!err.contains("site/clean.js"), "only the planted file is named: {err}");
        }
    }

    /// Goal: a page's inline scripts are scanned, at their own lines, and
    /// nothing else of the page is. Method: a page with an inline script, a
    /// script by `src`, and the words in its markup.
    #[test]
    fn a_pages_inline_script_is_scanned_at_its_line() {
        let page = "<!doctype html>\n<p>innerHTML is a word here</p>\n<script src=\"./app.js\"></script>\n<SCRIPT type=\"module\">\n  const a = 1;\n  b.innerHTML = a;\n</script>\n";
        let scripts = inline_scripts("site/index.html", page).expect("the page's scripts");
        assert_eq!(scripts.len(), 1, "the inline one alone");
        assert_eq!(scripts[0].first_line, 4);
        assert_eq!(sinks(&scripts[0]), [Sink { path: "site/index.html".into(), line: 6, name: "innerHTML" }]);
        assert!(inline_scripts("x.html", "<script>never closed").is_err(), "a script with no end is an error, not a pass");
        let data_src = inline_scripts("y.html", "<script data-src=\"x.js\">a.innerHTML = b;</script><script defer src=x.js></script>").expect("scripts");
        assert_eq!(data_src.len(), 1, "a data-src is no src: its body is scanned, and a real src's is not");
    }

    /// Goal: an exception exempts its file's name and nothing else, and a
    /// stale one fails. Method: the same sink allowed, allowed in another
    /// file, and an entry that matches nothing.
    #[test]
    fn an_exception_is_exact_and_never_stale() {
        let probe = source("fixtures/probe.mjs", "eval(\"1\");\n");
        check(&[source("fixtures/probe.mjs", "eval(\"1\");\n")], &[("fixtures/probe.mjs", "eval(", "a probe")]).expect("allowed");
        assert!(check(&[probe], &[("fixtures/other.mjs", "eval(", "elsewhere")]).is_err(), "another file's entry exempts nothing");
        let err = check(&[source("a.js", "ok();\n")], &[("fixtures/probe.mjs", "eval(", "a probe")]).expect_err("stale").to_string();
        assert!(err.contains("ALLOWED names eval( in fixtures/probe.mjs"), "{err}");
    }

    /// Goal: the repo itself holds no sink but its listed exceptions, and
    /// the walk reaches the renderer, the shell, the templates and pages.
    #[test]
    fn the_repo_holds_no_html_sink() {
        let root = devstack::repo_root();
        let found = sources(&root).expect("the repo's sources");
        for path in ["cell/markdown.mjs", "cell/files.mjs", "cell/shell/shell.js", "templates/chat/site/chat.js", "templates/notes/site/assets/viewer.js", "templates/notes/site/index.html"] {
            assert!(found.iter().any(|s| s.path == path), "{path} is scanned");
        }
        check(&found, &ALLOWED).expect("no HTML sink in first-party JavaScript");
    }
}
