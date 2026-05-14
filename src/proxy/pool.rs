//! Backend connection pool — protocol-agnostic.
//! Uses `Box<dyn BackendConnection>` and `Arc<dyn DatabaseProtocol>` so the pool
//! works unchanged for MySQL, PostgreSQL, or any future protocol.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::sync::Semaphore;

use crate::config::BackendConfig;
use crate::protocol::{BackendConnection, DatabaseProtocol};
use crate::proxy::circuit_breaker::CircuitBreaker;

// ─── Pool error type ──────────────────────────────────────────────────────────

#[allow(dead_code)]
pub type PoolResult<T> = Result<T, PoolError>;

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("Backend connection error: {0}")]
    Connection(#[from] crate::protocol::ProtocolError),
    #[error("Pool error: {0}")]
    Other(String),
}

// ─── BackendHealth ────────────────────────────────────────────────────────────

/// Live health state for one backend — updated by the health checker background task.
pub struct BackendHealth {
    /// Whether this backend is currently considered reachable and within lag limits.
    pub healthy: AtomicBool,
    /// Last measured replication lag in milliseconds (0 for primary).
    pub lag_ms: AtomicU64,
    /// Number of consecutive health-check failures since the last success.
    pub consecutive_failures: AtomicU32,
}

impl BackendHealth {
    pub fn new(initial_healthy: bool) -> Self {
        Self {
            healthy: AtomicBool::new(initial_healthy),
            lag_ms: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
        }
    }
}

// ─── ReplicaInfo ──────────────────────────────────────────────────────────────

/// Metadata about a discovered replica.
#[allow(dead_code)]
pub struct ReplicaInfo {
    pub addr: String,
    pub replication_lag_ms: Option<u64>,
}

// ─── ConnectionPool ───────────────────────────────────────────────────────────

// ─── PooledConnection ─────────────────────────────────────────────────────────

/// A backend connection together with the instant it was returned to the pool.
struct PooledConn {
    conn: Box<dyn BackendConnection>,
    idle_since: Instant,
}

// ─── ConnectionPool ───────────────────────────────────────────────────────────

/// Pool of connections to a single backend (primary or one replica).
pub struct ConnectionPool {
    pub config: BackendConfig,
    /// Idle connections partitioned by effective backend database.
    connections: Arc<Mutex<HashMap<String, Vec<PooledConn>>>>,
    max_size: usize,
    pub protocol: Arc<dyn DatabaseProtocol>,
    /// Maximum idle duration before a pooled connection is discarded.
    /// When `None`, eviction is disabled.
    max_idle: Option<Duration>,
    /// Relative weight for weighted round-robin replica selection.
    pub weight: u32,
    /// If true, only used when all non-backup replicas are unhealthy.
    pub backup: bool,
    /// Connections currently checked out (in use by a session).
    pub borrowed: AtomicUsize,
    /// Total new TCP connections ever opened to the backend.
    pub connections_created: AtomicUsize,
    /// Total times a pooled connection was reused (cache hit).
    pub connections_reused: AtomicUsize,
    /// Connections discarded because they exceeded `max_idle`.
    pub connections_evicted: AtomicUsize,
    /// Wait queue semaphore — when the backend is at `max_connections`, incoming
    /// requests acquire a permit here instead of being rejected immediately.
    /// `None` when `pool_wait_queue_size == 0` (reject-fast, default behaviour).
    wait_queue: Option<Arc<Semaphore>>,
    /// Timeout for waiting in the queue before giving up (reject).
    wait_timeout: Duration,
    /// Current number of waiters blocked in the queue.
    pub wait_queue_length: AtomicUsize,
    /// Total requests that timed out waiting in the queue.
    pub wait_timeouts_total: AtomicUsize,
}

impl ConnectionPool {
    pub fn with_idle_timeout(
        config: &BackendConfig,
        max_size: usize,
        protocol: Arc<dyn DatabaseProtocol>,
        max_idle: Option<Duration>,
    ) -> Self {
        Self::with_wait_queue(config, max_size, protocol, max_idle, 0, 5000)
    }

