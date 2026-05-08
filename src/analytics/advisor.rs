//! Index Advisor — analyses EXPLAIN output and suggests DDL CREATE INDEX statements.
//!
//! Flow:
//!   1. `Collector` detects a slow query and sends it to `AdvisorTask` via channel.
//!   2. `AdvisorTask` runs `EXPLAIN FORMAT=JSON <sql>` on the primary backend.
//!   3. `MySQLExplainAnalyzer::analyze()` parses the JSON and extracts signals.
//!   4. `MySQLExplainAnalyzer::suggest_index()` generates a DDL statement.
//!   5. Suggestions are stored in-memory and exposed via the dashboard API.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use crate::config::BackendConfig;
use crate::protocol::{BackendConnection, DatabaseProtocol};

// ─── Public types ─────────────────────────────────────────────────────────────

/// The table access type reported by EXPLAIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessType {
    /// Full table scan — highest priority for indexing.
    All,
    Index,
    Range,
    Ref,
    EqRef,
    Const,
    Other,
}

impl AccessType {
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "all" => Self::All,
            "index" => Self::Index,
            "range" => Self::Range,
            "ref" => Self::Ref,
            "eq_ref" => Self::EqRef,
            "const" | "system" => Self::Const,
            _ => Self::Other,
        }
    }

    fn is_problematic(&self) -> bool {
        matches!(self, Self::All | Self::Index)
    }
}

/// Signals extracted from a single table node in an EXPLAIN response.
#[derive(Debug, Clone)]
pub struct ExplainSignals {
    pub table: String,
    pub access_type: AccessType,
    pub rows_examined: u64,
    /// Percentage of rows expected to survive the WHERE filter (0–100).
    pub filtered_pct: f32,
    pub has_filesort: bool,
    pub has_temp_table: bool,
    pub used_index: Option<String>,
    pub possible_indexes: Vec<String>,
    /// Columns referenced in the WHERE clause (extracted from EXPLAIN JSON).
    pub where_columns: Vec<String>,
}

/// A suggested index with a ready-to-apply DDL statement.
#[derive(Debug, Clone)]
pub struct IndexSuggestion {
    pub table: String,
    pub columns: Vec<String>,
    pub reason: String,
    pub ddl: String,
}

// ─── ExplainAnalyzer trait ────────────────────────────────────────────────────

/// Protocol/database-specific EXPLAIN analysis.
pub trait ExplainAnalyzer: Send + Sync {
    fn explain_sql(&self, query: &str) -> String;
    fn analyze(&self, explain_output: &str) -> Vec<ExplainSignals>;
    fn suggest_index(&self, signals: &ExplainSignals) -> Option<IndexSuggestion>;
}

// ─── MySQLExplainAnalyzer ─────────────────────────────────────────────────────

pub struct MySQLExplainAnalyzer;

impl ExplainAnalyzer for MySQLExplainAnalyzer {
    fn explain_sql(&self, query: &str) -> String {
        format!("EXPLAIN FORMAT=JSON {}", query)
    }

    fn analyze(&self, explain_output: &str) -> Vec<ExplainSignals> {
        parse_explain_json(explain_output)
    }

    fn suggest_index(&self, signals: &ExplainSignals) -> Option<IndexSuggestion> {
        build_suggestion(signals)
    }
}

// ─── JSON text parser ─────────────────────────────────────────────────────────

fn parse_explain_json(json: &str) -> Vec<ExplainSignals> {
    let mut results = Vec::new();
    let mut remaining = json;
    while let Some(pos) = remaining.find("\"table_name\"") {
        remaining = &remaining[pos..];
        let table = extract_str_value(remaining, "table_name").unwrap_or_default();
        let access_type = AccessType::from_str(
            &extract_str_value(remaining, "access_type").unwrap_or_default(),
        );
        let rows_examined = extract_u64_value(remaining, "rows_examined_per_scan").unwrap_or(0);
        let filtered_pct = extract_f32_value(remaining, "filtered").unwrap_or(100.0);
        let has_filesort = extract_bool_value(remaining, "using_filesort");
        let has_temp_table = extract_bool_value(remaining, "using_temporary_table");
        let used_index = extract_str_value(remaining, "key");
        let possible_indexes = extract_array_strings(remaining, "possible_keys");
        let where_columns = extract_where_columns(remaining);

        results.push(ExplainSignals {
            table,
            access_type,
            rows_examined,
            filtered_pct,
            has_filesort,
            has_temp_table,
            used_index,
            possible_indexes,
            where_columns,
        });
        remaining = &remaining[1..];
    }
    results
}

