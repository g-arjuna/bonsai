// Graph explorer: Cypher query sanitiser and execution wrapper.
//
// The explorer endpoint allows arbitrary read-only Cypher from operators and AI agents.
// Mutation keywords are rejected before the query reaches the database.
// Column names are extracted from the RETURN clause (AS aliases preferred).

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ─── sanitiser ───────────────────────────────────────────────────────────────

/// Mutation Cypher keywords that are never permitted in the explorer.
const BANNED_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "DROP", "MERGE", "REMOVE", "DETACH", "CALL", "SET",
];

/// Returns Ok if the query is read-only, Err with the offending keyword otherwise.
pub fn validate_query(cypher: &str) -> Result<(), String> {
    let upper = cypher.to_uppercase();
    let upper_bytes = upper.as_bytes();
    for &kw in BANNED_KEYWORDS {
        if keyword_present(upper_bytes, kw.as_bytes()) {
            return Err(format!(
                "query contains disallowed keyword '{}' — explorer is read-only",
                kw
            ));
        }
    }
    Ok(())
}

/// Word-boundary search: `kw` must not be flanked by `[A-Za-z0-9_]` bytes.
fn keyword_present(text: &[u8], kw: &[u8]) -> bool {
    let n = text.len();
    let wn = kw.len();
    if wn > n {
        return false;
    }
    let mut i = 0;
    while i + wn <= n {
        if text[i..i + wn] == *kw {
            let before_ok = i == 0 || !is_ident_byte(text[i - 1]);
            let after_ok = i + wn == n || !is_ident_byte(text[i + wn]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ─── column name extraction ───────────────────────────────────────────────────

/// Extract column names from the RETURN clause.
/// Prefers AS aliases; falls back to the last dot-separated identifier in each term.
pub fn extract_columns(cypher: &str) -> Vec<String> {
    let upper = cypher.to_uppercase();
    // Use the LAST occurrence of RETURN (handles subqueries)
    let Some(ret_pos) = upper.rfind("RETURN") else {
        return vec![];
    };
    let after = &cypher[ret_pos + 6..]; // skip "RETURN"

    // Find where the RETURN clause ends
    let end = clause_end(after);
    let return_clause = &after[..end];

    return_clause
        .split(',')
        .enumerate()
        .map(|(i, term)| {
            let term = term.trim();
            let term_upper = term.to_uppercase();
            // If there's an AS alias use it
            if let Some(as_pos) = term_upper.rfind(" AS ") {
                term[as_pos + 4..].trim().to_string()
            } else {
                // Last identifier after . or space (e.g. "d.address" → "address")
                let last = term
                    .split(|c: char| c == '.' || c == '(' || c == ')' || c == ' ')
                    .filter(|s| !s.is_empty() && s.chars().next().map_or(false, |c| c.is_alphanumeric()))
                    .last();
                last.map(|s| s.to_string()).unwrap_or_else(|| format!("col_{}", i))
            }
        })
        .collect()
}

/// Byte offset in `s` where the RETURN clause ends (ORDER/LIMIT/SKIP/EOF).
fn clause_end(s: &str) -> usize {
    let upper = s.to_uppercase();
    for kw in &[" ORDER ", " LIMIT ", " SKIP ", "\nORDER", "\nLIMIT", "\nSKIP"] {
        if let Some(pos) = upper.find(kw) {
            return pos;
        }
    }
    s.len()
}

// ─── result types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorerResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    /// True if results were truncated to MAX_ROWS.
    pub truncated: bool,
}

const MAX_ROWS: usize = 500;

// ─── execution ────────────────────────────────────────────────────────────────

/// Validate and execute a read-only Cypher query. Returns column names and rows.
pub fn execute_query(conn: &Connection<'_>, cypher: &str) -> Result<ExplorerResult> {
    validate_query(cypher).map_err(|e| anyhow::anyhow!("{}", e))?;

    let columns = extract_columns(cypher);

    let iter = conn.query(cypher).context("explorer query execution")?;

    let mut rows = Vec::new();
    let mut truncated = false;
    for row in iter {
        if rows.len() >= MAX_ROWS {
            truncated = true;
            break;
        }
        let json_row: Vec<serde_json::Value> = row.iter().map(value_to_json).collect();
        rows.push(json_row);
    }

    let row_count = rows.len();
    Ok(ExplorerResult { columns, rows, row_count, truncated })
}

// ─── Value → serde_json::Value ───────────────────────────────────────────────

fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::{json, Value as J};
    match v {
        Value::String(s) => J::String(s.clone()),
        Value::Int64(n) => json!(*n),
        Value::Int32(n) => json!(*n),
        Value::Float(f) => json!(*f),
        Value::Bool(b) => J::Bool(*b),
        Value::Null(_) => J::Null,
        Value::TimestampNs(dt) => {
            // Format as ISO 8601 string
            J::String(format_ts(*dt))
        }
        Value::TimestampTz(dt) => J::String(format_ts(*dt)),
        _ => J::String(format!("{:?}", v)),
    }
}

fn format_ts(dt: OffsetDateTime) -> String {
    let (y, m, d) = (dt.year(), dt.month() as u8, dt.day());
    let (h, min, s, ns) = (dt.hour(), dt.minute(), dt.second(), dt.nanosecond());
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z", y, m, d, h, min, s, ns)
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_create() {
        assert!(validate_query("CREATE (n:Device)").is_err());
    }

    #[test]
    fn validate_rejects_delete() {
        assert!(validate_query("MATCH (n) DELETE n").is_err());
    }

    #[test]
    fn validate_rejects_merge() {
        assert!(validate_query("MERGE (n:Device {address: '1.2.3.4'})").is_err());
    }

    #[test]
    fn validate_rejects_set() {
        assert!(validate_query("MATCH (n) SET n.foo = 1").is_err());
    }

    #[test]
    fn validate_rejects_detach_delete() {
        assert!(validate_query("MATCH (n) DETACH DELETE n").is_err());
    }

    #[test]
    fn validate_allows_match_return() {
        assert!(validate_query("MATCH (d:Device) RETURN d.address").is_ok());
    }

    #[test]
    fn validate_allows_optional_match() {
        assert!(
            validate_query(
                "MATCH (d:Device) OPTIONAL MATCH (d)-[:LOCATED_AT]->(s:Site) RETURN d.address, s.name"
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_does_not_false_positive_on_inlined_words() {
        // "dataset" contains "set" but is not the keyword SET
        assert!(validate_query("MATCH (d:Device) WHERE d.hostname STARTS WITH 'dataset' RETURN d.address").is_ok());
    }

    #[test]
    fn extract_columns_simple() {
        let cols = extract_columns("MATCH (d:Device) RETURN d.address, d.hostname");
        assert_eq!(cols, vec!["address", "hostname"]);
    }

    #[test]
    fn extract_columns_with_as_aliases() {
        let cols = extract_columns("MATCH (d:Device) RETURN d.address AS addr, count(*) AS total");
        assert_eq!(cols, vec!["addr", "total"]);
    }

    #[test]
    fn extract_columns_with_order_by() {
        let cols =
            extract_columns("MATCH (d:Device) RETURN d.address, d.vendor ORDER BY d.vendor");
        assert_eq!(cols, vec!["address", "vendor"]);
    }

    #[test]
    fn execute_query_runs_against_test_graph() {
        use crate::graph::test_fixtures::TestGraph;
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let result =
            execute_query(&conn, "MATCH (d:Device) RETURN d.address ORDER BY d.address LIMIT 3")
                .unwrap();
        assert_eq!(result.columns, vec!["address"]);
        assert_eq!(result.rows.len(), 3);
        assert!(!result.truncated);
    }

    #[test]
    fn execute_query_rejects_create() {
        use crate::graph::test_fixtures::TestGraph;
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let err = execute_query(&conn, "CREATE (n:Device {address: 'x'})");
        assert!(err.is_err());
    }
}
