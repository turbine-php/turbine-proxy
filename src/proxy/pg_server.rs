//! PostgreSQL proxy server — Phase 2 (fully featured).
//!
//! Feature-complete alongside the MySQL `ProxyServer`:
//! - Read/write splitting, connection pooling, RYOW
//! - Whitelist enforcement, SQL audit log
//! - Error event capture with `protocol = "postgres"`
//! - COPY operations counter
//! - Mid-transaction backend-death recovery (SQLSTATE 25P02)
//! - Slow-query tracking via shared `Collector`
//! - Meta-command interception (`\d`, `\dt`, `\l`, `\di`, `\dv`)

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::analytics::{Collector, ThroughputCounters};
use crate::config::PgsqlConfig;
use crate::protocol::postgres::extract_ready_status;
use crate::protocol::{BackendConnection, ClientAuthConfig, Command, DatabaseProtocol};
use crate::proxy::classifier::{classify, QueryIntent};
use crate::proxy::error_events::{ErrorEvent, ErrorEventStore};
use crate::proxy::fingerprint::{fingerprint, fingerprint_with_hash};
use crate::proxy::heatmap::HeatmapStore;
use crate::proxy::n1::N1Store;
use crate::proxy::pool::BackendPool;
use crate::proxy::regression::RegressionStore;
use crate::proxy::rewriter::Rewriter;
use crate::proxy::router::Router;
use crate::proxy::rules::RuleEngine;
use crate::proxy::security::{AuditLogger, InjectionDetector, QueryWhitelist};
use crate::proxy::server::ProxyMetrics;
use crate::proxy::stmt_shadow::{scan_pg_pipeline, PgStmtShadow};
use crate::proxy::tracer::{ActiveTrace, TracerStore};

// ─── N+1 / repeated-query tracker (PG) ─────────────────────────────────────

struct SessionQueryTracker {
    counts: HashMap<u64, (u32, String)>,
    exact_counts: HashMap<u64, (u32, String)>,
    conn_id: u32,
    n1_store: Arc<N1Store>,
}

impl SessionQueryTracker {
    const WARN_THRESHOLD: u32 = 5;
    const HOT_KEY_THRESHOLD: u32 = 30;

    fn new(conn_id: u32, n1_store: Arc<N1Store>) -> Self {
        Self {
            counts: HashMap::new(),
            exact_counts: HashMap::new(),
            conn_id,
            n1_store,
        }
    }

    fn record(&mut self, sql: &str) {
        let (fp, hash) = fingerprint_with_hash(sql);
        let entry = self.counts.entry(hash).or_insert_with(|| (0, fp));
        entry.0 += 1;
        if entry.0 == Self::WARN_THRESHOLD {
            log::warn!(
                "[pg conn {}] N+1 detected: query executed {} times — {}",
                self.conn_id,
                entry.0,
                entry.1
            );
        }
    }

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
            log::info!(
                "[pg conn {}] repeated query x{}: {}",
                self.conn_id,
                count,
                fp
            );
        }
    }
}

// ─── PgProxyServer ────────────────────────────────────────────────────────────

pub struct PgProxyServer {
    config: PgsqlConfig,
    pool: Arc<BackendPool>,
    router: Router,
    protocol: Arc<dyn DatabaseProtocol>,
    metrics: Arc<ProxyMetrics>,
    collector: Arc<Collector>,
    n1_store: Arc<N1Store>,
    tracer_store: Arc<TracerStore>,
    regression_store: Arc<RegressionStore>,
    semaphore: Arc<Semaphore>,
    conn_id: AtomicU32,
    users_config: Arc<Vec<crate::config::UserConfig>>,
    heatmap: Arc<HeatmapStore>,
    throughput: Arc<ThroughputCounters>,
    // ── Security ──────────────────────────────────────────────────────────────
    query_whitelist: Arc<QueryWhitelist>,
    sqli_detector: Option<Arc<InjectionDetector>>,
    audit_logger: Arc<AuditLogger>,
    error_events: Arc<ErrorEventStore>,
    // ── Observability ─────────────────────────────────────────────────────────
    /// Number of active COPY data-stream operations.
    pub copy_active: Arc<AtomicUsize>,
}

