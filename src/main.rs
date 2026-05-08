mod analytics;
mod config;
mod dashboard;
mod protocol;
mod proxy;

use std::sync::Arc;
use std::time::Duration;

use analytics::{AnalyticsStorage, Collector, TimeseriesStore};
use crate::proxy::regression::RegressionStore;
use config::ProxyConfig;
use dashboard::AppState;
use protocol::{mysql::tls::build_frontend_acceptor, MySQLProtocol};
use proxy::{AuthCache, GrChecker, HealthChecker, PgHealthChecker, PgProxyServer, ProxyMetrics, ProxyServer, Rewriter, RuleEngine};
use std::path::Path;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "turbineproxy.toml".to_string());

    let config = if Path::new(&config_path).exists() {
        ProxyConfig::from_file(Path::new(&config_path))?
    } else {
        log::warn!(
            "Config file '{}' not found, using defaults. Create a turbineproxy.toml to configure.",
            config_path
        );
        // Minimal default config for development
        ProxyConfig::from_str(
            r#"
            listen_addr = "0.0.0.0:3307"
            max_connections = 1000
            pool_size = 20

            [primary]
            addr = "127.0.0.1:3306"
            user = "root"
            password = ""

            [[replicas]]
            addr = "127.0.0.1:3306"
            user = "root"
            password = ""

            [analytics]
            enabled = true
            db_path = "turbineproxy_analytics.db"
            slow_query_ms = 100

            [dashboard]
            enabled = true
            listen_addr = "0.0.0.0:8080"
            "#,
        )?
    };
    let mysql_enabled = config.mysql_enabled;

    log::info!("TurbineProxy v{}", env!("CARGO_PKG_VERSION"));
    if mysql_enabled {
        log::info!("  Listen:  {}", config.listen_addr);
        log::info!("  Primary: {}", config.primary.addr);
        log::info!(
            "  Replicas: {}",
            if config.replicas.is_empty() {
                "none (reads go to primary)".to_string()
            } else {
                config
                    .replicas
                    .iter()
                    .map(|r| r.addr.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    } else {
        log::info!("  MySQL listener: disabled (using protocol-specific config only)");
    }

    let analytics_config = config.analytics.clone();
    let dashboard_config = config.dashboard.clone();
    let ha_config = config.ha.clone();
    let resolved_pgsql_config = config.resolved_pgsql();
    let gr_config = config.group_replication.clone();
    let frontend_tls_config = config.frontend_tls.clone();
    let primary_config = config.primary.clone();
    let replica_configs = config.replicas.clone();
    let users_config = config.users.clone();
    let auth_cache_ttl = config.auth_cache_ttl_secs;

    // Build rule engine — compiles all regexes at startup; fails fast on bad patterns.
    let rule_engine = Arc::new(
        RuleEngine::new(&config.query_rules, &config_path)
            .map_err(|e| anyhow::anyhow!("Query rule error: {}", e))?,
    );
    if config.query_rules.is_empty() {
        log::info!("Query rules: none (built-in heuristic active)");
    } else {
        log::info!("Query rules: {} configured", config.query_rules.len());
    }

    // Build rewrite engine — compiles all rewrite regexes at startup.
    let rewrite_rules_cfg = config.rewrite_rules.clone();
    let rewriter = Rewriter::new(&rewrite_rules_cfg, &config_path)
        .map_err(|e| anyhow::anyhow!("Rewrite rule error: {}", e))?;
    if rewrite_rules_cfg.is_empty() {
        log::info!("Rewrite rules: none");
    } else {
        log::info!("Rewrite rules: {} configured", rewrite_rules_cfg.len());
    }

    let collector = Arc::new(Collector::new(analytics_config.slow_query_ms));
    let metrics = Arc::new(ProxyMetrics::new());

    // Build auth cache from [[users]] config.
    let auth_cache = AuthCache::from_config(&users_config, auth_cache_ttl);
    if users_config.is_empty() {
        log::warn!("No [[users]] configured — running in open mode (any credentials accepted). Add [[users]] to turbineproxy.toml to enable auth.");
    } else {
        log::info!("Auth: {} user(s) configured", users_config.len());
    }

    // Build the protocol instance — with TLS acceptor if frontend_tls is enabled.
    let protocol: Arc<dyn protocol::DatabaseProtocol> = if frontend_tls_config.enabled {
        let acceptor = build_frontend_acceptor(&frontend_tls_config)
            .map_err(|e| anyhow::anyhow!("Frontend TLS setup failed: {}", e))?;
        log::info!("  Frontend TLS: enabled (cert={})", frontend_tls_config.cert);
        Arc::new(MySQLProtocol::with_tls_and_auth(acceptor, auth_cache))
    } else {
        Arc::new(MySQLProtocol::with_auth(auth_cache))
    };

    let server = ProxyServer::new(config.clone(), rule_engine.clone(), rewriter.clone(), collector.clone(), metrics.clone(), protocol.clone());

    let storage: Option<Arc<AnalyticsStorage>> = if analytics_config.enabled {
        match AnalyticsStorage::new(&analytics_config.db_path) {
            Ok(s) => {
                log::info!("Analytics DB: {}", analytics_config.db_path);
                let storage = Arc::new(s);
                // Restore queries_total from persisted data so the counter
                // doesn't reset to 0 on every restart.
                if let Ok(prior) = storage.load_total_query_count() {
                    if prior > 0 {
                        metrics.queries_total.fetch_add(prior as usize, std::sync::atomic::Ordering::Relaxed);
                        log::info!("Restored {} prior queries from analytics DB", prior);
                    }
                }
                tokio::spawn(analytics_flush_loop(collector.clone(), storage.clone()));
                Some(storage)
            }
            Err(e) => {
                log::warn!("Analytics storage unavailable, running in-memory only: {}", e);
                None
            }
        }
    } else {
        None
    };

    let timeseries: Option<Arc<TimeseriesStore>> = if analytics_config.enabled {
        match TimeseriesStore::new(&analytics_config.db_path) {
            Ok(ts) => {
                let ts = Arc::new(ts);
                let retention = analytics_config.retention_days;
                tokio::spawn(timeseries_loop(
                    server.throughput(),
                    ts.clone(),
                    retention,
                ));
                Some(ts)
            }
            Err(e) => {
                log::warn!("Timeseries store unavailable: {}", e);
                None
            }
        }
    } else {
        None
    };

    let regression_store = server.regression_store();
    tokio::spawn(regression_check_loop(collector.clone(), regression_store.clone()));

    // ── PostgreSQL proxy (Phase 2) ─────────────────────────────────────────
    let (pg_pool, pg_proxy_router, pg_copy_active): (
        Option<Arc<crate::proxy::pool::BackendPool>>,
        Option<crate::proxy::router::Router>,
        Arc<std::sync::atomic::AtomicUsize>
    ) = if resolved_pgsql_config.enabled {
        if resolved_pgsql_config.primary.is_none() {
            log::warn!("pgsql.enabled = true but no [pgsql.primary] configured — PG listener disabled");
            (None, None, Arc::new(std::sync::atomic::AtomicUsize::new(0)))
        } else {
            match PgProxyServer::new(
                resolved_pgsql_config.clone(),
                metrics.clone(),
                collector.clone(),
                server.n1_store(),
                server.tracer_store(),
                server.regression_store(),
                rule_engine.clone(),
                rewriter.clone(),
                server.error_events(),
                    server.heatmap(),
                    server.throughput(),
            ) {
                Some(pg_srv) => {
                    let pool = pg_srv.pool();
                    let pg_router = pg_srv.router();
                    let pg_copy_active = pg_srv.copy_active.clone();
                    let srv = Arc::new(pg_srv);
                    let srv2 = srv.clone();
                    tokio::spawn(async move {
                        if let Err(e) = srv2.run().await {
                            log::error!("PostgreSQL proxy error: {}", e);
                        }
                    });
                    // Start health checker if there are replicas or if HA is desired
                    if resolved_pgsql_config.health_check_interval_secs > 0 {
                        let pg_protocol_for_health: Arc<dyn crate::protocol::DatabaseProtocol> =
                            Arc::new(crate::protocol::PostgreSQLProtocol::new(resolved_pgsql_config.users.clone()));
                        if let Some(checker) = PgHealthChecker::new(
                            pool.clone(),
                            pg_protocol_for_health,
                            &resolved_pgsql_config,
                        ) {
                            tokio::spawn(checker.run());
                        }
                    }
                    (Some(pool), Some(pg_router), pg_copy_active)
                }
                None => {
                    log::warn!("PostgreSQL proxy could not be started (config error)");
                    (None, None, Arc::new(std::sync::atomic::AtomicUsize::new(0)))
                }
            }
        }
    } else {
        (None, None, Arc::new(std::sync::atomic::AtomicUsize::new(0)))
    };

    // ── SIGHUP handler — hot-reloads query rules + rewrite rules ─────────────
    let last_reload_secs = Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let rule_engine = rule_engine.clone();
        let rewriter = rewriter.clone();
        let last_reload = last_reload_secs.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut stream = match signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(e) => { log::warn!("SIGHUP handler setup failed: {}", e); return; }
            };
            loop {
                stream.recv().await;
                log::info!("SIGHUP received — reloading config");
                let r1 = rule_engine.reload_from_file().await;
                let r2 = rewriter.reload_from_file().await;
                if let Err(e) = r1 { log::error!("Rule reload failed: {}", e); }
                if let Err(e) = r2 { log::error!("Rewriter reload failed: {}", e); }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_secs();
                last_reload.store(now, std::sync::atomic::Ordering::Relaxed);
                log::info!("Config reloaded successfully");
            }
        });
    }

    // ── SIGTERM handler — graceful shutdown ───────────────────────────────────
    let shutdown_notify = server.shutdown_notify();
    let draining = server.draining();
    {
        let draining_flag = draining.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sig = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => { log::warn!("SIGTERM handler setup failed: {}", e); return; }
            };
            sig.recv().await;
            log::info!("SIGTERM received — initiating graceful shutdown");
            draining_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            shutdown_notify.notify_one();
        });
    }

    if dashboard_config.enabled {
        // ── Runtime config store (Fase 0.5) ───────────────────────────────────
        let config_store: Option<Arc<config::ConfigStore>> = if analytics_config.enabled {
            match config::ConfigStore::new(&analytics_config.db_path) {
                Ok(cs) => {
                    if let Err(e) = cs.seed_if_empty(
                        &config.query_rules,
                        &config.rewrite_rules,
                        &config.primary,
                        &config.replicas,
                        &config.users,
                    ) {
                        log::warn!("Config store seed failed: {}", e);
                    } else {
                        log::info!("Config store: ready ({})", analytics_config.db_path);
                    }
                    Some(Arc::new(cs))
                }
                Err(e) => {
                    log::warn!("Config store unavailable: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let state = AppState {
            metrics,
            collector,
            storage,
            timeseries,
            regression_store,
            n1_store: server.n1_store(),
            pool: server.pool().await,
            user_registry: server.user_registry(),
            rule_engine,
            rewriter,
            tracer_store: server.tracer_store(),
            app_analytics: server.app_analytics(),
            heatmap: server.heatmap(),
            dashboard_username: dashboard_config.username.clone(),
            dashboard_password: dashboard_config.password.clone(),
            tokens: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            config_path: config_path.clone(),
            proxy_router: server.router(),
            pg_proxy_router,
            proxy_config: Arc::new(std::sync::RwLock::new(config.clone())),
            last_reload_secs: last_reload_secs.clone(),
            queries_killed: server.router().queries_killed.clone(),
            cluster: config.cluster.clone(),
            config_store,
            error_events: server.error_events(),
            draining: draining.clone(),
            pg_pool,
            pg_copy_active,
            config_export_ts:   Arc::new(std::sync::atomic::AtomicU64::new(0)),
            config_mutation_ts: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let addr = dashboard_config.listen_addr.clone();
        tokio::spawn(async move {
            if let Err(e) = dashboard::run(&addr, state).await {
                log::error!("Dashboard error: {}", e);
            }
        });
    }

    if mysql_enabled && ha_config.enabled {
        let checker = HealthChecker::new(
            server.pool().await,
            protocol.clone(),
            primary_config.clone(),
            replica_configs.clone(),
            &ha_config,
        );
        tokio::spawn(checker.run());
    }

    if mysql_enabled && gr_config.enabled {
        log::info!("Group Replication monitor: enabled (interval={}s)", gr_config.check_interval_secs);
        let checker = GrChecker::new(
            server.pool().await,
            protocol,
            primary_config,
            replica_configs,
            gr_config.check_interval_secs,
        );
        tokio::spawn(checker.run());
    }

    if mysql_enabled {
        server.run().await
    } else {
        std::future::pending::<()>().await;
        Ok(())
    }
}

/// Periodically runs regression checks against current in-memory collector stats.
async fn regression_check_loop(collector: Arc<Collector>, store: Arc<RegressionStore>) {
    // Initial delay so the collector has time to accumulate some samples.
    tokio::time::sleep(Duration::from_secs(60)).await;
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        let stats = collector.get_stats().await;
        if !stats.is_empty() {
            store.check(&stats);
        }
    }
}

/// Periodically drains in-memory stats and flushes them to SQLite.
async fn analytics_flush_loop(collector: Arc<Collector>, storage: Arc<AnalyticsStorage>) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        let stats = collector.drain().await;
        if stats.is_empty() {
            continue;
        }
        let s = storage.clone();
        match tokio::task::spawn_blocking(move || s.flush(&stats)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("Analytics flush error: {}", e),
            Err(e) => log::warn!("Analytics flush task panicked: {:?}", e),
        }
    }
}