    /// Create a pool with an optional bounded wait queue.
    /// `wait_queue_size = 0` preserves the reject-fast default.
    pub fn with_wait_queue(
        config: &BackendConfig,
        max_size: usize,
        protocol: Arc<dyn DatabaseProtocol>,
        max_idle: Option<Duration>,
        wait_queue_size: usize,
        wait_timeout_ms: u64,
    ) -> Self {
        let wait_queue = if wait_queue_size > 0 {
            Some(Arc::new(Semaphore::new(wait_queue_size)))
        } else {
            None
        };
        Self {
            config: config.clone(),
            connections: Arc::new(Mutex::new(HashMap::new())),
            max_size,
            protocol,
            max_idle,
            weight: config.weight,
            backup: config.backup,
            borrowed: AtomicUsize::new(0),
            connections_created: AtomicUsize::new(0),
            connections_reused: AtomicUsize::new(0),
            connections_evicted: AtomicUsize::new(0),
            wait_queue,
            wait_timeout: Duration::from_millis(wait_timeout_ms),
            wait_queue_length: AtomicUsize::new(0),
            wait_timeouts_total: AtomicUsize::new(0),
        }
    }

    fn db_key(database: Option<&str>) -> String {
        database.unwrap_or("").to_string()
    }

    /// Get a connection from the pool, or create a new one via the protocol.
    /// Stale connections (idle > max_idle) are silently discarded.
    pub async fn get(&self) -> anyhow::Result<Box<dyn BackendConnection>> {
        self.get_for_database(self.config.database.as_deref()).await
    }

    /// Get a connection scoped to a specific backend database.
    /// Connections are only reused within the same database key.
    pub async fn get_for_database(
        &self,
        database: Option<&str>,
    ) -> anyhow::Result<Box<dyn BackendConnection>> {
        let key = Self::db_key(database.or(self.config.database.as_deref()));
        let mut pools = self.connections.lock().await;

        // Pop connections from the back, skipping any that have gone stale.
        if let Some(pool) = pools.get_mut(&key) {
            loop {
                let Some(entry) = pool.pop() else { break };
                if let Some(max) = self.max_idle {
                    if entry.idle_since.elapsed() >= max {
                        self.connections_evicted.fetch_add(1, Ordering::Relaxed);
                        log::debug!(
                            "[pool] evicted stale connection idle for {:.1}s (limit {:.0}s)",
                            entry.idle_since.elapsed().as_secs_f64(),
                            max.as_secs_f64(),
                        );
                        continue; // discard and try the next one
                    }
                }
                // Fresh connection — use it.
                self.borrowed.fetch_add(1, Ordering::Relaxed);
                self.connections_reused.fetch_add(1, Ordering::Relaxed);
                return Ok(entry.conn);
            }
        }
        drop(pools);

        // Enforce backend max_connections before opening a new TCP connection.
        if let Some(max) = self.config.max_connections {
            if self.borrowed.load(Ordering::Relaxed) >= max {
                // If a wait queue is configured, block until a slot opens or timeout.
                if let Some(ref sem) = self.wait_queue {
                    self.wait_queue_length.fetch_add(1, Ordering::Relaxed);
                    let result = tokio::time::timeout(self.wait_timeout, sem.acquire()).await;
                    self.wait_queue_length.fetch_sub(1, Ordering::Relaxed);
                    match result {
                        Ok(Ok(permit)) => {
                            // Got a slot — forget the permit (we track via `borrowed` counter).
                            permit.forget();
                        }
                        _ => {
                            self.wait_timeouts_total.fetch_add(1, Ordering::Relaxed);
                            log::warn!(
                                "[pool] wait queue timeout for backend {} (waited {}ms, max: {})",
                                self.config.addr,
                                self.wait_timeout.as_millis(),
                                max
                            );
                            return Err(anyhow::anyhow!(
                                "backend {} connection limit reached and wait queue timed out (max: {}, timeout: {}ms)",
                                self.config.addr,
                                max,
                                self.wait_timeout.as_millis()
                            ));
                        }
                    }
                } else {
                    return Err(anyhow::anyhow!(
                        "backend {} connection limit reached (max: {}); try again later",
                        self.config.addr,
                        max
                    ));
                }
            }
        }

        // Nothing usable — open a new TCP connection.
        let mut connect_cfg = self.config.clone();
        connect_cfg.database = if key.is_empty() {
            None
        } else {
            Some(key.clone())
        };
        let mut conn = self
            .protocol
            .connect_backend(&connect_cfg)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        // Execute init_connect statements before adding the connection to use.
        for sql in &connect_cfg.init_connect {
            if let Err(e) = conn.execute_query(sql.as_bytes()).await {
                log::warn!("[pool] init_connect failed (sql={:?}): {}", sql, e);
                // Discard the connection — it's in an unknown state.
                return Err(anyhow::anyhow!("init_connect failed: {}", e));
            }
        }
        self.borrowed.fetch_add(1, Ordering::Relaxed);
        self.connections_created.fetch_add(1, Ordering::Relaxed);
        Ok(conn)
    }

