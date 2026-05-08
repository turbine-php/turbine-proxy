//! Active health checker — background task that keeps `BackendPool` health state fresh.
//!
//! Every `health_check_interval_secs`:
//! - **Primary**: pings via fresh connection. After `primary_failover_threshold` consecutive
//!   failures, promotes the healthiest replica as primary fallover (`failover_idx`).
//!   On recovery, clears the failover automatically.
//! - **Each replica**: opens a fresh connection and runs `SHOW REPLICA STATUS` to read
//!   `Seconds_Behind_Source` (MySQL 8.0.22+) or `Seconds_Behind_Master` (older).
//!   Marks the replica unhealthy if lag > `max_replica_lag_ms` or connection fails.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{BackendConfig, HaConfig};
use crate::protocol::DatabaseProtocol;
use crate::proxy::pool::BackendPool;

// ─── HealthChecker ────────────────────────────────────────────────────────────

pub struct HealthChecker {
    pool: Arc<BackendPool>,
    protocol: Arc<dyn DatabaseProtocol>,
    primary_config: BackendConfig,
    replica_configs: Vec<BackendConfig>,
    max_lag_ms: u64,
    failover_threshold: u32,
    interval: Duration,
    galera_check: bool,
}

impl HealthChecker {
    pub fn new(
        pool: Arc<BackendPool>,
        protocol: Arc<dyn DatabaseProtocol>,
        primary_config: BackendConfig,
        replica_configs: Vec<BackendConfig>,
        ha: &HaConfig,
    ) -> Self {
        Self {
            pool,
            protocol,
            primary_config,
            replica_configs,
            max_lag_ms: ha.max_replica_lag_ms,
            failover_threshold: ha.primary_failover_threshold,
            interval: Duration::from_secs(ha.health_check_interval_secs),
            galera_check: ha.galera_check,
        }
    }

    /// Run forever — meant to be spawned as a `tokio::spawn` task.
    pub async fn run(self) {
        log::info!(
            "Health checker started — interval={}s, max_replica_lag={}ms, failover_threshold={}",
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
        let (ok, reason) = match self.protocol.connect_backend(&self.primary_config).await {
            Ok(mut conn) => match conn.ping().await {
                Ok(_) => (true, None),
                Err(err) => (false, Some(format!("ping failed: {err}"))),
            },
            Err(err) => (false, Some(format!("connect failed: {err}"))),
        };

        if ok {
            let prev_failures = self
                .pool
                .primary_health
                .consecutive_failures
                .swap(0, Ordering::Relaxed);
            let was_down = !self
                .pool
                .primary_health
                .healthy
                .swap(true, Ordering::Relaxed);

            if was_down || prev_failures >= self.failover_threshold {
                let had_failover = self.pool.failover_idx.load(Ordering::Relaxed) >= 0;
                self.pool.failover_idx.store(-1, Ordering::Relaxed);
                if had_failover {
                    log::info!(
                        "[HA] Primary {} recovered — failover cleared",
                        self.primary_config.addr
                    );
                }
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

            match reason {
                Some(reason) => log::warn!(
                    "[HA] Primary {} unreachable ({}/{}) — {}{}",
                    self.primary_config.addr,
                    failures,
                    self.failover_threshold,
                    reason,
                    if reason.to_lowercase().contains("early eof") {
                        " (hint: backend may be speaking a different protocol)"
                    } else {
                        ""
                    },
                ),
                None => log::warn!(
                    "[HA] Primary {} unreachable ({}/{})",
                    self.primary_config.addr,
                    failures,
                    self.failover_threshold,
                ),
            }

            if failures >= self.failover_threshold
                && self.pool.failover_idx.load(Ordering::Relaxed) < 0
            {
                self.trigger_failover();
            }
        }
    }

    fn trigger_failover(&self) {
        // Pick the healthy replica with the lowest lag.
        let best = self
            .replica_configs
            .iter()
            .enumerate()
            .filter(|(i, _)| self.pool.replica_health[*i].healthy.load(Ordering::Relaxed))
            .min_by_key(|(i, _)| self.pool.replica_health[*i].lag_ms.load(Ordering::Relaxed));

        match best {
            Some((idx, cfg)) => {
                self.pool.failover_idx.store(idx as i64, Ordering::Relaxed);
                log::error!(
                    "[HA] FAILOVER: primary {} down after {} checks — promoting replica [{}] {}",
                    self.primary_config.addr,
                    self.failover_threshold,
                    idx,
                    cfg.addr,
                );
            }
            None => {
                log::error!(
                    "[HA] PRIMARY DOWN ({}) — no healthy replica available for failover",
                    self.primary_config.addr,
                );
            }
        }
    }

    // ── Replicas ──────────────────────────────────────────────────────────────

    async fn check_replica(&self, idx: usize, config: &BackendConfig) {
        match self.protocol.connect_backend(config).await {
            Ok(mut conn) => {
                // ── Galera / Percona XtraDB Cluster node-state check ──────────
                if self.galera_check {
                    let synced = query_wsrep_state(&mut *conn).await;
                    match synced {
                        Some(false) => {
                            // Node is in the cluster but not SYNCED — skip remaining checks.
                            let was_healthy = self.pool.replica_health[idx]
                                .healthy
                                .swap(false, Ordering::Relaxed);
                            if was_healthy {
                                log::warn!(
                                    "[HA/Galera] Replica [{}] {} wsrep_local_state != 4 — removed from read pool",
                                    idx, config.addr,
                                );
                            }
                            return;
                        }
                        Some(true) => {
                            log::debug!(
                                "[HA/Galera] Replica [{}] {} wsrep_local_state=4 (SYNCED)",
                                idx,
                                config.addr
                            );
                        }
                        None => {
                            // wsrep_local_state not present — not a Galera node, continue normally.
                        }
                    }
                }

                let lag_ms = query_replica_lag(&mut *conn).await.unwrap_or(0);
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
                        "[HA] Replica [{}] {} lag {}ms > {}ms — removed from read pool",
                        idx,
                        config.addr,
                        lag_ms,
                        self.max_lag_ms,
                    ),
                    (false, true) => log::info!(
                        "[HA] Replica [{}] {} lag {}ms — back in read pool",
                        idx,
                        config.addr,
                        lag_ms,
                    ),
                    _ => {}
                }
            }
            Err(e) => {
                let failures = self.pool.replica_health[idx]
                    .consecutive_failures
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;
                let was_healthy = self.pool.replica_health[idx]
                    .healthy
                    .swap(false, Ordering::Relaxed);

                // Only log first failure and transitions to avoid noise.
                if was_healthy || failures == 1 {
                    log::warn!("[HA] Replica [{}] {} unreachable: {}", idx, config.addr, e);
                }
            }
        }
    }
}

