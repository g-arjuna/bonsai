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
    "CREATE",
    "DELETE",
    "DROP",
    "MERGE",
    "REMOVE",
    "DETACH",
    "CALL",
    "SET",
    "LOAD",
    "COPY",
    "FOREACH",
    "ALTER",
    "RENAME",
    "IMPORT",
    "EXPORT",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "CHECKPOINT",
];

/// State of the Cypher lexical scanner.
#[derive(Copy, Clone, PartialEq, Eq)]
enum LexState {
    Normal,
    InSingleQuote,
    InDoubleQuote,
    InBacktick,
    InBlockComment,
    InLineComment,
}

/// Strip comments and mask string literal contents from a Cypher query for keyword validation.
///
/// This ensures:
/// 1. Comment delimiters (`/*`, `//`, `--`) inside quoted strings are NOT treated as comments.
/// 2. Quote characters inside comments do NOT toggle string literal state.
/// 3. Escape characters (`\'`, `\"`, `\\`) and doubled quotes (`''`, `""`) inside string literals are respected.
/// 4. Mutation keywords inside string literals (e.g. `WHERE x = 'CREATE'`) are masked and not falsely rejected.
/// 5. Mutation keywords in Cypher clauses (e.g. `WITH '/*' AS x CREATE ...`) are strictly detected and rejected.
pub fn sanitize_cypher_for_validation(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut state = LexState::Normal;

    while i < len {
        let c = chars[i];
        let next = if i + 1 < len {
            Some(chars[i + 1])
        } else {
            None
        };

        match state {
            LexState::Normal => {
                if c == '\'' {
                    state = LexState::InSingleQuote;
                    out.push('\'');
                    i += 1;
                } else if c == '"' {
                    state = LexState::InDoubleQuote;
                    out.push('"');
                    i += 1;
                } else if c == '`' {
                    state = LexState::InBacktick;
                    out.push('`');
                    i += 1;
                } else if c == '/' && next == Some('*') {
                    state = LexState::InBlockComment;
                    out.push(' ');
                    i += 2;
                } else if (c == '/' && next == Some('/')) || (c == '-' && next == Some('-')) {
                    state = LexState::InLineComment;
                    out.push(' ');
                    i += 2;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            LexState::InSingleQuote => {
                if c == '\\' {
                    i += 2; // skip escape
                } else if c == '\'' {
                    if next == Some('\'') {
                        i += 2; // doubled single quote ''
                    } else {
                        state = LexState::Normal;
                        out.push('\'');
                        i += 1;
                    }
                } else {
                    i += 1; // mask string contents
                }
            }
            LexState::InDoubleQuote => {
                if c == '\\' {
                    i += 2;
                } else if c == '"' {
                    if next == Some('"') {
                        i += 2;
                    } else {
                        state = LexState::Normal;
                        out.push('"');
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            LexState::InBacktick => {
                if c == '`' {
                    state = LexState::Normal;
                    out.push('`');
                    i += 1;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            LexState::InBlockComment => {
                if c == '*' && next == Some('/') {
                    state = LexState::Normal;
                    out.push(' ');
                    i += 2;
                } else {
                    i += 1;
                }
            }
            LexState::InLineComment => {
                if c == '\n' {
                    state = LexState::Normal;
                    out.push('\n');
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
    }
    out
}

pub fn strip_cypher_comments(s: &str) -> String {
    sanitize_cypher_for_validation(s)
}

/// Returns Ok if the query is read-only, Err with the offending keyword otherwise.
pub fn validate_query(cypher: &str) -> Result<(), String> {
    if cypher.len() > 16_384 {
        return Err("diagnostic query exceeds 16384 byte input limit".into());
    }
    let sanitized = sanitize_cypher_for_validation(cypher);
    let trimmed = sanitized.trim();
    let without_trailing_semi = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
    if without_trailing_semi.contains(';') {
        return Err("multi-statement queries are not permitted in read-only queries".to_string());
    }

    let upper = sanitized.to_uppercase();
    // RANGE constants are evaluated during planning, before the engine execution
    // timer/interrupt checkpoints. Keep expansion bounded; dynamic bounds fail closed.
    for (index, _) in upper.match_indices("RANGE") {
        if index > 0 && is_ident_byte(upper.as_bytes()[index - 1]) {
            continue;
        }
        let tail = upper[index + 5..].trim_start();
        let Some(arguments) = tail.strip_prefix('(') else {
            continue;
        };
        let end = arguments.find(')').ok_or("unterminated RANGE")?;
        let values: Vec<i64> = arguments[..end]
            .split(',')
            .map(|v| v.trim().parse::<i64>())
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| "RANGE requires bounded integer literals")?;
        if !(2..=3).contains(&values.len()) {
            return Err("invalid RANGE arguments".into());
        }
        let step = values.get(2).copied().unwrap_or(1);
        let distance = i128::from(values[1]) - i128::from(values[0]);
        if step == 0 || distance.unsigned_abs() / i128::from(step).unsigned_abs() >= 10_000 {
            return Err("RANGE expansion exceeds 10000 values".into());
        }
    }
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

// ─── BN-022: principal-scoped read policy ────────────────────────────────────

/// Access tier for ad-hoc read-only graph queries.
///
/// `Standard` applies to every non-admin principal — Viewer, Operator,
/// ApiReadonly, scoped API keys, and unauthenticated callers when auth is
/// disabled. Only positively allowlisted node labels may be referenced and
/// every node pattern must be label-bound; anything else fails closed before
/// the query reaches the engine. `Admin` retains unrestricted read access
/// (queries remain mutation-safe via [`validate_query`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphReadAccess {
    Standard,
    Admin,
}

/// Node labels non-admin principals may reference in ad-hoc queries.
///
/// Positive allowlist audited against the `GraphStore` schema (BN-022).
/// Excluded on purpose:
/// - `ConfigItem` — stores credential material (JWT signing secret, user
///   password verifiers, API-key hashes, sessions, revocation markers).
/// - `ConfigSnapshot` / `ConfigChange` — captured device configuration can
///   embed device credentials (SNMP communities, keys, passwords).
///
/// New node tables are denied by default until audited and added here.
const ADHOC_ALLOWED_LABELS: &[&str] = &[
    "AclSummary",
    "AffectedPrefix",
    "AgentToolCall",
    "AppFlow",
    "Application",
    "AttackVector",
    "BfdSession",
    "BgpLsLink",
    "BgpLsNode",
    "BgpNeighbor",
    "BgpRibEntry",
    "BmpSession",
    "ChangeRequest",
    "ComputeNode",
    "DdosEvent",
    "DdosRouteEvent",
    "DdosScope",
    "DetectionEvent",
    "Device",
    "DeviceAddress",
    "DeviceConfigEmbedding",
    "DeviceEmbedding",
    "EnrichmentProperty",
    "EntityIdentity",
    "Environment",
    "EventEmbedding",
    "GnmiReadiness",
    "GnnAttentionSnapshot",
    "GnnInferenceResult",
    "GnnScore",
    "HostEndpoint",
    "Incident",
    "Interface",
    "Investigation",
    "InvestigationFeedback",
    "IsisAdjacency",
    "LldpNeighbor",
    "Location",
    "MigrationMarker",
    "MitigationAction",
    "MlJobRun",
    "MlJobSchedule",
    "ModelArtifact",
    "MplsLsp",
    "NtpPeer",
    "OpticalChannel",
    "OpticsTelemetry",
    "OspfNeighbor",
    "OwnershipBinding",
    "ParquetExport",
    "PerformanceBaseline",
    "Playbook",
    "PlaybookExecution",
    "PowerUnit",
    "Prefix",
    "PropertyProvenance",
    "QoSPolicy",
    "Rack",
    "RedundancyGroup",
    "Remediation",
    "RemediationProposal",
    "RemediationTrustMark",
    "SavedQuery",
    "SecurityIncident",
    "SecurityPolicy",
    "SecurityPosture",
    "SensorReading",
    "ServiceEndpoint",
    "ShunRule",
    "SignalPolicy",
    "Site",
    "SrPolicy",
    "StateChangeEvent",
    "StpInstance",
    "StreamingReadiness",
    "SubscriptionStatus",
    "TrafficBaseline",
    "VLAN",
    "Vrf",
    "Vulnerability",
];

/// Validate a read-only query for the given access tier.
///
/// In addition to the mutation checks in [`validate_query`], `Standard`
/// access requires every node pattern to reference only allowlisted labels.
/// Unlabeled node patterns are rejected unless the variable was bound by an
/// earlier labeled pattern in the same query scope (a bound-variable
/// reference filters an existing node; it does not scan every table).
pub fn validate_query_for_access(cypher: &str, access: GraphReadAccess) -> Result<(), String> {
    validate_query(cypher)?;
    if access == GraphReadAccess::Admin {
        return Ok(());
    }
    let sanitized = sanitize_cypher_for_validation(cypher);
    // UNION starts an independent query scope: variables bound in one branch
    // do not carry into the next, so bound-variable tracking resets.
    // Scanned on the sanitized bytes (ASCII case-insensitive) so positions
    // stay aligned with node_patterns output.
    let mut union_positions: Vec<usize> = Vec::new();
    let san_bytes = sanitized.as_bytes();
    let mut i = 0;
    while i + 5 <= san_bytes.len() {
        if san_bytes[i..i + 5].eq_ignore_ascii_case(b"union")
            && (i == 0 || !is_ident_byte(san_bytes[i - 1]))
            && (i + 5 == san_bytes.len() || !is_ident_byte(san_bytes[i + 5]))
        {
            union_positions.push(i);
            i += 5;
        } else {
            i += 1;
        }
    }

    let mut bound: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut union_idx = 0;
    for pattern in node_patterns(&sanitized) {
        while union_idx < union_positions.len() && pattern.pos > union_positions[union_idx] {
            bound.clear();
            union_idx += 1;
        }
        if pattern.labels.is_empty() {
            let is_bound_ref = pattern.var.as_ref().is_some_and(|v| bound.contains(v));
            if !is_bound_ref {
                return Err(
                    "unlabeled node patterns are not permitted for this principal — \
                     every node must declare an allowlisted label"
                        .to_string(),
                );
            }
            continue;
        }
        for label in &pattern.labels {
            if !ADHOC_ALLOWED_LABELS.contains(&label.as_str()) {
                return Err(format!(
                    "node label '{label}' is not available to this principal"
                ));
            }
        }
        if let Some(v) = pattern.var {
            bound.insert(v);
        }
    }
    Ok(())
}

/// One parenthesized node pattern found in sanitized Cypher.
struct NodePattern {
    /// Byte offset in the sanitized query (used for UNION scope resets).
    pos: usize,
    /// Variable name when the pattern binds one.
    var: Option<String>,
    /// Labels declared on the pattern (`:`-prefixed idents before `{`).
    labels: Vec<String>,
}

/// Scan sanitized Cypher for parenthesized node patterns.
///
/// Sanitization has already masked string contents and removed comments, so
/// parentheses inside literals cannot evade the scan. Backtick-quoted
/// identifiers are preserved verbatim and parsed as idents.
fn node_patterns(sanitized: &str) -> Vec<NodePattern> {
    let bytes = sanitized.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < len {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i),
            b'(' => {
                // A '(' that is part of a function call (e.g. labels(n),
                // size(x)) has arguments, not node patterns. Detect this by
                // the word preceding the paren: clause keywords (MATCH,
                // WHERE, RETURN, ...) mean a pattern may follow; anything
                // else is a function name. Inner parens are still visited
                // on their own '(' so nested patterns like exists((n)-->())
                // are examined.
                if preceded_by_function_call(bytes, i) {
                    i += 1;
                    continue;
                }
                let close = matching_paren(bytes, i);
                let inner = &sanitized[i + 1..close.saturating_sub(1).min(len)];
                if let Some((var, labels)) = classify_node_pattern(inner) {
                    out.push(NodePattern {
                        pos: i,
                        var,
                        labels,
                    });
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Clause/expression keywords after which a parenthesized node pattern (or
/// pattern-bearing expression) may legally appear. If the word before '('
/// is one of these, the group is classified; otherwise it is treated as a
/// function call and skipped.
const PRECEDING_KEYWORDS: &[&str] = &[
    "ALL",
    "AND",
    "AS",
    "ASC",
    "ASCENDING",
    "BY",
    "CASE",
    "CONTAINS",
    "DESC",
    "DESCENDING",
    "DISTINCT",
    "ELSE",
    "END",
    "ENDS",
    "EXISTS",
    "IN",
    "IS",
    "LIMIT",
    "MATCH",
    "NOT",
    "NULL",
    "OR",
    "ORDER",
    "RETURN",
    "SKIP",
    "STARTS",
    "THEN",
    "TRUE",
    "FALSE",
    "UNION",
    "UNWIND",
    "WHEN",
    "WHERE",
    "WITH",
    "XOR",
];

/// True when the '(' at `pos` is the argument list of a function call —
/// i.e. the word immediately before it is an identifier that is not a
/// Cypher clause keyword.
fn preceded_by_function_call(bytes: &[u8], pos: usize) -> bool {
    // Find the end of the preceding word: the last non-ws byte before pos.
    let mut i = pos;
    while i > 0 {
        i -= 1;
        if !bytes[i].is_ascii_whitespace() {
            break;
        }
        if i == 0 {
            return false;
        }
    }
    if i == 0 && bytes[0].is_ascii_whitespace() {
        return false;
    }
    if !is_ident_byte(bytes[i]) {
        return false;
    }
    // Scan back to the start of the identifier.
    let mut start = i;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let word = &bytes[start..=i];
    let word_upper = word.to_ascii_uppercase();
    let word_str = String::from_utf8_lossy(&word_upper);
    !PRECEDING_KEYWORDS.contains(&word_str.as_ref())
}

/// Skip a quoted section starting at `start` (a quote byte). Returns the
/// index just past the closing quote. Handles backslash escapes and doubled
/// quote escapes. Unterminated quotes consume the rest of input.
fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            if i + 1 < bytes.len() && bytes[i + 1] == quote {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// Index of the `)` matching the `(` at `start`, accounting for nesting and
/// quoted sections. Returns `bytes.len()` when unterminated.
fn matching_paren(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1usize;
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i),
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Parse a paren group's contents as a node pattern head.
///
/// Returns `Some((var, labels))` when the head (the text before the first
/// `{` property map) has the shape `[var] (:Label)*` — including the empty
/// shape `()` / `({props})`, which is an anonymous unlabeled pattern.
/// Returns `None` for anything else (function arguments, expressions),
/// which cannot introduce a node binding.
fn classify_node_pattern(inner: &str) -> Option<(Option<String>, Vec<String>)> {
    let head_end = inner.find('{').unwrap_or(inner.len());
    let head = inner[..head_end].trim();
    let bytes = head.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    let mut var = None;
    if i < len && (is_ident_start(bytes[i]) || bytes[i] == b'`') {
        let (name, next) = parse_ident(head, i);
        var = Some(name);
        i = next;
        i = skip_ws(bytes, i);
    }

    let mut labels = Vec::new();
    while i < len {
        if bytes[i] != b':' {
            return None;
        }
        i += 1;
        i = skip_ws(bytes, i);
        if i >= len || !(is_ident_start(bytes[i]) || bytes[i] == b'`') {
            return None;
        }
        let (label, next) = parse_ident(head, i);
        labels.push(label);
        i = skip_ws(bytes, next);
    }
    Some((var, labels))
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Parse a bare or backtick-quoted identifier starting at `i`.
/// Backtick escapes are doubled backticks (```` `` ````).
fn parse_ident(s: &str, i: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    if bytes[i] == b'`' {
        let mut j = i + 1;
        let mut name = String::new();
        while j < bytes.len() {
            if bytes[j] == b'`' {
                if j + 1 < bytes.len() && bytes[j + 1] == b'`' {
                    name.push('`');
                    j += 2;
                } else {
                    return (name, j + 1);
                }
            } else {
                // Idents are ASCII in practice; collect the raw byte span.
                name.push(bytes[j] as char);
                j += 1;
            }
        }
        return (name, j);
    }
    let start = i;
    let mut j = i;
    while j < bytes.len() && is_ident_byte(bytes[j]) {
        j += 1;
    }
    (s[start..j].to_string(), j)
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
                let last = term.split(['.', '(', ')', ' ']).rfind(|s| {
                    !s.is_empty() && s.chars().next().is_some_and(|c| c.is_alphanumeric())
                });
                last.map(|s| s.to_string())
                    .unwrap_or_else(|| format!("col_{}", i))
            }
        })
        .collect()
}

/// Byte offset in `s` where the RETURN clause ends (ORDER/LIMIT/SKIP/EOF).
fn clause_end(s: &str) -> usize {
    let upper = s.to_uppercase();
    for kw in &[
        " ORDER ", " LIMIT ", " SKIP ", "\nORDER", "\nLIMIT", "\nSKIP",
    ] {
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
pub const MAX_QUERY_OUTPUT_BYTES: usize = 65_536;

// ─── execution ────────────────────────────────────────────────────────────────

/// Number of running diagnostic workers, including cancellation cleanup.
static ACTIVE_DIAGNOSTIC_QUERIES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
pub fn active_diagnostic_queries() -> usize {
    ACTIVE_DIAGNOSTIC_QUERIES.load(std::sync::atomic::Ordering::SeqCst)
}

/// Async diagnostic runner. Dropping the request interrupts its actual engine
/// connection; the blocking worker rolls back before releasing the connection.
pub async fn execute_query_with_deadline(
    db: std::sync::Arc<lbug::Database>,
    cypher: String,
    deadline: std::time::Duration,
) -> Result<ExplorerResult, String> {
    validate_query(&cypher)?;
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    struct CancelOnDrop(std::sync::mpsc::Sender<bool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(true);
        }
    }
    let _cancel = CancelOnDrop(tx.clone());
    tokio::task::spawn_blocking(move || {
        struct Active;
        impl Drop for Active {
            fn drop(&mut self) {
                ACTIVE_DIAGNOSTIC_QUERIES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        ACTIVE_DIAGNOSTIC_QUERIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _active = Active;
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        std::thread::scope(|scope| {
            let conn_ref = &conn;
            scope.spawn(move || {
                if !matches!(rx.recv_timeout(deadline), Ok(false)) {
                    // The engine resets its interrupt flag when entering a statement.
                    // Reissue until completion to cover cancellation during BEGIN/planning.
                    loop {
                        let _ = conn_ref.interrupt();
                        if matches!(
                            rx.recv_timeout(std::time::Duration::from_millis(10)),
                            Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                        ) {
                            break;
                        }
                    }
                }
            });
            let result = execute_query_with_timeout(
                &conn,
                &cypher,
                deadline.as_millis().clamp(1, 5000) as u64,
            )
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(false);
            result
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Validate and execute a read-only Cypher query. Returns column names and rows.
pub fn execute_query(conn: &Connection<'_>, cypher: &str) -> Result<ExplorerResult> {
    execute_query_with_timeout(conn, cypher, 5000)
}

struct ReadOnlyRollback<'connection, 'database>(&'connection Connection<'database>);
impl Drop for ReadOnlyRollback<'_, '_> {
    fn drop(&mut self) {
        let _ = self.0.query("ROLLBACK");
    }
}

fn execute_query_with_timeout(
    conn: &Connection<'_>,
    cypher: &str,
    timeout_ms: u64,
) -> Result<ExplorerResult> {
    validate_query(cypher).map_err(|e| anyhow::anyhow!("{}", e))?;

    let columns = extract_columns(cypher);

    // Enforce database engine-level query execution timeout (5000 ms)
    conn.set_query_timeout(timeout_ms);

    conn.query("BEGIN TRANSACTION READ ONLY")
        .context("begin read-only transaction")?;
    let _transaction = ReadOnlyRollback(conn);

    let query_res = conn.query(cypher);
    let iter = match query_res {
        Ok(it) => it,
        Err(e) => return Err(anyhow::anyhow!("explorer query execution: {e}")),
    };

    // Count encoded bytes, including column names and the result envelope.
    // MAX_ROWS reserves the largest row_count representation this result can use.
    let mut output_bytes = serde_json::to_vec(&ExplorerResult {
        columns: columns.clone(),
        rows: Vec::new(),
        row_count: MAX_ROWS,
        truncated: false,
    })
    .context("encode explorer envelope")?
    .len();
    if output_bytes > MAX_QUERY_OUTPUT_BYTES {
        return Err(anyhow::anyhow!(
            "explorer column metadata exceeds output byte cap"
        ));
    }
    let mut rows = Vec::new();
    let mut truncated = false;
    for row in iter {
        if rows.len() >= MAX_ROWS {
            truncated = true;
            break;
        }
        let json_row: Vec<serde_json::Value> = row.iter().map(value_to_json).collect();
        let row_bytes = serde_json::to_vec(&json_row)
            .context("encode explorer row")?
            .len()
            + usize::from(!rows.is_empty());
        if output_bytes + row_bytes > MAX_QUERY_OUTPUT_BYTES {
            truncated = true;
            break;
        }
        output_bytes += row_bytes;
        rows.push(json_row);
    }

    let row_count = rows.len();
    Ok(ExplorerResult {
        columns,
        rows,
        row_count,
        truncated,
    })
}

// ─── Value → serde_json::Value ───────────────────────────────────────────────

fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::{Value as J, json};
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
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        y, m, d, h, min, s, ns
    )
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
        assert!(
            validate_query(
                "MATCH (d:Device) WHERE d.hostname STARTS WITH 'dataset' RETURN d.address"
            )
            .is_ok()
        );
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
        let cols = extract_columns("MATCH (d:Device) RETURN d.address, d.vendor ORDER BY d.vendor");
        assert_eq!(cols, vec!["address", "vendor"]);
    }

    #[test]
    fn execute_query_runs_against_test_graph() {
        use crate::graph::test_fixtures::TestGraph;
        let g = TestGraph::build();
        let conn = lbug::Connection::new(&g.db).unwrap();
        let result = execute_query(
            &conn,
            "MATCH (d:Device) RETURN d.address ORDER BY d.address LIMIT 3",
        )
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

    #[test]
    fn validate_rejects_comment_evasion() {
        assert!(validate_query("/* harmless comment */ CREATE (n:Device)").is_err());
        assert!(validate_query("// line comment\nDELETE (n)").is_err());
        assert!(validate_query("-- sql style comment\nMERGE (n:Device)").is_err());
    }

    #[test]
    fn validate_rejects_extended_mutation_keywords() {
        assert!(validate_query("ALTER TABLE Device ADD foo STRING").is_err());
        assert!(validate_query("COPY Device FROM 'file.csv'").is_err());
        assert!(validate_query("LOAD FROM 'file.csv'").is_err());
        assert!(validate_query("FOREACH (x in [1,2] | CREATE (n))").is_err());
    }

    #[test]
    fn validate_allows_legitimate_comments() {
        assert!(validate_query("/* read only query */ MATCH (d:Device) RETURN d.address").is_ok());
        assert!(validate_query("// get all devices\nMATCH (d:Device) RETURN d.address").is_ok());
    }

    #[test]
    fn validate_rejects_quoted_comment_mutation_evasion() {
        // Probe 2 exact vector:
        let q = "WITH '/*' AS marker CREATE (n:Device {address: 'bn005-audit-created'}) RETURN n.address";
        assert!(validate_query(q).is_err());

        let q2 = "WITH \"/*\" AS marker DELETE n RETURN n";
        assert!(validate_query(q2).is_err());

        let q3 = "WITH '//' AS marker MERGE (n:Device {address: 'x'}) RETURN n";
        assert!(validate_query(q3).is_err());
    }

    #[test]
    fn validate_allows_mutation_keywords_inside_string_literals() {
        // Legitimate read query with "CREATE" or "DELETE" inside string literal
        assert!(
            validate_query("MATCH (d:Device) WHERE d.vendor = 'CREATE' RETURN d.address").is_ok()
        );
        assert!(
            validate_query("MATCH (d:Device) WHERE d.role = \"DELETE\" RETURN d.address").is_ok()
        );
    }

    #[test]
    fn standard_access_allows_labeled_safe_query() {
        assert!(
            validate_query_for_access(
                "MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) RETURN d.address, i.name",
                GraphReadAccess::Standard,
            )
            .is_ok()
        );
    }

    #[test]
    fn standard_access_rejects_configitem_direct() {
        for q in [
            "MATCH (c:ConfigItem) RETURN c.content_json",
            "MATCH (c:ConfigItem {config_class: 'user'}) RETURN c",
            "MATCH (d:Device) OPTIONAL MATCH (c:ConfigItem) RETURN d, c",
            "MATCH (d:Device) RETURN d UNION MATCH (c:ConfigItem) RETURN c",
            "MATCH (c:`ConfigItem`) RETURN c",
        ] {
            assert!(
                validate_query_for_access(q, GraphReadAccess::Standard).is_err(),
                "expected rejection: {q}"
            );
        }
    }

    #[test]
    fn standard_access_rejects_unlabeled_and_anonymous_patterns() {
        for q in [
            "MATCH (n) RETURN n",
            "MATCH () RETURN 1",
            "MATCH (d:Device)-[:CONNECTED_TO]->(i) RETURN d, i",
            "MATCH (d:Device) WHERE EXISTS { MATCH ()-->() } RETURN d",
        ] {
            assert!(
                validate_query_for_access(q, GraphReadAccess::Standard).is_err(),
                "expected rejection: {q}"
            );
        }
    }

    #[test]
    fn standard_access_allows_bound_variable_references() {
        // (de) inside the EXISTS subquery refers to the already-bound node;
        // it filters rather than scans, so it must remain permitted.
        assert!(
            validate_query_for_access(
                "MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) \
                 WHERE NOT EXISTS { MATCH (r:Remediation)-[:RESOLVES]->(de) } \
                 RETURN d.hostname, de.rule_id",
                GraphReadAccess::Standard,
            )
            .is_ok()
        );
    }

    #[test]
    fn standard_access_union_resets_bound_variables() {
        // The second UNION branch rebinds d with an unlabeled pattern —
        // must be rejected even though d was bound in the first branch.
        assert!(
            validate_query_for_access(
                "MATCH (d:Device) RETURN d.address UNION MATCH (d) RETURN d",
                GraphReadAccess::Standard,
            )
            .is_err()
        );
    }

    #[test]
    fn admin_access_keeps_full_read_scope() {
        assert!(
            validate_query_for_access(
                "MATCH (c:ConfigItem) RETURN c.content_json",
                GraphReadAccess::Admin,
            )
            .is_ok()
        );
        // Mutation safety still applies to admins.
        assert!(
            validate_query_for_access("MATCH (c:ConfigItem) DELETE c", GraphReadAccess::Admin)
                .is_err()
        );
    }

    #[test]
    fn standard_access_function_calls_are_not_patterns() {
        assert!(
            validate_query_for_access(
                "MATCH (d:Device) RETURN labels(d), size(d.address), count(d)",
                GraphReadAccess::Standard,
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_rejects_transaction_control_and_multistatement() {
        assert!(validate_query("BEGIN TRANSACTION").is_err());
        assert!(validate_query("COMMIT").is_err());
        assert!(validate_query("ROLLBACK").is_err());
        assert!(validate_query("MATCH (d:Device) RETURN d; MATCH (d:Device) RETURN d").is_err());
        // Trailing semicolon on a single query is fine
        assert!(validate_query("MATCH (d:Device) RETURN d.address;").is_ok());
        // Semicolon inside string literal is fine
        assert!(validate_query("MATCH (d:Device) WHERE d.name = 'foo;bar' RETURN d").is_ok());
    }
}
