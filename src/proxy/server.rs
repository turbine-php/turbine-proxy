//! Proxy server — accepts client connections, hands off the MySQL handshake to
//! the protocol layer, then runs the protocol-agnostic command loop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::analytics::Collector;
use crate::analytics::ThroughputCounters;
use crate::config::ProxyConfig;
use crate::protocol::{
    BackendConnection, ClientAuthConfig, ClientSession, Command, DatabaseProtocol,
};
use crate::proxy::app_analytics::AppAnalyticsStore;
use crate::proxy::classifier::{classify, extract_sticky_hint, QueryIntent};
use crate::proxy::error_events::{ErrorEvent, ErrorEventStore};
use crate::proxy::fingerprint::{fingerprint, fingerprint_with_hash};
use crate::proxy::heatmap::HeatmapStore;
use crate::proxy::histogram::QueryHistogram;
use crate::proxy::n1::N1Store;
use crate::proxy::pool::BackendPool;
use crate::proxy::regression::RegressionStore;
use crate::proxy::rewriter::Rewriter;
use crate::proxy::router::Router;
use crate::proxy::rules::RuleEngine;
use crate::proxy::security::{AuditLogger, InjectionDetector, QueryWhitelist};
use crate::proxy::stmt_shadow::MysqlStmtShadow;
use crate::proxy::tracer::{ActiveTrace, TracerStore};
use crate::proxy::user_registry::UserRegistry;

// ─── N+1 / repeated-query tracker ────────────────────────────────────────────

/// Per-connection tracker for repeated query patterns.
/// Warns once when the same fingerprint exceeds `WARN_THRESHOLD` within a single
/// connection, and emits a summary on disconnect for any fingerprint that fired.
/// Entirely stack-local — no locks, no allocations outside this task.
struct SessionQueryTracker {
    /// fingerprint_hash → (count, fingerprint_string)
    counts: HashMap<u64, (u32, String)>,
    /// sql_hash → (count, example_sql) — for hot-key detection
    exact_counts: HashMap<u64, (u32, String)>,
    conn_id: u32,
    n1_store: Arc<N1Store>,
}

impl SessionQueryTracker {
    /// Emit a warning when the same fingerprint is seen this many times.
    const WARN_THRESHOLD: u32 = 5;
    /// Same exact SQL (literals included) this many times in one session → hot key.
    const HOT_KEY_THRESHOLD: u32 = 30;

    fn new(conn_id: u32, n1_store: Arc<N1Store>) -> Self {
        Self {
            counts: HashMap::new(),
            exact_counts: HashMap::new(),
            conn_id,
            n1_store,
        }
    }

    /// Record one execution. Emits a warning the first time threshold is hit.
    fn record(&mut self, sql: &str) {
        let (fp, hash) = fingerprint_with_hash(sql);
        let entry = self.counts.entry(hash).or_insert_with(|| (0, fp.clone()));
        entry.0 += 1;
        if entry.0 == Self::WARN_THRESHOLD {
            log::warn!(
                "[conn {}] N+1 detected: query executed {} times — {}",
                self.conn_id,
                entry.0,
                entry.1
            );
        }
    }

    /// Track exact SQL (with literals) for hot-key detection.
    /// Returns `(fingerprint, example_sql, count)` the first time the threshold
    /// is crossed, so the caller can report to `RegressionStore`.
    fn track_exact(&mut self, sql: &str) -> Option<(String, String, u64)> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        sql.hash(&mut h);
        let sql_hash = h.finish();
        let entry = self
            .exact_counts
            .entry(sql_hash)
            .or_insert_with(|| (0, sql.chars().take(300).collect::<String>()));
        entry.0 += 1;
        if entry.0 == Self::HOT_KEY_THRESHOLD {
            let (fp, _) = fingerprint_with_hash(sql);
            return Some((fp, entry.1.clone(), entry.0 as u64));
        }
        None
    }

    /// Push repeated patterns to the shared N1Store and log a summary.
    fn summarise(&self) {
        let repeated: Vec<(u64, String, u32)> = self
            .counts
            .iter()
            .filter(|(_, (c, _))| *c >= Self::WARN_THRESHOLD)
            .map(|(hash, (c, fp))| (*hash, fp.clone(), *c))
            .collect();
        if repeated.is_empty() {
            return;
        }
        self.n1_store.record_connection(&repeated);
        for (_, fp, count) in &repeated {
            log::info!("[conn {}] repeated query x{}: {}", self.conn_id, count, fp);
        }
    }
}

/// Shared proxy metrics — updated atomically on the hot path.
pub struct ProxyMetrics {
    pub connections_total: AtomicUsize,
    pub connections_active: AtomicUsize,
    pub queries_total: AtomicUsize,
    pub queries_read: AtomicUsize,
    pub queries_write: AtomicUsize,
    /// Latency histogram for SELECT / read queries.
    pub read_hist: QueryHistogram,
    /// Latency histogram for INSERT/UPDATE/DELETE / write queries.
    pub write_hist: QueryHistogram,
    /// Number of transactions aborted because they exceeded `max_transaction_time_ms`.
    pub transactions_killed: AtomicUsize,
    /// Number of queries killed because they exceeded `max_query_time_ms`.
    #[allow(dead_code)]
    pub queries_killed: AtomicUsize,
    /// Number of queries rejected by the SQL injection detector.
    pub sqli_blocked: AtomicUsize,
    /// Number of queries rejected by the whitelist (allowlist mode).
    pub whitelist_blocked: AtomicUsize,
    /// Sessions that became sticky (user-defined variable or LOCK TABLES).
    /// Sticky sessions cannot multiplex backend connections.
    pub sessions_pinned_total: AtomicUsize,
    /// Total failed dashboard authentication attempts (wrong credentials or expired tokens).
    pub dashboard_auth_failures: AtomicUsize,
}

impl ProxyMetrics {
    pub fn new() -> Self {
        Self {
            connections_total: AtomicUsize::new(0),
            connections_active: AtomicUsize::new(0),
            queries_total: AtomicUsize::new(0),
            queries_read: AtomicUsize::new(0),
            queries_write: AtomicUsize::new(0),
            read_hist: QueryHistogram::new(),
            write_hist: QueryHistogram::new(),
            transactions_killed: AtomicUsize::new(0),
            queries_killed: AtomicUsize::new(0),
            sqli_blocked: AtomicUsize::new(0),
            whitelist_blocked: AtomicUsize::new(0),
            sessions_pinned_total: AtomicUsize::new(0),
            dashboard_auth_failures: AtomicUsize::new(0),
        }
    }
}

/// The main proxy server.
pub struct ProxyServer {
    config: ProxyConfig,
    router: Router,
    metrics: Arc<ProxyMetrics>,
    collector: Arc<Collector>,
    connection_id: AtomicU32,
    semaphore: Arc<Semaphore>,
    protocol: Arc<dyn DatabaseProtocol>,
    n1_store: Arc<N1Store>,
    user_registry: Arc<UserRegistry>,
    tracer_store: Arc<TracerStore>,
    app_analytics: Arc<AppAnalyticsStore>,
    heatmap: Arc<HeatmapStore>,
    throughput: Arc<ThroughputCounters>,
    regression_store: Arc<RegressionStore>,
    sqli_detector: Option<Arc<InjectionDetector>>,
    query_whitelist: Arc<QueryWhitelist>,
    audit_logger: Arc<AuditLogger>,
    error_events: Arc<ErrorEventStore>,
    /// Notified when a graceful shutdown is requested (SIGTERM).
    shutdown_notify: Arc<tokio::sync::Notify>,
    /// Set to true once a graceful shutdown begins; /health returns 503.
    draining: Arc<AtomicBool>,
    /// Per-user config snapshot for max_connections, default_schema, tx_isolation.
    users_config: Arc<Vec<crate::config::UserConfig>>,
}