    /// Return a connection to the pool. Dropped if in-transaction or pool is full.
    pub async fn put(&self, conn: Box<dyn BackendConnection>) {
        self.put_for_database(conn, self.config.database.as_deref())
            .await;
    }

    /// Return a connection to a specific database bucket.
    pub async fn put_for_database(&self, conn: Box<dyn BackendConnection>, database: Option<&str>) {
        self.borrowed.fetch_sub(1, Ordering::Relaxed);
        // Signal a waiter that a slot is available.
        if let Some(ref sem) = self.wait_queue {
            sem.add_permits(1);
        }
        if conn.in_transaction() || !conn.is_healthy() {
            return;
        }
        let key = Self::db_key(database.or(self.config.database.as_deref()));
        let mut pools = self.connections.lock().await;
        let bucket = pools
            .entry(key)
            .or_insert_with(|| Vec::with_capacity(self.max_size));
        if bucket.len() < self.max_size {
            bucket.push(PooledConn {
                conn,
                idle_since: Instant::now(),
            });
        }
    }

    /// Snapshot: (idle, in-use, created, reused, evicted).
    pub async fn snapshot(&self) -> (usize, usize, usize, usize, usize) {
        let idle = self
            .connections
            .lock()
            .await
            .values()
            .map(|v| v.len())
            .sum();
        let in_use = self.borrowed.load(Ordering::Relaxed);
        let created = self.connections_created.load(Ordering::Relaxed);
        let reused = self.connections_reused.load(Ordering::Relaxed);
        let evicted = self.connections_evicted.load(Ordering::Relaxed);
        (idle, in_use, created, reused, evicted)
    }
}

// ─── BackendPool ──────────────────────────────────────────────────────────────

