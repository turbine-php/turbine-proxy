//! N+1 / repeated-query store.
//!
//! Aggregates per-connection repeated-query patterns into a global store
//! so the dashboard can surface them as actionable warnings.

use std::collections::HashMap;

use parking_lot::Mutex;

use serde::Serialize;

/// An N+1 pattern detected across one or more connections.
#[derive(Debug, Clone, Serialize)]
pub struct N1Pattern {
    pub fingerprint: String,
    /// Number of distinct connections where the pattern fired.
    pub connections: u32,
    /// Maximum repetitions seen in a single connection.
    pub max_per_conn: u32,
    pub last_seen: String, // RFC3339
}

/// Global store for N+1 patterns, fed by `SessionQueryTracker` on disconnect.
pub struct N1Store {
    inner: Mutex<HashMap<u64, N1Pattern>>,
}

impl N1Store {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record patterns detected in one connection session.
    /// `patterns` is a slice of `(hash, fingerprint, count)`.
    pub fn record_connection(&self, patterns: &[(u64, String, u32)]) {
        if patterns.is_empty() {
            return;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut map = self.inner.lock();
        for (hash, fp, count) in patterns {
            let entry = map.entry(*hash).or_insert_with(|| N1Pattern {
                fingerprint: fp.clone(),
                connections: 0,
                max_per_conn: 0,
                last_seen: now.clone(),
            });
            entry.connections += 1;
            if *count > entry.max_per_conn {
                entry.max_per_conn = *count;
            }
            entry.last_seen = now.clone();
        }
    }

    /// Return all detected patterns sorted by connection count descending.
    pub fn get_all(&self) -> Vec<N1Pattern> {
        let map = self.inner.lock();
        let mut v: Vec<_> = map.values().cloned().collect();
        v.sort_by(|a, b| {
            b.connections
                .cmp(&a.connections)
                .then(b.max_per_conn.cmp(&a.max_per_conn))
        });
        v
    }
}
