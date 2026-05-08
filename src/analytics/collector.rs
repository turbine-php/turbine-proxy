//! Analytics collector — receives query events from the hot path via an async channel,
//! aggregates stats in memory, and exposes them for dashboard reads and storage flushes.
//!
//! Hot path contract: `Collector::try_record` never blocks — events are dropped when the
//! channel is full rather than stalling the query.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

use crate::analytics::advisor::AdvisorTask;
use crate::proxy::fingerprint;

// ── Public types ────────────────────────────────────────────────────────────

/// Aggregated statistics for a single query fingerprint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryStats {
    /// Stable hash of the fingerprint (used as map key and SQLite PK).
    pub hash: u64,
    pub fingerprint: String,
    pub count: u64,
    #[serde(skip)]
    pub total_duration: Duration,
    #[serde(skip)]
    pub min_duration: Duration,
    #[serde(skip)]
    pub max_duration: Duration,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    /// Bounded sample of recent latencies for percentile calculation.
    #[serde(skip)]
    latencies: Vec<Duration>,
    /// Serialisable duration summaries (µs).
    pub total_us: u64,
    pub min_us: u64,
    pub max_us: u64,
}

impl QueryStats {
    fn new(hash: u64, fingerprint: String) -> Self {
        Self {
            hash,
            fingerprint,
            count: 0,
            total_duration: Duration::ZERO,
            min_duration: Duration::MAX,
            max_duration: Duration::ZERO,
            last_seen: chrono::Utc::now(),
            latencies: Vec::new(),
            total_us: 0,
            min_us: u64::MAX,
            max_us: 0,
        }
    }

    fn record(&mut self, duration: Duration) {
        self.count += 1;
        self.total_duration += duration;
        self.min_duration = self.min_duration.min(duration);
        self.max_duration = self.max_duration.max(duration);
        let us = duration.as_micros() as u64;
        self.total_us = self.total_us.saturating_add(us);
        self.min_us = self.min_us.min(us);
        self.max_us = self.max_us.max(us);
        self.last_seen = chrono::Utc::now();
        // Keep the last 1 000 samples — enough for stable p95/p99.
        if self.latencies.len() >= 1_000 {
            self.latencies.remove(0);
        }
        self.latencies.push(duration);
    }

    // TODO: used by dashboard /api/queries endpoint
    #[allow(dead_code)]
    pub fn avg_duration(&self) -> Duration {
        if self.count == 0 {
            Duration::ZERO
        } else {
            self.total_duration / self.count as u32
        }
    }

    pub fn p95(&self) -> Duration {
        percentile(&self.latencies, 95)
    }

    pub fn p99(&self) -> Duration {
        percentile(&self.latencies, 99)
    }
}

// ── Collector ───────────────────────────────────────────────────────────────

/// Internal event sent from the hot path.
struct QueryEvent {
    sql: String,
    duration: Duration,
}

/// Analytics collector. Create with `Collector::new` — it spawns its own background task.
pub struct Collector {
    /// Bounded sender: `try_send` never blocks.
    sender: mpsc::Sender<QueryEvent>,
    /// Shared stats map — written by the background task, read by dashboard / flush.
    stats: Arc<Mutex<HashMap<u64, QueryStats>>>,
    /// Optional index advisor — receives slow queries for background EXPLAIN analysis.
    #[allow(dead_code)]
    advisor: Option<Arc<AdvisorTask>>,
}

impl Collector {
    /// Channel capacity. Events are dropped (not queued) beyond this limit.
    const CHANNEL_CAP: usize = 10_000;

    /// Create a new collector and spawn its aggregation background task.
    pub fn new(slow_query_ms: u64) -> Self {
        let (tx, rx) = mpsc::channel(Self::CHANNEL_CAP);
        let stats: Arc<Mutex<HashMap<u64, QueryStats>>> = Arc::new(Mutex::new(HashMap::new()));
        let stats_bg = stats.clone();
        let slow_threshold = Duration::from_millis(slow_query_ms);

        tokio::spawn(aggregation_loop(rx, stats_bg, slow_threshold));

        Self {
            sender: tx,
            stats,
            advisor: None,
        }
    }

    /// Attach an `AdvisorTask` so that slow queries are forwarded for EXPLAIN analysis.
    #[allow(dead_code)]
    pub fn set_advisor(&mut self, advisor: Arc<AdvisorTask>) {
        self.advisor = Some(advisor);
    }

    /// Record a query from the hot path. **Never blocks** — drops the event if
    /// the channel is full to avoid stalling the query.
    pub fn try_record(&self, sql: &str, duration: Duration, _was_read: bool) {
        let event = QueryEvent {
            sql: sql.to_owned(),
            duration,
        };
        // Intentional discard: metrics are best-effort, queries are not.
        let _ = self.sender.try_send(event);
    }

    /// Forward a slow query to the index advisor (if attached). Never blocks.
    #[allow(dead_code)]
    pub fn try_advise(
        &self,
        sql: &str,
        fingerprint: &str,
        slow_threshold: Duration,
        duration: Duration,
    ) {
        if duration >= slow_threshold {
            if let Some(advisor) = &self.advisor {
                advisor.try_submit(sql.to_owned(), fingerprint.to_owned());
            }
        }
    }

    /// Drain all accumulated stats and reset the in-memory state.
    /// Called periodically by the storage flush task.
    pub async fn drain(&self) -> Vec<QueryStats> {
        let mut map = self.stats.lock().await;
        let stats: Vec<QueryStats> = map.values().cloned().collect();
        map.clear();
        stats
    }

    // TODO: used by dashboard /api/queries endpoint
    #[allow(dead_code)]
    pub async fn get_stats(&self) -> Vec<QueryStats> {
        let map = self.stats.lock().await;
        map.values().cloned().collect()
    }

    // TODO: used by dashboard /api/slow-queries endpoint
    #[allow(dead_code)]
    pub async fn get_slow_queries(&self, limit: usize) -> Vec<QueryStats> {
        let mut stats = self.get_stats().await;
        stats.sort_by_key(|b| std::cmp::Reverse(b.p95()));
        stats.truncate(limit);
        stats
    }
}

// ── Background aggregation loop ─────────────────────────────────────────────

async fn aggregation_loop(
    mut rx: mpsc::Receiver<QueryEvent>,
    stats: Arc<Mutex<HashMap<u64, QueryStats>>>,
    slow_threshold: Duration,
) {
    while let Some(event) = rx.recv().await {
        let (fp, hash) = fingerprint::fingerprint_with_hash(&event.sql);

        if event.duration >= slow_threshold {
            log::info!(
                "Slow query ({:.1}ms): {}",
                event.duration.as_secs_f64() * 1_000.0,
                &fp[..fp.len().min(200)]
            );
        }

        let mut map = stats.lock().await;
        map.entry(hash)
            .or_insert_with(|| QueryStats::new(hash, fp))
            .record(event.duration);
    }
}

// ── Utilities ───────────────────────────────────────────────────────────────

fn percentile(latencies: &[Duration], pct: usize) -> Duration {
    if latencies.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted = latencies.to_vec();
    sorted.sort();
    let idx = (sorted.len() * pct / 100).min(sorted.len() - 1);
    sorted[idx]
}