impl PgProxyServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: PgsqlConfig,
        metrics: Arc<ProxyMetrics>,
        collector: Arc<Collector>,
        n1_store: Arc<N1Store>,
        tracer_store: Arc<TracerStore>,
        regression_store: Arc<RegressionStore>,
        rules: Arc<RuleEngine>,
        rewriter: Arc<Rewriter>,
        error_events: Arc<ErrorEventStore>,
        heatmap: Arc<HeatmapStore>,
        throughput: Arc<ThroughputCounters>,
    ) -> Option<Self> {
        let primary_cfg = config.primary.as_ref()?;

        let users = config.users.clone();
        let protocol: Arc<dyn DatabaseProtocol> =
            if !config.ssl_cert.is_empty() && !config.ssl_key.is_empty() {
                // Build TLS-aware protocol handler
                let tls_cfg = crate::config::FrontendTlsConfig {
                    enabled: true,
                    cert: config.ssl_cert.clone(),
                    key: config.ssl_key.clone(),
                };
                match crate::protocol::mysql::tls::build_frontend_acceptor(&tls_cfg) {
                    Ok(acceptor) => {
                        log::info!("[pg] Frontend TLS enabled using cert={}", config.ssl_cert);
                        Arc::new(crate::protocol::PostgreSQLProtocol::new_with_tls(
                            users.clone(),
                            acceptor,
                        ))
                    }
                    Err(e) => {
                        log::warn!(
                            "[pg] Failed to build TLS acceptor ({}), falling back to plain",
                            e
                        );
                        Arc::new(crate::protocol::PostgreSQLProtocol::new(users.clone()))
                    }
                }
            } else {
                Arc::new(crate::protocol::PostgreSQLProtocol::new(users.clone()))
            };

        let idle = if config.connection_max_idle_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(config.connection_max_idle_secs))
        };

        let pool = Arc::new(BackendPool::with_idle_timeout(
            primary_cfg,
            &config.replicas,
            config.pool_size,
            protocol.clone(),
            idle,
        ));

        let router = Router::new(pool.clone(), rules, rewriter, protocol.clone());
        let max_conn = if config.max_connections == 0 {
            10_000
        } else {
            config.max_connections
        };

        let query_whitelist = Arc::new(QueryWhitelist::new(&config.query_whitelist));
        let audit_logger = Arc::new(AuditLogger::new(&config.audit_log));
        let sqli_detector = if config.sql_injection_protection {
            Some(Arc::new(InjectionDetector::new()))
        } else {
            None
        };

        Some(Self {
            users_config: Arc::new(users),
            config,
            pool,
            router,
            protocol,
            metrics,
            collector,
            n1_store,
            tracer_store,
            regression_store,
            semaphore: Arc::new(Semaphore::new(max_conn)),
            conn_id: AtomicU32::new(1),
            query_whitelist,
            sqli_detector,
            audit_logger,
            error_events,
            copy_active: Arc::new(AtomicUsize::new(0)),
            heatmap,
            throughput,
        })
    }

    pub fn pool(&self) -> Arc<BackendPool> {
        self.pool.clone()
    }

    pub fn router(&self) -> Router {
        self.router.clone()
    }

    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config.listen_addr)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to bind PostgreSQL listener to {} — {}",
                    self.config.listen_addr,
                    e
                )
            })?;

        log::info!("PostgreSQL proxy listening on {}", self.config.listen_addr);

        loop {
            let (socket, addr) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("[pg] accept error: {}", e);
                    continue;
                }
            };

            let permit = match self.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    log::warn!("[pg] max connections reached — rejecting {}", addr);
                    continue;
                }
            };

            let id = self.conn_id.fetch_add(1, Ordering::Relaxed);
            let srv = self.clone();
            let metrics = self.metrics.clone();

            metrics.connections_total.fetch_add(1, Ordering::Relaxed);
            metrics.connections_active.fetch_add(1, Ordering::Relaxed);

            tokio::spawn(async move {
                if let Err(e) = handle_pg_connection(socket, id, addr.to_string(), srv).await {
                    log::debug!("[pg conn {}] error: {}", id, e);
                }
                metrics.connections_active.fetch_sub(1, Ordering::Relaxed);
                drop(permit);
            });
        }
    }
}

