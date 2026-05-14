//! PostgreSQL active health checker — Phase 2.
//!
//! Runs on an interval for each configured backend:
//! - **Primary** — `SELECT 1` ping; `pg_is_in_recovery()` must return `f`.
//!   Triggers pool failover when `primary_failover_threshold` consecutive checks fail.
//! - **Each replica** — `SELECT 1` ping + replication lag via
//!   `SELECT EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp()))::bigint`.
//!   Marks replica unhealthy if lag > `max_replica_lag_ms`.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{BackendConfig, PgsqlConfig};
use crate::protocol::DatabaseProtocol;
use crate::proxy::pool::BackendPool;

// ─── PgHealthChecker ──────────────────────────────────────────────────────────

pub struct PgHealthChecker {
    pool: Arc<BackendPool>,
    protocol: Arc<dyn DatabaseProtocol>,
    primary_config: BackendConfig,
    replica_configs: Vec<BackendConfig>,
    max_lag_ms: u64,
    failover_threshold: u32,
    interval: Duration,
    patroni_check: bool,
    patroni_api_port: u16,
    health_check_database: String,
    cooldown_secs: u64,
    min_recovery_checks: u32,
}

impl PgHealthChecker {
    pub fn new(
        pool: Arc<BackendPool>,
        protocol: Arc<dyn DatabaseProtocol>,
        cfg: &PgsqlConfig,
    ) -> Option<Self> {
        let primary_config = cfg.primary.clone()?;
        Some(Self {
            pool,
            protocol,
            primary_config,
            replica_configs: cfg.replicas.clone(),
            max_lag_ms: cfg.max_replica_lag_ms,
            failover_threshold: cfg.primary_failover_threshold,
            interval: Duration::from_secs(cfg.health_check_interval_secs.max(1)),
            patroni_check: cfg.patroni_check,
            patroni_api_port: cfg.patroni_api_port,
            health_check_database: cfg.health_check_database.trim().to_string(),
            cooldown_secs: cfg.failover_cooldown_secs,
            min_recovery_checks: cfg.failover_min_recovery_checks,
        })
    }

    fn control_db_config(&self, base: &BackendConfig) -> BackendConfig {
        let mut cfg = base.clone();
        if !self.health_check_database.is_empty() {
            cfg.database = Some(self.health_check_database.clone());
        }
        cfg
    }

