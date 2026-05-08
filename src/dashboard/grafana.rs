//! Grafana Simple JSON datasource handlers.
//!
//! Implements the [Simple JSON datasource protocol][1] so TurbineProxy metrics
//! can be displayed in any Grafana dashboard without a separate Prometheus
//! scraper (though `/metrics` still works for that use-case).
//!
//! Endpoints mounted under `/grafana/`:
//!   GET  /grafana/          — datasource health check
//!   POST /grafana/search    — list available metric targets
//!   POST /grafana/query     — return time-series datapoints
//!   POST /grafana/annotations — regression alerts as Grafana annotations
//!   POST /grafana/tag-keys  — label keys for ad-hoc filtering
//!   POST /grafana/tag-values — label values for a given key
//!
//! [1]: https://grafana.com/grafana/plugins/grafana-simple-json-datasource/
//!
//! **Available targets (time-series)**
//!
//! | Target              | Source                  | Unit          |
//! |---------------------|-------------------------|---------------|
//! | `queries`           | TimeseriesStore         | queries/min   |
//! | `slow_queries`      | TimeseriesStore         | queries/min   |
//! | `avg_latency_ms`    | TimeseriesStore         | milliseconds  |
//! | `max_latency_ms`    | TimeseriesStore         | milliseconds  |
//! | `connections_active`| ProxyMetrics (live)     | connections   |
//! | `connections_total` | ProxyMetrics (live)     | connections   |
//! | `queries_total`     | ProxyMetrics (live)     | queries       |
//! | `queries_read`      | ProxyMetrics (live)     | queries       |
//! | `queries_write`     | ProxyMetrics (live)     | queries       |

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::AppState;

// ── Health check ─────────────────────────────────────────────────────────────

pub async fn health() -> StatusCode {
    StatusCode::OK
}

// ── /grafana/search ───────────────────────────────────────────────────────────

/// Grafana sends `{ "target": "..." }` — we ignore the filter and return all.
#[derive(Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    #[allow(dead_code)]
    pub target: String,
}

pub async fn search(_body: Json<SearchRequest>) -> Json<Vec<&'static str>> {
    Json(vec![
        "queries",
        "slow_queries",
        "avg_latency_ms",
        "max_latency_ms",
        "connections_active",
        "connections_total",
        "queries_total",
        "queries_read",
        "queries_write",
    ])
}

// ── /grafana/query ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct QueryRequest {
    pub range: TimeRange,
    /// Maximum number of data points the panel can consume.
    #[serde(rename = "maxDataPoints", default = "default_max_datapoints")]
    pub max_datapoints: usize,
    pub targets: Vec<Target>,
}

#[derive(Deserialize)]
pub struct TimeRange {
    pub from: String,
    pub to: String,
}

#[derive(Deserialize)]
pub struct Target {
    pub target: String,
    #[serde(rename = "type", default = "default_target_type")]
    #[allow(dead_code)]
    pub kind: String,
}

fn default_max_datapoints() -> usize { 500 }
fn default_target_type() -> String { "timeserie".to_string() }

/// A single time-series result.
#[derive(Serialize)]
pub struct TimeserieResult {
    pub target: String,
    /// Each element is `[value, unix_ms]`.
    pub datapoints: Vec<[f64; 2]>,
}

pub async fn query(
    State(state): State<AppState>,
    Json(req): Json<QueryRequest>,
) -> Json<Vec<TimeserieResult>> {
    let from_unix = parse_iso_to_unix_secs(&req.range.from).unwrap_or(0);
    let to_unix   = parse_iso_to_unix_secs(&req.range.to)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());

    // Choose resolution automatically based on the requested range.
    let range_secs = (to_unix - from_unix).max(1);
    let resolution = auto_resolution(range_secs, req.max_datapoints);

    let now_ms = chrono::Utc::now().timestamp_millis() as f64;

    let mut results = Vec::with_capacity(req.targets.len());

    for t in &req.targets {
        let datapoints: Vec<[f64; 2]> = match t.target.as_str() {
            // ── Time-series metrics from TimeseriesStore ─────────────────────
            name @ ("queries" | "slow_queries" | "avg_latency_ms" | "max_latency_ms") => {
                match &state.timeseries {
                    None => vec![],
                    Some(ts) => {
                        let pts = ts
                            .query_range(resolution, from_unix, to_unix, req.max_datapoints)
                            .unwrap_or_default();
                        pts.iter().map(|p| {
                            let val = match name {
                                "queries"         => p.queries as f64,
                                "slow_queries"    => p.slow_queries as f64,
                                "avg_latency_ms"  => p.avg_us / 1_000.0,
                                "max_latency_ms"  => p.max_us as f64 / 1_000.0,
                                _                 => 0.0,
                            };
                            [val, (p.bucket_unix * 1000) as f64]
                        }).collect()
                    }
                }
            }

            // ── Live single-point metrics from ProxyMetrics ──────────────────
            "connections_active" => {
                let v = state.metrics.connections_active.load(Ordering::Relaxed) as f64;
                vec![[v, now_ms]]
            }
            "connections_total" => {
                let v = state.metrics.connections_total.load(Ordering::Relaxed) as f64;
                vec![[v, now_ms]]
            }
            "queries_total" => {
                let v = state.metrics.queries_total.load(Ordering::Relaxed) as f64;
                vec![[v, now_ms]]
            }
            "queries_read" => {
                let v = state.metrics.queries_read.load(Ordering::Relaxed) as f64;
                vec![[v, now_ms]]
            }
            "queries_write" => {
                let v = state.metrics.queries_write.load(Ordering::Relaxed) as f64;
                vec![[v, now_ms]]
            }

            _ => vec![],
        };

        results.push(TimeserieResult {
            target: t.target.clone(),
            datapoints,
        });
    }

    Json(results)
}

