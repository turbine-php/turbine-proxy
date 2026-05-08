//! Per-user connection and query counters — updated atomically on the hot path.
//!
//! Used by `GET /api/users` to show live activity per MySQL user.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::RwLock;

// ─── UserStats ────────────────────────────────────────────────────────────────

#[derive(Clone, serde::Serialize)]
pub struct UserStats {
    pub connections_active: usize,
    pub connections_total: usize,
    pub queries_total: usize,
    /// RFC 3339 string of last connection time.
    pub last_seen: Option<String>,
    pub allow_writes: bool,
}

// ─── UserRegistry ─────────────────────────────────────────────────────────────

pub struct UserRegistry {
    inner: Arc<RwLock<HashMap<String, MutableStats>>>,
}

struct MutableStats {
    connections_active: usize,
    connections_total: usize,
    queries_total: usize,
    last_seen: Option<SystemTime>,
    allow_writes: bool,
}

impl UserRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record a new connection for `user`.
    pub async fn on_connect(&self, user: &str, allow_writes: bool) {
        let mut map = self.inner.write().await;
        let e = map.entry(user.to_string()).or_insert_with(|| MutableStats {
            connections_active: 0,
            connections_total: 0,
            queries_total: 0,
            last_seen: None,
            allow_writes,
        });
        e.connections_active += 1;
        e.connections_total += 1;
        e.last_seen = Some(SystemTime::now());
        e.allow_writes = allow_writes;
    }

    /// Record a disconnect for `user`.
    pub async fn on_disconnect(&self, user: &str) {
        let mut map = self.inner.write().await;
        if let Some(e) = map.get_mut(user) {
            e.connections_active = e.connections_active.saturating_sub(1);
        }
    }

    /// Increment query counter for `user`.
    pub async fn on_query(&self, user: &str) {
        let mut map = self.inner.write().await;
        if let Some(e) = map.get_mut(user) {
            e.queries_total += 1;
        }
    }

    /// Returns the number of currently active connections for `user`.
    /// Returns 0 if the user has no recorded connections yet.
    pub async fn active_connections(&self, user: &str) -> usize {
        let map = self.inner.read().await;
        map.get(user).map(|s| s.connections_active).unwrap_or(0)
    }

    /// Snapshot for the dashboard API.
    pub async fn snapshot(&self) -> Vec<(String, UserStats)> {
        let map = self.inner.read().await;
        map.iter()
            .map(|(name, e)| {
                let last_seen = e.last_seen.and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| {
                        // Format as ISO 8601 manually — no chrono dependency needed.
                        let secs = d.as_secs();
                        fmt_epoch(secs)
                    })
                });
                (
                    name.clone(),
                    UserStats {
                        connections_active: e.connections_active,
                        connections_total: e.connections_total,
                        queries_total: e.queries_total,
                        last_seen,
                        allow_writes: e.allow_writes,
                    },
                )
            })
            .collect()
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Minimal epoch → RFC 3339 formatter that avoids pulling in chrono.
fn fmt_epoch(secs: u64) -> String {
    // Days since Unix epoch
    let days = secs / 86400;
    let day_s = secs % 86400;
    let h = day_s / 3600;
    let m = (day_s % 3600) / 60;
    let s = day_s % 60;

    // Gregorian calendar from days since 1970-01-01.
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // 400-year Gregorian cycle = 146097 days
    let mut y = 1970u64;
    loop {
        let in_year = if is_leap(y) { 366 } else { 365 };
        if days < in_year {
            break;
        }
        days -= in_year;
        y += 1;
    }
    let leap = is_leap(y);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u64;
    for &mdays in &months {
        if days < mdays {
            break;
        }
        days -= mdays;
        mo += 1;
    }
    (y, mo, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