/// Time-series background task: record per-minute throughput, roll up hourly/daily, prune.
async fn timeseries_loop(
    throughput: Arc<analytics::ThroughputCounters>,
    ts: Arc<TimeseriesStore>,
    retention_days: u32,
) {
    use std::time::Instant;

    let mut tick     = tokio::time::interval(Duration::from_secs(60));
    let mut last_day = Instant::now();

    loop {
        tick.tick().await;

        // --- 1-minute bucket ---
        let snap = throughput.take_snapshot();
        let bucket = (chrono::Utc::now().timestamp() / 60) * 60;
        let ts2 = ts.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || ts2.record_minute(bucket, &snap)).await {
            log::warn!("Timeseries record error: {:?}", e);
        }

        // --- hourly roll-up (runs every tick — idempotent INSERT OR REPLACE) ---
        let ts3 = ts.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || ts3.rollup_hourly()).await {
            log::warn!("Timeseries hourly rollup error: {:?}", e);
        }

        // --- daily roll-up (idempotent, run every tick so 1d charts stay live) ---
        let ts4 = ts.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || ts4.rollup_daily()).await {
            log::warn!("Timeseries daily rollup error: {:?}", e);
        }

        // --- prune retention (once per day) ---
        if last_day.elapsed() >= Duration::from_secs(86_400) {
            last_day = Instant::now();
            let ts5 = ts.clone();
            let rd = retention_days;
            if let Err(e) = tokio::task::spawn_blocking(move || {
                ts5.prune(rd)
            }).await {
                log::warn!("Timeseries daily rollup/prune error: {:?}", e);
            }
        }
    }
}