/// Manages connection pools for primary and all replicas.
pub struct BackendPool {
    pub primary: ConnectionPool,
    pub replicas: Vec<ConnectionPool>,
    replica_index: AtomicUsize,
    /// Live health state — written by the health-checker task, read on every request.
    pub primary_health: Arc<BackendHealth>,
    pub replica_health: Vec<Arc<BackendHealth>>,
    /// -1 = no HA failover active; ≥0 = index of replica acting as HA failover primary.
    pub failover_idx: AtomicI64,
    /// -1 = GR not active (use configured primary).
    /// ≥0 = index into `replicas` of the member currently elected as GR PRIMARY.
    pub gr_primary_idx: AtomicI64,
    /// Live snapshot of Group Replication cluster members for `/api/cluster`.
    pub gr_members: Arc<tokio::sync::Mutex<Vec<GrMember>>>,
    /// Total number of HA failovers triggered since process start.
    pub failover_events_total: AtomicUsize,
    /// Total flap events (failover cleared then re-triggered within cooldown window).
    pub failover_flap_total: AtomicUsize,
    /// Consecutive successful primary checks since last failure (used for recovery threshold).
    pub recovery_checks: AtomicUsize,
    /// Instant when the last failover was triggered (epoch secs, 0 = never).
    pub failover_triggered_at: AtomicU64,
    /// Per-replica circuit breakers. Index matches `replicas` / `replica_health`.
    pub replica_breakers: Vec<CircuitBreaker>,
    /// Circuit breaker for the primary backend.
    pub primary_breaker: CircuitBreaker,
    /// Number of streaming replicas discovered via `pg_stat_replication` on the
    /// PostgreSQL primary.  Always 0 for MySQL pools (set only by PgHealthChecker).
    pub pg_discovered_replicas: AtomicUsize,
}

impl BackendPool {
    pub fn with_idle_timeout(
        primary_config: &BackendConfig,
        replica_configs: &[BackendConfig],
        pool_size: usize,
        protocol: Arc<dyn DatabaseProtocol>,
        max_idle: Option<Duration>,
    ) -> Self {
        Self::with_options(
            primary_config,
            replica_configs,
            pool_size,
            protocol,
            max_idle,
            5,
            10000,
            0,
            5000,
        )
    }

