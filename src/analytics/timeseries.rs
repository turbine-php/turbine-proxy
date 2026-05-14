//! Lock-free throughput counters + SQLite time-series store for query-load
//! persistence > 30 days.
//!
//! **Hot-path contract**: `ThroughputCounters::record` is fully lock-free — it
//! only uses `fetch_add` / `fetch_max` on `AtomicU64`.
//!
//! **Background task** (spawned from `main.rs`):
//! - Every 60 s: `take_snapshot` → `record_minute`
//! - Every 1 h:  `rollup_hourly`  (1 min → 1 h, idempotent)
//! - Every 24 h: `rollup_daily` + `prune` (1 h → 1 d; delete rows older than
//!   `retention_days`)

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

// ── ThroughputCounters ───────────────────────────────────────────────────────

/// Lock-free throughput accumulators. Shared between `handle_connection` tasks
/// and the background time-series task via `Arc<ThroughputCounters>`.
pub struct ThroughputCounters {
    queries: AtomicU64,
    slow: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    slow_threshold_us: u64,
}

/// Point-in-time drain produced by [`ThroughputCounters::take_snapshot`].
/// All accumulators are reset to zero atomically (swap).
pub struct MinuteSnapshot {
    pub queries: u64,
    pub slow: u64,
    pub total_us: u64,
    pub max_us: u64,
}

impl ThroughputCounters {
    pub fn new(slow_threshold_ms: u64) -> Self {
        Self {
            queries: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
            slow_threshold_us: slow_threshold_ms.saturating_mul(1_000),
        }
    }

