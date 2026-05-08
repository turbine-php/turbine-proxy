//! Transaction-level tracing — capture the full query timeline of a MySQL
//! transaction so the dashboard can render a waterfall / flame-graph view.
//!
//! Design goals:
//! - Zero allocation on the hot path when no transaction is open.
//! - Lock only on COMMIT / ROLLBACK (not per-query during a transaction).
//! - Bounded memory: ring buffer of the last `CAPACITY` traces.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;

/// How many completed transaction traces to keep in memory.
const CAPACITY: usize = 1_000;

// ─── Wire types ───────────────────────────────────────────────────────────────

/// One query entry inside a transaction trace.
#[derive(Clone, Serialize)]
pub struct TraceEntry {
    /// Normalized (fingerprinted) SQL — no literal values.
    pub fingerprint: String,
    /// Original SQL — truncated to 4 KB so we never OOM on a 1 MB query.
    pub sql: String,
    /// How long this query took, in milliseconds.
    pub duration_ms: f64,
    /// Backend address that executed this query.
    pub backend_addr: String,
    /// `"read"` | `"write"` | `"transaction"` | `"other"`
    pub intent: &'static str,
    /// Unix epoch, milliseconds — when this query started.
    pub started_at_ms: u64,
}

/// A complete captured transaction (or an autocommit sequence when tracking
/// is forced on by the user setting `trace_autocommit = true`).
#[derive(Clone, Serialize)]
pub struct TransactionTrace {
    /// Monotonically increasing trace ID (global counter).
    pub id: u64,
    /// Proxy-internal connection / session ID.
    pub session_id: u32,
    /// MySQL username that opened this session.
    pub user: String,
    /// Client TCP address (IP:port).
    pub client_addr: String,
    /// Unix epoch, milliseconds — when `BEGIN` was received.
    pub started_at_ms: u64,
    /// Total transaction wall-clock duration (COMMIT timestamp − BEGIN timestamp), ms.
    pub duration_ms: f64,
    /// Number of queries inside this transaction.
    pub query_count: usize,
    /// How the transaction ended: `"commit"` | `"rollback"` | `"disconnect"`
    pub outcome: &'static str,
    /// Stable fingerprint of this transaction: FNV-1a hash of all query
    /// fingerprints joined in order. Identical workload patterns produce the
    /// same fingerprint regardless of literal values.
    pub tx_fingerprint: String,
    /// Per-query timeline, in execution order.
    pub queries: Vec<TraceEntry>,
}

// ─── TracerStore ──────────────────────────────────────────────────────────────

/// Shared store for completed transaction traces.
/// Bounded ring buffer — the oldest trace is evicted when full.
pub struct TracerStore {
    traces: Mutex<VecDeque<TransactionTrace>>,
    next_id: AtomicU64,
}

impl TracerStore {
    pub fn new() -> Self {
        Self {
            traces: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            next_id: AtomicU64::new(1),
        }
    }

    /// Push a completed trace. Drops the oldest if the buffer is full.
    pub fn push(&self, mut trace: TransactionTrace) {
        trace.id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut buf = self.traces.lock().expect("tracer lock");
        if buf.len() == CAPACITY {
            buf.pop_front();
        }
        buf.push_back(trace);
    }

    /// Return the most recent `limit` traces, newest first.
    /// If `fingerprint` is `Some`, only traces matching that tx_fingerprint are returned.
    pub fn snapshot(&self, limit: usize, fingerprint: Option<&str>) -> Vec<TransactionTrace> {
        let buf = self.traces.lock().expect("tracer lock");
        buf.iter()
            .rev()
            .filter(|t| fingerprint.map_or(true, |fp| t.tx_fingerprint == fp))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Return all unique `(tx_fingerprint, count)` pairs, sorted by count descending.
    pub fn fingerprint_counts(&self) -> Vec<(String, usize)> {
        let buf = self.traces.lock().expect("tracer lock");
        let mut map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for t in buf.iter() {
            *map.entry(t.tx_fingerprint.clone()).or_default() += 1;
        }
        let mut pairs: Vec<_> = map.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        pairs
    }
}

// ─── Per-session builder ──────────────────────────────────────────────────────

/// Accumulates query entries for one in-progress transaction.
/// Lives entirely on the per-connection task stack — no Arc, no Mutex.
pub struct ActiveTrace {
    pub session_id: u32,
    pub user: String,
    pub client_addr: String,
    pub started_at_ms: u64,
    pub tx_start: std::time::Instant,
    pub entries: Vec<TraceEntry>,
}

impl ActiveTrace {
    pub fn new(session_id: u32, user: &str, client_addr: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            session_id,
            user: user.to_string(),
            client_addr: client_addr.to_string(),
            started_at_ms: now,
            tx_start: std::time::Instant::now(),
            entries: Vec::new(),
        }
    }

    /// Record a single query execution.
    pub fn record(
        &mut self,
        sql: &str,
        fingerprint: String,
        duration_ms: f64,
        backend_addr: &str,
        intent: &'static str,
    ) {
        let started_at_ms = self.started_at_ms
            + self.tx_start.elapsed().as_millis() as u64
            - duration_ms as u64; // approx start of this query

        const MAX_SQL: usize = 4096;
        let sql_stored = if sql.len() > MAX_SQL {
            format!("{}…", &sql[..MAX_SQL])
        } else {
            sql.to_string()
        };

        self.entries.push(TraceEntry {
            fingerprint,
            sql: sql_stored,
            duration_ms,
            backend_addr: backend_addr.to_string(),
            intent,
            started_at_ms,
        });
    }

    /// Finalise and return a `TransactionTrace`.
    pub fn finish(self, outcome: &'static str) -> TransactionTrace {
        let duration_ms = self.tx_start.elapsed().as_secs_f64() * 1000.0;
        let tx_fingerprint = compute_tx_fingerprint(&self.entries);
        let query_count = self.entries.len();
        TransactionTrace {
            id: 0, // assigned by TracerStore::push
            session_id: self.session_id,
            user: self.user,
            client_addr: self.client_addr,
            started_at_ms: self.started_at_ms,
            duration_ms,
            query_count,
            outcome,
            tx_fingerprint,
            queries: self.entries,
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// FNV-1a hash of all query fingerprints concatenated with `|` separators.
/// Produces a short hex string stable across literal value changes.
fn compute_tx_fingerprint(entries: &[TraceEntry]) -> String {
    const FNV_BASIS: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;

    let mut hash = FNV_BASIS;
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            for b in b"|" {
                hash ^= *b as u64;
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        }
        for b in entry.fingerprint.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    format!("{:016x}", hash)
}