// ─── SHOW REPLICA STATUS parser ───────────────────────────────────────────────

/// Run `SHOW REPLICA STATUS` (MySQL 8.0.22+) or `SHOW SLAVE STATUS` (older),
/// extract `Seconds_Behind_Source` / `Seconds_Behind_Master`, convert to ms.
/// Returns `None` if the server is not a replica (standalone).
async fn query_replica_lag(conn: &mut dyn crate::protocol::BackendConnection) -> Option<u64> {
    // Try current syntax first; fall back if server returns an error.
    let resp = conn.execute_query(b"SHOW REPLICA STATUS").await.ok()?;
    if resp.is_error {
        let resp2 = conn.execute_query(b"SHOW SLAVE STATUS").await.ok()?;
        if resp2.is_error {
            return Some(0);
        }
        return parse_lag_bytes(&resp2.bytes);
    }
    parse_lag_bytes(&resp.bytes)
}

// ─── Galera / Percona XtraDB Cluster state check ─────────────────────────────

/// Query `SHOW GLOBAL STATUS LIKE 'wsrep_local_state'` and return:
/// - `Some(true)`  — node is SYNCED (state == 4) → safe for reads
/// - `Some(false)` — node is in the cluster but *not* SYNCED → skip reads
/// - `None`        — wsrep_local_state is absent → not a Galera node, ignore
async fn query_wsrep_state(conn: &mut dyn crate::protocol::BackendConnection) -> Option<bool> {
    let resp = conn
        .execute_query(b"SHOW GLOBAL STATUS LIKE 'wsrep_local_state'")
        .await
        .ok()?;
    if resp.is_error {
        return None;
    }
    parse_wsrep_state(&resp.bytes)
}

/// Parse the `Value` column from `SHOW GLOBAL STATUS LIKE 'wsrep_local_state'`.
/// Returns `None` if the result set is empty (variable does not exist).
fn parse_wsrep_state(bytes: &[u8]) -> Option<bool> {
    // Layout: col_count | col_defs… | EOF | row | EOF
    // For `SHOW STATUS LIKE`, the result set always has exactly 2 columns: Variable_name, Value.
    let mut pos = 0;

    // col_count packet — we expect 2 columns
    let _col_count_pkt = next_packet(bytes, &mut pos)?;

    // Skip 2 column-definition packets
    let _col1 = next_packet(bytes, &mut pos)?; // Variable_name
    let _col2 = next_packet(bytes, &mut pos)?; // Value

    // EOF after column defs
    next_packet(bytes, &mut pos)?;

    // First data row (or EOF/error if no rows)
    let row_pkt = next_packet(bytes, &mut pos)?;
    if row_pkt.first().copied() == Some(0xFE) {
        // Empty result — wsrep_local_state variable not present on this server.
        return None;
    }

    // Row: lenenc_str(Variable_name), lenenc_str(Value)
    let mut rp = 0;
    let _var_name = read_lenenc_str(row_pkt, &mut rp)?; // skip Variable_name
    let value_bytes = read_lenenc_str(row_pkt, &mut rp)??; // Value (non-NULL)
    let state_str = std::str::from_utf8(value_bytes).ok()?;
    let state: u32 = state_str.trim().parse().ok()?;

    // wsrep_local_state values:
    //   1 = Joining, 2 = Donor/Desynced, 3 = Joined, 4 = Synced
    Some(state == 4)
}

