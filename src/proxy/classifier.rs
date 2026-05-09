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

// ─── ASCII lookup table ───────────────────────────────────────────────────────

/// Maps every byte to its ASCII uppercase equivalent.
/// Non-alphabetic bytes are unchanged. Built at compile time, zero runtime cost.
const UPPER: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        t[i] = if i >= b'a' as usize && i <= b'z' as usize {
            (i - 32) as u8
        } else {
            i as u8
        };
        i += 1;
    }
    t
};

/// Case-insensitive ASCII prefix match using the lookup table.
/// `kw` must be all-uppercase ASCII bytes.
#[inline(always)]
fn starts_with_kw(bytes: &[u8], kw: &[u8]) -> bool {
    bytes.len() >= kw.len()
        && bytes[..kw.len()]
            .iter()
            .zip(kw)
            .all(|(&b, &k)| UPPER[b as usize] == k)
}

/// Case-insensitive substring scan using the lookup table.
/// `pattern` must be all-uppercase ASCII bytes.
/// Runs in O(n·m) but `m` is always ≤ 10 bytes for SQL keywords.
#[inline]
fn contains_kw(bytes: &[u8], pattern: &[u8]) -> bool {
    let plen = pattern.len();
    if bytes.len() < plen {
        return false;
    }
    bytes
        .windows(plen)
        .any(|w| w.iter().zip(pattern).all(|(&b, &k)| UPPER[b as usize] == k))
}

// ─── Standalone classify (kept for tests and direct use) ─────────────────────