    /// Called from the query hot path. Never blocks.
    #[inline]
    pub fn record(&self, duration_us: u64) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(duration_us, Ordering::Relaxed);
        self.max_us.fetch_max(duration_us, Ordering::Relaxed);
        if duration_us >= self.slow_threshold_us {
            self.slow.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Atomically drain and reset all counters.
    /// The tiny race on `max_us` between reads is acceptable for time-series data.
    pub fn take_snapshot(&self) -> MinuteSnapshot {
        MinuteSnapshot {
            queries: self.queries.swap(0, Ordering::Relaxed),
            slow: self.slow.swap(0, Ordering::Relaxed),
            total_us: self.total_us.swap(0, Ordering::Relaxed),
            max_us: self.max_us.swap(0, Ordering::Relaxed),
        }
    }
}

// ── TimeseriesStore ──────────────────────────────────────────────────────────

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS timeseries (
    bucket_unix  INTEGER NOT NULL,
    resolution   TEXT    NOT NULL,
    queries      INTEGER NOT NULL DEFAULT 0,
    slow_queries INTEGER NOT NULL DEFAULT 0,
    total_us     INTEGER NOT NULL DEFAULT 0,
    max_us       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (bucket_unix, resolution)
);
CREATE INDEX IF NOT EXISTS ts_res_bucket ON timeseries(resolution, bucket_unix);
";

/// Persistent SQLite store for aggregated query throughput over time.
pub struct TimeseriesStore {
    conn: Mutex<Connection>,
}

/// A single time-series data point returned by [`TimeseriesStore::query`].
#[derive(serde::Serialize)]
pub struct TsPoint {
    pub bucket_unix: i64,
    pub queries: i64,
    pub slow_queries: i64,
    pub avg_us: f64,
    pub max_us: i64,
}

impl TimeseriesStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Opening timeseries DB at '{db_path}'"))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Upsert a 1-minute bucket. Safe to call multiple times for the same
    /// minute (counters accumulate via `ON CONFLICT DO UPDATE`).
    pub fn record_minute(&self, bucket_unix: i64, snap: &MinuteSnapshot) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO timeseries
                 (bucket_unix, resolution, queries, slow_queries, total_us, max_us)
             VALUES (?1, '1m', ?2, ?3, ?4, ?5)
             ON CONFLICT(bucket_unix, resolution) DO UPDATE SET
                 queries      = queries      + excluded.queries,
                 slow_queries = slow_queries + excluded.slow_queries,
                 total_us     = total_us     + excluded.total_us,
                 max_us       = MAX(max_us,    excluded.max_us)",
            params![
                bucket_unix,
                snap.queries as i64,
                snap.slow as i64,
                snap.total_us as i64,
                snap.max_us as i64,
            ],
        )?;
        Ok(())
    }

    /// Aggregate 1-minute buckets into 1-hour rows. Idempotent — safe to call
    /// every minute. Includes the current (partial) hour so data appears
    /// immediately without waiting for the hour to complete.
    pub fn rollup_hourly(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(
            "INSERT OR REPLACE INTO timeseries
                 (bucket_unix, resolution, queries, slow_queries, total_us, max_us)
             SELECT
                 (bucket_unix / 3600) * 3600,
                 '1h',
                 SUM(queries),
                 SUM(slow_queries),
                 SUM(total_us),
                 MAX(max_us)
             FROM timeseries
             WHERE resolution = '1m'
             GROUP BY (bucket_unix / 3600) * 3600;",
        )?;
        Ok(())
    }

    /// Aggregate 1-hour buckets into 1-day rows. Idempotent — includes the
    /// current (partial) day so data is visible without waiting until midnight.
    pub fn rollup_daily(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(
            "INSERT OR REPLACE INTO timeseries
                 (bucket_unix, resolution, queries, slow_queries, total_us, max_us)
             SELECT
                 (bucket_unix / 86400) * 86400,
                 '1d',
                 SUM(queries),
                 SUM(slow_queries),
                 SUM(total_us),
                 MAX(max_us)
             FROM timeseries
             WHERE resolution = '1h'
             GROUP BY (bucket_unix / 86400) * 86400;",
        )?;
        Ok(())
    }

    /// Delete rows whose bucket is older than `retention_days` days.
    pub fn prune(&self, retention_days: u32) -> Result<()> {
        let cutoff = chrono::Utc::now().timestamp() - (retention_days as i64 * 86_400);
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM timeseries WHERE bucket_unix < ?1",
            params![cutoff],
        )?;
        Ok(())
    }

    /// Return the most recent `limit` points at the given resolution
    /// (`'1m'`, `'1h'`, or `'1d'`), in chronological order.
    pub fn query(&self, resolution: &str, limit: usize) -> Result<Vec<TsPoint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT bucket_unix, queries, slow_queries, total_us, max_us
             FROM timeseries
             WHERE resolution = ?1
             ORDER BY bucket_unix DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![resolution, limit as i64], |row| {
            let queries: i64 = row.get(1)?;
            let total_us: i64 = row.get(3)?;
            Ok(TsPoint {
                bucket_unix: row.get(0)?,
                queries,
                slow_queries: row.get(2)?,
                avg_us: if queries > 0 {
                    total_us as f64 / queries as f64
                } else {
                    0.0
                },
                max_us: row.get(4)?,
            })
        })?;
        let mut pts: Vec<TsPoint> = rows.collect::<rusqlite::Result<_>>()?;
        pts.reverse(); // chronological order for the chart
        Ok(pts)
    }

    /// Return points for a specific time range (unix seconds, inclusive) at the
    /// given resolution.  Used by the Grafana Simple JSON datasource handler.
    pub fn query_range(
        &self,
        resolution: &str,
        from_unix: i64,
        to_unix: i64,
        limit: usize,
    ) -> Result<Vec<TsPoint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT bucket_unix, queries, slow_queries, total_us, max_us
             FROM timeseries
             WHERE resolution = ?1
               AND bucket_unix >= ?2
               AND bucket_unix <= ?3
             ORDER BY bucket_unix ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![resolution, from_unix, to_unix, limit as i64],
            |row| {
                let queries: i64 = row.get(1)?;
                let total_us: i64 = row.get(3)?;
                Ok(TsPoint {
                    bucket_unix: row.get(0)?,
                    queries,
                    slow_queries: row.get(2)?,
                    avg_us: if queries > 0 {
                        total_us as f64 / queries as f64
                    } else {
                        0.0
                    },
                    max_us: row.get(4)?,
                })
            },
        )?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }
}