/// Parse `Seconds_Behind_Source` (or `Seconds_Behind_Master`) from the raw
/// MySQL text-protocol result set bytes collected by `collect_response`.
///
/// Result set layout (packet-framed):
///   [col_count_pkt] [col_def_pkt × N] [EOF] [row_pkt | EOF] [EOF]
fn parse_lag_bytes(bytes: &[u8]) -> Option<u64> {
    let mut pos = 0;

    // Column count
    let col_count_pkt = next_packet(bytes, &mut pos)?;
    let mut cp = 0;
    let col_count = read_lenenc_int(col_count_pkt, &mut cp)?;
    if col_count == 0 {
        return None;
    }

    // Column definitions — find target column index
    let mut target: Option<usize> = None;
    for i in 0..col_count {
        let pkt = next_packet(bytes, &mut pos)?;
        if let Some(name) = col_def_name(pkt) {
            if name == b"Seconds_Behind_Source" || name == b"Seconds_Behind_Master" {
                target = Some(i);
            }
        }
    }

    // EOF after column defs
    next_packet(bytes, &mut pos)?;

    // First data row (or EOF if no replicas are running)
    let row_pkt = next_packet(bytes, &mut pos)?;
    if row_pkt.first().copied() == Some(0xFE) {
        return None; // empty result → not a replica
    }

    let target_idx = target?;
    let mut rp = 0;
    for i in 0..=target_idx {
        let val = read_lenenc_str(row_pkt, &mut rp)?;
        if i == target_idx {
            return match val {
                None => Some(0), // NULL → replica thread not running
                Some(v) => {
                    let secs: u64 = std::str::from_utf8(v).ok()?.parse().ok()?;
                    Some(secs * 1000)
                }
            };
        }
    }
    None
}

// ─── Low-level MySQL packet helpers ──────────────────────────────────────────

/// Yield the payload of the next 4-byte-framed MySQL packet.
fn next_packet<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    if *pos + 4 > bytes.len() {
        return None;
    }
    let len = (bytes[*pos] as usize)
        | ((bytes[*pos + 1] as usize) << 8)
        | ((bytes[*pos + 2] as usize) << 16);
    *pos += 4;
    if *pos + len > bytes.len() {
        return None;
    }
    let payload = &bytes[*pos..*pos + len];
    *pos += len;
    Some(payload)
}

/// Read a length-encoded integer and advance `pos`.
fn read_lenenc_int(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let b = *bytes.get(*pos)?;
    *pos += 1;
    match b {
        0..=250 => Some(b as usize),
        0xFC => {
            let lo = *bytes.get(*pos)? as usize;
            let hi = *bytes.get(*pos + 1)? as usize;
            *pos += 2;
            Some(lo | (hi << 8))
        }
        0xFD => {
            let b0 = *bytes.get(*pos)? as usize;
            let b1 = *bytes.get(*pos + 1)? as usize;
            let b2 = *bytes.get(*pos + 2)? as usize;
            *pos += 3;
            Some(b0 | (b1 << 8) | (b2 << 16))
        }
        _ => None,
    }
}

/// Read a length-encoded string. Returns `Some(None)` for SQL NULL, `Some(Some(bytes))` for value.
fn read_lenenc_str<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<Option<&'a [u8]>> {
    let b = *bytes.get(*pos)?;
    if b == 0xFB {
        *pos += 1;
        return Some(None);
    }
    let len = read_lenenc_int(bytes, pos)?;
    let s = bytes.get(*pos..*pos + len)?;
    *pos += len;
    Some(Some(s))
}

/// Extract the column name from a MySQL column-definition packet payload.
///
/// Column def layout: catalog + schema + table + org_table + **name** + org_name + …
/// All are length-encoded strings.
fn col_def_name(payload: &[u8]) -> Option<&[u8]> {
    let mut pos = 0;
    // Skip catalog, schema, table, org_table
    for _ in 0..4 {
        let b = *payload.get(pos)?;
        if b == 0xFB {
            pos += 1;
        } else {
            let len = read_lenenc_int(payload, &mut pos)?;
            pos += len;
        }
    }
    // name
    let b = *payload.get(pos)?;
    if b == 0xFB {
        return None;
    }
    let mut np = pos;
    let len = read_lenenc_int(payload, &mut np)?;
    payload.get(np..np + len)
}
