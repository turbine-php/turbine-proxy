//! Lock-free latency histogram for Prometheus exposition.
//!
//! Uses 11 pre-defined upper bounds (seconds) plus a mandatory +Inf bucket.
//! Every counter is an `AtomicU64` — zero allocations on the hot path.
//!
//! Bucket semantics follow the Prometheus convention: bucket[i] counts the
//! number of observations whose value is **≤ BUCKET_BOUNDS[i]** (cumulative).
//! The +Inf bucket (index 11) is identical to the overall `count`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Upper bounds of the 11 finite buckets, in seconds.
/// Chosen to cover the full range from sub-millisecond to 5-second queries.
pub const BUCKET_BOUNDS: [f64; 11] = [
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
];

/// Number of buckets: 11 finite + 1 for +Inf.
const N_BUCKETS: usize = 12;

/// A single-metric, lock-free histogram.
///
/// `record()` is wait-free: each observation touches at most `N_BUCKETS`
/// atomic increments.  `snapshot()` reads all counters with `Relaxed` ordering
/// which is acceptable for metrics (slight momentary imprecision is fine).
pub struct QueryHistogram {
    /// Cumulative bucket counts.  `counts[11]` == +Inf == `total`.
    counts: [AtomicU64; N_BUCKETS],
    /// Sum of all observation values, stored as integer microseconds.
    /// Allows representing fractions of a millisecond without floating-point
    /// accumulation errors, while supporting totals up to ~585,000 years.
    sum_micros: AtomicU64,
    /// Total number of observations recorded.
    pub total: AtomicU64,
}

impl QueryHistogram {
    pub fn new() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_micros: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }
    }

    /// Record one observation (duration in seconds).
    ///
    /// Called on the hot path — must be as cheap as possible.
    /// Complexity: O(N_BUCKETS) atomic increments worst-case.
    #[inline]
    pub fn record(&self, secs: f64) {
        // Increment all finite buckets whose upper bound ≥ secs.
        for (i, &bound) in BUCKET_BOUNDS.iter().enumerate() {
            if secs <= bound {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        // +Inf bucket always incremented.
        self.counts[N_BUCKETS - 1].fetch_add(1, Ordering::Relaxed);
        // Accumulate sum in integer microseconds.
        let micros = (secs * 1_000_000.0) as u64;
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot of all bucket counts, sum (seconds), and observation count.
    /// The returned `counts` array is indexed identically to `BUCKET_BOUNDS`,
    /// with `counts[11]` being the +Inf bucket.
    pub fn snapshot(&self) -> ([u64; N_BUCKETS], f64, u64) {
        let mut counts = [0u64; N_BUCKETS];
        for (i, a) in self.counts.iter().enumerate() {
            counts[i] = a.load(Ordering::Relaxed);
        }
        let sum_secs = self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let count = self.total.load(Ordering::Relaxed);
        (counts, sum_secs, count)
    }
}

impl Default for QueryHistogram {
    fn default() -> Self {
        Self::new()
    }
}