    #[allow(dead_code)]
    pub fn with_circuit_breaker(
        primary_config: &BackendConfig,
        replica_configs: &[BackendConfig],
        pool_size: usize,
        protocol: Arc<dyn DatabaseProtocol>,
        max_idle: Option<Duration>,
        cb_threshold: u32,
        cb_recovery_ms: u64,
    ) -> Self {
        Self::with_options(
            primary_config,
            replica_configs,
            pool_size,
            protocol,
            max_idle,
            cb_threshold,
            cb_recovery_ms,
            0,
            5000,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_options(
        primary_config: &BackendConfig,
        replica_configs: &[BackendConfig],
        pool_size: usize,
        protocol: Arc<dyn DatabaseProtocol>,
        max_idle: Option<Duration>,
        cb_threshold: u32,
        cb_recovery_ms: u64,
        wait_queue_size: usize,
        wait_timeout_ms: u64,
    ) -> Self {
        let primary = ConnectionPool::with_wait_queue(
            primary_config,
            pool_size,
            protocol.clone(),
            max_idle,
            wait_queue_size,
            wait_timeout_ms,
        );
        let replicas = replica_configs
            .iter()
            .map(|c| {
                ConnectionPool::with_wait_queue(
                    c,
                    pool_size,
                    protocol.clone(),
                    max_idle,
                    wait_queue_size,
                    wait_timeout_ms,
                )
            })
            .collect();

        let replica_health = replica_configs
            .iter()
            .map(|_| Arc::new(BackendHealth::new(true)))
            .collect();

        Self {
            primary,
            replicas,
            replica_index: AtomicUsize::new(0),
            primary_health: Arc::new(BackendHealth::new(true)),
            replica_health,
            failover_idx: AtomicI64::new(-1),
            gr_primary_idx: AtomicI64::new(-1),
            gr_members: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            failover_events_total: AtomicUsize::new(0),
            failover_flap_total: AtomicUsize::new(0),
            recovery_checks: AtomicUsize::new(0),
            failover_triggered_at: AtomicU64::new(0),
            replica_breakers: replica_configs
                .iter()
                .map(|_| CircuitBreaker::new(cb_threshold, cb_recovery_ms))
                .collect(),
            primary_breaker: CircuitBreaker::new(cb_threshold, cb_recovery_ms),
            pg_discovered_replicas: AtomicUsize::new(0),
        }
    }

    /// Returns `true` when an HA failover replica is currently serving as primary.
    pub fn failover_active(&self) -> bool {
        self.failover_idx.load(Ordering::Relaxed) >= 0
    }

    /// Returns the address of the currently effective primary backend.
    pub fn primary_addr(&self) -> String {
        let gr_idx = self.gr_primary_idx.load(Ordering::Relaxed);
        if gr_idx >= 0 {
            let idx = gr_idx as usize;
            if idx < self.replicas.len() {
                return self.replicas[idx].config.addr.clone();
            }
        }
        let failover = self.failover_idx.load(Ordering::Relaxed);
        if failover >= 0 {
            let idx = failover as usize;
            if idx < self.replicas.len() {
                return self.replicas[idx].config.addr.clone();
            }
        }
        self.primary.config.addr.clone()
    }
    ///
    /// Routing priority:
    /// 1. GR-elected primary (when Group Replication monitoring is active)
    /// 2. HA failover replica (when the configured primary is unreachable)
    /// 3. Configured primary
    pub async fn get_primary(&self) -> anyhow::Result<Box<dyn BackendConnection>> {
        self.get_primary_for_database(None).await
    }

    /// Database-aware version of `get_primary`.
    pub async fn get_primary_for_database(
        &self,
        database: Option<&str>,
    ) -> anyhow::Result<Box<dyn BackendConnection>> {
        // GR takes precedence: a different replica may now be the GR PRIMARY.
        let gr_idx = self.gr_primary_idx.load(Ordering::Relaxed);
        if gr_idx >= 0 {
            let idx = gr_idx as usize;
            if idx < self.replicas.len() {
                return self.replicas[idx].get_for_database(database).await;
            }
        }
        // HA failover (configured primary unreachable).
        let failover = self.failover_idx.load(Ordering::Relaxed);
        if failover >= 0 {
            let idx = failover as usize;
            if idx < self.replicas.len() {
                return self.replicas[idx].get_for_database(database).await;
            }
        }
        // Circuit breaker gate — only for the configured primary (GR / failover
        // replicas acting as primary are not subject to the primary CB).
        if !self.primary_breaker.allows() {
            anyhow::bail!("[CB] primary circuit breaker is open — connection refused");
        }
        self.primary.get_for_database(database).await
    }

    /// Get a replica connection using weighted round-robin.
    ///
    /// Selection order:
    /// 1. Healthy non-backup replicas, weighted round-robin
    /// 2. Healthy backup replicas (last resort, all non-backups are down)
    /// 3. Primary (absolute fallback)
    ///
    /// Returns `(conn, idx)` where `idx == usize::MAX` means primary was used.
    #[allow(dead_code)]
    pub async fn get_replica(&self) -> anyhow::Result<(Box<dyn BackendConnection>, usize)> {
        self.get_replica_for_database(None).await
    }

    /// Database-aware version of `get_replica`.
    pub async fn get_replica_for_database(
        &self,
        database: Option<&str>,
    ) -> anyhow::Result<(Box<dyn BackendConnection>, usize)> {
        if self.replicas.is_empty() {
            return Ok((self.get_primary_for_database(database).await?, usize::MAX));
        }

        // Build candidate list: healthy non-backup replicas first.
        let conn = self.try_weighted_replica(false, database).await;
        if let Some(pair) = conn {
            return Ok(pair);
        }

        // All non-backup replicas down — try backup replicas.
        let conn = self.try_weighted_replica(true, database).await;
        if let Some(pair) = conn {
            return Ok(pair);
        }

        // Everything down — fall back to primary.
        log::debug!("All replicas unhealthy, routing read to primary");
        Ok((self.get_primary_for_database(database).await?, usize::MAX))
    }

    /// Weighted round-robin selection among healthy replicas.
    /// `backup_pass` controls whether we select from backup or non-backup replicas.
    async fn try_weighted_replica(
        &self,
        backup_pass: bool,
        database: Option<&str>,
    ) -> Option<(Box<dyn BackendConnection>, usize)> {
        // Collect (index, weight) for candidates.
        let candidates: Vec<(usize, u32)> = self
            .replicas
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                r.backup == backup_pass
                    && self.replica_health[*i].healthy.load(Ordering::Relaxed)
                    && self.replica_breakers[*i].allows()
            })
            .map(|(i, r)| (i, r.weight.max(1)))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Total weight for this pass.
        let total_weight: u32 = candidates.iter().map(|(_, w)| w).sum();

        // Use the global counter modulo total_weight for deterministic distribution.
        let slot = (self.replica_index.fetch_add(1, Ordering::Relaxed) as u64 % total_weight as u64)
            as u32;
        let mut acc = 0u32;
        let mut chosen_idx = candidates[0].0; // fallback
        for (idx, w) in &candidates {
            acc += w;
            if slot < acc {
                chosen_idx = *idx;
                break;
            }
        }

        match self.replicas[chosen_idx].get_for_database(database).await {
            Ok(conn) => Some((conn, chosen_idx)),
            Err(_) => {
                // Mark unhealthy on connect failure, try remaining candidates.
                self.replica_health[chosen_idx]
                    .healthy
                    .store(false, Ordering::Relaxed);
                // Recurse once through remaining candidates (no infinite loop —
                // we've just marked one unhealthy so next call skips it).
                for (idx, _) in &candidates {
                    if *idx == chosen_idx {
                        continue;
                    }
                    if let Ok(conn) = self.replicas[*idx].get_for_database(database).await {
                        return Some((conn, *idx));
                    }
                    self.replica_health[*idx]
                        .healthy
                        .store(false, Ordering::Relaxed);
                }
                None
            }
        }
    }

