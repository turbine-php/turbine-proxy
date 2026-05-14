//! SQLite persistence for analytics data.
//! `flush` must be called from a blocking context (e.g., `tokio::task::spawn_blocking`).

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};

use super::collector::QueryStats;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS query_stats (
    fingerprint_hash INTEGER PRIMARY KEY,
    fingerprint      TEXT    NOT NULL,
    count            INTEGER NOT NULL DEFAULT 0,
    total_us         INTEGER NOT NULL DEFAULT 0,
    min_us           INTEGER,
    max_us           INTEGER,
    p95_us           INTEGER,
    p99_us           INTEGER,
    last_seen        TEXT,
    updated_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);
";

pub struct AnalyticsStorage {
    conn: Mutex<Connection>,
}

impl AnalyticsStorage {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Opening analytics DB at '{db_path}'"))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Flush a batch of in-memory stats to SQLite.
    /// Increments existing rows so history accumulates across flushes.
    pub fn flush(&self, stats: &[QueryStats]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO query_stats
                    (fingerprint_hash, fingerprint, count, total_us, min_us, max_us,
                     p95_us, p99_us, last_seen, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, CURRENT_TIMESTAMP)
                 ON CONFLICT(fingerprint_hash) DO UPDATE SET
                     fingerprint = excluded.fingerprint,
                     count       = count       + excluded.count,
                     total_us    = total_us    + excluded.total_us,
                     min_us      = MIN(COALESCE(min_us, excluded.min_us), excluded.min_us),
                     max_us      = MAX(COALESCE(max_us, excluded.max_us), excluded.max_us),
                     p95_us      = excluded.p95_us,
                     p99_us      = excluded.p99_us,
                     last_seen   = excluded.last_seen,
                     updated_at  = CURRENT_TIMESTAMP",
            )?;

            for s in stats {
                stmt.execute(params![
                    s.hash as i64,
                    s.fingerprint,
                    s.count as i64,
                    s.total_duration.as_micros() as i64,
                    s.min_duration.as_micros() as i64,
                    s.max_duration.as_micros() as i64,
                    s.p95().as_micros() as i64,
                    s.p99().as_micros() as i64,
                    s.last_seen.to_rfc3339(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // TODO: used by dashboard /api/queries endpoint
    #[allow(dead_code)]
    pub fn get_top_by_count(&self, limit: usize) -> Result<Vec<StoredQueryStats>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint_hash, fingerprint, count, total_us, min_us, max_us,
                    p95_us, p99_us, last_seen
             FROM query_stats ORDER BY count DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_stats)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    // TODO: used by dashboard /api/slow-queries endpoint
    #[allow(dead_code)]
    pub fn get_top_by_p95(&self, limit: usize) -> Result<Vec<StoredQueryStats>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint_hash, fingerprint, count, total_us, min_us, max_us,
                    p95_us, p99_us, last_seen
             FROM query_stats ORDER BY p95_us DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_stats)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Returns the sum of all `count` rows — used at startup to restore the
    /// in-memory `queries_total` counter so it doesn't reset on restart.
    pub fn load_total_query_count(&self) -> Result<u64> {
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row("SELECT COALESCE(SUM(count), 0) FROM query_stats", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        Ok(n.max(0) as u64)
    }
}

/// A row from the `query_stats` table.
// TODO: used by dashboard endpoints
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct StoredQueryStats {
    pub hash: i64,
    pub fingerprint: String,
    pub count: i64,
    pub total_us: i64,
    pub min_us: Option<i64>,
    pub max_us: Option<i64>,
    pub p95_us: Option<i64>,
    pub p99_us: Option<i64>,
    pub last_seen: Option<String>,
}

// TODO: used by get_top_by_count / get_top_by_p95
#[allow(dead_code)]
fn row_to_stats(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredQueryStats> {
    Ok(StoredQueryStats {
        hash: row.get(0)?,
        fingerprint: row.get(1)?,
        count: row.get(2)?,
        total_us: row.get(3)?,
        min_us: row.get(4)?,
        max_us: row.get(5)?,
        p95_us: row.get(6)?,
        p99_us: row.get(7)?,
        last_seen: row.get(8)?,
    })
}
