//! Active regression detection — compares current query latency against a
//! rolling baseline and surfaces three classes of alert:
//!
//! 1. **LatencyRegression** — a fingerprint's current p95 is ≥ 40% above its
//!    baseline p95 (updated every check cycle).
//! 2. **HotKey** — the same exact SQL string (literals included) was executed
//!    ≥ 30 times within a single client session.
//! 3. **FullScanRisk** — static analysis of the fingerprint matches patterns
//!    that indicate a likely full table scan (no WHERE, ORDER BY RAND(), etc.).
//!
//! # Thread-safety
//! `RegressionStore` uses `std::sync::Mutex` so it can be called from both
//! async (`report_hot_key`) and blocking (`check`) contexts without holding a
//! lock across await points.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use serde::Serialize;

// ── Constants ────────────────────────────────────────────────────────────────

/// A fingerprint whose current p95 is this many times its baseline triggers an alert.
const LATENCY_RATIO_THRESHOLD: f64 = 1.40;

/// Minimum execution count before a fingerprint is eligible for regression checking.
const MIN_SAMPLES: u64 = 10;

/// Maximum alerts kept in memory (oldest resolved are evicted first).
const MAX_ALERTS: usize = 200;

// ── Alert model ──────────────────────────────────────────────────────────────

/// The specific details for each alert kind (internally tagged for clean JSON).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertKind {
    /// Current p95 latency is significantly above the recent baseline.
    LatencyRegression {
        baseline_p95_us: i64,
        current_p95_us: i64,
        /// `current_p95_us / baseline_p95_us`
        ratio: f64,
    },
    /// The same exact SQL string was executed many times in one client session.
    HotKey {
        /// The repeated SQL (truncated to 300 chars).
        example_sql: String,
        hit_count: u64,
    },
    /// Static analysis flagged this fingerprint as a potential full table scan.
    FullScanRisk { reason: String, call_count: u64 },
}

/// A single regression alert.
#[derive(Debug, Clone, Serialize)]
pub struct RegressionAlert {
    pub id: u64,
    pub fingerprint: String,
    pub details: AlertKind,
    pub detected_at_ms: i64,
    /// `true` once the condition is no longer present (latency dropped back).
    pub resolved: bool,
}

// ── Internal baseline ────────────────────────────────────────────────────────

struct FpBaseline {
    p95_us: u64,
}

// ── RegressionStore ──────────────────────────────────────────────────────────

/// Shared regression alert store — cheap to clone (Arc-backed).
pub struct RegressionStore {
    alerts: Mutex<Vec<RegressionAlert>>,
    baseline: Mutex<HashMap<u64, FpBaseline>>,
    next_id: AtomicU64,
}