// ── /grafana/annotations ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AnnotationsRequest {
    pub range: TimeRange,
    pub annotation: AnnotationQuery,
}

#[derive(Deserialize)]
pub struct AnnotationQuery {
    pub name: String,
}

#[derive(Serialize)]
pub struct Annotation {
    pub annotation: AnnotationMeta,
    pub time: i64,
    pub title: String,
    pub tags: Vec<String>,
    pub text: String,
}

/// Grafana requires the annotation object to be echoed back in each item.
#[derive(Clone, Serialize)]
pub struct AnnotationMeta {
    pub name: String,
}

pub async fn annotations(
    State(state): State<AppState>,
    Json(req): Json<AnnotationsRequest>,
) -> Json<Vec<Annotation>> {
    let from_ms = parse_iso_to_unix_secs(&req.range.from).unwrap_or(0) * 1000;
    let to_ms   = parse_iso_to_unix_secs(&req.range.to)
        .unwrap_or_else(|| chrono::Utc::now().timestamp()) * 1000;

    let meta = AnnotationMeta { name: req.annotation.name.clone() };

    let alerts = state.regression_store.snapshot();
    let items: Vec<Annotation> = alerts
        .iter()
        .filter(|a| {
            let ms = a.detected_at_ms;
            ms >= from_ms && ms <= to_ms
        })
        .map(|a| {
            let kind_str = match &a.details {
                crate::proxy::regression::AlertKind::LatencyRegression { .. } => "latency_regression",
                crate::proxy::regression::AlertKind::HotKey { .. }            => "hot_key",
                crate::proxy::regression::AlertKind::FullScanRisk { .. }      => "full_scan_risk",
            };
            let title = format!(
                "[{}] {}",
                kind_str.replace('_', " "),
                &a.fingerprint[..a.fingerprint.len().min(60)]
            );
            let text = match &a.details {
                crate::proxy::regression::AlertKind::LatencyRegression { baseline_p95_us, current_p95_us, ratio } =>
                    format!("p95 {:.1}ms → {:.1}ms (+{:.0}%)",
                        *baseline_p95_us as f64 / 1_000.0,
                        *current_p95_us as f64 / 1_000.0,
                        (ratio - 1.0) * 100.0),
                crate::proxy::regression::AlertKind::HotKey { example_sql, hit_count } =>
                    format!("{}× — {}", hit_count, &example_sql[..example_sql.len().min(120)]),
                crate::proxy::regression::AlertKind::FullScanRisk { reason, call_count } =>
                    format!("{} ({} calls)", reason, call_count),
            };
            let mut tags = vec![kind_str.to_string()];
            if a.resolved { tags.push("resolved".to_string()); }

            Annotation {
                annotation: meta.clone(),
                time: a.detected_at_ms,
                title,
                tags,
                text,
            }
        })
        .collect();

    Json(items)
}

// ── /grafana/tag-keys ─────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct TagKey {
    pub r#type: &'static str,
    pub text: &'static str,
}

pub async fn tag_keys() -> Json<Vec<TagKey>> {
    Json(vec![
        TagKey { r#type: "string", text: "metric" },
        TagKey { r#type: "string", text: "resolution" },
    ])
}

// ── /grafana/tag-values ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TagValuesRequest {
    pub key: String,
}

#[derive(Serialize)]
pub struct TagValueItem {
    pub text: String,
}

pub async fn tag_values(Json(req): Json<TagValuesRequest>) -> Json<Vec<TagValueItem>> {
    let values: &[&str] = match req.key.as_str() {
        "metric" => &[
            "queries", "slow_queries", "avg_latency_ms", "max_latency_ms",
            "connections_active", "connections_total",
            "queries_total", "queries_read", "queries_write",
        ],
        "resolution" => &["1m", "1h", "1d"],
        _ => &[],
    };
    Json(values.iter().map(|v| TagValueItem { text: v.to_string() }).collect())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse an ISO 8601 timestamp string (Grafana sends these in UTC) to unix
/// seconds.  Returns `None` if the string cannot be parsed.
fn parse_iso_to_unix_secs(s: &str) -> Option<i64> {
    s.parse::<DateTime<Utc>>().ok().map(|dt| dt.timestamp())
}

/// Pick the best resolution based on the time range width and the panel's
/// `maxDataPoints` budget:
/// - range ≤ 2 h and budget ≥ 120 → `"1m"`
/// - range ≤ 7 days and budget ≥ 24  → `"1h"`
/// - otherwise                        → `"1d"`
fn auto_resolution(range_secs: i64, max_points: usize) -> &'static str {
    const TWO_HOURS:  i64 = 2 * 3600;
    const SEVEN_DAYS: i64 = 7 * 86400;
    if range_secs <= TWO_HOURS && max_points >= 120 {
        "1m"
    } else if range_secs <= SEVEN_DAYS && max_points >= 24 {
        "1h"
    } else {
        "1d"
    }
}
