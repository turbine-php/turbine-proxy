//! Query intent classification for read/write splitting.
//! This is intentionally simple — we only need to classify the top-level intent,
//! not parse the full SQL.
//!
//! `QueryClassifier` is a trait so future PostgreSQL support can plug in a
//! different classifier without touching the routing logic. Use concrete types
//! (not `dyn QueryClassifier`) on the hot path to get monomorphization.

/// The intent of a SQL query for routing purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    /// SELECT queries — can go to replicas.
    Read,
    /// INSERT, UPDATE, DELETE, DDL — must go to primary.
    Write,
    /// BEGIN, COMMIT, ROLLBACK, SAVEPOINT — transaction control.
    Transaction,
    /// SET, SHOW, USE, CALL — route to primary for safety.
    Other,
}

// ─── QueryClassifier trait ────────────────────────────────────────────────────

/// Protocol-agnostic query classifier.
///
/// Use with generics on the hot path (`fn route<C: QueryClassifier>`) to get
/// zero-cost monomorphization. Never use `&dyn QueryClassifier` in a loop.
#[allow(dead_code)]
pub trait QueryClassifier: Send + Sync {
    fn classify(&self, sql: &str) -> QueryIntent;

    /// Whether the query is guaranteed read-only (never modifies data).
    fn is_read_only(&self, sql: &str) -> bool {
        matches!(self.classify(sql), QueryIntent::Read)
    }

    /// Extract table names touched by the query (best-effort, for cache invalidation).
    fn extract_tables(&self, sql: &str) -> Vec<String>;
}

// ─── MySQLClassifier ──────────────────────────────────────────────────────────

/// MySQL query classifier. Zero-size type — all logic is stateless.
#[allow(dead_code)]
pub struct MySQLClassifier;

impl QueryClassifier for MySQLClassifier {
    fn classify(&self, sql: &str) -> QueryIntent {
        classify(sql)
    }

    fn extract_tables(&self, sql: &str) -> Vec<String> {
        extract_tables_simple(sql)
    }
}

// ─── Standalone classify (kept for tests and direct use) ─────────────────────

/// Classify a SQL query string into a routing intent.
/// Uses simple prefix matching — no full SQL parsing needed.
pub fn classify(sql: &str) -> QueryIntent {
    let trimmed = sql.trim_start();
    let effective = skip_leading_comments(trimmed);
    let upper: String = effective.chars().take(20).collect::<String>().to_uppercase();

    if upper.starts_with("SELECT") || upper.starts_with("(SELECT") {
        let sql_upper = sql.to_uppercase();
        if sql_upper.contains("FOR UPDATE") || sql_upper.contains("FOR SHARE") {
            return QueryIntent::Write;
        }
        QueryIntent::Read
    } else if upper.starts_with("INSERT")
        || upper.starts_with("UPDATE")
        || upper.starts_with("DELETE")
        || upper.starts_with("REPLACE")
        || upper.starts_with("CREATE")
        || upper.starts_with("ALTER")
        || upper.starts_with("DROP")
        || upper.starts_with("TRUNCATE")
        || upper.starts_with("RENAME")
        || upper.starts_with("LOAD")
        || upper.starts_with("GRANT")
        || upper.starts_with("REVOKE")
    {
        QueryIntent::Write
    } else if upper.starts_with("BEGIN")
        || upper.starts_with("START TRANSACTION")
        || upper.starts_with("COMMIT")
        || upper.starts_with("ROLLBACK")
        || upper.starts_with("SAVEPOINT")
        || upper.starts_with("RELEASE SAVEPOINT")
        || upper.starts_with("XA ")
    {
        QueryIntent::Transaction
    } else {
        QueryIntent::Other
    }
}

/// Best-effort extraction of table names from a SQL statement.
/// Used for query cache invalidation — false negatives are acceptable,
/// false positives cause unnecessary cache invalidations (safe but suboptimal).
#[allow(dead_code)]
pub fn extract_tables_simple(sql: &str) -> Vec<String> {
    let upper = sql.to_uppercase();
    let mut tables = Vec::new();

    // Look for FROM <table> and JOIN <table> patterns
    for keyword in &["FROM ", "JOIN ", "INTO ", "UPDATE ", "TABLE "] {
        let mut search = upper.as_str();
        while let Some(pos) = search.find(keyword) {
            let after = search[pos + keyword.len()..].trim_start();
            // Extract identifier (stop at space, comma, paren, semicolon)
            let end = after
                .find(|c: char| c == ' ' || c == ',' || c == '(' || c == ';' || c == '\n')
                .unwrap_or(after.len());
            let table = after[..end]
                .trim_matches('`')
                .trim_matches('"')
                .to_lowercase();
            if !table.is_empty() && !table.contains('.') || table.contains('.') {
                // Strip schema prefix if present (schema.table → table)
                let name = table.split('.').last().unwrap_or(&table).to_string();
                if !name.is_empty() {
                    tables.push(name);
                }
            }
            search = &search[pos + keyword.len()..];
        }
    }

    tables.sort();
    tables.dedup();
    tables
}

/// Skip leading SQL comments (block /* */ and line -- comments).
fn skip_leading_comments(s: &str) -> &str {
    let mut remaining = s;
    loop {
        remaining = remaining.trim_start();
        if remaining.starts_with("/*") {
            if let Some(end) = remaining.find("*/") {
                remaining = &remaining[end + 2..];
                continue;
            }
        }
        if remaining.starts_with("--") {
            if let Some(end) = remaining.find('\n') {
                remaining = &remaining[end + 1..];
                continue;
            }
        }
        break;
    }
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_select() {
        assert_eq!(classify("SELECT * FROM users"), QueryIntent::Read);
        assert_eq!(classify("  select id from t"), QueryIntent::Read);
    }

    #[test]
    fn test_classify_select_for_update() {
        assert_eq!(
            classify("SELECT * FROM users FOR UPDATE"),
            QueryIntent::Write
        );
    }

    #[test]
    fn test_classify_write() {
        assert_eq!(classify("INSERT INTO t VALUES (1)"), QueryIntent::Write);
        assert_eq!(classify("UPDATE t SET x=1"), QueryIntent::Write);
        assert_eq!(classify("DELETE FROM t"), QueryIntent::Write);
        assert_eq!(classify("CREATE TABLE t (id INT)"), QueryIntent::Write);
    }

    #[test]
    fn test_classify_transaction() {
        assert_eq!(classify("BEGIN"), QueryIntent::Transaction);
        assert_eq!(classify("COMMIT"), QueryIntent::Transaction);
        assert_eq!(classify("ROLLBACK"), QueryIntent::Transaction);
        assert_eq!(classify("START TRANSACTION"), QueryIntent::Transaction);
    }

    #[test]
    fn test_classify_with_comment() {
        assert_eq!(
            classify("/* hint */ SELECT * FROM t"),
            QueryIntent::Read
        );
    }
}

