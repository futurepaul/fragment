//! Literal `references/…` paths in skill text, shared by image build checks
//! and the managed template's host tests. Paths are relative to the skill
//! root, as Hermes' `skill_view(file_path=…)` resolves them.

pub fn paths(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices("references/").filter_map(move |(start, _)| {
        let prefix = text[..start].rsplit(|c: char| c.is_whitespace() || "`\"'()[]<>".contains(c)).next().unwrap_or("");
        if prefix.contains("://") { return None; }
        // Ignore the generic directory and ellipsis used in descriptions
        // of the skill format; concrete files and subdirectories are checked.
        let tail = &text[start..];
        let end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || "_./-".contains(c))).unwrap_or(tail.len());
        let path = tail[..end].trim_end_matches('.');
        (path.len() > "references/".len()).then_some(path)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn paths_in_prose_links_and_tool_calls() {
        let text = "`references/desktop.md`, [API](references/api.json#scope), file_path=\"references/a-b.md\". Read references/deep/page.md. Generic references/… and references/...; external https://example.com/references/api.md";
        assert_eq!(super::paths(text).collect::<Vec<_>>(), ["references/desktop.md", "references/api.json", "references/a-b.md", "references/deep/page.md"]);
    }
}
