//! Prometheus text-format exposition.
//!
//! `render()` is called on every scrape of `GET /metrics`.  It reads all
//! in-memory counters and histograms plus an async snapshot of the backend
//! pool, then formats everything as the Prometheus text exposition format
//! (version 0.0.4).
//!
//! No external crate is required — the format is trivially simple text.

use std::fmt::Write as _;
use std::sync::atomic::Ordering;

use crate::proxy::histogram::BUCKET_BOUNDS;
use crate::proxy::pool::BackendPool;
use crate::proxy::server::ProxyMetrics;

/// Render all metrics as a Prometheus text exposition document.
///
/// The function is `async` because reading pool idle-connection counts requires
/// briefly locking the pool's `Mutex<Vec<…>>`.
pub async fn render(metrics: &ProxyMetrics, pool: &BackendPool) -> String {
    let mut out = String::with_capacity(8192);

    // ── turbineproxy_build_info ─────────────────────────────────────────────
    out.push_str("# HELP turbineproxy_build_info Static build information.\n");
    out.push_str("# TYPE turbineproxy_build_info gauge\n");
    writeln!(
        out,
        "turbineproxy_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    )
    .ok();

    // ── turbineproxy_connections_total ──────────────────────────────────────
    out.push_str("\n# HELP turbineproxy_connections_total Total TCP client connections accepted since start.\n");
    out.push_str("# TYPE turbineproxy_connections_total counter\n");
    writeln!(
        out,
        "turbineproxy_connections_total {}",
        metrics.connections_total.load(Ordering::Relaxed)
    )
    .ok();

    // ── turbineproxy_connections_active ────────────────────────────────────
    out.push_str(
        "\n# HELP turbineproxy_connections_active Number of currently open client connections.\n",
    );
    out.push_str("# TYPE turbineproxy_connections_active gauge\n");
    writeln!(
        out,
        "turbineproxy_connections_active {}",
        metrics.connections_active.load(Ordering::Relaxed)
    )
    .ok();

    // ── turbineproxy_queries_total ──────────────────────────────────────────
    let queries_read = metrics.queries_read.load(Ordering::Relaxed);
    let queries_write = metrics.queries_write.load(Ordering::Relaxed);
    let queries_total = metrics.queries_total.load(Ordering::Relaxed);
    let queries_other = queries_total.saturating_sub(queries_read + queries_write);

    out.push_str(
        "\n# HELP turbineproxy_queries_total Total queries processed, partitioned by intent.\n",
    );
    out.push_str("# TYPE turbineproxy_queries_total counter\n");
    writeln!(
        out,
        "turbineproxy_queries_total{{intent=\"read\"}}  {queries_read}"
    )
    .ok();
    writeln!(
        out,
        "turbineproxy_queries_total{{intent=\"write\"}} {queries_write}"
    )
    .ok();
    writeln!(
        out,
        "turbineproxy_queries_total{{intent=\"other\"}} {queries_other}"
    )
    .ok();

    // ── turbineproxy_query_duration_seconds (histogram) ─────────────────────
    out.push_str("\n# HELP turbineproxy_query_duration_seconds End-to-end query execution time in seconds.\n");
    out.push_str("# TYPE turbineproxy_query_duration_seconds histogram\n");

    for (intent, hist) in [("read", &metrics.read_hist), ("write", &metrics.write_hist)] {
        let (counts, sum, count) = hist.snapshot();
        for (i, &bound) in BUCKET_BOUNDS.iter().enumerate() {
            // Use a format that avoids trailing ".0" for whole numbers.
            let le = format_bound(bound);
            writeln!(
                out,
                "turbineproxy_query_duration_seconds_bucket{{intent=\"{intent}\",le=\"{le}\"}} {}",
                counts[i]
            )
            .ok();
        }
        writeln!(
            out,
            "turbineproxy_query_duration_seconds_bucket{{intent=\"{intent}\",le=\"+Inf\"}} {}",
            counts[11]
        )
        .ok();
        writeln!(
            out,
            "turbineproxy_query_duration_seconds_sum{{intent=\"{intent}\"}} {sum:.6}"
        )
        .ok();
        writeln!(
            out,
            "turbineproxy_query_duration_seconds_count{{intent=\"{intent}\"}} {count}"
        )
        .ok();
    }

    // ── Per-backend pool metrics ─────────────────────────────────────────────
    let backends = pool.backend_stats().await;

    out.push_str("\n# HELP turbineproxy_pool_connections Backend connection pool size by state.\n");
    out.push_str("# TYPE turbineproxy_pool_connections gauge\n");
    for b in &backends {
        writeln!(
            out,
            "turbineproxy_pool_connections{{backend=\"{}\",role=\"{}\",state=\"idle\"}} {}",
            b.addr, b.role, b.idle
        )
        .ok();
        writeln!(
            out,
            "turbineproxy_pool_connections{{backend=\"{}\",role=\"{}\",state=\"in_use\"}} {}",
            b.addr, b.role, b.in_use
        )
        .ok();
    }

    out.push_str("\n# HELP turbineproxy_pool_connections_created_total Total backend connections ever opened.\n");
    out.push_str("# TYPE turbineproxy_pool_connections_created_total counter\n");
    for b in &backends {
        writeln!(
            out,
            "turbineproxy_pool_connections_created_total{{backend=\"{}\",role=\"{}\"}} {}",
            b.addr, b.role, b.created
        )
        .ok();
    }

    out.push_str("\n# HELP turbineproxy_pool_connections_evicted_total Idle connections discarded (exceeded max_idle).\n");
    out.push_str("# TYPE turbineproxy_pool_connections_evicted_total counter\n");
    for b in &backends {
        writeln!(
            out,
            "turbineproxy_pool_connections_evicted_total{{backend=\"{}\",role=\"{}\"}} {}",
            b.addr, b.role, b.evicted
        )
        .ok();
    }

    // ── turbineproxy_replica_lag_seconds ────────────────────────────────────
    out.push_str("\n# HELP turbineproxy_replica_lag_seconds Measured replication lag in seconds (replicas only).\n");
    out.push_str("# TYPE turbineproxy_replica_lag_seconds gauge\n");
    for b in &backends {
        if b.role == "replica" {
            let lag_secs = b.lag_ms as f64 / 1000.0;
            writeln!(
                out,
                "turbineproxy_replica_lag_seconds{{backend=\"{}\"}} {lag_secs:.3}",
                b.addr
            )
            .ok();
        }
    }

    // ── turbineproxy_backend_healthy ────────────────────────────────────────
    out.push_str("\n# HELP turbineproxy_backend_healthy 1 if the backend passed its last health check, 0 otherwise.\n");
    out.push_str("# TYPE turbineproxy_backend_healthy gauge\n");
    for b in &backends {
        writeln!(
            out,
            "turbineproxy_backend_healthy{{backend=\"{}\",role=\"{}\"}} {}",
            b.addr,
            b.role,
            if b.healthy { 1 } else { 0 }
        )
        .ok();
    }

    // ── turbineproxy_sqli_blocked_total ─────────────────────────────────────
    out.push_str("\n# HELP turbineproxy_sqli_blocked_total Total queries blocked by the SQL injection protection filter.\n");
    out.push_str("# TYPE turbineproxy_sqli_blocked_total counter\n");
    writeln!(
        out,
        "turbineproxy_sqli_blocked_total {}",
        metrics.sqli_blocked.load(Ordering::Relaxed)
    )
    .ok();

    // ── HA / failover metrics ────────────────────────────────────────────────
    let pool_stats = pool.pool_stats().await;

    out.push_str("\n# HELP turbineproxy_ha_failover_active 1 when an HA failover replica is currently serving as primary, 0 otherwise.\n");
    out.push_str("# TYPE turbineproxy_ha_failover_active gauge\n");
    writeln!(
        out,
        "turbineproxy_ha_failover_active {}",
        if pool_stats.failover_active { 1 } else { 0 }
    )
    .ok();

    out.push_str("\n# HELP turbineproxy_ha_failover_events_total Total number of HA failovers triggered since process start.\n");
    out.push_str("# TYPE turbineproxy_ha_failover_events_total counter\n");
    writeln!(
        out,
        "turbineproxy_ha_failover_events_total {}",
        pool_stats.failover_events_total
    )
    .ok();

    out
}

/// Format a bucket upper bound without unnecessary trailing digits.
/// `1.0` → `"1"`, `0.001` → `"0.001"`, `2.5` → `"2.5"`.
fn format_bound(v: f64) -> String {
    // If the value is a whole number, omit the decimal point.
    if v.fract() == 0.0 {
        format!("{}", v as u64)
    } else {
        // Strip trailing zeros but keep at least one decimal place.
        let s = format!("{v}");
        s
    }
}