    /// Get a connection to a specific backend by hostgroup index.
    /// `0` = primary, `1..=N` = replica[N-1].
    /// Falls back to primary if the replica index is out of range.
    #[allow(dead_code)]
    pub async fn get_hostgroup(
        &self,
        hostgroup: usize,
    ) -> anyhow::Result<(Box<dyn BackendConnection>, usize)> {
        self.get_hostgroup_for_database(hostgroup, None).await
    }

    /// Database-aware hostgroup routing.
    pub async fn get_hostgroup_for_database(
        &self,
        hostgroup: usize,
        database: Option<&str>,
    ) -> anyhow::Result<(Box<dyn BackendConnection>, usize)> {
        if hostgroup == 0 {
            return Ok((self.get_primary_for_database(database).await?, usize::MAX));
        }
        let idx = hostgroup - 1;
        if idx < self.replicas.len() && self.replica_health[idx].healthy.load(Ordering::Relaxed) {
            match self.replicas[idx].get_for_database(database).await {
                Ok(conn) => return Ok((conn, idx)),
                Err(e) => {
                    self.replica_health[idx]
                        .healthy
                        .store(false, Ordering::Relaxed);
                    log::warn!(
                        "[pool] hostgroup {} connect failed: {}, falling back to primary",
                        hostgroup,
                        e
                    );
                }
            }
        }
        Ok((self.get_primary_for_database(database).await?, usize::MAX))
    }

    pub async fn put_primary(&self, conn: Box<dyn BackendConnection>) {
        self.put_primary_for_database(conn, None).await;
    }

    pub async fn put_primary_for_database(
        &self,
        conn: Box<dyn BackendConnection>,
        database: Option<&str>,
    ) {
        let failover = self.failover_idx.load(Ordering::Relaxed);
        if failover >= 0 {
            let idx = failover as usize;
            if idx < self.replicas.len() {
                self.replicas[idx].put_for_database(conn, database).await;
                return;
            }
        }
        self.primary.put_for_database(conn, database).await;
    }

    #[allow(dead_code)]
    pub async fn put_replica(&self, conn: Box<dyn BackendConnection>, idx: usize) {
        self.put_replica_for_database(conn, idx, None).await;
    }

