//! MySQL Group Replication / InnoDB Cluster awareness.
//!
//! Polls `performance_schema.replication_group_members` on any reachable backend
//! to discover the current elected PRIMARY. When the primary changes,
//! `BackendPool::gr_primary_idx` is updated atomically so writes are re-routed
//! without a proxy restart.
//!
//! Routing priority in `BackendPool::get_primary()`:
//!   GR primary (gr_primary_idx) > HA failover (failover_idx) > configured primary

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::config::BackendConfig;
use crate::protocol::DatabaseProtocol;
use crate::proxy::pool::{BackendPool, GrMember};

// ─── GrChecker ────────────────────────────────────────────────────────────────

/// Background task that polls the GR cluster state and updates `BackendPool`.
pub struct GrChecker {
    pool: Arc<BackendPool>,
    protocol: Arc<dyn DatabaseProtocol>,
    primary_config: BackendConfig,
    replica_configs: Vec<BackendConfig>,
    interval: Duration,
}

impl GrChecker {
    pub fn new(
        pool: Arc<BackendPool>,
        protocol: Arc<dyn DatabaseProtocol>,
        primary_config: BackendConfig,
        replica_configs: Vec<BackendConfig>,
        interval_secs: u64,
    ) -> Self {
        Self {
            pool,
            protocol,
            primary_config,
            replica_configs,
            interval: Duration::from_secs(interval_secs.max(1)),
        }
    }

    /// Run forever — meant to be spawned with `tokio::spawn`.
    pub async fn run(self) {
        log::info!(
            "Group Replication monitor started — interval={}s",
            self.interval.as_secs()
        );

        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            self.poll_once().await;
        }
    }

    async fn poll_once(&self) {
        // Try each configured backend until one responds (any group member can
        // serve `performance_schema` queries).
        let all_configs: Vec<&BackendConfig> = std::iter::once(&self.primary_config)
            .chain(self.replica_configs.iter())
            .collect();
        let backend_count = all_configs.len();

        for config in all_configs {
            match self.query_gr_members(config).await {
                Ok(members) => {
                    self.apply_members(members).await;
                    return;
                }
                Err(_) => continue,
            }
        }
        // All backends unreachable — GR routing state is frozen at last known value.
        log::warn!(
            "[GR] all {} backend(s) unreachable during poll — GR primary discovery stalled, routing unchanged",
            backend_count
        );
    }

    async fn query_gr_members(&self, config: &BackendConfig) -> anyhow::Result<Vec<GrMember>> {
        let mut conn = self.protocol.connect_backend(config).await?;
        let resp = conn
            .execute_query(
                b"SELECT MEMBER_HOST, MEMBER_PORT, MEMBER_ROLE, MEMBER_STATE, MEMBER_VERSION \
                  FROM performance_schema.replication_group_members",
            )
            .await?;

        if resp.is_error {
            // GR not enabled on this server — treat as empty cluster.
            return Ok(Vec::new());
        }

        Ok(parse_gr_members(&resp.bytes))
    }

    async fn apply_members(&self, members: Vec<GrMember>) {
        if members.is_empty() {
            // Server is standalone (GR not configured). Clear GR routing.
            let old = self.pool.gr_primary_idx.swap(-1, Ordering::Relaxed);
            if old >= 0 {
                log::info!("[GR] No cluster members — reverting to configured primary");
            }
            *self.pool.gr_members.lock().await = Vec::new();
            return;
        }

        // Find the ONLINE PRIMARY member.
        let primary_member = members
            .iter()
            .find(|m| m.role == "PRIMARY" && m.state == "ONLINE");

        if let Some(pm) = primary_member {
            let new_idx = self.find_backend_index(&pm.addr);
            let old_idx = self.pool.gr_primary_idx.load(Ordering::Relaxed);

            if new_idx != old_idx {
                let label = if new_idx < 0 {
                    format!("configured primary ({})", self.primary_config.addr)
                } else {
                    format!("replica[{}] ({})", new_idx, pm.addr)
                };
                log::info!("[GR] Primary re-routed to {}", label);
                self.pool.gr_primary_idx.store(new_idx, Ordering::Relaxed);
            }
        }

        *self.pool.gr_members.lock().await = members;
    }

    /// Map a GR member address (`host:port`) to a `gr_primary_idx` value:
    /// `-1` = original configured primary, `0..N` = replicas[index].
    fn find_backend_index(&self, addr: &str) -> i64 {
        if addr_matches(&self.primary_config.addr, addr) {
            return -1;
        }
        for (i, r) in self.replica_configs.iter().enumerate() {
            if addr_matches(&r.addr, addr) {
                return i as i64;
            }
        }
        // Unknown member — fall back to configured primary (safe default).
        -1
    }
}

/// Case-insensitive, whitespace-tolerant address comparison.
fn addr_matches(config_addr: &str, gr_addr: &str) -> bool {
    config_addr.trim().eq_ignore_ascii_case(gr_addr.trim())
}

// ─── Result-set parser ────────────────────────────────────────────────────────

/// Parse GR member rows from a raw MySQL text-protocol result set.
///
/// Expected columns (in order): MEMBER_HOST, MEMBER_PORT, MEMBER_ROLE,
/// MEMBER_STATE, MEMBER_VERSION.
fn parse_gr_members(bytes: &[u8]) -> Vec<GrMember> {
    let rows = match parse_text_rows(bytes) {
        Some(r) => r,
        None => return Vec::new(),
    };

    rows.into_iter()
        .map(|row| {
            let host = str_col(&row, 0);
            let port = str_col(&row, 1);
            let role = str_col(&row, 2);
            let state = str_col(&row, 3);
            let version = str_col(&row, 4);
            GrMember {
                addr: if port.is_empty() || port == "3306" {
                    host.clone()
                } else {
                    format!("{}:{}", host, port)
                },
                role,
                state,
                version,
            }
        })
        .collect()
}

fn str_col(row: &[Option<String>], idx: usize) -> String {
    row.get(idx)
        .and_then(|v| v.as_deref())
        .unwrap_or("")
        .to_string()
}

/// Parse a MySQL text-protocol result set into rows × columns of `Option<String>`.
fn parse_text_rows(bytes: &[u8]) -> Option<Vec<Vec<Option<String>>>> {
    let mut pos = 0;

    // Column count packet.
    let col_count_pkt = next_packet(bytes, &mut pos)?;
    let mut cp = 0;
    let col_count = read_lenenc_int(col_count_pkt, &mut cp)?;
    if col_count == 0 {
        return Some(Vec::new());
    }

    // Skip column definition packets.
    for _ in 0..col_count {
        next_packet(bytes, &mut pos)?;
    }
    // Skip EOF marker after column defs.
    next_packet(bytes, &mut pos)?;

    // Data rows until EOF (0xFE) or ERR (0xFF).
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    loop {
        let pkt = next_packet(bytes, &mut pos)?;
        match pkt.first().copied() {
            Some(0xFE) | Some(0xFF) => break,
            _ => {}
        }
        let mut rp = 0;
        let mut row = Vec::with_capacity(col_count);
        for _ in 0..col_count {
            match read_lenenc_str(pkt, &mut rp)? {
                None => row.push(None),
                Some(v) => row.push(Some(String::from_utf8_lossy(v).into_owned())),
            }
        }
        rows.push(row);
    }
    Some(rows)
}

// ─── Low-level MySQL packet helpers ──────────────────────────────────────────

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
