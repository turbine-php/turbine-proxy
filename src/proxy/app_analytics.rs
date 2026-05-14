//! Per-dimension analytics: per-user, per-client-IP, and per-app query counters.
//!
//! Three separate `RwLock<HashMap>` avoid cross-dimension lock contention.
//! All updates are `async` but complete in microseconds (no I/O, no allocations
//! on the hot path beyond the initial first-seen insertion).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::RwLock;

// ─── DimStats ─────────────────────────────────────────────────────────────────

struct DimStats {
    queries_total: usize,
    queries_read: usize,
    queries_write: usize,
    connections_active: usize,
    connections_total: usize,
    first_seen_ms: u64,
    last_seen_ms: u64,
}

impl DimStats {
    fn new(now_ms: u64) -> Self {
        Self {
            queries_total: 0,
            queries_read: 0,
            queries_write: 0,
            connections_active: 0,
            connections_total: 0,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
        }
    }
}

// ─── DimEntry (serialisable snapshot) ────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct DimEntry {
    pub key: String,
    pub queries_total: usize,
    pub queries_read: usize,
    pub queries_write: usize,
    pub connections_active: usize,
    pub connections_total: usize,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
}

// ─── AppAnalyticsStore ────────────────────────────────────────────────────────

pub struct AppAnalyticsStore {
    by_user: Arc<RwLock<HashMap<String, DimStats>>>,
    by_ip: Arc<RwLock<HashMap<String, DimStats>>>,
    by_app: Arc<RwLock<HashMap<String, DimStats>>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl AppAnalyticsStore {
    pub fn new() -> Self {
        Self {
            by_user: Arc::new(RwLock::new(HashMap::new())),
            by_ip: Arc::new(RwLock::new(HashMap::new())),
            by_app: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record a new connection. `app` may be an empty string if unknown.
    pub async fn on_connect(&self, user: &str, ip: &str, app: &str) {
        let now = now_ms();
        Self::conn_open(&self.by_user, user, now).await;
        Self::conn_open(&self.by_ip, ip, now).await;
        if !app.is_empty() {
            Self::conn_open(&self.by_app, app, now).await;
        }
    }

    /// Record a disconnection.
    pub async fn on_disconnect(&self, user: &str, ip: &str, app: &str) {
        Self::conn_close(&self.by_user, user).await;
        Self::conn_close(&self.by_ip, ip).await;
        if !app.is_empty() {
            Self::conn_close(&self.by_app, app).await;
        }
    }

    /// Record one query execution.
    /// `is_read` / `is_write` follow the same semantics as `ProxyMetrics`.
    pub async fn on_query(&self, user: &str, ip: &str, app: &str, is_read: bool, is_write: bool) {
        let now = now_ms();
        Self::query(&self.by_user, user, is_read, is_write, now).await;
        Self::query(&self.by_ip, ip, is_read, is_write, now).await;
        if !app.is_empty() {
            Self::query(&self.by_app, app, is_read, is_write, now).await;
        }
    }

    // ── snapshots ─────────────────────────────────────────────────────────────

    pub async fn snapshot_users(&self) -> Vec<DimEntry> {
        Self::snapshot(&self.by_user).await
    }

    pub async fn snapshot_ips(&self) -> Vec<DimEntry> {
        Self::snapshot(&self.by_ip).await
    }

    pub async fn snapshot_apps(&self) -> Vec<DimEntry> {
        Self::snapshot(&self.by_app).await
    }

    // ── private helpers ───────────────────────────────────────────────────────

    async fn conn_open(map: &Arc<RwLock<HashMap<String, DimStats>>>, key: &str, now: u64) {
        let mut m = map.write().await;
        let e = m
            .entry(key.to_string())
            .or_insert_with(|| DimStats::new(now));
        e.connections_active += 1;
        e.connections_total += 1;
        e.last_seen_ms = now;
    }

    async fn conn_close(map: &Arc<RwLock<HashMap<String, DimStats>>>, key: &str) {
        let mut m = map.write().await;
        if let Some(e) = m.get_mut(key) {
            e.connections_active = e.connections_active.saturating_sub(1);
        }
    }

    async fn query(
        map: &Arc<RwLock<HashMap<String, DimStats>>>,
        key: &str,
        is_read: bool,
        is_write: bool,
        now: u64,
    ) {
        let mut m = map.write().await;
        if let Some(e) = m.get_mut(key) {
            e.queries_total += 1;
            if is_read {
                e.queries_read += 1;
            }
            if is_write {
                e.queries_write += 1;
            }
            e.last_seen_ms = now;
        }
    }

    async fn snapshot(map: &Arc<RwLock<HashMap<String, DimStats>>>) -> Vec<DimEntry> {
        let m = map.read().await;
        let mut entries: Vec<DimEntry> = m
            .iter()
            .map(|(k, s)| DimEntry {
                key: k.clone(),
                queries_total: s.queries_total,
                queries_read: s.queries_read,
                queries_write: s.queries_write,
                connections_active: s.connections_active,
                connections_total: s.connections_total,
                first_seen_ms: s.first_seen_ms,
                last_seen_ms: s.last_seen_ms,
            })
            .collect();
        entries.sort_by_key(|b| std::cmp::Reverse(b.queries_total));
        entries
    }
}