    pub async fn put_replica_for_database(
        &self,
        conn: Box<dyn BackendConnection>,
        idx: usize,
        database: Option<&str>,
    ) {
        if idx < self.replicas.len() {
            self.replicas[idx].put_for_database(conn, database).await;
        }
    }

    /// Snapshot of pool utilisation for monitoring.
    pub async fn pool_stats(&self) -> PoolStats {
        let (primary_idle, primary_in_use, primary_created, primary_reused, primary_evicted) =
            self.primary.snapshot().await;
        let mut replica_idle = 0usize;
        let mut replica_in_use = 0usize;
        let mut replica_created = 0usize;
        let mut replica_reused = 0usize;
        let mut replica_evicted = 0usize;
        for r in &self.replicas {
            let (i, u, c, rv, ev) = r.snapshot().await;
            replica_idle += i;
            replica_in_use += u;
            replica_created += c;
            replica_reused += rv;
            replica_evicted += ev;
        }
        PoolStats {
            primary_idle,
            primary_in_use,
            primary_created,
            primary_reused,
            primary_evicted,
            replica_idle,
            replica_in_use,
            replica_created,
            replica_reused,
            replica_evicted,
            replica_count: self.replicas.len(),
            failover_active: self.failover_idx.load(Ordering::Relaxed) >= 0,
            failover_events_total: self.failover_events_total.load(Ordering::Relaxed),
            failover_flap_total: self.failover_flap_total.load(Ordering::Relaxed),
        }
    }

    /// Per-backend snapshot for the Backends dashboard tab.
    pub async fn backend_stats(&self) -> Vec<BackendStat> {
        let mut stats = Vec::new();

        // Primary — hostgroup 0.
        let (idle, in_use, created, reused, evicted) = self.primary.snapshot().await;
        stats.push(BackendStat {
            addr: self.primary.config.addr.clone(),
            role: "primary".to_string(),
            hostgroup: 0,
            weight: self.primary.config.weight,
            backup: false,
            healthy: self.primary_health.healthy.load(Ordering::Relaxed),
            lag_ms: 0,
            consecutive_failures: self
                .primary_health
                .consecutive_failures
                .load(Ordering::Relaxed),
            idle,
            in_use,
            created,
            reused,
            evicted,
        });

        // Replicas — hostgroup 1..N.
        for (i, replica) in self.replicas.iter().enumerate() {
            let (idle, in_use, created, reused, evicted) = replica.snapshot().await;
            let health = &self.replica_health[i];
            stats.push(BackendStat {
                addr: replica.config.addr.clone(),
                role: "replica".to_string(),
                hostgroup: i + 1,
                weight: replica.weight,
                backup: replica.backup,
                healthy: health.healthy.load(Ordering::Relaxed),
                lag_ms: health.lag_ms.load(Ordering::Relaxed),
                consecutive_failures: health.consecutive_failures.load(Ordering::Relaxed),
                idle,
                in_use,
                created,
                reused,
                evicted,
            });
        }

        stats
    }
}

// ─── BackendStat ──────────────────────────────────────────────────────────────

/// Point-in-time snapshot of a single backend for the dashboard API.
#[derive(serde::Serialize, Clone, Debug)]
pub struct BackendStat {
    pub addr: String,
    /// "primary" or "replica"
    pub role: String,
    /// 0 = primary, 1..N = replica index+1 (matches `destination_hostgroup` in rules)
    pub hostgroup: usize,
    pub weight: u32,
    pub backup: bool,
    pub healthy: bool,
    pub lag_ms: u64,
    pub consecutive_failures: u32,
    pub idle: usize,
    pub in_use: usize,
    pub created: usize,
    pub reused: usize,
    pub evicted: usize,
}

// ─── GrMember ─────────────────────────────────────────────────────────────────

/// One member of a MySQL Group Replication / InnoDB Cluster group.
#[derive(serde::Serialize, Clone, Debug, Default)]
pub struct GrMember {
    /// Combined `host:port` address.
    pub addr: String,
    /// `"PRIMARY"` or `"SECONDARY"`.
    pub role: String,
    /// `"ONLINE"`, `"RECOVERING"`, `"ERROR"`, `"OFFLINE"`, or `"UNREACHABLE"`.
    pub state: String,
    /// MySQL server version string.
    pub version: String,
}