// ─── Connection handler ───────────────────────────────────────────────────────

async fn handle_pg_connection(
    stream: TcpStream,
    conn_id: u32,
    client_ip: String,
    srv: Arc<PgProxyServer>,
) -> anyhow::Result<()> {
    let auth_cfg = ClientAuthConfig {
        connection_id: conn_id,
        server_version: "16.0",
    };

    let mut session = srv
        .protocol
        .accept_client(stream, &auth_cfg)
        .await
        .map_err(|e| anyhow::anyhow!("PG handshake: {}", e))?;

    let username = session.username().to_string();
    let client_database = {
        let db = session.database();
        if db.is_empty() {
            "postgres".to_string()
        } else {
            db.to_string()
        }
    };
    log::debug!(
        "[pg conn {}] accepted user='{}' from {}",
        conn_id,
        username,
        client_ip
    );

    // Per-user init: search_path, isolation level
    let user_cfg = srv
        .users_config
        .iter()
        .find(|u| u.name == username)
        .cloned();
    let mut session_init_sqls: Vec<String> = Vec::new();
    if let Some(ref uc) = user_cfg {
        if !uc.default_schema.is_empty() {
            session_init_sqls.push(format!("SET search_path TO {}", uc.default_schema));
        }
        if !uc.transaction_isolation.is_empty() {
            session_init_sqls.push(format!(
                "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {}",
                uc.transaction_isolation
            ));
        }
    }

    let mut tx_conn: Option<Box<dyn BackendConnection>> = None;
    let mut stmt_conn: Option<Box<dyn BackendConnection>> = None;
    let mut pg_shadow = PgStmtShadow::new();
    let mut query_tracker = SessionQueryTracker::new(conn_id, srv.n1_store.clone());
    let mut active_trace: Option<ActiveTrace> = None;

    // RYOW window
    let ryow_ms = srv.config.read_your_own_writes_ms;
    let mut ryow_until: Option<Instant> = None;

    // Apply session init sqls
    if !session_init_sqls.is_empty() {
        if let Ok(mut conn) = srv
            .pool
            .get_primary_for_database(Some(&client_database))
            .await
        {
            for sql in &session_init_sqls {
                let _ = conn.execute_query(sql.as_bytes()).await;
            }
            srv.router
                .put_primary_for_database(conn, &client_database)
                .await;
        }
    }

    loop {
        let cmd = match session.read_command().await {
            Ok(c) => c,
            Err(e) => {
                if !e.to_string().contains("EOF") && !e.to_string().contains("reset") {
                    log::debug!("[pg conn {}] read error: {}", conn_id, e);
                }
                break;
            }
        };

        match cmd {
            Command::Quit => break,

            Command::Query(sql) => {
                let sql_str = std::str::from_utf8(&sql).unwrap_or("").trim();
                srv.metrics.queries_total.fetch_add(1, Ordering::Relaxed);

                // ── Meta-command interception ───────────────────────────────
                if sql_str.starts_with('\\') {
                    match translate_meta_command(sql_str) {
                        Some(translated_sql) => {
                            // Route the translated SQL through the backend as a regular read query
                            let result = srv
                                .router
                                .route_query_with_database(
                                    &translated_sql,
                                    &mut tx_conn,
                                    session.is_in_transaction(),
                                    true, // meta-commands are read-only
                                    &username,
                                    "",
                                    &client_database,
                                )
                                .await;
                            match result {
                                Ok(r) => {
                                    let _ = session.write_response(&r.bytes).await;
                                }
                                Err(e) => {
                                    let _ = session.write_error("XX000", &e.to_string()).await;
                                }
                            }
                            let _ = session.flush().await;
                            continue;
                        }
                        None => { /* unknown meta-command — fall through to backend */ }
                    }
                }

                let intent = classify(sql_str);
                let is_read = matches!(intent, QueryIntent::Read);
                let is_write = matches!(intent, QueryIntent::Write);

                if is_read {
                    srv.metrics.queries_read.fetch_add(1, Ordering::Relaxed);
                } else if is_write {
                    srv.metrics.queries_write.fetch_add(1, Ordering::Relaxed);
                }

                // ── Write permission check ──────────────────────────────────
                if !session.allow_writes() && matches!(intent, QueryIntent::Write) {
                    let _ = session
                        .write_error("42501", "User is not allowed to write (read-only account)")
                        .await;
                    let _ = session.flush().await;
                    continue;
                }

                // ── SQL injection detection ───────────────────────────────
                if let Some(ref detector) = srv.sqli_detector {
                    if let Some(pattern) = detector.check(sql_str) {
                        srv.metrics.sqli_blocked.fetch_add(1, Ordering::Relaxed);
                        log::warn!(
                            "[pg conn {}] SQL injection blocked (user={} ip={}): matched pattern {}",
                            conn_id, username, client_ip, pattern
                        );
                        srv.audit_logger.log(
                            &username,
                            &client_ip,
                            sql_str,
                            "blocked:sqli",
                            0.0,
                            true,
                        );
                        srv.error_events.push(ErrorEvent::new_pg(
                            format!("SQL injection blocked: {}", pattern),
                            fingerprint(sql_str),
                            &client_ip,
                            &username,
                            0.0,
                        ));
                        let _ = session
                            .write_error("42501", "Query blocked: potential SQL injection detected")
                            .await;
                        let _ = session.flush().await;
                        continue;
                    }
                }

                // ── Whitelist enforcement ───────────────────────────────────
                if !srv.query_whitelist.is_allowed(sql_str) {
                    srv.metrics
                        .whitelist_blocked
                        .fetch_add(1, Ordering::Relaxed);
                    log::warn!(
                        "[pg conn {}] whitelist blocked user='{}': {}",
                        conn_id,
                        username,
                        &sql_str[..sql_str.len().min(120)]
                    );
                    srv.audit_logger.log(
                        &username,
                        &client_ip,
                        sql_str,
                        "blocked:whitelist",
                        0.0,
                        true,
                    );
                    srv.error_events.push(ErrorEvent::new_pg(
                        "Query not permitted: not in the PostgreSQL query allowlist",
                        fingerprint(sql_str),
                        &client_ip,
                        &username,
                        0.0,
                    ));
                    let _ = session
                        .write_error("42501", "Query not permitted: not in the query allowlist")
                        .await;
                    let _ = session.flush().await;
                    continue;
                }

                // ── Transaction boundary tracking ───────────────────────────
                let upper = sql_str.trim().to_uppercase();
                let starts_tx = upper.starts_with("BEGIN")
                    || upper.starts_with("START TRANSACTION")
                    || upper.starts_with("START WORK");
                let ends_tx = upper.starts_with("COMMIT")
                    || upper.starts_with("ROLLBACK")
                    || upper == "END"
                    || upper.starts_with("END ");

                if starts_tx {
                    session.set_in_transaction(true);
                }
                if starts_tx {
                    active_trace = Some(ActiveTrace::new(conn_id, &username, &client_ip));
                }

                // ── RYOW ───────────────────────────────────────────────────
                let ryow_active = ryow_ms > 0 && ryow_until.is_some_and(|t| t > Instant::now());
                let use_replica = matches!(intent, QueryIntent::Read) && !ryow_active;

                let t0 = Instant::now();
                let result = srv
                    .router
                    .route_query_with_database(
                        &sql,
                        &mut tx_conn,
                        session.is_in_transaction(),
                        use_replica,
                        &username,
                        "",
                        &client_database,
                    )
                    .await;
                let elapsed = t0.elapsed();

                // Post-routing tx state
                if ends_tx {
                    session.set_in_transaction(false);
                    if let Some(conn) = tx_conn.take() {
                        srv.router
                            .put_primary_for_database(conn, &client_database)
                            .await;
                    }
                } else if let Some(status) = result
                    .as_ref()
                    .ok()
                    .and_then(|r| extract_ready_status(&r.bytes))
                {
                    let in_tx = status == b'T' || status == b'E';
                    session.set_in_transaction(in_tx);
                    if !in_tx {
                        if let Some(conn) = tx_conn.take() {
                            srv.router
                                .put_primary_for_database(conn, &client_database)
                                .await;
                        }
                    }
                }

                match result {
                    Ok(response) => {
                        if response.is_error {
                            log::debug!(
                                "[pg conn {}] backend error for '{}'",
                                conn_id,
                                &sql_str[..sql_str.len().min(100)]
                            );
                        } else {
                            let fp = fingerprint(sql_str);
                            srv.collector.try_record(
                                sql_str,
                                elapsed,
                                matches!(intent, QueryIntent::Read),
                            );
                            srv.audit_logger.log(
                                &username,
                                &client_ip,
                                sql_str,
                                "primary",
                                elapsed.as_secs_f64() * 1000.0,
                                false,
                            );
                            // Heatmap + timeseries (lock-free, never blocks the hot path).
                            let secs = elapsed.as_secs_f64();
                            srv.heatmap.record((secs * 1000.0) as u64);
                            srv.throughput.record((secs * 1_000_000.0) as u64);
                            // Slow query log
                            let slow_ms = srv.config.slow_query_log_ms;
                            if slow_ms > 0 && elapsed.as_millis() as u64 >= slow_ms {
                                log::warn!(
                                    "[pg slow {}ms] {}",
                                    elapsed.as_millis(),
                                    &fp[..fp.len().min(200)]
                                );
                            }
                            // RYOW window
                            if ryow_ms > 0 && matches!(intent, QueryIntent::Write) {
                                ryow_until = Some(Instant::now() + Duration::from_millis(ryow_ms));
                            }

                            query_tracker.record(sql_str);
                            if let Some((fp, example_sql, count)) =
                                query_tracker.track_exact(sql_str)
                            {
                                srv.regression_store
                                    .report_hot_key(&fp, &example_sql, count);
                            }

                            if let Some(ref mut trace) = active_trace {
                                let intent_str: &'static str = match intent {
                                    QueryIntent::Read => "read",
                                    QueryIntent::Write => "write",
                                    QueryIntent::Transaction => "transaction",
                                    _ => "other",
                                };
                                let backend_addr = srv.router.pool().await.primary_addr();
                                trace.record(
                                    sql_str,
                                    fp,
                                    elapsed.as_secs_f64() * 1000.0,
                                    &backend_addr,
                                    intent_str,
                                );
                            }
                        }
                        if let Err(e) = session.write_response(&response.bytes).await {
                            log::debug!("[pg conn {}] write response: {}", conn_id, e);
                            break;
                        }
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        log::warn!("[pg conn {}] query error: {}", conn_id, err_str);

                        srv.error_events.push(ErrorEvent::new_pg(
                            err_str.clone(),
                            fingerprint(sql_str),
                            &client_ip,
                            &username,
                            elapsed.as_secs_f64() * 1000.0,
                        ));

                        // Mid-transaction backend-death recovery
                        let is_gone = err_str.contains("broken pipe")
                            || err_str.to_lowercase().contains("connection reset")
                            || err_str.to_lowercase().contains("gone away")
                            || err_str.contains("08006")
                            || err_str.contains("08001");
                        let sqlstate = if is_gone && session.is_in_transaction() {
                            log::warn!(
                                "[pg conn {}] backend died mid-tx — clearing tx state, client can retry with ROLLBACK",
                                conn_id
                            );
                            session.set_in_transaction(false);
                            if let Some(conn) = tx_conn.take() {
                                drop(conn);
                            }
                            "25P02" // in_failed_sql_transaction
                        } else {
                            "XX000"
                        };

                        if let Err(we) = session.write_error(sqlstate, &err_str).await {
                            log::debug!("[pg conn {}] write_error: {}", conn_id, we);
                        }
                    }
                }

                if let Err(e) = session.flush().await {
                    log::debug!("[pg conn {}] flush: {}", conn_id, e);
                    break;
                }

                if ends_tx {
                    if let Some(trace) = active_trace.take() {
                        let outcome = if upper.starts_with("COMMIT") {
                            "commit"
                        } else {
                            "rollback"
                        };
                        srv.tracer_store.push(trace.finish(outcome));
                    }
                }
            }

            Command::Stmt(raw) => {
                srv.metrics.queries_total.fetch_add(1, Ordering::Relaxed);

                // Update shadow map BEFORE routing so re-prepare logic has
                // the full picture (including stmts being prepared by this pipeline).
                let scan = scan_pg_pipeline(&raw);
                pg_shadow.apply_scan(&scan);

                let t0 = Instant::now();
                let result = srv
                    .router
                    .route_stmt_pg_shadow_with_database(
                        &raw,
                        &scan,
                        &pg_shadow,
                        &mut stmt_conn,
                        &mut tx_conn,
                        session.is_in_transaction(),
                        &client_database,
                    )
                    .await;
                let elapsed = t0.elapsed();

                if let Ok(ref response) = result {
                    if let Some(status) = extract_ready_status(&response.bytes) {
                        let in_tx = status == b'T' || status == b'E';
                        session.set_in_transaction(in_tx);
                        if !in_tx {
                            if let Some(conn) = tx_conn.take() {
                                srv.router
                                    .put_primary_for_database(conn, &client_database)
                                    .await;
                            }
                        }
                    }
                }

                match result {
                    Ok(response) => {
                        let mut recorded_any = false;
                        for p in &scan.parses {
                            let q = p.query.trim();
                            if q.is_empty() {
                                continue;
                            }
                            let q_intent = classify(q);
                            srv.collector.try_record(
                                q,
                                elapsed,
                                matches!(q_intent, QueryIntent::Read),
                            );
                            query_tracker.record(q);
                            if let Some((fp, example_sql, count)) = query_tracker.track_exact(q) {
                                srv.regression_store
                                    .report_hot_key(&fp, &example_sql, count);
                            }
                            recorded_any = true;
                        }
                        if !recorded_any {
                            srv.collector.try_record("extended_query", elapsed, false);
                        }
                        srv.heatmap.record((elapsed.as_secs_f64() * 1000.0) as u64);
                        srv.throughput
                            .record((elapsed.as_secs_f64() * 1_000_000.0) as u64);
                        if let Err(e) = session.write_response(&response.bytes).await {
                            log::debug!("[pg conn {}] write stmt: {}", conn_id, e);
                            break;
                        }
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        log::warn!("[pg conn {}] stmt error: {}", conn_id, err_str);
                        srv.error_events.push(ErrorEvent::new_pg(
                            err_str.clone(),
                            "extended_query",
                            &client_ip,
                            &username,
                            elapsed.as_secs_f64() * 1000.0,
                        ));
                        if let Err(we) = session.write_error("XX000", &err_str).await {
                            log::debug!("[pg conn {}] write_error: {}", conn_id, we);
                        }
                    }
                }

                if let Err(e) = session.flush().await {
                    log::debug!("[pg conn {}] flush: {}", conn_id, e);
                    break;
                }

                // Release stmt_conn back to the pool when all named prepared
                // statements have been closed and we are not in a transaction.
                if pg_shadow.is_empty() && !session.is_in_transaction() {
                    if let Some(conn) = stmt_conn.take() {
                        log::debug!(
                            "[pg conn {}] all stmts closed — releasing stmt_conn to pool",
                            conn_id
                        );
                        srv.router
                            .put_primary_for_database(conn, &client_database)
                            .await;
                    }
                }
            }

            Command::Other(raw) => {
                // COPY data stream or unknown commands
                srv.copy_active.fetch_add(1, Ordering::Relaxed);

                let mut conn = match srv
                    .pool
                    .get_primary_for_database(Some(&client_database))
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        srv.copy_active.fetch_sub(1, Ordering::Relaxed);
                        log::warn!("[pg conn {}] get primary: {}", conn_id, e);
                        let _ = session
                            .write_error("08006", "could not connect to primary")
                            .await;
                        let _ = session.flush().await;
                        break;
                    }
                };
                let send_result = conn.send_raw(&raw).await;
                srv.copy_active.fetch_sub(1, Ordering::Relaxed);

                match send_result {
                    Ok(response) => {
                        if let Some(status) = extract_ready_status(&response.bytes) {
                            session.set_in_transaction(status == b'T' || status == b'E');
                        }
                        srv.router
                            .put_primary_for_database(conn, &client_database)
                            .await;
                        if let Err(e) = session.write_response(&response.bytes).await {
                            log::debug!("[pg conn {}] write other: {}", conn_id, e);
                            break;
                        }
                    }
                    Err(e) => {
                        log::warn!("[pg conn {}] other cmd error: {}", conn_id, e);
                        let _ = session.write_error("XX000", &e.to_string()).await;
                    }
                }
                if let Err(e) = session.flush().await {
                    log::debug!("[pg conn {}] flush: {}", conn_id, e);
                    break;
                }
            }

            _ => {}
        }
    }

    if let Some(conn) = tx_conn {
        srv.router
            .put_primary_for_database(conn, &client_database)
            .await;
    }
    if let Some(conn) = stmt_conn {
        srv.router
            .put_primary_for_database(conn, &client_database)
            .await;
    }

    query_tracker.summarise();
    if let Some(trace) = active_trace.take() {
        srv.tracer_store.push(trace.finish("disconnect"));
    }

    log::debug!("[pg conn {}] closed", conn_id);
    Ok(())
}