fn extract_str_value(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let after = json[pos + needle.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    if after.starts_with('"') {
        let inner = &after[1..];
        let end = inner.find('"')?;
        let val = inner[..end].to_string();
        if val == "null" || val.is_empty() { None } else { Some(val) }
    } else {
        None
    }
}

fn extract_u64_value(json: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let after = json[pos + needle.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let end = after.find(|c: char| !c.is_ascii_digit())?;
    after[..end].parse().ok()
}

fn extract_f32_value(json: &str, key: &str) -> Option<f32> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let after = json[pos + needle.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let after = after.trim_matches('"');
    let end = after.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')?;
    after[..end].parse().ok()
}

fn extract_bool_value(json: &str, key: &str) -> bool {
    let needle = format!("\"{}\"", key);
    if let Some(pos) = json.find(&needle) {
        let after = json[pos + needle.len()..].trim_start();
        if let Some(after) = after.strip_prefix(':') {
            return after.trim_start().starts_with("true");
        }
    }
    false
}

fn extract_array_strings(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{}\"", key);
    let pos = match json.find(&needle) {
        Some(p) => p,
        None => return vec![],
    };
    let after = json[pos + needle.len()..].trim_start();
    let after = match after.strip_prefix(':') {
        Some(a) => a.trim_start(),
        None => return vec![],
    };
    if !after.starts_with('[') {
        return vec![];
    }
    let end = match after.find(']') {
        Some(e) => e,
        None => return vec![],
    };
    let content = &after[1..end];
    content
        .split(',')
        .filter_map(|s| {
            let trimmed = s.trim().trim_matches('"');
            if trimmed.is_empty() || trimmed == "null" { None } else { Some(trimmed.to_string()) }
        })
        .collect()
}

/// Extract column names from `attached_condition` JSON field.
/// Handles qualified names: `` `schema`.`table`.`column` `` → ["column"]
fn extract_where_columns(json: &str) -> Vec<String> {
    let condition = match extract_str_value(json, "attached_condition") {
        Some(c) => c,
        None => return vec![],
    };
    let mut cols = Vec::new();
    let mut s = condition.as_str();
    while let Some(dot) = s.find("`.`") {
        let after_dot = &s[dot + 3..];
        let end = after_dot.find('`').unwrap_or(after_dot.len());
        let col = after_dot[..end].to_string();
        // Skip mid-chain elements (e.g. `schema`.`table`.`col` — "table" is followed by `.`)
        let rest = &after_dot[end..];
        let is_mid_chain = rest.starts_with("`.`");
        if !is_mid_chain && !col.is_empty() && !cols.contains(&col) {
            cols.push(col);
        }
        s = &s[dot + 3..];
    }
    cols
}

// ─── Suggestion builder ───────────────────────────────────────────────────────

fn build_suggestion(s: &ExplainSignals) -> Option<IndexSuggestion> {
    if s.table.is_empty() {
        return None;
    }
    let cols = if !s.where_columns.is_empty() {
        s.where_columns.clone()
    } else {
        return None;
    };

    let reason = build_reason(s);
    let idx_name = format!("idx_{}_{}", s.table, cols.join("_"));
    let col_list = cols.join(", ");
    let ddl = format!("CREATE INDEX {} ON {} ({});", idx_name, s.table, col_list);

    Some(IndexSuggestion {
        table: s.table.clone(),
        columns: cols,
        reason,
        ddl,
    })
}

fn build_reason(s: &ExplainSignals) -> String {
    let mut parts = Vec::new();
    if s.access_type.is_problematic() {
        parts.push(format!(
            "full {} scan ({} rows)",
            if matches!(s.access_type, AccessType::All) { "table" } else { "index" },
            s.rows_examined
        ));
    }
    if s.has_filesort {
        parts.push("filesort (ORDER BY without index)".to_string());
    }
    if s.has_temp_table {
        parts.push("temporary table (GROUP BY/DISTINCT without index)".to_string());
    }
    if s.filtered_pct < 10.0 {
        parts.push(format!("low selectivity ({:.1}% rows pass filter)", s.filtered_pct));
    }
    if parts.is_empty() {
        "query performance may benefit from an index".to_string()
    } else {
        parts.join("; ")
    }
}

// ─── AdvisorTask ──────────────────────────────────────────────────────────────

/// Work item sent from `Collector` to `AdvisorTask`.
pub struct ExplainRequest {
    pub sql: String,
    pub fingerprint: String,
}

/// Background task that runs EXPLAIN on slow queries and accumulates suggestions.
///
/// Uses a bounded channel — requests are silently dropped when busy, which is
/// correct: index suggestions are best-effort and must never affect query latency.
pub struct AdvisorTask {
    sender: mpsc::Sender<ExplainRequest>,
    suggestions: Arc<Mutex<HashMap<String, IndexSuggestion>>>,
}

impl AdvisorTask {
    const CHANNEL_CAP: usize = 64;

    pub fn new(protocol: Arc<dyn DatabaseProtocol>, primary_config: BackendConfig) -> Self {
        let (tx, rx) = mpsc::channel(Self::CHANNEL_CAP);
        let suggestions = Arc::new(Mutex::new(HashMap::new()));
        let suggestions_bg = suggestions.clone();
        tokio::spawn(advisor_loop(rx, protocol, primary_config, suggestions_bg));
        Self { sender: tx, suggestions }
    }

    /// Submit a slow query for background EXPLAIN analysis. Never blocks.
    pub fn try_submit(&self, sql: String, fingerprint: String) {
        let _ = self.sender.try_send(ExplainRequest { sql, fingerprint });
    }

    /// Return a snapshot of all accumulated index suggestions.
    pub async fn get_suggestions(&self) -> Vec<IndexSuggestion> {
        self.suggestions.lock().await.values().cloned().collect()
    }
}

async fn advisor_loop(
    mut rx: mpsc::Receiver<ExplainRequest>,
    protocol: Arc<dyn DatabaseProtocol>,
    config: BackendConfig,
    suggestions: Arc<Mutex<HashMap<String, IndexSuggestion>>>,
) {
    let analyzer = MySQLExplainAnalyzer;
    while let Some(req) = rx.recv().await {
        let mut conn: Box<dyn BackendConnection> = match protocol.connect_backend(&config).await {
            Ok(c) => c,
            Err(e) => {
                log::debug!("AdvisorTask: backend connect failed: {}", e);
                continue;
            }
        };

        let explain_sql = analyzer.explain_sql(&req.sql);
        let response = match conn.execute_query(explain_sql.as_bytes()).await {
            Ok(r) => r,
            Err(e) => {
                log::debug!("AdvisorTask: EXPLAIN failed: {}", e);
                continue;
            }
        };

        let explain_text = extract_explain_text(&response.bytes);
        let signals_list = analyzer.analyze(&explain_text);

        let mut map = suggestions.lock().await;
        for signals in &signals_list {
            if let Some(suggestion) = analyzer.suggest_index(signals) {
                map.insert(req.fingerprint.clone(), suggestion);
            }
        }
    }
}

/// Extract the JSON payload from a MySQL result set byte stream.
/// Scans for the outermost `{...}` block.
fn extract_explain_text(bytes: &[u8]) -> String {
    if let Some(start) = bytes.iter().position(|&b| b == b'{') {
        if let Some(end) = bytes.iter().rposition(|&b| b == b'}') {
            if end >= start {
                return String::from_utf8_lossy(&bytes[start..=end]).into_owned();
            }
        }
    }
    String::new()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EXPLAIN_FULL_SCAN: &str = r#"{
      "query_block": {
        "select_id": 1,
        "table": {
          "table_name": "orders",
          "access_type": "ALL",
          "rows_examined_per_scan": 15000,
          "filtered": "10.00",
          "using_filesort": true,
          "attached_condition": "(`shop`.`orders`.`status` = 'pending')"
        }
      }
    }"#;

    const EXPLAIN_INDEX_RANGE: &str = r#"{
      "query_block": {
        "table": {
          "table_name": "users",
          "access_type": "range",
          "rows_examined_per_scan": 42,
          "filtered": "100.00",
          "key": "idx_users_email",
          "possible_keys": ["idx_users_email"]
        }
      }
    }"#;

    #[test]
    fn test_parse_full_scan() {
        let analyzer = MySQLExplainAnalyzer;
        let signals = analyzer.analyze(EXPLAIN_FULL_SCAN);
        assert_eq!(signals.len(), 1);
        let s = &signals[0];
        assert_eq!(s.table, "orders");
        assert_eq!(s.access_type, AccessType::All);
        assert_eq!(s.rows_examined, 15000);
        assert!(s.has_filesort);
        assert_eq!(s.filtered_pct, 10.0);
        assert_eq!(s.where_columns, vec!["status"]);
    }

    #[test]
    fn test_parse_range_scan() {
        let analyzer = MySQLExplainAnalyzer;
        let signals = analyzer.analyze(EXPLAIN_INDEX_RANGE);
        assert_eq!(signals.len(), 1);
        let s = &signals[0];
        assert_eq!(s.access_type, AccessType::Range);
        assert_eq!(s.used_index.as_deref(), Some("idx_users_email"));
    }

    #[test]
    fn test_suggest_index_full_scan() {
        let analyzer = MySQLExplainAnalyzer;
        let signals = analyzer.analyze(EXPLAIN_FULL_SCAN);
        let suggestion = analyzer.suggest_index(&signals[0]).unwrap();
        assert_eq!(suggestion.table, "orders");
        assert_eq!(suggestion.columns, vec!["status"]);
        assert!(suggestion.ddl.contains("CREATE INDEX"));
        assert!(suggestion.ddl.contains("orders"));
        assert!(suggestion.ddl.contains("status"));
    }

    #[test]
    fn test_no_suggestion_for_range() {
        let analyzer = MySQLExplainAnalyzer;
        let signals = analyzer.analyze(EXPLAIN_INDEX_RANGE);
        // Range scan on an existing index — no new suggestion needed.
        assert!(analyzer.suggest_index(&signals[0]).is_none());
    }

    #[test]
    fn test_access_type_from_str() {
        assert_eq!(AccessType::from_str("ALL"), AccessType::All);
        assert_eq!(AccessType::from_str("range"), AccessType::Range);
        assert_eq!(AccessType::from_str("eq_ref"), AccessType::EqRef);
        assert_eq!(AccessType::from_str("const"), AccessType::Const);
        assert_eq!(AccessType::from_str("unknown"), AccessType::Other);
    }
}
