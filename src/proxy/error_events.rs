//! In-memory ring buffer for MySQL error events (ERR_Packet captures).
//!
//! Errors are emitted asynchronously on the hot path — one atomic counter bump
//! + one try_send on a bounded channel.
//!
//! The collector task drains the channel and writes to SQLite. Zero allocations
//! when no error occurs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ─── Error category ───────────────────────────────────────────────────────────

/// Broad category derived from the MySQL error code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ErrorCategory {
    Auth,
    Syntax,
    Connection,
    Resource,
    Constraint,
    Proxy,
    Other,
}

impl ErrorCategory {
    pub fn from_code(code: u16) -> Self {
        match code {
            1044 | 1045 | 1142 | 1143 => Self::Auth,
            1064 | 1065 | 1149 => Self::Syntax,
            2006 | 2013 | 1927 | 1152 => Self::Connection,
            1040 | 1226 | 1227 => Self::Resource,
            1062 | 1048 | 1452 | 1264 => Self::Constraint,
            0 => Self::Proxy, // proxy-generated errors use code 0
            _ => Self::Other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "AUTH",
            Self::Syntax => "SYNTAX",
            Self::Connection => "CONNECTION",
            Self::Resource => "RESOURCE",
            Self::Constraint => "CONSTRAINT",
            Self::Proxy => "PROXY",
            Self::Other => "OTHER",
        }
    }
}

// ─── ErrorEvent ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEvent {
    pub ts: i64, // Unix timestamp (seconds)
    pub code: u16,
    pub category: String,
    pub message: String,
    pub fingerprint: String,
    pub backend_addr: String,
    pub client_ip: String,
    pub user: String,
    pub duration_ms: f64,
    /// Protocol that generated this error: `"mysql"` or `"postgres"`.
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

#[allow(dead_code)]
fn default_protocol() -> String {
    "mysql".to_string()
}

impl ErrorEvent {
    pub fn new(
        code: u16,
        message: impl Into<String>,
        fingerprint: impl Into<String>,
        backend_addr: impl Into<String>,
        client_ip: impl Into<String>,
        user: impl Into<String>,
        duration_ms: f64,
    ) -> Self {
        Self {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            category: ErrorCategory::from_code(code).as_str().to_string(),
            code,
            message: message.into(),
            fingerprint: fingerprint.into(),
            backend_addr: backend_addr.into(),
            client_ip: client_ip.into(),
            user: user.into(),
            duration_ms,
            protocol: "mysql".to_string(),
        }
    }

    /// Create a PostgreSQL error event.
    pub fn new_pg(
        message: impl Into<String>,
        fingerprint: impl Into<String>,
        client_ip: impl Into<String>,
        user: impl Into<String>,
        duration_ms: f64,
    ) -> Self {
        Self {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            category: ErrorCategory::Connection.as_str().to_string(),
            code: 0,
            message: message.into(),
            fingerprint: fingerprint.into(),
            backend_addr: String::new(),
            client_ip: client_ip.into(),
            user: user.into(),
            duration_ms,
            protocol: "postgres".to_string(),
        }
    }
}

// ─── ErrorEventStore ─────────────────────────────────────────────────────────

/// Shared store backed by a bounded ring-buffer (most recent 1 000 events).
pub struct ErrorEventStore {
    events: std::sync::Mutex<std::collections::VecDeque<ErrorEvent>>,
    capacity: usize,
    pub total: AtomicUsize,
    /// Channel sender for async SQLite persistence (optional).
    persist_tx: Option<mpsc::Sender<ErrorEvent>>,
}