impl ProxyServer {
    pub fn new(
        config: ProxyConfig,
        rule_engine: Arc<RuleEngine>,
        rewriter: Arc<Rewriter>,
        collector: Arc<Collector>,
        metrics: Arc<ProxyMetrics>,
        protocol: Arc<dyn DatabaseProtocol>,
    ) -> Self {
        let idle_timeout = if config.connection_max_idle_secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(
                config.connection_max_idle_secs,
            ))
        };
        let pool = Arc::new(BackendPool::with_options(
            &config.primary,
            &config.replicas,
            config.pool_size,
            protocol.clone(),
            idle_timeout,
            config.ha.circuit_breaker_threshold,
            config.ha.circuit_breaker_recovery_ms,
            config.pool_wait_queue_size,
            config.pool_wait_timeout_ms,
        ));

        let max_query_time_ms = config.max_query_time_ms;
        let router = {
            let r = Router::new(pool, rule_engine, rewriter, protocol.clone());
            r.set_max_query_time_ms(max_query_time_ms);
            r
        };

        let users_config = Arc::new(config.users.clone());

        Self {
            semaphore: Arc::new(Semaphore::new(config.max_connections)),
            heatmap: Arc::new(HeatmapStore::new(config.analytics.slow_query_ms)),
            throughput: Arc::new(ThroughputCounters::new(config.analytics.slow_query_ms)),
            regression_store: Arc::new(RegressionStore::new()),
            sqli_detector: if config.sql_injection_protection {
                Some(Arc::new(InjectionDetector::new()))
            } else {
                None
            },
            query_whitelist: Arc::new(QueryWhitelist::new(&config.query_whitelist)),
            audit_logger: Arc::new(AuditLogger::new(&config.audit_log)),
            error_events: ErrorEventStore::new(1_000),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
            draining: Arc::new(AtomicBool::new(false)),
            users_config,
            config,
            router,
            metrics,
            collector,
            connection_id: AtomicU32::new(1),
            protocol,
            n1_store: Arc::new(N1Store::new()),
            user_registry: Arc::new(UserRegistry::new()),
            tracer_store: Arc::new(TracerStore::new()),
            app_analytics: Arc::new(AppAnalyticsStore::new()),
        }
    }

    /// Access the shared N+1 pattern store (for the dashboard).
    pub fn n1_store(&self) -> Arc<N1Store> {
        self.n1_store.clone()
    }

    /// Access the backend pool (for the health checker).
    pub async fn pool(&self) -> Arc<crate::proxy::pool::BackendPool> {
        self.router.pool().await
    }

    /// Access the proxy router (for live backend reload from the dashboard).
    pub fn router(&self) -> crate::proxy::router::Router {
        self.router.clone()
    }

    /// Number of queries killed by the per-query timeout (`max_query_time_ms`).
    #[allow(dead_code)]
    pub fn queries_killed(&self) -> usize {
        self.router
            .queries_killed
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Hot-reload the backend pool from a new config, without restarting.
    ///
    /// In-flight queries finish on the old pool; new queries use the new pool
    /// from the moment this method returns.  Old idle connections are silently
    /// dropped when the old Arc refcount falls to zero.
    #[allow(dead_code)]
    pub async fn reload_backends(&self, new_config: &crate::config::ProxyConfig) {
        let idle_timeout = if new_config.connection_max_idle_secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(
                new_config.connection_max_idle_secs,
            ))
        };
        let new_pool = Arc::new(crate::proxy::pool::BackendPool::with_options(
            &new_config.primary,
            &new_config.replicas,
            new_config.pool_size,
            self.protocol.clone(),
            idle_timeout,
            new_config.ha.circuit_breaker_threshold,
            new_config.ha.circuit_breaker_recovery_ms,
            new_config.pool_wait_queue_size,
            new_config.pool_wait_timeout_ms,
        ));
        self.router.reload_pool(new_pool).await;
        log::info!(
            "[reload] backend pool swapped — primary={} replicas={}",
            new_config.primary.addr,
            new_config.replicas.len()
        );
    }

    /// Access the user registry (for the dashboard).
    pub fn user_registry(&self) -> Arc<UserRegistry> {
        self.user_registry.clone()
    }

    /// Access the transaction trace store (for the dashboard).
    pub fn tracer_store(&self) -> Arc<TracerStore> {
        self.tracer_store.clone()
    }

    /// Access the per-app/user/IP analytics store (for the dashboard).
    pub fn app_analytics(&self) -> Arc<AppAnalyticsStore> {
        self.app_analytics.clone()
    }

    /// Access the temporal heatmap store (for the dashboard).
    pub fn heatmap(&self) -> Arc<HeatmapStore> {
        self.heatmap.clone()
    }

    /// Access the throughput counters (for the time-series background task).
    pub fn throughput(&self) -> Arc<ThroughputCounters> {
        self.throughput.clone()
    }

    /// Access the regression alert store (for the dashboard and check task).
    pub fn regression_store(&self) -> Arc<RegressionStore> {
        self.regression_store.clone()
    }

    /// Access the error event store (for the dashboard).
    pub fn error_events(&self) -> Arc<ErrorEventStore> {
        self.error_events.clone()
    }

    /// Access the shutdown notification handle (for the SIGTERM handler).
    pub fn shutdown_notify(&self) -> Arc<tokio::sync::Notify> {
        self.shutdown_notify.clone()
    }

    /// Access the draining flag (for the /health endpoint).
    pub fn draining(&self) -> Arc<AtomicBool> {
        self.draining.clone()
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config.listen_addr)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to bind to {}: {} — is the port already in use?",
                    self.config.listen_addr,
                    e
                )
            })?;
        log::info!("TurbineProxy listening on {}", self.config.listen_addr);

        let proxy_protocol_enabled = self.config.proxy_protocol.enabled;
        let client_error_limit = self.config.client_error_limit;
        let client_error_window_secs = self.config.client_error_window_secs;

        loop {
            let (socket, addr) = tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok(pair) => pair,
                        Err(e) => {
                            log::error!("Accept error: {}", e);
                            continue;
                        }
                    }
                }
                _ = self.shutdown_notify.notified() => {
                    log::info!("Graceful shutdown: stopped accepting new connections");
                    self.draining.store(true, Ordering::Relaxed);
                    break;
                }
            };
            socket.set_nodelay(true)?;

            let permit = match self.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    log::warn!("Max connections reached, rejecting {}", addr);
                    drop(socket);
                    continue;
                }
            };

            let conn_id = self.connection_id.fetch_add(1, Ordering::Relaxed);
            let router = self.router.clone();
            let metrics = self.metrics.clone();
            let collector = self.collector.clone();
            let protocol = self.protocol.clone();
            let n1_store = self.n1_store.clone();
            let user_registry = self.user_registry.clone();
            let tracer_store = self.tracer_store.clone();
            let app_analytics = self.app_analytics.clone();
            let heatmap = self.heatmap.clone();
            let throughput = self.throughput.clone();
            let regression_store = self.regression_store.clone();
            let max_transaction_time_ms = self.config.max_transaction_time_ms;
            let max_transaction_idle_ms = self.config.max_transaction_idle_ms;
            let read_your_own_writes_ms = self.config.read_your_own_writes_ms;
            let gtid_aware_ryow = self.config.gtid_aware_ryow;
            let fast_forward = self.config.fast_forward;
            let select_version_forwarding = self.config.select_version_forwarding;
            let server_version = self.config.server_version.clone();
            let sqli_detector = self.sqli_detector.clone();
            let query_whitelist = self.query_whitelist.clone();
            let audit_logger = self.audit_logger.clone();
            let error_events = self.error_events.clone();
            let users_config = self.users_config.clone();
            let log_prepared_params = self.config.log_prepared_params;

            metrics.connections_total.fetch_add(1, Ordering::Relaxed);
            metrics.connections_active.fetch_add(1, Ordering::Relaxed);

            tokio::spawn(async move {
                if let Err(e) = handle_connection(
                    socket,
                    conn_id,
                    addr.to_string(),
                    router,
                    metrics.clone(),
                    collector,
                    protocol,
                    n1_store,
                    user_registry,
                    tracer_store,
                    app_analytics,
                    heatmap,
                    throughput,
                    regression_store,
                    max_transaction_time_ms,
                    max_transaction_idle_ms,
                    read_your_own_writes_ms,
                    gtid_aware_ryow,
                    fast_forward,
                    select_version_forwarding,
                    server_version,
                    sqli_detector,
                    query_whitelist,
                    audit_logger,
                    error_events,
                    users_config,
                    proxy_protocol_enabled,
                    client_error_limit,
                    client_error_window_secs,
                    log_prepared_params,
                )
                .await
                {
                    log::debug!("Connection {} error: {}", conn_id, e);
                }
                metrics.connections_active.fetch_sub(1, Ordering::Relaxed);
                drop(permit);
            });
        }

        // Drain: wait for active connections to finish, up to shutdown_timeout_secs.
        let timeout_secs = self.config.shutdown_timeout_secs;
        if timeout_secs > 0 && self.metrics.connections_active.load(Ordering::Relaxed) > 0 {
            log::info!(
                "Draining {} active connections (timeout {}s)…",
                self.metrics.connections_active.load(Ordering::Relaxed),
                timeout_secs
            );
            let deadline = Instant::now() + std::time::Duration::from_secs(timeout_secs);
            while self.metrics.connections_active.load(Ordering::Relaxed) > 0 {
                if Instant::now() >= deadline {
                    log::warn!("Shutdown drain timeout reached — forcing exit");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        log::info!("TurbineProxy shutdown complete");
        Ok(())
    }
}

/// Handle a single client connection.
#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    mut stream: TcpStream,
    conn_id: u32,
    client_addr: String,
    router: Router,
    metrics: Arc<ProxyMetrics>,
    collector: Arc<Collector>,
    protocol: Arc<dyn DatabaseProtocol>,
    n1_store: Arc<N1Store>,
    user_registry: Arc<UserRegistry>,
    tracer_store: Arc<TracerStore>,
    app_analytics: Arc<AppAnalyticsStore>,
    heatmap: Arc<HeatmapStore>,
    throughput: Arc<ThroughputCounters>,
    regression_store: Arc<RegressionStore>,
    max_transaction_time_ms: u64,
    max_transaction_idle_ms: u64,
    read_your_own_writes_ms: u64,
    gtid_aware_ryow: bool,
    fast_forward: bool,
    select_version_forwarding: bool,
    server_version: String,
    sqli_detector: Option<Arc<InjectionDetector>>,
    query_whitelist: Arc<QueryWhitelist>,
    audit_logger: Arc<AuditLogger>,
    error_events: Arc<ErrorEventStore>,
    users_config: Arc<Vec<crate::config::UserConfig>>,
    proxy_protocol_enabled: bool,
    client_error_limit: u32,
    client_error_window_secs: u64,
    log_prepared_params: bool,
) -> anyhow::Result<()> {
    // PROXY Protocol v1: extract real client IP before the MySQL handshake.
    // When enabled (e.g. behind HAProxy), the load balancer prefixes every TCP
    // connection with: "PROXY TCP4 <real_ip> <proxy_ip> <sport> <dport>\r\n"
    let client_addr = if proxy_protocol_enabled {
        parse_proxy_header(&mut stream).await.unwrap_or(client_addr)
    } else {
        client_addr
    };

    let auth_config = ClientAuthConfig {
        connection_id: conn_id,
        server_version,
    };

    let mut session: Box<dyn ClientSession> =
        match protocol.accept_client(stream, &auth_config).await {
            Ok(s) => s,
            Err(e) => {
                log::debug!("Handshake failed for conn {}: {}", conn_id, e);
                return Ok(());
            }
        };

    // Register this connection in the user registry.
    user_registry
        .on_connect(session.username(), session.allow_writes())
        .await;
    let session_username = session.username().to_string();
    let session_app = session.app_name().to_string();
    // client_addr may be "ip:port" — strip port for the per-IP dimension.
    let client_ip = client_addr
        .rsplit_once(':')
        .map(|(ip, _)| ip.to_string())
        .unwrap_or_else(|| client_addr.clone());
    app_analytics
        .on_connect(&session_username, &client_ip, &session_app)
        .await;

    // Sticky connection for transactions — must use the same backend.
    let mut tx_conn: Option<Box<dyn BackendConnection>> = None;
    // Sticky connection for prepared statements — held while mysql_shadow has
    // open entries; released immediately when all stmts are closed.
    let mut stmt_conn: Option<Box<dyn BackendConnection>> = None;
    // Once a session executes SET @variable or references LAST_INSERT_ID() after
    // a write, we must keep the same backend connection (conservative safety).
    let mut user_var_sticky = false;
    // Session-initialisation SQL statements to re-apply when we acquire a new
    // backend connection.  Populated whenever is_session_pinning_query fires.
    // Kept deliberately small — in practice only a handful of SET statements.
    let mut session_init_sqls: Vec<String> = Vec::new();
    // Per-session MySQL statement shadow map (proxy-level stmt_id remapping +
    // transparent re-prepare on backend death).
    let mut mysql_shadow = MysqlStmtShadow::new();
    // Wall-clock timestamp when the current transaction opened (None = no open tx).
    let mut tx_start: Option<Instant> = None;
    // Timestamp of the last query inside the current transaction (for idle timeout).
    let mut last_query_in_tx: Option<Instant> = None;
    // Read-Your-Own-Writes: after a write, route reads to primary until this instant.
    // None means RYOW is not active (reads go to replica as normal).
    let mut ryow_until: Option<Instant> = None;
    // GTID-aware RYOW: the last GTID position returned by a successful write.
    // When `gtid_aware_ryow` is enabled, the proxy checks replicas for this GTID
    // before routing reads there, rather than using the time-based window.
    let mut last_write_gtid: Option<String> = None;
    let mut query_tracker = SessionQueryTracker::new(conn_id, n1_store);
    // Per-session transaction trace accumulator (None when not in a transaction).
    let mut active_trace: Option<ActiveTrace> = None;
    // Per-connection consecutive error counter for client_error_limit enforcement.
    let mut consecutive_errors: u32 = 0;
    // Sticky-backend hint: `/* sticky_backend=1 */` pins reads to one replica
    // for read-consistency without a full transaction.
    // sticky_hint_conn holds the pinned connection; sticky_hint_idx is the
    // replica pool index (usize::MAX = primary fallback).
    let mut sticky_hint_conn: Option<Box<dyn BackendConnection>> = None;
    let mut sticky_hint_idx: usize = usize::MAX;
    let mut sticky_hint_active = false;
    let mut error_window_start: Option<Instant> = None;
    // Warn once per session when a write query is routed through the failover
    // backend (primary is down).  Reset to false if failover clears.
    let mut warned_failover_write = false;

    // ── Per-user session initialisation ────────────────────────────────────────
    // Look up user config for: max_connections check, default_schema injection,
    // and per-user transaction isolation level injection.
    if let Some(user_cfg) = users_config.iter().find(|u| u.name == session_username) {
        // max_connections enforcement (0 = unlimited)
        if user_cfg.max_connections > 0 {
            // on_connect has already incremented the counter — check if we exceeded it.
            let active = user_registry.active_connections(&session_username).await;
            if active > user_cfg.max_connections {
                log::warn!(
                    "[conn {}] user '{}' exceeded max_connections ({}/{})",
                    conn_id,
                    session_username,
                    active,
                    user_cfg.max_connections
                );
                let _ = session
                    .write_error("1040", "Too many connections for this user")
                    .await;
                let _ = session.flush().await;
                user_registry.on_disconnect(&session_username).await;
                app_analytics
                    .on_disconnect(&session_username, &client_ip, &session_app)
                    .await;
                return Ok(());
            }
        }
        // Inject default_schema as the first session init SQL.
        if !user_cfg.default_schema.is_empty() {
            session_init_sqls.push(format!("USE `{}`", user_cfg.default_schema));
        }
        // Inject per-user transaction isolation level.
        if !user_cfg.transaction_isolation.is_empty() {
            session_init_sqls.push(format!(
                "SET SESSION TRANSACTION ISOLATION LEVEL {}",
                user_cfg.transaction_isolation
            ));
        }
    }

    loop {
        let cmd = match session.read_command().await {
            Ok(c) => c,
            Err(_) => break, // client disconnected
        };

        match cmd {
            Command::Quit => break,

            Command::Ping => {
                if let Err(e) = session.send_ok().await {
                    log::debug!("Ping send_ok error: {}", e);
                    break;
                }
            }

            Command::ResetConnection => {
                // Clear all session-local state without closing the connection.
                // The client wants a clean slate (typical after connection-pool recycle).
                session_init_sqls.clear();
                user_var_sticky = false;
                mysql_shadow = MysqlStmtShadow::new();
                tx_start = None;
                last_query_in_tx = None;
                session.set_in_transaction(false);
                consecutive_errors = 0;
                error_window_start = None;
                // Drain traces for the abandoned in-flight transaction.
                if let Some(trace) = active_trace.take() {
                    tracer_store.push(trace.finish("reset"));
                }
                // Return sticky connections to pool.
                if let Some(conn) = stmt_conn.take() {
                    router.put_primary(conn).await;
                }
                if let Some(conn) = tx_conn.take() {
                    router.put_primary(conn).await;
                }
                if let Some(conn) = sticky_hint_conn.take() {
                    router.put_replica(conn, sticky_hint_idx).await;
                }
                sticky_hint_idx = usize::MAX;
                sticky_hint_active = false;
                if let Err(e) = session.send_ok().await {
                    log::debug!("COM_RESET_CONNECTION send_ok error: {}", e);
                    break;
                }
            }

            Command::Query(sql_bytes) => {
                let sql = std::str::from_utf8(&sql_bytes).unwrap_or("");
                let in_tx = session.is_in_transaction();

                // ── FAST-FORWARD: bypass routing/analytics/security overhead ────────────
                // When enabled, every COM_QUERY is forwarded directly to the
                // primary with no fingerprinting, rewriting, rules, cache, RYOW,
                // N+1 detection, or audit logging.  Only basic transaction state
                // (BEGIN/COMMIT/ROLLBACK) is tracked to keep sticky connections
                // correct.  Metrics.queries_total is still incremented.
                if fast_forward {
                    let upper = sql.trim().to_ascii_uppercase();
                    let tx_was = in_tx; // capture before any state change
                    if upper.starts_with("BEGIN") || upper.starts_with("START ") {
                        session.set_in_transaction(true);
                    } else if upper.starts_with("COMMIT") || upper.starts_with("ROLLBACK") {
                        session.set_in_transaction(false);
                        // execute below on the existing tx_conn, then release it
                    }
                    metrics.queries_total.fetch_add(1, Ordering::Relaxed);
                    let result = router.route_fast(&sql_bytes, &mut tx_conn, tx_was).await;
                    // After COMMIT / ROLLBACK, return the sticky conn to the pool.
                    if upper.starts_with("COMMIT") || upper.starts_with("ROLLBACK") {
                        if let Some(conn) = tx_conn.take() {
                            router.put_primary(conn).await;
                        }
                    }
                    match result {
                        Ok(response) => {
                            if let Err(e) = session.write_response(&response.bytes).await {
                                log::debug!("[ff] write error: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            if let Err(we) = session.write_error("1", &e.to_string()).await {
                                log::debug!("[ff] write error packet failed: {}", we);
                            }
                        }
                    }
                    if let Err(e) = session.flush().await {
                        log::debug!("[ff] flush error: {}", e);
                        break;
                    }
                    continue;
                }

                // ── PER-RULE FAST-FORWARD ────────────────────────────────────
                // When a query rule has `fast_forward = true`, bypass the full
                // routing/analytics pipeline for this specific query pattern,
                // identical to the global fast_forward path above but scoped to
                // queries that match the rule.
                if !fast_forward
                    && !in_tx
                    && router
                        .is_fast_forward_rule(sql, session.username(), session.database())
                        .await
                {
                    log::debug!(
                        "[conn {}] per-rule fast-forward: {}",
                        conn_id,
                        &sql[..sql.len().min(60)]
                    );
                    metrics.queries_total.fetch_add(1, Ordering::Relaxed);
                    let result = router.route_fast(&sql_bytes, &mut tx_conn, false).await;
                    match result {
                        Ok(response) => {
                            if let Err(e) = session.write_response(&response.bytes).await {
                                log::debug!("[rule-ff] write error: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            if let Err(we) = session.write_error("1", &e.to_string()).await {
                                log::debug!("[rule-ff] write error packet failed: {}", we);
                            }
                        }
                    }
                    if let Err(e) = session.flush().await {
                        log::debug!("[rule-ff] flush error: {}", e);
                        break;
                    }
                    continue;
                }

                let intent = classify(sql);
                let was_read = matches!(intent, QueryIntent::Read) && !in_tx;

                // Read-Your-Own-Writes: expire the RYOW window if it has passed.
                if let Some(until) = ryow_until {
                    if Instant::now() >= until {
                        ryow_until = None;
                    }
                }
                // GTID-aware RYOW: if enabled and we have a pending write GTID,
                // check whether any replica has applied it.  If yes, clear the GTID
                // (reads may use replicas freely).  If no, force primary.
                let gtid_ryow_force_primary = if gtid_aware_ryow && was_read {
                    if let Some(ref gtid) = last_write_gtid {
                        let replica_ready = router.check_replica_has_gtid(gtid).await;
                        if replica_ready {
                            last_write_gtid = None;
                            false // replica is up-to-date
                        } else {
                            true // replica lagging — force primary
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                // Force primary for reads during the time-based RYOW window OR
                // when GTID-aware check says replica is not ready.
                let ryow_active = ryow_until.is_some() || gtid_ryow_force_primary;

                metrics.queries_total.fetch_add(1, Ordering::Relaxed);
                let t0 = Instant::now();

                // Detect session-pinning statements: user vars (@var), system vars
                // (SET @@session.x), SET NAMES, SET CHARACTER SET.
                // Once triggered we must keep using the same backend connection
                // because these settings are connection-scoped on MySQL.
                // ── Session-state detection (Fase A multiplexing) ────────────────
                // Hard-pin only when session state cannot be replayed:
                //   @user_var assignment, SELECT @var :=, LOCK TABLES.
                // Replayable SET statements (SET NAMES, SET CHARACTER SET,
                // SET SESSION var=literal) go into session_init_sqls without
                // pinning — the router replays them on every fresh connection.
                let hard_pin = needs_hard_pin(sql);
                let replayable = !hard_pin && is_replayable_session_stmt(sql);
                if hard_pin || replayable {
                    let owned = sql.to_string();
                    if !session_init_sqls.contains(&owned) {
                        session_init_sqls.push(owned);
                    }
                }
                if hard_pin && !user_var_sticky {
                    user_var_sticky = true;
                    metrics
                        .sessions_pinned_total
                        .fetch_add(1, Ordering::Relaxed);
                    log::debug!(
                        "[conn {}] [multiplex] session pinned: user-var / LOCK TABLES — multiplexing disabled",
                        conn_id
                    );
                }

                // Update client-side transaction state for routing decisions.
                if matches!(intent, QueryIntent::Transaction) {
                    let upper = sql.trim().to_uppercase();
                    if upper.starts_with("BEGIN") || upper.starts_with("START") {
                        session.set_in_transaction(true);
                        tx_start = Some(Instant::now());
                        // Start a new trace for this transaction.
                        active_trace =
                            Some(ActiveTrace::new(conn_id, &session_username, &client_addr));
                    } else if upper.starts_with("COMMIT") || upper.starts_with("ROLLBACK") {
                        session.set_in_transaction(false);
                        tx_start = None;
                        // Finalise the trace and push to the store.
                        if let Some(trace) = active_trace.take() {
                            let outcome = if upper.starts_with("COMMIT") {
                                "commit"
                            } else {
                                "rollback"
                            };
                            tracer_store.push(trace.finish(outcome));
                        }
                        // Release user-var sticky conn only when transaction ends
                        // and no open stmts — the tx_conn will be returned to pool.
                    }
                }

                // Enforce max_transaction_time_ms: kill transactions that have been
                // open longer than the configured limit. 0 = disabled.
                let max_tx_ms = max_transaction_time_ms;
                if max_tx_ms > 0 {
                    if let Some(ts) = tx_start {
                        if ts.elapsed().as_millis() as u64 > max_tx_ms {
                            // Abort the transaction.
                            session.set_in_transaction(false);
                            tx_start = None;
                            if let Some(conn) = tx_conn.take() {
                                drop(conn); // discard the sticky conn
                            }
                            if let Some(trace) = active_trace.take() {
                                tracer_store.push(trace.finish("killed"));
                            }
                            metrics.transactions_killed.fetch_add(1, Ordering::Relaxed);
                            log::warn!(
                                "[conn {}] transaction killed: exceeded max_transaction_time_ms ({}ms)",
                                conn_id, max_tx_ms
                            );
                            if let Err(e) = session
                                .write_error(
                                    "1205",
                                    &format!(
                                        "Transaction timeout: exceeded max_transaction_time_ms ({}ms)",
                                        max_tx_ms
                                    ),
                                )
                                .await
                            {
                                log::debug!("Write error packet failed: {}", e);
                            }
                            if let Err(e) = session.flush().await {
                                log::debug!("Flush error: {}", e);
                                break;
                            }
                            continue;
                        }
                    }
                }

                // Enforce max_transaction_idle_ms: kill transactions that have been
                // idle (no query) longer than the configured limit. 0 = disabled.
                if max_transaction_idle_ms > 0 {
                    if let Some(last_q) = last_query_in_tx {
                        if tx_start.is_some()
                            && last_q.elapsed().as_millis() as u64 > max_transaction_idle_ms
                        {
                            session.set_in_transaction(false);
                            tx_start = None;
                            last_query_in_tx = None;
                            if let Some(conn) = tx_conn.take() {
                                drop(conn);
                            }
                            if let Some(trace) = active_trace.take() {
                                tracer_store.push(trace.finish("idle_killed"));
                            }
                            metrics.transactions_killed.fetch_add(1, Ordering::Relaxed);
                            log::warn!(
                                "[conn {}] transaction killed: idle for > {}ms",
                                conn_id,
                                max_transaction_idle_ms
                            );
                            if let Err(e) = session
                                .write_error(
                                    "1205",
                                    &format!(
                                        "Transaction idle timeout: no query for {}ms",
                                        max_transaction_idle_ms
                                    ),
                                )
                                .await
                            {
                                log::debug!("Write error packet failed: {}", e);
                            }
                            if let Err(e) = session.flush().await {
                                log::debug!("Flush error: {}", e);
                                break;
                            }
                            continue;
                        }
                    }
                }

                if was_read {
                    metrics.queries_read.fetch_add(1, Ordering::Relaxed);
                } else if matches!(intent, QueryIntent::Write) {
                    metrics.queries_write.fetch_add(1, Ordering::Relaxed);
                    // Warn once per session if this write will land on a failover
                    // replica because the configured primary is unreachable.
                    if !warned_failover_write && router.failover_active_nowait() {
                        warned_failover_write = true;
                        log::warn!(
                            "[HA conn {}] write query routed through failover backend — configured primary is down",
                            conn_id
                        );
                    } else if warned_failover_write && !router.failover_active_nowait() {
                        // Primary recovered during this session — reset so we warn again if it fails again.
                        warned_failover_write = false;
                    }
                }

                // Enforce per-user write restrictions before routing.
                if !session.allow_writes() && matches!(intent, QueryIntent::Write) {
                    log::warn!(
                        "[conn {}] user '{}' attempted write — denied (read-only)",
                        conn_id,
                        session.username()
                    );
                    if let Err(e) = session
                        .write_error(
                            "1290",
                            "User is not allowed to write (read-only proxy account)",
                        )
                        .await
                    {
                        log::debug!("Write error packet failed: {}", e);
                    }
                    if let Err(e) = session.flush().await {
                        log::debug!("Flush error: {}", e);
                        break;
                    }
                    continue;
                }

                // User-var stickiness: reuse tx_conn slot so the same backend
                // connection is used for queries that set @variables.
                let effective_in_tx = session.is_in_transaction() || user_var_sticky;

                // ── SELECT VERSION() forwarding ─────────────────────────────
                // When select_version_forwarding is enabled and the query is a
                // simple VERSION probe, respond with a synthetic resultset so we
                // avoid a backend round-trip.  We build a minimal MySQL text
                // resultset inline (1 column, 1 row) using the same framing that
                // ClientSession::write_response() expects.
                if select_version_forwarding && !effective_in_tx {
                    let sql_up = sql.trim().to_uppercase();
                    if sql_up == "SELECT VERSION()"
                        || sql_up == "SELECT @@VERSION"
                        || sql_up == "SELECT @@GLOBAL.VERSION"
                    {
                        let version_str = b"8.0.36-TurbineProxy";
                        // A minimal MySQL text-protocol resultset:
                        // 1. Column-count  packet  (0x01)
                        // 2. Column-def packet
                        // 3. EOF marker (0xfe 0x00 0x00 0x02 0x00)
                        // 4. Row  data packet (length-encoded string)
                        // 5. EOF marker
                        // Each packet is prefixed with 3-byte length + 1-byte seq.
                        fn mysql_pkt(payload: &[u8], seq: u8) -> Vec<u8> {
                            let len = payload.len();
                            let mut p = Vec::with_capacity(4 + len);
                            p.push((len & 0xff) as u8);
                            p.push(((len >> 8) & 0xff) as u8);
                            p.push(((len >> 16) & 0xff) as u8);
                            p.push(seq);
                            p.extend_from_slice(payload);
                            p
                        }
                        fn lc_str(s: &[u8]) -> Vec<u8> {
                            let mut v = Vec::with_capacity(1 + s.len());
                            v.push(s.len() as u8);
                            v.extend_from_slice(s);
                            v
                        }
                        let col_name = b"VERSION()";
                        // Column definition packet
                        let mut col_def: Vec<u8> = Vec::new();
                        col_def.extend(lc_str(b"def")); // catalog
                        col_def.extend(lc_str(b"")); // schema
                        col_def.extend(lc_str(b"")); // table
                        col_def.extend(lc_str(b"")); // org_table
                        col_def.extend(lc_str(col_name)); // name
                        col_def.extend(lc_str(col_name)); // org_name
                        col_def.push(0x0c); // length of fixed-length fields
                        col_def.extend_from_slice(&[0x2d, 0x00]); // charset utf8
                        col_def.extend_from_slice(&[0x1e, 0x00, 0x00, 0x00]); // col_length
                        col_def.push(0xfd); // MYSQL_TYPE_VAR_STRING
                        col_def.extend_from_slice(&[0x00, 0x00]); // flags
                        col_def.push(0x1f); // decimals
                        col_def.extend_from_slice(&[0x00, 0x00]); // filler
                        let eof = [0xfe_u8, 0x00, 0x00, 0x02, 0x00];
                        let row_data = lc_str(version_str);
                        let mut resp: Vec<u8> = Vec::new();
                        resp.extend(mysql_pkt(&[0x01], 1)); // col count
                        resp.extend(mysql_pkt(&col_def, 2)); // col def
                        resp.extend(mysql_pkt(&eof, 3)); // EOF
                        resp.extend(mysql_pkt(&row_data, 4)); // row
                        resp.extend(mysql_pkt(&eof, 5)); // EOF
                        if let Err(e) = session.write_response(&resp).await {
                            log::debug!("VERSION response write error: {}", e);
                        }
                        let _ = session.flush().await;
                        continue;
                    }
                }

                // ── SQL injection detection ─────────────────────────────────
                if let Some(ref detector) = sqli_detector {
                    if let Some(pattern) = detector.check(sql) {
                        metrics.sqli_blocked.fetch_add(1, Ordering::Relaxed);
                        log::warn!(
                            "[conn {}] SQL injection blocked (user={} ip={}): matched pattern {}",
                            conn_id,
                            session.username(),
                            client_ip,
                            pattern
                        );
                        audit_logger.log(
                            session.username(),
                            &client_ip,
                            sql,
                            "blocked:sqli",
                            0.0,
                            true,
                        );
                        error_events.push(ErrorEvent::new(
                            0,
                            format!("SQL injection blocked: {}", pattern),
                            fingerprint(sql),
                            "",
                            &client_ip,
                            session.username(),
                            0.0,
                        ));
                        if let Err(e) = session
                            .write_error("1045", "Query blocked: potential SQL injection detected")
                            .await
                        {
                            log::debug!("Write error packet failed: {}", e);
                        }
                        let _ = session.flush().await;
                        continue;
                    }
                }

                // ── Whitelist enforcement ────────────────────────────────────
                if !query_whitelist.is_allowed(sql) {
                    metrics.whitelist_blocked.fetch_add(1, Ordering::Relaxed);
                    log::warn!(
                        "[conn {}] whitelist blocked (user={} ip={}): query not in allowlist",
                        conn_id,
                        session.username(),
                        client_ip
                    );
                    audit_logger.log(
                        session.username(),
                        &client_ip,
                        sql,
                        "blocked:whitelist",
                        0.0,
                        true,
                    );
                    error_events.push(ErrorEvent::new(
                        0,
                        "Query not permitted: not in the query allowlist",
                        fingerprint(sql),
                        "",
                        &client_ip,
                        session.username(),
                        0.0,
                    ));
                    if let Err(e) = session
                        .write_error("1045", "Query not permitted: not in the query allowlist")
                        .await
                    {
                        log::debug!("Write error packet failed: {}", e);
                    }
                    let _ = session.flush().await;
                    continue;
                }

                // ── Sticky-backend hint ──────────────────────────────────────
                // Process `/* sticky_backend=N */` before routing.  The hint
                // only applies to reads outside of an explicit transaction.
                if let Some(val) = extract_sticky_hint(sql) {
                    if val {
                        sticky_hint_active = true;
                        log::debug!(
                            "[conn {}] sticky_backend=1 — replica stickiness enabled",
                            conn_id
                        );
                    } else if sticky_hint_active {
                        sticky_hint_active = false;
                        if let Some(conn) = sticky_hint_conn.take() {
                            router.put_replica(conn, sticky_hint_idx).await;
                        }
                        sticky_hint_idx = usize::MAX;
                        log::debug!(
                            "[conn {}] sticky_backend=0 — replica stickiness cleared",
                            conn_id
                        );
                    }
                }

                // When sticky hint is active and we are not already in an
                // explicit transaction, route through the pinned replica.
                if sticky_hint_active && !effective_in_tx && was_read {
                    match router
                        .route_sticky_query(&sql_bytes, &mut sticky_hint_conn, &mut sticky_hint_idx)
                        .await
                    {
                        Ok(response) => {
                            if let Err(e) = session.write_response(&response.bytes).await {
                                log::debug!(
                                    "[conn {}] write sticky response error: {}",
                                    conn_id,
                                    e
                                );
                                break;
                            }
                            if let Err(e) = session.flush().await {
                                log::debug!("[conn {}] flush error (sticky): {}", conn_id, e);
                                break;
                            }
                            continue;
                        }
                        Err(e) => {
                            let msg = e.to_string().to_lowercase();
                            let is_conn_lost = msg.contains("gone away")
                                || msg.contains("eof")
                                || msg.contains("broken pipe")
                                || msg.contains("connection reset")
                                || msg.contains("2006")
                                || msg.contains("2013");
                            if is_conn_lost {
                                log::warn!(
                                    "[conn {}] sticky backend lost — clearing hint, retrying normally",
                                    conn_id
                                );
                                // Drop rather than return — connection is dead.
                                sticky_hint_conn = None;
                                sticky_hint_active = false;
                                sticky_hint_idx = usize::MAX;
                                // Fall through to normal routing below.
                            } else {
                                if let Err(we) = session.write_error("1", &e.to_string()).await {
                                    log::debug!("Write error packet failed: {}", we);
                                }
                                let _ = session.flush().await;
                                continue;
                            }
                        }
                    }
                }

                let result = if session_init_sqls.is_empty() {
                    router
                        .route_query(
                            &sql_bytes,
                            &mut tx_conn,
                            effective_in_tx,
                            // RYOW: if the window is active, force primary (use_replica = false).
                            was_read && !user_var_sticky && !ryow_active,
                            session.username(),
                            "",
                        )
                        .await
                } else {
                    router
                        .route_query_with_session_vars(
                            &sql_bytes,
                            &mut tx_conn,
                            effective_in_tx,
                            was_read && !user_var_sticky && !ryow_active,
                            session.username(),
                            "",
                            &session_init_sqls,
                        )
                        .await
                };

                let is_error = result.is_err();
                // Read-Your-Own-Writes: set/extend the RYOW window after a successful write.
                if !is_error
                    && !was_read
                    && matches!(intent, QueryIntent::Write)
                    && read_your_own_writes_ms > 0
                {
                    ryow_until = Some(
                        Instant::now() + std::time::Duration::from_millis(read_your_own_writes_ms),
                    );
                }
                // Update last-query-in-tx timestamp for idle timeout tracking.
                if tx_start.is_some() {
                    last_query_in_tx = Some(Instant::now());
                }
                match result {
                    Ok(response) => {
                        consecutive_errors = 0;
                        error_window_start = None;
                        // ── GTID-aware RYOW: capture write GTID from OK packet ────────
                        if gtid_aware_ryow && !was_read && !response.is_error {
                            if let Some(ref gtid) = response.write_gtid {
                                if !gtid.is_empty() {
                                    last_write_gtid = Some(gtid.clone());
                                    log::debug!(
                                        "[conn {}] GTID captured after write: {}",
                                        conn_id,
                                        gtid
                                    );
                                }
                            }
                        }
                        // ── Session-track: capture variable changes from OK packets ───
                        // When MySQL `session_track_system_variables='*'` is active on the
                        // backend, changes inside stored procedures / triggers arrive here.
                        // We convert each to a SET statement and add it to session_init_sqls
                        // so the next backend connection re-applies them.
                        if !response.session_changes.is_empty() {
                            for (name, value) in &response.session_changes {
                                let set_stmt = format!("SET SESSION {}={:?}", name, value);
                                if !session_init_sqls.contains(&set_stmt) {
                                    // session_track changes are always replayable system
                                    // variables — add to init_sqls but do NOT pin the
                                    // session. The router will replay them on fresh conns.
                                    session_init_sqls.push(set_stmt);
                                }
                            }
                            log::debug!(
                                "[conn {}] session-track: {} variable change(s) captured",
                                conn_id,
                                response.session_changes.len()
                            );
                        }
                        if let Err(e) = session.write_response(&response.bytes).await {
                            log::debug!("Write response error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        log::warn!("Backend error for query: {}", e);
                        let err_str = e.to_string();
                        let fp = fingerprint(sql);
                        error_events.push(ErrorEvent::new(
                            1,
                            err_str.clone(),
                            &fp,
                            "",
                            &client_ip,
                            session.username(),
                            t0.elapsed().as_secs_f64() * 1000.0,
                        ));

                        // Mid-transaction backend-death recovery:
                        // If the backend connection died while a transaction was open,
                        // clean up the sticky connection so the client can retry with a
                        // new BEGIN rather than leaving the session in a broken state.
                        let is_gone = err_str.contains("2006")
                            || err_str.contains("2013")
                            || err_str.to_lowercase().contains("gone away")
                            || err_str.to_lowercase().contains("broken pipe")
                            || err_str.to_lowercase().contains("connection reset");
                        if is_gone && session.is_in_transaction() {
                            log::warn!(
                                "[conn {}] backend died mid-transaction — clearing tx state, client can retry",
                                conn_id
                            );
                            session.set_in_transaction(false);
                            tx_start = None;
                            last_query_in_tx = None;
                            if let Some(conn) = tx_conn.take() {
                                drop(conn); // discard the dead connection
                            }
                            if let Some(trace) = active_trace.take() {
                                tracer_store.push(trace.finish("backend_error"));
                            }
                            // Fall through to send the error to the client — do NOT break.
                        }

                        if let Err(we) = session.write_error("1", &err_str).await {
                            log::debug!("Write error packet failed: {}", we);
                        }

                        // Client error limit: disconnect after N consecutive errors.
                        if client_error_limit > 0 {
                            let now = Instant::now();
                            // Reset window if expired.
                            if error_window_start
                                .map(|s| {
                                    now.duration_since(s).as_secs() >= client_error_window_secs
                                })
                                .unwrap_or(false)
                            {
                                consecutive_errors = 0;
                                error_window_start = None;
                            }
                            consecutive_errors += 1;
                            if error_window_start.is_none() {
                                error_window_start = Some(now);
                            }
                            if consecutive_errors >= client_error_limit {
                                log::warn!(
                                    "[conn {}] client error limit reached ({}/{} errors in {}s window) — closing",
                                    conn_id, consecutive_errors, client_error_limit, client_error_window_secs
                                );
                                let _ = session.flush().await;
                                break;
                            }
                        }
                    }
                }

                // Track repeated queries (N+1 detection) — pure stack, no alloc on hot path.
                query_tracker.record(sql);

                // Hot-key detection: same exact SQL > threshold in this session.
                if let Some((fp, example_sql, count)) = query_tracker.track_exact(sql) {
                    regression_store.report_hot_key(&fp, &example_sql, count);
                }

                // Update per-user query counter.
                user_registry.on_query(&session_username).await;

                // Update per-app / per-IP / per-user analytics.
                app_analytics
                    .on_query(
                        &session_username,
                        &client_ip,
                        &session_app,
                        was_read,
                        matches!(intent, QueryIntent::Write),
                    )
                    .await;

                // Record query duration in the appropriate histogram (lock-free).
                let elapsed = t0.elapsed();
                let secs = elapsed.as_secs_f64();
                if was_read {
                    metrics.read_hist.record(secs);
                } else if matches!(intent, QueryIntent::Write) {
                    metrics.write_hist.record(secs);
                }

                // Audit log (fire-and-forget, never blocks).
                if audit_logger.is_active() {
                    let destination = if was_read { "replica" } else { "primary" };
                    audit_logger.log(
                        &session_username,
                        &client_ip,
                        sql,
                        destination,
                        secs * 1000.0,
                        is_error,
                    );
                }

                // Record in the temporal heatmap (lock-free).
                heatmap.record((secs * 1000.0) as u64);

                // Record in the throughput counters (lock-free, for time-series).
                throughput.record((secs * 1_000_000.0) as u64);

                // Append to the active transaction trace if one is open.
                // We also capture queries that arrive while in_tx was already true
                // (i.e. queries *after* BEGIN but before COMMIT).
                if let Some(ref mut trace) = active_trace {
                    let intent_str: &'static str = match intent {
                        QueryIntent::Read => "read",
                        QueryIntent::Write => "write",
                        QueryIntent::Transaction => "transaction",
                        _ => "other",
                    };
                    let fp = fingerprint(sql);
                    let backend_addr = router.pool().await.primary_addr();
                    trace.record(sql, fp, secs * 1000.0, &backend_addr, intent_str);
                }

                // Record analytics — try_send never blocks the hot path.
                collector.try_record(sql, elapsed, was_read);

                if let Err(e) = session.flush().await {
                    log::debug!("Flush error: {}", e);
                    break;
                }
            }

            Command::Other(raw) => {
                // Log CDC/binary log commands for observability — always routed to primary.
                use crate::protocol::mysql::command as cmd;
                if matches!(
                    raw.first().copied(),
                    Some(cmd::COM_BINLOG_DUMP) | Some(cmd::COM_BINLOG_DUMP_GTID)
                ) {
                    log::warn!(
                        "[conn {}] CDC command received (COM_BINLOG_DUMP) user={} — routing to primary",
                        conn_id, session.username()
                    );
                }
                // Pass-through to primary (COM_INIT_DB, COM_BINLOG_DUMP, etc.).
                let result = router
                    .route_raw(&raw, &mut tx_conn, session.is_in_transaction())
                    .await;

                match result {
                    Ok(response) => {
                        if let Err(e) = session.write_response(&response.bytes).await {
                            log::debug!("Write response error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        log::warn!("Backend pass-through error: {}", e);
                        if let Err(we) = session.write_error("1", &e.to_string()).await {
                            log::debug!("Write error packet failed: {}", we);
                        }
                    }
                }

                if let Err(e) = session.flush().await {
                    log::debug!("Flush error: {}", e);
                    break;
                }
            }

            Command::Stmt(raw) => {
                use crate::protocol::mysql::command as cmd;

                // Analytics for COM_STMT_EXECUTE.
                if raw.first().copied() == Some(cmd::COM_STMT_EXECUTE) {
                    user_registry.on_query(&session_username).await;
                    app_analytics
                        .on_query(&session_username, &client_ip, &session_app, false, false)
                        .await;

                    if log_prepared_params && raw.len() > 5 {
                        let param_bytes = &raw[5..]; // skip cmd byte + stmt_id (4 bytes)
                        log::warn!(
                            "[conn {}] COM_STMT_EXECUTE params ({}B): {}",
                            conn_id,
                            param_bytes.len(),
                            param_bytes
                                .iter()
                                .map(|b| format!("{:02x}", b))
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                    }
                }

                // Route through the shadow-aware method which handles proxy-level
                // stmt_id remapping and transparent re-prepare on backend death.
                let result = router
                    .route_stmt_mysql_shadow(
                        &raw,
                        &mut mysql_shadow,
                        &mut stmt_conn,
                        &mut tx_conn,
                        session.is_in_transaction(),
                    )
                    .await;

                match result {
                    Ok(response) => {
                        if let Err(e) = session.write_response(&response.bytes).await {
                            log::debug!("Write stmt response error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        log::warn!("[conn {}] Stmt error: {}", conn_id, e);
                        if let Err(we) = session.write_error("1", &e.to_string()).await {
                            log::debug!("Write error packet failed: {}", we);
                        }
                    }
                }

                if let Err(e) = session.flush().await {
                    log::debug!("Flush error: {}", e);
                    break;
                }

                // Release stmt_conn back to the pool when all prepared statements
                // have been closed and we are not in a transaction.
                if mysql_shadow.is_empty() && !session.is_in_transaction() {
                    if let Some(conn) = stmt_conn.take() {
                        log::debug!(
                            "[conn {}] all stmts closed \u{2014} releasing stmt_conn to pool",
                            conn_id
                        );
                        router.put_primary(conn).await;
                    }
                }
            }
        }
    }

    // Emit N+1 summary before closing.
    query_tracker.summarise();

    // Auto-finalise any open transaction trace (client disconnected mid-tx).
    if let Some(trace) = active_trace.take() {
        tracer_store.push(trace.finish("disconnect"));
    }

    // Return sticky connections to the pool on client disconnect.
    if let Some(conn) = tx_conn {
        router.put_primary(conn).await;
    }
    if let Some(conn) = stmt_conn {
        router.put_primary(conn).await;
    }
    if let Some(conn) = sticky_hint_conn {
        router.put_replica(conn, sticky_hint_idx).await;
    }

    user_registry.on_disconnect(&session_username).await;
    app_analytics
        .on_disconnect(&session_username, &client_ip, &session_app)
        .await;
    Ok(())
}

// ─── PROXY Protocol v1 / v2 parser ───────────────────────────────────────────

/// 12-byte binary signature that begins every PROXY Protocol v2 header.
const PPV2_SIGNATURE: &[u8; 12] = b"\r\n\r\n\x00\r\nQUIT\n";

/// Detect and parse a PROXY Protocol header (v1 text or v2 binary).
///
/// Returns `Some("ip:port")` when a valid PROXY header is found, `None`
/// otherwise (the connection is then processed with the original socket addr).
///
/// Auto-detection:
/// - v2 binary: starts with the 12-byte signature `\r\n\r\n\x00\r\nQUIT\n`
/// - v1 text  : starts with the ASCII string `"PROXY "`
///
/// Consumes exactly the header bytes and leaves the stream positioned at the
/// first database-protocol handshake byte.
async fn parse_proxy_header(stream: &mut TcpStream) -> Option<String> {
    // Peek 12 bytes — enough to detect both v2 signature and v1 prefix.
    let mut peek_buf = [0u8; 12];
    let n = stream.peek(&mut peek_buf).await.ok()?;
    if n >= 12 && peek_buf.starts_with(PPV2_SIGNATURE) {
        parse_proxy_v2(stream).await
    } else if n >= 6 && peek_buf[..6] == *b"PROXY " {
        parse_proxy_v1(stream).await
    } else {
        None
    }
}

/// Parse a PROXY Protocol v2 binary header.
///
/// Fixed-size 16-byte header layout:
///   [0..12]  signature (already verified by caller)
///   [12]     version (high nibble) | command (low nibble)
///   [13]     address family (high nibble) | transport (low nibble)
///   [14..16] address block length (big-endian u16)
///
/// Followed by a variable-length address block consumed regardless of command.
async fn parse_proxy_v2(stream: &mut TcpStream) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut hdr = [0u8; 16];
    stream.read_exact(&mut hdr).await.ok()?;

    let ver_cmd = hdr[12];
    let family = hdr[13];
    let addr_len = u16::from_be_bytes([hdr[14], hdr[15]]) as usize;

    // Always consume the address block to keep the stream positioned correctly.
    let mut addr_block = vec![0u8; addr_len];
    if addr_len > 0 {
        stream.read_exact(&mut addr_block).await.ok()?;
    }

    // Version nibble must be 2.
    if (ver_cmd >> 4) != 0x2 {
        return None;
    }

    match ver_cmd & 0x0F {
        // LOCAL (0x0): health-check — no client address to expose.
        0x00 => None,
        // PROXY (0x1): extract real source address.
        0x01 => match family {
            // AF_INET + STREAM (0x11): 4 src + 4 dst + 2 sport + 2 dport = 12 bytes.
            0x11 if addr_block.len() >= 12 => {
                let src_ip = std::net::Ipv4Addr::new(
                    addr_block[0],
                    addr_block[1],
                    addr_block[2],
                    addr_block[3],
                );
                let src_port = u16::from_be_bytes([addr_block[8], addr_block[9]]);
                Some(format!("{}:{}", src_ip, src_port))
            }
            // AF_INET6 + STREAM (0x21): 16 src + 16 dst + 2 sport + 2 dport = 36 bytes.
            0x21 if addr_block.len() >= 36 => {
                let octets: [u8; 16] = addr_block[..16].try_into().ok()?;
                let src_ip = std::net::Ipv6Addr::from(octets);
                let src_port = u16::from_be_bytes([addr_block[32], addr_block[33]]);
                Some(format!("[{}]:{}", src_ip, src_port))
            }
            // UNSPEC or other families: no usable address.
            _ => None,
        },
        _ => None,
    }
}

/// Parse a PROXY Protocol v1 text header.
///
/// PROXY v1 grammar: `PROXY (TCP4|TCP6|UNKNOWN) src dst sport dport\r\n`
/// Maximum header length is 108 bytes per the HAProxy PROXY Protocol spec.
async fn parse_proxy_v1(stream: &mut TcpStream) -> Option<String> {
    use tokio::io::AsyncReadExt;
    // Read byte-by-byte until \r\n (max 108 bytes).
    let mut line: Vec<u8> = Vec::with_capacity(108);
    loop {
        let mut b = [0u8; 1];
        stream.read_exact(&mut b).await.ok()?;
        line.push(b[0]);
        if line.len() >= 2 && line[line.len() - 2] == b'\r' && line[line.len() - 1] == b'\n' {
            break;
        }
        if line.len() > 108 {
            return None; // malformed — too long
        }
    }
    let s = std::str::from_utf8(&line).ok()?;
    let parts: Vec<&str> = s.trim().split_ascii_whitespace().collect();
    // "PROXY LOCAL\r\n" — HAProxy health check; no real IP to extract.
    if parts.get(1).copied() == Some("LOCAL") {
        return None;
    }
    // "PROXY TCP4 <src_ip> <dst_ip> <src_port> <dst_port>"
    if parts.len() >= 6 {
        let src_ip = parts[2];
        let src_port = parts[4];
        return Some(format!("{}:{}", src_ip, src_port));
    }
    None
}

///
/// Patterns detected (case-insensitive):
///   SET @name = ...
///   SET @name := ...
///   SELECT @name := ...   (assignment via SELECT)
///   SET NAMES utf8mb4
///   SET CHARACTER SET utf8mb4
///   SET CHARSET utf8mb4
///   SET @@session.sql_mode = ...
///   SET @@global.x = ...
///
/// When any of these are detected we enable a sticky backend connection for the
/// session: the assigned variables / charset are connection-scoped on MySQL and
/// would be lost if the connection were handed back to the pool and re-used by
/// another session.
/// Returns `true` for statements that create session state that **cannot** be
/// replayed on a fresh backend connection:
/// - User-defined variable assignments (`SET @var = …`, `SELECT @var := …`)
/// - `LOCK TABLES` (connection-scoped lock)
///
/// These sessions must use a sticky backend connection (multiplexing disabled).
fn needs_hard_pin(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();

    // SET @user_var = ... (user variables — value type unknown at replay time)
    if upper.starts_with("SET") && upper.contains('@') && !upper.contains("@@") {
        return true;
    }
    // SELECT @var := ... (assignment-in-SELECT idiom)
    if !upper.starts_with("SET") && upper.contains(":=") && upper.contains('@') {
        return true;
    }
    // LOCK TABLES — connection-scoped; UNLOCK TABLES must happen on same conn.
    if upper.starts_with("LOCK TABLES") {
        return true;
    }

    false
}

/// Returns `true` for session-state-changing statements that **can** be safely
/// replayed on any fresh backend connection:
/// - `SET NAMES charset`
/// - `SET CHARACTER SET charset`
/// - `SET [SESSION|LOCAL] system_var = literal`
/// - `SET @@session.x = …` / `SET @@global.x = …`
///
/// These are stored in `session_init_sqls` and replayed by the router on
/// every new connection checkout — multiplexing is preserved.
fn is_replayable_session_stmt(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();
    if !upper.starts_with("SET") {
        return false;
    }
    // @user_var — not replayable (handled by needs_hard_pin)
    if upper.contains('@') && !upper.contains("@@") {
        return false;
    }
    // SELECT @var := — not replayable (and doesn't start with SET anyway)
    // Everything else starting with SET is a system/session variable — replayable.
    true
}

// ─── Panic recovery tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod panic_recovery_tests {
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// Verify parking_lot::Mutex is not poisoned when a thread panics while
    /// holding the lock. Subsequent acquires must succeed without unwrap hacks.
    #[test]
    fn parking_lot_mutex_not_poisoned_after_thread_panic() {
        let m: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let m2 = m.clone();

        let handle = std::thread::spawn(move || {
            let _guard = m2.lock();
            panic!("intentional panic while holding lock");
        });

        // The thread panicked — join will return Err but that is expected.
        let _ = handle.join();

        // Crucially: the next acquire must NOT panic / block.
        // With std::sync::Mutex this would return PoisonError and require .unwrap().
        // With parking_lot::Mutex the lock is released cleanly on drop (no poison).
        let mut val = m.lock();
        *val = 42;
        assert_eq!(*val, 42, "parking_lot mutex usable after thread panic");
    }

    /// Verify parking_lot::RwLock is not poisoned after a writer thread panics.
    #[test]
    fn parking_lot_rwlock_not_poisoned_after_writer_panic() {
        use parking_lot::RwLock;

        let rw: Arc<RwLock<String>> = Arc::new(RwLock::new("initial".to_string()));
        let rw2 = rw.clone();

        let handle = std::thread::spawn(move || {
            let _guard = rw2.write();
            panic!("intentional panic while holding write lock");
        });

        let _ = handle.join();

        // Must succeed without any poison check.
        let val = rw.read();
        assert_eq!(*val, "initial");
        drop(val);

        *rw.write() = "updated".to_string();
        assert_eq!(*rw.read(), "updated");
    }
}
