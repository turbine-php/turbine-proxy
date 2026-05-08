//! In-memory query result cache for SELECT queries.
//!
//! Design:
//! - Keyed on the normalised SQL string (exact bytes, not fingerprint — two
//!   queries with different literals are different cache entries).
//! - LRU eviction with a configurable max-entry cap.
//! - TTL per entry — stale entries are evicted lazily on read.
//! - Table-level invalidation: any write to a table drops all cache entries
//!   that touched that table (tracked via `extract_tables_simple`).
//! - Queries containing non-deterministic functions are never cached.
//!
//! All public methods are `&self` — internal mutation via `Mutex<HashMap>`.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::proxy::classifier::extract_tables_simple;

// ─── Config ───────────────────────────────────────────────────────────────────

const DEFAULT_MAX_ENTRIES: usize = 1_000;
const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// Functions that make a SELECT non-deterministic — never cache these.
const NON_DETERMINISTIC: &[&str] = &[
    "NOW()",
    "SYSDATE()",
    "CURRENT_TIMESTAMP",
    "RAND()",
    "UUID()",
    "LAST_INSERT_ID()",
    "@",   // user/session variables
];

// ─── CacheEntry ───────────────────────────────────────────────────────────────

struct CacheEntry {
    bytes: Vec<u8>,
    tables: Vec<String>,
    expires_at: Instant,
    /// LRU: timestamp of last access — used to pick eviction victim.
    last_used: Instant,
}

// ─── QueryCache ───────────────────────────────────────────────────────────────

/// Thread-safe in-memory query cache.
pub struct QueryCache {
    inner: Mutex<CacheInner>,
    ttl: Duration,
    max_entries: usize,
}

struct CacheInner {
    entries: HashMap<String, CacheEntry>,
    hits: u64,
    misses: u64,
}

impl QueryCache {
    pub fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                entries: HashMap::with_capacity(max_entries.min(256)),
                hits: 0,
                misses: 0,
            }),
            ttl,
            max_entries,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_TTL)
    }

    /// Try to retrieve a cached response for `sql`.
    /// Returns `None` on miss or TTL expiry.
    pub async fn get(&self, sql: &str) -> Option<Vec<u8>> {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();

        if let Some(entry) = inner.entries.get_mut(sql) {
            if entry.expires_at > now {
                entry.last_used = now;
                let bytes = entry.bytes.clone();
                inner.hits += 1;
                return Some(bytes);
            }
            // Expired — remove lazily.
            inner.entries.remove(sql);
        }
        inner.misses += 1;
        None
    }

    /// Store a response in the cache.
    /// Does nothing if `sql` is not cacheable (non-deterministic, too large, etc.).
    pub async fn put(&self, sql: &str, bytes: Vec<u8>) {
        if !is_cacheable(sql) {
            return;
        }
        let tables = extract_tables_simple(sql);
        let now = Instant::now();
        let entry = CacheEntry {
            bytes,
            tables,
            expires_at: now + self.ttl,
            last_used: now,
        };

        let mut inner = self.inner.lock().await;

        // Evict one LRU entry if at capacity.
        if inner.entries.len() >= self.max_entries {
            if let Some(victim) = inner
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
            {
                inner.entries.remove(&victim);
            }
        }

        inner.entries.insert(sql.to_owned(), entry);
    }

    /// Invalidate all cache entries that reference any of `tables`.
    /// Called after every write query — cheap because writes are infrequent
    /// relative to reads in typical OLTP workloads.
    pub async fn invalidate_tables(&self, tables: &[String]) {
        if tables.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().await;
        inner.entries.retain(|_, entry| {
            !entry.tables.iter().any(|t| tables.contains(t))
        });
    }

    /// Current (hits, misses) counters — for dashboard metrics.
    #[allow(dead_code)]
    pub async fn stats(&self) -> (u64, u64) {
        let inner = self.inner.lock().await;
        (inner.hits, inner.misses)
    }
}

// ─── Cacheability check ───────────────────────────────────────────────────────

/// Returns `true` if the query result may be safely cached.
/// Conservative — false negatives are fine (miss → backend hit), false
/// positives would serve stale data so we must avoid them.
pub fn is_cacheable(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    // Must be a SELECT (not SELECT ... FOR UPDATE/SHARE).
    if !upper.trim_start().starts_with("SELECT") {
        return false;
    }
    if upper.contains("FOR UPDATE") || upper.contains("FOR SHARE") {
        return false;
    }
    // Non-deterministic functions → skip.
    for token in NON_DETERMINISTIC {
        if upper.contains(token) {
            return false;
        }
    }
    true
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_miss_then_hit() {
        let cache = QueryCache::with_defaults();
        let sql = "SELECT * FROM users WHERE id = 1";
        assert!(cache.get(sql).await.is_none());
        cache.put(sql, b"result".to_vec()).await;
        assert_eq!(cache.get(sql).await.unwrap(), b"result");
        let (hits, misses) = cache.stats().await;
        assert_eq!(hits, 1);
        assert_eq!(misses, 1);
    }

    #[tokio::test]
    async fn test_ttl_expiry() {
        let cache = QueryCache::new(100, Duration::from_millis(1));
        let sql = "SELECT 1";
        cache.put(sql, b"ok".to_vec()).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(cache.get(sql).await.is_none());
    }

    #[tokio::test]
    async fn test_table_invalidation() {
        let cache = QueryCache::with_defaults();
        cache
            .put("SELECT * FROM orders", b"orders".to_vec())
            .await;
        cache
            .put("SELECT * FROM users", b"users".to_vec())
            .await;
        cache
            .invalidate_tables(&["orders".to_string()])
            .await;
        assert!(cache.get("SELECT * FROM orders").await.is_none());
        assert!(cache.get("SELECT * FROM users").await.is_some());
    }

    #[test]
    fn test_is_cacheable() {
        assert!(is_cacheable("SELECT id FROM t WHERE x = 1"));
        assert!(!is_cacheable("SELECT NOW()"));
        assert!(!is_cacheable("SELECT RAND()"));
        assert!(!is_cacheable("SELECT * FROM t WHERE id = @user_id"));
        assert!(!is_cacheable("SELECT * FROM t FOR UPDATE"));
        assert!(!is_cacheable("INSERT INTO t VALUES (1)"));
    }
}