impl ErrorEventStore {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            events: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(capacity)),
            capacity,
            total: AtomicUsize::new(0),
            persist_tx: None,
        })
    }

    pub fn push(&self, ev: ErrorEvent) {
        self.total.fetch_add(1, Ordering::Relaxed);
        if let Some(ref tx) = self.persist_tx {
            let _ = tx.try_send(ev.clone());
        }
        let mut guard = self.events.lock().unwrap();
        if guard.len() >= self.capacity {
            guard.pop_front();
        }
        guard.push_back(ev);
    }

    /// Returns the last `limit` events in reverse-chronological order.
    #[allow(dead_code)]
    pub fn list(&self, limit: usize) -> Vec<ErrorEvent> {
        let guard = self.events.lock().unwrap();
        guard.iter().rev().take(limit).cloned().collect()
    }

    /// Returns the last `limit` events filtered by protocol (`"mysql"` or `"postgres"`).
    /// When `protocol` is `None`, returns all events.
    pub fn list_filtered(&self, limit: usize, protocol: Option<&str>) -> Vec<ErrorEvent> {
        let guard = self.events.lock().unwrap();
        guard
            .iter()
            .rev()
            .filter(|ev| protocol.is_none_or(|p| ev.protocol == p))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Returns counts for the last 1h / 24h / 7d by category,
    /// plus the top-10 fingerprints and top-10 error codes by frequency.
    #[allow(dead_code)]
    pub fn stats(&self) -> serde_json::Value {
        self.stats_filtered(None)
    }

    /// Same as `stats()`, but optionally filtered by protocol (`mysql` or `postgres`).
    pub fn stats_filtered(&self, protocol: Option<&str>) -> serde_json::Value {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let guard = self.events.lock().unwrap();
        let mut by_cat_1h: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut by_cat_24h: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut by_cat_7d: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut filtered_total: usize = 0;

        // Fingerprint → (count_1h, last_seen)
        let mut fp_counts: std::collections::HashMap<String, (usize, i64)> =
            std::collections::HashMap::new();
        // Error code → (count_24h, category)
        let mut code_counts: std::collections::HashMap<u16, (usize, String)> =
            std::collections::HashMap::new();

        for ev in guard.iter() {
            if protocol.is_some_and(|p| ev.protocol != p) {
                continue;
            }
            filtered_total += 1;
            let age = now - ev.ts;
            let cat = ev.category.as_str();
            if age <= 3600 {
                *by_cat_1h.entry(cat).or_default() += 1;
            }
            if age <= 86400 {
                *by_cat_24h.entry(cat).or_default() += 1;
            }
            if age <= 604800 {
                *by_cat_7d.entry(cat).or_default() += 1;
            }

            // Top fingerprints (last 1h)
            if age <= 3600 && !ev.fingerprint.is_empty() {
                let entry = fp_counts
                    .entry(ev.fingerprint.clone())
                    .or_insert((0, ev.ts));
                entry.0 += 1;
                if ev.ts > entry.1 {
                    entry.1 = ev.ts;
                }
            }

            // Top error codes (last 24h)
            if age <= 86400 && ev.code > 0 {
                let entry = code_counts
                    .entry(ev.code)
                    .or_insert((0, ev.category.clone()));
                entry.0 += 1;
            }
        }

        // Top 10 fingerprints sorted by count desc
        let mut top_fps: Vec<_> = fp_counts
            .into_iter()
            .map(|(fp, (count, last_seen))| serde_json::json!({ "fingerprint": fp, "error_count": count, "last_seen": last_seen }))
            .collect();
        top_fps.sort_by(|a, b| b["error_count"].as_u64().cmp(&a["error_count"].as_u64()));
        top_fps.truncate(10);

        // Top 10 error codes sorted by count desc
        let mut top_codes: Vec<_> = code_counts
            .into_iter()
            .map(|(code, (count, cat))| serde_json::json!({ "code": code, "count": count, "category": cat }))
            .collect();
        top_codes.sort_by(|a, b| b["count"].as_u64().cmp(&a["count"].as_u64()));
        top_codes.truncate(10);

        serde_json::json!({
            "1h":  by_cat_1h,
            "24h": by_cat_24h,
            "7d":  by_cat_7d,
            "total": filtered_total,
            "top_fingerprints": top_fps,
            "top_codes": top_codes,
        })
    }
}
