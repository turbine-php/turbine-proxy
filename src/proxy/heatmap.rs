//! Temporal heatmap — query volume and slow-query rate per (day-of-week, hour-of-day).
//!
//! Data model:
//!   7 days × 24 hours = 168 cells, each holding:
//!     - `queries`     — total query count
//!     - `slow`        — queries that exceeded the slow threshold
//!     - `duration_ms` — cumulative duration (for average calculation)
//!
//! All counters are `AtomicU64` → zero locks on the hot path.
//!
//! Anomaly detection (sigma method):
//!   For a given (day, hour) cell we compare the **current-window** QPS against
//!   the mean ± k·σ across all 168 cells.  If the cell is > mean + k·σ it is
//!   flagged as a spike.  k defaults to 2.0.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

// ─── Constants ────────────────────────────────────────────────────────────────

const DAYS: usize = 7;
const HOURS: usize = 24;
const CELLS: usize = DAYS * HOURS; // 168

// Default σ multiplier for spike detection.
const SIGMA_K: f64 = 2.0;

// ─── Cell ─────────────────────────────────────────────────────────────────────

struct Cell {
    queries:     AtomicU64,
    slow:        AtomicU64,
    duration_ms: AtomicU64,
}

impl Cell {
    const fn new() -> Self {
        Self {
            queries:     AtomicU64::new(0),
            slow:        AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
        }
    }
}

// ─── Serialisable snapshot ────────────────────────────────────────────────────

/// One cell in the heatmap grid.
#[derive(Serialize, Clone)]
pub struct HeatCell {
    pub day:         u8,   // 0=Sun … 6=Sat (matches JS Date.getDay())
    pub hour:        u8,   // 0 … 23
    pub queries:     u64,
    pub slow:        u64,
    pub avg_ms:      f64,
    pub is_anomaly:  bool,
}

/// Full API response.
#[derive(Serialize)]
pub struct HeatmapSnapshot {
    pub cells:   Vec<HeatCell>,
    pub anomaly_threshold: f64,
    pub total_queries: u64,
    pub total_slow:    u64,
    /// Top-3 peak (day, hour) cells by query count.
    pub peaks: Vec<HeatCell>,
}

// ─── HeatmapStore ─────────────────────────────────────────────────────────────

/// Global heatmap.  Allocated once; never dropped.
pub struct HeatmapStore {
    cells:           [Cell; CELLS],
    slow_threshold_ms: u64,
}

impl HeatmapStore {
    pub fn new(slow_threshold_ms: u64) -> Self {
        Self {
            cells: std::array::from_fn(|_| Cell::new()),
            slow_threshold_ms,
        }
    }

    /// Record one query.  `duration_ms` is the wall-clock latency.
    /// Called from the hot path; never blocks.
    pub fn record(&self, duration_ms: u64) {
        let (day, hour) = current_day_hour();
        let idx = day as usize * HOURS + hour as usize;
        let cell = &self.cells[idx];
        cell.queries.fetch_add(1, Ordering::Relaxed);
        cell.duration_ms.fetch_add(duration_ms, Ordering::Relaxed);
        if duration_ms >= self.slow_threshold_ms {
            cell.slow.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot all cells and compute anomaly flags.
    pub fn snapshot(&self) -> HeatmapSnapshot {
        // Load raw values.
        let mut cells: Vec<HeatCell> = (0..CELLS)
            .map(|idx| {
                let day  = (idx / HOURS) as u8;
                let hour = (idx % HOURS) as u8;
                let q    = self.cells[idx].queries.load(Ordering::Relaxed);
                let s    = self.cells[idx].slow.load(Ordering::Relaxed);
                let d    = self.cells[idx].duration_ms.load(Ordering::Relaxed);
                let avg  = if q > 0 { d as f64 / q as f64 } else { 0.0 };
                HeatCell { day, hour, queries: q, slow: s, avg_ms: avg, is_anomaly: false }
            })
            .collect();

        let total_queries: u64 = cells.iter().map(|c| c.queries).sum();
        let total_slow:    u64 = cells.iter().map(|c| c.slow).sum();

        // Anomaly detection — sigma method over query counts.
        let counts: Vec<f64> = cells.iter().map(|c| c.queries as f64).collect();
        let mean = counts.iter().sum::<f64>() / counts.len() as f64;
        let variance = counts.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / counts.len() as f64;
        let sigma = variance.sqrt();
        let threshold = mean + SIGMA_K * sigma;

        for cell in &mut cells {
            if sigma > 0.0 && cell.queries as f64 > threshold {
                cell.is_anomaly = true;
            }
        }

        // Top-3 peaks by query count.
        let mut sorted = cells.clone();
        sorted.sort_by(|a, b| b.queries.cmp(&a.queries));
        let peaks: Vec<HeatCell> = sorted.into_iter().filter(|c| c.queries > 0).take(3).collect();

        HeatmapSnapshot {
            cells,
            anomaly_threshold: threshold,
            total_queries,
            total_slow,
            peaks,
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Returns (weekday 0=Sun…6=Sat, hour 0…23) in local wall-clock time using UTC.
/// We use UTC for simplicity; the frontend can adjust for local timezone if needed.
fn current_day_hour() -> (u8, u8) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // secs since epoch → day of week and hour.
    // Days since epoch: epoch (1970-01-01) was a Thursday = day 4.
    let days_since_epoch = secs / 86400;
    let hour = ((secs % 86400) / 3600) as u8;
    let weekday = ((days_since_epoch + 4) % 7) as u8; // 0=Sun … 6=Sat
    (weekday, hour)
}