impl RegressionStore {
    pub fn new() -> Self {
        Self {
            alerts: Mutex::new(Vec::new()),
            baseline: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    // ── Called from handle_connection (async, lock held only briefly) ─────────

    /// Report a hot-key detected in a session (same exact SQL > threshold).
    /// Deduplicates: updates an existing active alert rather than adding a duplicate.
    pub fn report_hot_key(&self, fingerprint: &str, example_sql: &str, hit_count: u64) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut alerts = self.alerts.lock();
        for a in alerts.iter_mut() {
            if a.fingerprint == fingerprint
                && matches!(&a.details, AlertKind::HotKey { .. })
                && !a.resolved
            {
                a.details = AlertKind::HotKey {
                    example_sql: example_sql.chars().take(300).collect(),
                    hit_count,
                };
                return;
            }
        }
        self.push_alert_inner(
            &mut alerts,
            fingerprint,
            AlertKind::HotKey {
                example_sql: example_sql.chars().take(300).collect(),
                hit_count,
            },
            now_ms,
        );
    }

    // ── Called from the background check task (every 5 min) ──────────────────

    /// Compare `current` in-memory stats against the stored baseline and update
    /// the alert list. Also runs static full-scan heuristics on each fingerprint.
    pub fn check(&self, current: &[crate::analytics::collector::QueryStats]) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut baseline = self.baseline.lock();
        let mut alerts = self.alerts.lock();

        // Collect active fingerprints for auto-resolve pass.
        let active_fps: std::collections::HashSet<&str> =
            current.iter().map(|s| s.fingerprint.as_str()).collect();

        // Auto-resolve latency-regression alerts whose fingerprint is gone.
        for a in alerts.iter_mut() {
            if matches!(&a.details, AlertKind::LatencyRegression { .. })
                && !a.resolved
                && !active_fps.contains(a.fingerprint.as_str())
            {
                a.resolved = true;
            }
        }

        for stat in current {
            // ── Full scan heuristics (static, no EXPLAIN needed) ─────────────
            if let Some(reason) = full_scan_reason(&stat.fingerprint) {
                let already = alerts.iter().any(|a| {
                    a.fingerprint == stat.fingerprint
                        && matches!(&a.details, AlertKind::FullScanRisk { .. })
                        && !a.resolved
                });
                if !already {
                    self.push_alert_inner(
                        &mut alerts,
                        &stat.fingerprint,
                        AlertKind::FullScanRisk {
                            reason: reason.to_string(),
                            call_count: stat.count,
                        },
                        now_ms,
                    );
                } else {
                    // Keep call_count fresh.
                    for a in alerts.iter_mut() {
                        if a.fingerprint == stat.fingerprint {
                            if let AlertKind::FullScanRisk { call_count, .. } = &mut a.details {
                                *call_count = stat.count;
                            }
                        }
                    }
                }
            }

            // ── Latency regression ────────────────────────────────────────────
            if stat.count < MIN_SAMPLES {
                baseline.insert(
                    stat.hash,
                    FpBaseline {
                        p95_us: stat.p95().as_micros() as u64,
                    },
                );
                continue;
            }

            let current_p95_us = stat.p95().as_micros() as u64;
            if current_p95_us == 0 {
                continue;
            }

            if let Some(base) = baseline.get(&stat.hash) {
                if base.p95_us > 0 {
                    let ratio = current_p95_us as f64 / base.p95_us as f64;
                    if ratio >= LATENCY_RATIO_THRESHOLD {
                        // Upsert active alert.
                        let mut found = false;
                        for a in alerts.iter_mut() {
                            if a.fingerprint == stat.fingerprint
                                && matches!(&a.details, AlertKind::LatencyRegression { .. })
                                && !a.resolved
                            {
                                a.details = AlertKind::LatencyRegression {
                                    baseline_p95_us: base.p95_us as i64,
                                    current_p95_us: current_p95_us as i64,
                                    ratio,
                                };
                                a.detected_at_ms = now_ms;
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            self.push_alert_inner(
                                &mut alerts,
                                &stat.fingerprint,
                                AlertKind::LatencyRegression {
                                    baseline_p95_us: base.p95_us as i64,
                                    current_p95_us: current_p95_us as i64,
                                    ratio,
                                },
                                now_ms,
                            );
                        }
                    } else {
                        // Resolve existing latency alert if latency recovered.
                        for a in alerts.iter_mut() {
                            if a.fingerprint == stat.fingerprint
                                && matches!(&a.details, AlertKind::LatencyRegression { .. })
                                && !a.resolved
                            {
                                a.resolved = true;
                            }
                        }
                    }
                }
            }
            // Slide the baseline forward so future checks compare against recent state.
            baseline.insert(
                stat.hash,
                FpBaseline {
                    p95_us: current_p95_us,
                },
            );
        }
    }

    // ── Dashboard read ────────────────────────────────────────────────────────

    /// Return up to 100 alerts: active first (sorted by detected_at desc), then resolved.
    pub fn snapshot(&self) -> Vec<RegressionAlert> {
        let alerts = self.alerts.lock();
        let mut result: Vec<RegressionAlert> = alerts.clone();
        result.sort_by(|a, b| {
            // Active before resolved; within each group, newest first.
            b.resolved
                .cmp(&a.resolved)
                .reverse()
                .then(b.detected_at_ms.cmp(&a.detected_at_ms))
        });
        result.truncate(100);
        result
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn push_alert_inner(
        &self,
        alerts: &mut Vec<RegressionAlert>,
        fingerprint: &str,
        details: AlertKind,
        now_ms: i64,
    ) {
        if alerts.len() >= MAX_ALERTS {
            // Evict the oldest resolved alert to make room.
            if let Some(pos) = alerts.iter().position(|a| a.resolved) {
                alerts.remove(pos);
            } else {
                return; // all active — skip until some resolve
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        alerts.push(RegressionAlert {
            id,
            fingerprint: fingerprint.to_string(),
            details,
            detected_at_ms: now_ms,
            resolved: false,
        });
    }
}

// ── Static full-scan heuristics ───────────────────────────────────────────────

fn full_scan_reason(fingerprint: &str) -> Option<&'static str> {
    let fp = fingerprint.trim_start().to_ascii_lowercase();
    // SELECT without WHERE and without LIMIT (likely full table scan).
    if fp.starts_with("select")
        && fp.contains(" from ")
        && !fp.contains("where")
        && !fp.contains("limit")
        && !fp.contains("dual")
    // SELECT 1 FROM DUAL etc.
    {
        return Some("SELECT without WHERE or LIMIT — potential full table scan");
    }
    // ORDER BY RAND() / RANDOM() — forces filesort over full result.
    if fp.contains("order by rand()") || fp.contains("order by random()") {
        return Some("ORDER BY RAND() — forces full scan for random ordering");
    }
    // LIKE patterns (fingerprinter replaces literals with ?, so we flag all LIKE).
    if fp.contains(" like ") {
        return Some("LIKE pattern — verify no leading wildcard (e.g. LIKE '%value')");
    }
    None
}