    /// Run forever — spawn as a background Tokio task.
    pub async fn run(self) {
        log::info!(
            "[pg health] checker started — interval={}s, max_replica_lag={}ms, failover_threshold={}",
            self.interval.as_secs(),
            self.max_lag_ms,
            self.failover_threshold,
        );

        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            self.check_primary().await;
            for (idx, cfg) in self.replica_configs.iter().enumerate() {
                self.check_replica(idx, cfg).await;
            }
        }
    }

    // ── Primary ───────────────────────────────────────────────────────────────

    async fn check_primary(&self) {
        let control_cfg = self.control_db_config(&self.primary_config);
        let ok = self.ping_and_check_primary(&control_cfg).await;

        if ok {
            let prev = self
                .pool
                .primary_health
                .consecutive_failures
                .swap(0, Ordering::Relaxed);
            let was_down = !self
                .pool
                .primary_health
                .healthy
                .swap(true, Ordering::Relaxed);

            let had_failover = self.pool.failover_idx.load(Ordering::Relaxed) >= 0;

            if had_failover {
                let recovery_count = self.pool.recovery_checks.fetch_add(1, Ordering::Relaxed) + 1;

                if recovery_count < self.min_recovery_checks as usize {
                    log::info!(
                        "[pg health] Primary {} responding ({}/{} recovery checks) — failover still active",
                        self.primary_config.addr,
                        recovery_count,
                        self.min_recovery_checks,
                    );
                    return;
                }

                let triggered_at = self.pool.failover_triggered_at.load(Ordering::Relaxed);
                if triggered_at > 0 && self.cooldown_secs > 0 {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let elapsed = now.saturating_sub(triggered_at);
                    if elapsed < self.cooldown_secs {
                        log::warn!(
                            "[pg health] Primary {} recovered but cooldown active ({}/{}s) — failover held",
                            self.primary_config.addr,
                            elapsed,
                            self.cooldown_secs,
                        );
                        return;
                    }
                }

                self.pool.failover_idx.store(-1, Ordering::Relaxed);
                self.pool.recovery_checks.store(0, Ordering::Relaxed);
                self.pool.failover_triggered_at.store(0, Ordering::Relaxed);
                log::info!(
                    "[pg health] Primary {} recovered — failover cleared",
                    self.primary_config.addr
                );
            } else if was_down || prev >= self.failover_threshold {
                self.pool.recovery_checks.store(0, Ordering::Relaxed);
            }
        } else {
            let failures = self
                .pool
                .primary_health
                .consecutive_failures
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            self.pool
                .primary_health
                .healthy
                .store(false, Ordering::Relaxed);

            log::warn!(
                "[pg health] Primary {} unreachable ({}/{})",
                self.primary_config.addr,
                failures,
                self.failover_threshold,
            );

            if failures >= self.failover_threshold
                && self.pool.failover_idx.load(Ordering::Relaxed) < 0
            {
                self.trigger_failover();
            }
        }
    }

    async fn ping_and_check_primary(&self, config: &BackendConfig) -> bool {
        // ── Patroni API check (optional) ──────────────────────────────────────
        if self.patroni_check {
            let host = config.addr.split(':').next().unwrap_or("127.0.0.1");
            let url = format!("http://{}:{}/patroni", host, self.patroni_api_port);
            match tokio::time::timeout(Duration::from_secs(3), reqwest::get(&url)).await {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        let role = json["role"].as_str().unwrap_or("");
                        let is_leader = role == "master" || role == "primary" || role == "leader";
                        if !is_leader {
                            log::warn!(
                                "[pg health] Patroni reports primary {} as role='{}' (not leader)",
                                config.addr,
                                role
                            );
                            return false;
                        }
                    }
                }
                Ok(Err(e)) => {
                    log::warn!("[pg health] Patroni API request to {} failed: {}", url, e);
                    // Non-fatal: fall back to pg_is_in_recovery() check
                }
                Err(_) => {
                    log::warn!("[pg health] Patroni API request to {} timed out", url);
                }
                _ => {}
            }
        }

        // ── PostgreSQL wire-protocol check ────────────────────────────────────
        match self.protocol.connect_backend(config).await {
            Err(_) => false,
            Ok(mut conn) => {
                if conn.ping().await.is_err() {
                    return false;
                }
                // `pg_is_in_recovery()` must return 'f' for a writable primary
                match conn.execute_query(b"SELECT pg_is_in_recovery()").await {
                    Ok(resp) => {
                        !resp.is_error
                            && !response_contains(&resp.bytes, b"true")
                            && !response_contains(&resp.bytes, b"t")
                    }
                    Err(_) => false,
                }
            }
        }
    }

    fn trigger_failover(&self) {
        // Detect flapping.
        let prev_triggered_at = self.pool.failover_triggered_at.load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if prev_triggered_at > 0 && self.cooldown_secs > 0 {
            let elapsed = now.saturating_sub(prev_triggered_at);
            if elapsed < self.cooldown_secs * 2 {
                self.pool
                    .failover_flap_total
                    .fetch_add(1, Ordering::Relaxed);
                log::warn!(
                    "[pg health] Failover FLAP detected — re-triggering {}s after last failover (cooldown={}s)",
                    elapsed,
                    self.cooldown_secs,
                );
            }
        }

        self.pool
            .failover_triggered_at
            .store(now, Ordering::Relaxed);
        self.pool.recovery_checks.store(0, Ordering::Relaxed);

        let best = self
            .replica_configs
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                if *i < self.pool.replica_health.len() {
                    self.pool.replica_health[*i].healthy.load(Ordering::Relaxed)
                } else {
                    false
                }
            })
            .min_by_key(|(i, _)| {
                if *i < self.pool.replica_health.len() {
                    self.pool.replica_health[*i].lag_ms.load(Ordering::Relaxed)
                } else {
                    u64::MAX
                }
            });

        match best {
            Some((idx, cfg)) => {
                self.pool.failover_idx.store(idx as i64, Ordering::Relaxed);
                self.pool
                    .failover_events_total
                    .fetch_add(1, Ordering::Relaxed);
                log::error!(
                    "[pg health] FAILOVER: primary {} down after {} checks — promoting replica [{}] {} (total failovers: {})",
                    self.primary_config.addr, self.failover_threshold, idx, cfg.addr,
                    self.pool.failover_events_total.load(Ordering::Relaxed),
                );
            }
            None => {
                log::error!(
                    "[pg health] PRIMARY DOWN ({}) — no healthy replica for failover",
                    self.primary_config.addr,
                );
            }
        }
    }

    // ── Replicas ──────────────────────────────────────────────────────────────

    async fn check_replica(&self, idx: usize, config: &BackendConfig) {
        if idx >= self.pool.replica_health.len() {
            return;
        }

        // ── Patroni role check (optional) ─────────────────────────────────────
        if self.patroni_check {
            let host = config.addr.split(':').next().unwrap_or("127.0.0.1");
            let url = format!("http://{}:{}/patroni", host, self.patroni_api_port);
            match tokio::time::timeout(Duration::from_secs(3), reqwest::get(&url)).await {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        let role = json["role"].as_str().unwrap_or("");
                        let is_replica =
                            role == "replica" || role == "standby" || role == "standby_leader";
                        if !is_replica {
                            log::warn!("[pg health] Patroni reports replica [{}] {} as role='{}' (not standby)", idx, config.addr, role);
                            self.pool.replica_health[idx]
                                .healthy
                                .store(false, Ordering::Relaxed);
                            self.pool.replica_health[idx]
                                .consecutive_failures
                                .fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                _ => { /* patroni unreachable — fall through to pg check */ }
            }
        }

        let control_cfg = self.control_db_config(config);

        match self.protocol.connect_backend(&control_cfg).await {
            Err(e) => {
                let failures = self.pool.replica_health[idx]
                    .consecutive_failures
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;
                let was_healthy = self.pool.replica_health[idx]
                    .healthy
                    .swap(false, Ordering::Relaxed);
                if was_healthy || failures == 1 {
                    log::warn!(
                        "[pg health] Replica [{}] {} unreachable: {}",
                        idx,
                        control_cfg.addr,
                        e
                    );
                }
            }
            Ok(mut conn) => {
                // Confirm this is actually a standby
                let in_recovery = match conn.execute_query(b"SELECT pg_is_in_recovery()").await {
                    Ok(r) => {
                        !r.is_error
                            && (response_contains(&r.bytes, b"t")
                                || response_contains(&r.bytes, b"true"))
                    }
                    Err(_) => true, // assume standby if we can't check
                };

                // Measure replication lag
                let lag_ms = if in_recovery {
                    pg_replica_lag_ms(&mut *conn).await.unwrap_or(0)
                } else {
                    0 // primary acting as "replica" slot — treat as zero lag
                };

                self.pool.replica_health[idx]
                    .lag_ms
                    .store(lag_ms, Ordering::Relaxed);
                self.pool.replica_health[idx]
                    .consecutive_failures
                    .store(0, Ordering::Relaxed);

                let healthy = lag_ms <= self.max_lag_ms;
                let was_healthy = self.pool.replica_health[idx]
                    .healthy
                    .swap(healthy, Ordering::Relaxed);

                match (was_healthy, healthy) {
                    (true, false) => log::warn!(
                        "[pg health] Replica [{}] {} lag {}ms > {}ms — removed from read pool",
                        idx,
                        config.addr,
                        lag_ms,
                        self.max_lag_ms,
                    ),
                    (false, true) => log::info!(
                        "[pg health] Replica [{}] {} lag {}ms — back in read pool",
                        idx,
                        config.addr,
                        lag_ms,
                    ),
                    _ => {}
                }
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Query `pg_last_xact_replay_timestamp()` and return lag in milliseconds.
async fn pg_replica_lag_ms(conn: &mut dyn crate::protocol::BackendConnection) -> Option<u64> {
    let resp = conn
        .execute_query(
            b"SELECT EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp()))::bigint",
        )
        .await
        .ok()?;
    if resp.is_error {
        return Some(0);
    }

    // The response bytes contain a text row with the lag in seconds (bigint).
    // Find a DataRow ('D') and extract the text value.
    extract_text_value(&resp.bytes)
        .and_then(|s| s.parse::<i64>().ok())
        .map(|secs| (secs.max(0) as u64) * 1000)
}

/// Scan PG response bytes for a text value in the first DataRow ('D').
fn extract_text_value(bytes: &[u8]) -> Option<String> {
    let mut pos = 0;
    while pos + 5 <= bytes.len() {
        let t = bytes[pos];
        let len = u32::from_be_bytes([
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
        ]) as usize;
        if len < 4 || pos + 1 + len > bytes.len() {
            break;
        }
        if t == b'D' {
            // DataRow: int16 field count, then per field: int32 len + bytes
            let payload = &bytes[pos + 5..pos + 1 + len];
            if payload.len() < 2 {
                break;
            }
            let field_count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
            if field_count == 0 {
                break;
            }
            let mut fp = 2;
            let field_len = i32::from_be_bytes([
                payload.get(fp).copied()?,
                payload.get(fp + 1).copied()?,
                payload.get(fp + 2).copied()?,
                payload.get(fp + 3).copied()?,
            ]);
            fp += 4;
            if field_len < 0 {
                return None;
            } // NULL
            let flen = field_len as usize;
            if fp + flen > payload.len() {
                break;
            }
            return String::from_utf8(payload[fp..fp + flen].to_vec()).ok();
        }
        pos += 1 + len;
    }
    None
}

/// Check if a byte sequence appears anywhere in the response bytes.
fn response_contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle)
}
