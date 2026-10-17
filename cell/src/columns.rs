//! A column added to a table an object already has (docs/technical-debt-
//! ledger.md, "Objects made before a column of their table refuse what
//! writes it"): `CREATE TABLE IF NOT EXISTS` never adds one, so each column
//! added after a table first shipped is added here as its object starts.

use worker::SqlStorage;

/// Adds `column` (`declaration`: its type and default, as `CREATE TABLE`
/// says it) to `table` unless it has it. Answers whether it added it. A
/// column it cannot add is a fault of the object's (it panics, as a
/// schema that does not apply does).
pub fn add_if_missing(sql: &SqlStorage, table: &str, column: &str, declaration: &str) -> bool {
    assert!(table.bytes().chain(column.bytes()).all(|b| b.is_ascii_lowercase() || b == b'_'), "a table's and a column's names are words: {table}.{column}");
    let cols: Vec<serde_json::Value> = sql
        .exec(&format!("PRAGMA table_info({table})"), None)
        .and_then(|c| c.to_array())
        .unwrap_or_else(|e| panic!("{table}'s columns: {e}"));
    assert!(!cols.is_empty(), "{table} is made before its columns are added");
    if cols.iter().any(|c| c["name"] == column) {
        return false;
    }
    sql.exec(&format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"), None).unwrap_or_else(|e| panic!("adding {table}.{column}: {e}"));
    true
}