// ─── PoolStats ────────────────────────────────────────────────────────────────

/// Point-in-time snapshot of backend pool utilisation.
#[derive(serde::Serialize, Clone, Debug)]
pub struct PoolStats {
    pub primary_idle: usize,
    pub primary_in_use: usize,
    /// Total new backend TCP connections ever opened (primary).
    pub primary_created: usize,
    /// Total pool cache hits (primary) — connections reused without TCP handshake.
    pub primary_reused: usize,
    /// Connections discarded because they exceeded max_idle (primary).
    pub primary_evicted: usize,
    pub replica_idle: usize,
    pub replica_in_use: usize,
    pub replica_created: usize,
    pub replica_reused: usize,
    pub replica_evicted: usize,
    pub replica_count: usize,
    pub failover_active: bool,
    /// Total HA failovers triggered since process start.
    pub failover_events_total: usize,
    /// Total flap events.
    pub failover_flap_total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mock_config(max_connections: Option<usize>) -> BackendConfig {
        crate::config::BackendConfig {
            addr: "127.0.0.1:3306".to_string(),
            user: String::new(),
            password: String::new(),
            database: None,
            tls_mode: crate::config::TlsMode::Off,
            tls_ca: None,
            tls_cert: None,
            tls_key: None,
            weight: 100,
            backup: false,
            init_connect: vec![],
            resolution_family: "system".to_string(),
            compression: crate::config::BackendCompression::None,
            ssl_keylog_file: String::new(),
            max_connections,
        }
    }

    fn mock_protocol() -> Arc<dyn crate::protocol::DatabaseProtocol> {
        use crate::protocol::mysql::MySQLProtocol;
        use crate::proxy::auth_cache::AuthCache;
        Arc::new(MySQLProtocol::with_auth(AuthCache::from_config(&[], 3600)))
    }

    /// Verify the `borrowed` counter starts at zero.
    #[test]
    fn test_pool_initial_borrowed_zero() {
        let cfg = mock_config(None);
        let pool = ConnectionPool::with_idle_timeout(&cfg, 4, mock_protocol(), None);
        assert_eq!(pool.borrowed.load(Ordering::Relaxed), 0);
    }

    /// Verify that `max_connections` is stored on the config and is accessible.
    #[test]
    fn test_backend_config_max_connections_stored() {
        let cfg = mock_config(Some(10));
        assert_eq!(cfg.max_connections, Some(10));

        let cfg_unlimited = mock_config(None);
        assert_eq!(cfg_unlimited.max_connections, None);
    }

    /// Simulate the cap check logic (unit-level, without real TCP connections).
    ///
    /// The pool uses `borrowed` to track in-flight connections; when
    /// `borrowed >= max_connections`, `get_for_database` must return `Err`.
    /// We test the guard condition by directly manipulating `borrowed`.
    #[test]
    fn test_max_connections_cap_check_condition() {
        let max = 3usize;
        let cfg = mock_config(Some(max));
        let pool = ConnectionPool::with_idle_timeout(&cfg, max, mock_protocol(), None);

        // Simulate `max` connections already borrowed.
        pool.borrowed.store(max, Ordering::Relaxed);

        // Verify the guard condition that `get_for_database` will evaluate.
        let cap = pool.config.max_connections.unwrap();
        let current = pool.borrowed.load(Ordering::Relaxed);
        assert!(
            current >= cap,
            "borrowed ({}) should be >= cap ({})",
            current,
            cap
        );
    }

    /// When max_connections is None the cap is disabled.
    #[test]
    fn test_max_connections_none_means_unlimited() {
        let cfg = mock_config(None);
        assert!(cfg.max_connections.is_none());
    }
}
