//! What a Durable Object's schema makes, read from its own DDL: the
//! tables a fragment's delete drops and makes again empty (cell `ended.rs`),
//! so a table added to the schema is never one the delete forgets.

/// The tables `ddl` creates, in order: each statement that starts a line
/// with `CREATE TABLE IF NOT EXISTS <name>`. Indexes go with their tables.
pub fn tables(ddl: &str) -> Vec<&str> {
    let tables: Vec<&str> = ddl
        .lines()
        .filter_map(|line| line.strip_prefix("CREATE TABLE IF NOT EXISTS "))
        .map(|rest| rest.split([' ', '(']).next().unwrap_or(""))
        .collect();
    for (i, t) in tables.iter().enumerate() {
        assert!(!t.is_empty() && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "a table's name is [a-z0-9_]+: {t:?}");
        assert!(!tables[..i].contains(t), "the schema creates {t} once");
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_table_and_no_index() {
        let ddl = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS members (
  principal TEXT PRIMARY KEY, role TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS members_role ON members (role);
CREATE TABLE IF NOT EXISTS runs(id INTEGER PRIMARY KEY AUTOINCREMENT);
";
        assert_eq!(tables(ddl), ["meta", "members", "runs"]);
        assert!(tables("").is_empty());
    }

    #[test]
    #[should_panic(expected = "creates meta once")]
    fn a_table_twice_is_refused() {
        tables("CREATE TABLE IF NOT EXISTS meta (k TEXT);\nCREATE TABLE IF NOT EXISTS meta (k TEXT);\n");
    }

    #[test]
    #[should_panic(expected = "[a-z0-9_]+")]
    fn a_quoted_name_is_refused() {
        tables("CREATE TABLE IF NOT EXISTS \"odd name\" (k TEXT);\n");
    }
}