/// Classify a SQL query string into a routing intent.
/// Uses simple prefix matching — no full SQL parsing needed.
///
/// # Performance
/// All comparisons use a compile-time 256-byte ASCII uppercase lookup table.
/// No heap allocation, no `to_uppercase()` copy — safe to call on every query.
pub fn classify(sql: &str) -> QueryIntent {
    let trimmed = sql.trim_start();
    let effective = skip_leading_comments(trimmed);
    let bytes = effective.as_bytes();

    if starts_with_kw(bytes, b"SELECT") || starts_with_kw(bytes, b"(SELECT") {
        let sql_bytes = sql.as_bytes();
        if contains_kw(sql_bytes, b"FOR UPDATE") || contains_kw(sql_bytes, b"FOR SHARE") {
            return QueryIntent::Write;
        }
        QueryIntent::Read
    } else if starts_with_kw(bytes, b"INSERT")
        || starts_with_kw(bytes, b"UPDATE")
        || starts_with_kw(bytes, b"DELETE")
        || starts_with_kw(bytes, b"REPLACE")
        || starts_with_kw(bytes, b"CREATE")
        || starts_with_kw(bytes, b"ALTER")
        || starts_with_kw(bytes, b"DROP")
        || starts_with_kw(bytes, b"TRUNCATE")
        || starts_with_kw(bytes, b"RENAME")
        || starts_with_kw(bytes, b"LOAD")
        || starts_with_kw(bytes, b"GRANT")
        || starts_with_kw(bytes, b"REVOKE")
    {
        QueryIntent::Write
    } else if starts_with_kw(bytes, b"BEGIN")
        || starts_with_kw(bytes, b"START TRANSACTION")
        || starts_with_kw(bytes, b"COMMIT")
        || starts_with_kw(bytes, b"ROLLBACK")
        || starts_with_kw(bytes, b"SAVEPOINT")
        || starts_with_kw(bytes, b"RELEASE SAVEPOINT")
        || starts_with_kw(bytes, b"XA ")
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
                .find([' ', ',', '(', ';', '\n'])
                .unwrap_or(after.len());
            let table = after[..end]
                .trim_matches('`')
                .trim_matches('"')
                .to_lowercase();
            if !table.is_empty() && !table.contains('.') || table.contains('.') {
                // Strip schema prefix if present (schema.table → table)
                let name = table.split('.').next_back().unwrap_or(&table).to_string();
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

// ─── Sticky backend hint ──────────────────────────────────────────────────────

/// Extract the `sticky_backend` routing hint from SQL block comments.
///
/// The hint can appear anywhere inside a `/* … */` comment:
///
/// ```text
/// /* sticky_backend=1 */ SELECT …   →  Some(true)  enable stickiness
/// /* sticky_backend=0 */ SELECT …   →  Some(false) disable stickiness
/// SELECT …                          →  None         no change
/// ```
///
/// Matching is case-insensitive; surrounding whitespace inside the comment
/// value is ignored.  The first matching comment wins.
pub fn extract_sticky_hint(sql: &str) -> Option<bool> {
    const KEY: &str = "sticky_backend=";
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let comment_start = i + 2;
            if let Some(end_offset) = sql[comment_start..].find("*/") {
                let comment = &sql[comment_start..comment_start + end_offset];
                let comment_up = comment.to_ascii_uppercase();
                let key_up = KEY.to_ascii_uppercase();
                if let Some(pos) = comment_up.find(key_up.as_str()) {
                    let after = comment[pos + KEY.len()..].trim_start();
                    if after.starts_with('1') {
                        return Some(true);
                    }
                    if after.starts_with('0') {
                        return Some(false);
                    }
                }
                i = comment_start + end_offset + 2;
                continue;
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SELECT / Read ──────────────────────────────────────────────────────────

    #[test]
    fn test_classify_select() {
        assert_eq!(classify("SELECT * FROM users"), QueryIntent::Read);
        assert_eq!(classify("  select id from t"), QueryIntent::Read);
        assert_eq!(classify("\n\tSELECT 1"), QueryIntent::Read);
    }

    #[test]
    fn test_classify_subquery_select() {
        assert_eq!(classify("(SELECT * FROM orders)"), QueryIntent::Read);
        assert_eq!(classify("(select count(*) from t)"), QueryIntent::Read);
    }

    #[test]
    fn test_classify_select_for_update() {
        assert_eq!(
            classify("SELECT * FROM users FOR UPDATE"),
            QueryIntent::Write
        );
        assert_eq!(classify("SELECT id FROM t FOR SHARE"), QueryIntent::Write);
    }

    #[test]
    fn test_classify_explain() {
        // EXPLAIN is not explicitly mapped → falls to Other (safe: routed to primary)
        assert_eq!(classify("EXPLAIN SELECT * FROM t"), QueryIntent::Other);
    }

    // ── Writes ────────────────────────────────────────────────────────────────

    #[test]
    fn test_classify_insert() {
        assert_eq!(classify("INSERT INTO t VALUES (1)"), QueryIntent::Write);
        assert_eq!(
            classify("insert into logs (msg) values ('x')"),
            QueryIntent::Write
        );
    }

    #[test]
    fn test_classify_update() {
        assert_eq!(classify("UPDATE t SET x=1"), QueryIntent::Write);
        assert_eq!(
            classify("update users set active=0 where id=1"),
            QueryIntent::Write
        );
    }

    #[test]
    fn test_classify_delete() {
        assert_eq!(classify("DELETE FROM t"), QueryIntent::Write);
        assert_eq!(
            classify("delete from sessions where expired=1"),
            QueryIntent::Write
        );
    }

    #[test]
    fn test_classify_replace() {
        assert_eq!(
            classify("REPLACE INTO t VALUES (1, 'x')"),
            QueryIntent::Write
        );
    }

    #[test]
    fn test_classify_ddl() {
        assert_eq!(classify("CREATE TABLE t (id INT)"), QueryIntent::Write);
        assert_eq!(
            classify("ALTER TABLE t ADD COLUMN x INT"),
            QueryIntent::Write
        );
        assert_eq!(classify("DROP TABLE t"), QueryIntent::Write);
        assert_eq!(classify("TRUNCATE TABLE t"), QueryIntent::Write);
        assert_eq!(classify("RENAME TABLE old TO new"), QueryIntent::Write);
    }

    #[test]
    fn test_classify_grant_revoke() {
        assert_eq!(
            classify("GRANT SELECT ON db.* TO 'u'@'%'"),
            QueryIntent::Write
        );
        assert_eq!(
            classify("REVOKE ALL ON *.* FROM 'u'@'%'"),
            QueryIntent::Write
        );
    }

    // ── Transaction control ────────────────────────────────────────────────────

    #[test]
    fn test_classify_transaction() {
        assert_eq!(classify("BEGIN"), QueryIntent::Transaction);
        assert_eq!(classify("begin"), QueryIntent::Transaction);
        assert_eq!(classify("COMMIT"), QueryIntent::Transaction);
        assert_eq!(classify("ROLLBACK"), QueryIntent::Transaction);
        assert_eq!(classify("START TRANSACTION"), QueryIntent::Transaction);
        assert_eq!(classify("SAVEPOINT sp1"), QueryIntent::Transaction);
        assert_eq!(classify("RELEASE SAVEPOINT sp1"), QueryIntent::Transaction);
        assert_eq!(classify("XA START 'xid'"), QueryIntent::Transaction);
    }

    // ── Other ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_classify_other() {
        assert_eq!(classify("SET NAMES utf8mb4"), QueryIntent::Other);
        assert_eq!(classify("SHOW TABLES"), QueryIntent::Other);
        assert_eq!(classify("USE mydb"), QueryIntent::Other);
        assert_eq!(classify("CALL stored_proc()"), QueryIntent::Other);
    }

    // ── Comment stripping ─────────────────────────────────────────────────────

    #[test]
    fn test_classify_block_comment() {
        assert_eq!(classify("/* hint */ SELECT * FROM t"), QueryIntent::Read);
        assert_eq!(
            classify("/* a */ /* b */ INSERT INTO t VALUES (1)"),
            QueryIntent::Write
        );
    }

    #[test]
    fn test_classify_line_comment() {
        assert_eq!(
            classify("-- route:replica\nSELECT * FROM t"),
            QueryIntent::Read
        );
        assert_eq!(classify("-- comment\nUPDATE t SET x=1"), QueryIntent::Write);
    }

    // ── is_read_only helper ───────────────────────────────────────────────────

    #[test]
    fn test_is_read_only() {
        let c = MySQLClassifier;
        assert!(c.is_read_only("SELECT 1"));
        assert!(!c.is_read_only("INSERT INTO t VALUES (1)"));
        assert!(!c.is_read_only("BEGIN"));
    }

    // ── extract_tables ────────────────────────────────────────────────────────

    #[test]
    fn test_extract_tables_select() {
        let tables = extract_tables_simple("SELECT * FROM users WHERE id = 1");
        assert!(tables.contains(&"users".to_string()));
    }

    #[test]
    fn test_extract_tables_join() {
        let tables =
            extract_tables_simple("SELECT * FROM orders JOIN users ON orders.user_id = users.id");
        assert!(tables.contains(&"orders".to_string()));
        assert!(tables.contains(&"users".to_string()));
    }

    #[test]
    fn test_extract_tables_insert() {
        let tables = extract_tables_simple("INSERT INTO logs (msg) VALUES ('x')");
        assert!(tables.contains(&"logs".to_string()));
    }

    #[test]
    fn test_extract_tables_dedup() {
        let tables = extract_tables_simple("SELECT * FROM t JOIN t ON t.id = t.id");
        assert_eq!(
            tables.iter().filter(|&x| x == "t").count(),
            1,
            "duplicates should be removed"
        );
    }

    // ── extract_sticky_hint ───────────────────────────────────────────────────

    #[test]
    fn test_extract_sticky_hint_enable() {
        assert_eq!(
            extract_sticky_hint("/* sticky_backend=1 */ SELECT 1"),
            Some(true)
        );
    }

    #[test]
    fn test_extract_sticky_hint_disable() {
        assert_eq!(
            extract_sticky_hint("/* sticky_backend=0 */ SELECT 1"),
            Some(false)
        );
    }

    #[test]
    fn test_extract_sticky_hint_absent() {
        assert_eq!(extract_sticky_hint("SELECT 1"), None);
        assert_eq!(extract_sticky_hint("/* no_hint */ SELECT 1"), None);
    }

    #[test]
    fn test_extract_sticky_hint_case_insensitive() {
        assert_eq!(
            extract_sticky_hint("/* STICKY_BACKEND=1 */ SELECT 1"),
            Some(true)
        );
        assert_eq!(
            extract_sticky_hint("/* Sticky_Backend=0 */ SELECT 1"),
            Some(false)
        );
    }

    #[test]
    fn test_extract_sticky_hint_multiple_comments() {
        // First matching comment wins.
        assert_eq!(
            extract_sticky_hint("/* sticky_backend=1 */ /* other */ SELECT 1"),
            Some(true)
        );
        // Non-matching comment before matching one.
        assert_eq!(
            extract_sticky_hint("/* unrelated */ /* sticky_backend=0 */ SELECT 1"),
            Some(false)
        );
    }

    #[test]
    fn test_extract_sticky_hint_no_value() {
        // Key present but no valid digit after '='.
        assert_eq!(extract_sticky_hint("/* sticky_backend= */ SELECT 1"), None);
        assert_eq!(extract_sticky_hint("/* sticky_backend=x */ SELECT 1"), None);
    }

    // ── lookup-table helpers ──────────────────────────────────────────────────

    #[test]
    fn test_starts_with_kw_case_insensitive() {
        assert!(starts_with_kw(b"select 1", b"SELECT"));
        assert!(starts_with_kw(b"SELECT 1", b"SELECT"));
        assert!(starts_with_kw(b"SeLeCt 1", b"SELECT"));
        assert!(!starts_with_kw(b"INSERT", b"SELECT"));
        assert!(!starts_with_kw(b"SEL", b"SELECT")); // too short
    }

    #[test]
    fn test_contains_kw_case_insensitive() {
        assert!(contains_kw(b"SELECT * FROM t FOR UPDATE", b"FOR UPDATE"));
        assert!(contains_kw(b"select * from t for update", b"FOR UPDATE"));
        assert!(contains_kw(b"select * from t FOR SHARE", b"FOR SHARE"));
        assert!(!contains_kw(b"SELECT 1", b"FOR UPDATE"));
        assert!(!contains_kw(b"FOR", b"FOR UPDATE")); // too short
    }

    #[test]
    fn test_upper_lut_covers_all_ascii() {
        for b in 0u8..=127 {
            let expected = (b as char).to_ascii_uppercase() as u8;
            assert_eq!(UPPER[b as usize], expected, "UPPER[{}] mismatch", b);
        }
    }
}