// ─── Meta-command translation ─────────────────────────────────────────────────
//
// Intercept psql meta-commands before they reach the backend.  We synthesise a
// minimal ReadyForQuery response that includes a CommandComplete so psql is
// satisfied.  Unknown commands fall through as raw SQL.

/// Build a minimal PG "empty resultset" response to psql meta-commands.
/// Returns `None` for unknown backslash commands (let the backend handle them).
fn translate_meta_command(cmd: &str) -> Option<Vec<u8>> {
    let parts: Vec<&str> = cmd.splitn(3, ' ').collect();
    let sql: &str = match parts.first().copied().unwrap_or("") {
        r"\dt" | r"\dv" => {
            "SELECT schemaname, tablename AS name, 'table' AS type \
             FROM pg_tables WHERE schemaname NOT IN ('pg_catalog','information_schema') \
             ORDER BY schemaname, tablename"
        }
        r"\l" | r"\list" => {
            "SELECT datname AS database, pg_catalog.pg_get_userbyid(datdba) AS owner, \
             pg_encoding_to_char(encoding) AS encoding \
             FROM pg_database ORDER BY datname"
        }
        r"\di" => {
            "SELECT indexname, tablename, indexdef \
             FROM pg_indexes WHERE schemaname NOT IN ('pg_catalog','information_schema') \
             ORDER BY tablename, indexname"
        }
        r"\dn" => {
            "SELECT nspname AS schema, pg_catalog.pg_get_userbyid(nspowner) AS owner \
             FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname != 'information_schema' \
             ORDER BY nspname"
        }
        r"\du" | r"\dg" => {
            "SELECT rolname AS role, rolsuper, rolcreatedb, rolcreaterole, rolcanlogin \
             FROM pg_roles ORDER BY rolname"
        }
        r"\d" => {
            // \d [table] — if a table name is provided, show columns; else list tables
            if parts.len() > 1 && !parts[1].is_empty() {
                // return None to let backend handle \d table (it needs the table name as param)
                return None;
            }
            "SELECT table_schema, table_name, table_type \
             FROM information_schema.tables \
             WHERE table_schema NOT IN ('pg_catalog','information_schema') \
             ORDER BY table_schema, table_name"
        }
        _ => return None,
    };
    // Reuse the SQL — return the translated SQL bytes wrapped in a Query message.
    // The caller will re-route it through the backend.
    // We encode it as a raw bytes buffer so the caller can pass it to route_query.
    Some(sql.as_bytes().to_vec())
}
