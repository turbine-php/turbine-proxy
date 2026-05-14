mod analytics;
mod config;
mod dashboard;
mod protocol;
mod proxy;

use anyhow::Context;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use crate::proxy::regression::RegressionStore;
use analytics::{AnalyticsStorage, Collector, TimeseriesStore};
use config::ProxyConfig;
use dashboard::AppState;
use protocol::{mysql::tls::build_frontend_acceptor, MySQLProtocol};
use proxy::{
    AuthCache, GrChecker, HealthChecker, PgHealthChecker, PgProxyServer, ProxyMetrics, ProxyServer,
    Rewriter, RuleEngine,
};
use std::path::{Path, PathBuf};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(cmd) = args.get(1).map(String::as_str) {
        if cmd == "init" || cmd == "config:init" {
            run_init_wizard(&args[2..])?;
            return Ok(());
        }
        if cmd == "-h" || cmd == "--help" {
            print_cli_help();
            return Ok(());
        }
    }

    let config_path = args
        .get(1)
        .cloned()
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
        log::info!(
            "  Frontend TLS: enabled (cert={})",
            frontend_tls_config.cert
        );
        Arc::new(MySQLProtocol::with_tls_and_auth(acceptor, auth_cache))
    } else {
        Arc::new(MySQLProtocol::with_auth(auth_cache))
    };

    let server = ProxyServer::new(
        config.clone(),
        rule_engine.clone(),
        rewriter.clone(),
        collector.clone(),
        metrics.clone(),
        protocol.clone(),
    );

    let storage: Option<Arc<AnalyticsStorage>> = if analytics_config.enabled {
        match AnalyticsStorage::new(&analytics_config.db_path) {
            Ok(s) => {
                log::info!("Analytics DB: {}", analytics_config.db_path);
                let storage = Arc::new(s);
                // Restore queries_total from persisted data so the counter
                // doesn't reset to 0 on every restart.
                if let Ok(prior) = storage.load_total_query_count() {
                    if prior > 0 {
                        metrics
                            .queries_total
                            .fetch_add(prior as usize, std::sync::atomic::Ordering::Relaxed);
                        log::info!("Restored {} prior queries from analytics DB", prior);
                    }
                }
                tokio::spawn(analytics_flush_loop(collector.clone(), storage.clone()));
                Some(storage)
            }
            Err(e) => {
                log::warn!(
                    "Analytics storage unavailable, running in-memory only: {}",
                    e
                );
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
                tokio::spawn(timeseries_loop(server.throughput(), ts.clone(), retention));
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
    tokio::spawn(regression_check_loop(
        collector.clone(),
        regression_store.clone(),
    ));

    // ── PostgreSQL proxy (Phase 2) ─────────────────────────────────────────
    let (pg_pool, pg_proxy_router, pg_copy_active): (
        Option<Arc<crate::proxy::pool::BackendPool>>,
        Option<crate::proxy::router::Router>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) = if resolved_pgsql_config.enabled {
        if resolved_pgsql_config.primary.is_none() {
            log::warn!(
                "pgsql.enabled = true but no [pgsql.primary] configured — PG listener disabled"
            );
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
                            Arc::new(crate::protocol::PostgreSQLProtocol::new(
                                resolved_pgsql_config.users.clone(),
                            ));
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
                Err(e) => {
                    log::warn!("SIGHUP handler setup failed: {}", e);
                    return;
                }
            };
            loop {
                stream.recv().await;
                log::info!("SIGHUP received — reloading config");
                let r1 = rule_engine.reload_from_file().await;
                let r2 = rewriter.reload_from_file().await;
                if let Err(e) = r1 {
                    log::error!("Rule reload failed: {}", e);
                }
                if let Err(e) = r2 {
                    log::error!("Rewriter reload failed: {}", e);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
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
                Err(e) => {
                    log::warn!("SIGTERM handler setup failed: {}", e);
                    return;
                }
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
            dashboard_readonly_username: dashboard_config.readonly_username.clone(),
            dashboard_readonly_password: dashboard_config.readonly_password.clone(),
            token_ttl_secs: dashboard_config.token_ttl_secs,
            login_max_attempts: dashboard_config.login_max_attempts,
            tokens: std::sync::Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            rate_limits: std::sync::Arc::new(parking_lot::Mutex::new(
                std::collections::HashMap::new(),
            )),
            config_path: config_path.clone(),
            proxy_router: server.router(),
            pg_proxy_router,
            proxy_config: Arc::new(parking_lot::RwLock::new(config.clone())),
            last_reload_secs: last_reload_secs.clone(),
            queries_killed: server.router().queries_killed.clone(),
            cluster: config.cluster.clone(),
            config_store,
            error_events: server.error_events(),
            draining: draining.clone(),
            pg_pool,
            pg_copy_active,
            config_export_ts: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
        log::info!(
            "Group Replication monitor: enabled (interval={}s)",
            gr_config.check_interval_secs
        );
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

fn print_cli_help() {
    println!("TurbineProxy {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("  turbineproxy [config_path]");
    println!("  turbineproxy init [--output <path>] [--force]");
    println!();
    println!("COMMANDS:");
    println!("  init          Start the interactive config wizard");
    println!();
    println!("OPTIONS:");
    println!("  -o, --output  Output path for generated config (default: turbineproxy.toml)");
    println!("  -f, --force   Overwrite output file without confirmation");
    println!("  -h, --help    Print this help text");
}

#[derive(Debug)]
struct InitOptions {
    output: PathBuf,
    force: bool,
}

fn parse_init_options(args: &[String]) -> anyhow::Result<InitOptions> {
    let mut output = PathBuf::from("turbineproxy.toml");
    let mut force = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                let next = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("missing value for {}", args[i]))?;
                output = PathBuf::from(next);
                i += 2;
            }
            "-f" | "--force" => {
                force = true;
                i += 1;
            }
            "-h" | "--help" => {
                print_cli_help();
                std::process::exit(0);
            }
            other => {
                return Err(anyhow::anyhow!(
                    "unknown init option: {} (use --help for usage)",
                    other
                ));
            }
        }
    }

    Ok(InitOptions { output, force })
}

fn prompt_with_default(label: &str, default: &str) -> anyhow::Result<String> {
    print!("{} [{}]: ", label, default);
    io::stdout().flush().context("flush stdout")?;

    let mut input = String::new();
    io::stdin().read_line(&mut input).context("read stdin")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_bool(label: &str, default: bool) -> anyhow::Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        print!("{} [{}]: ", label, hint);
        io::stdout().flush().context("flush stdout")?;

        let mut input = String::new();
        io::stdin().read_line(&mut input).context("read stdin")?;
        let t = input.trim().to_ascii_lowercase();
        if t.is_empty() {
            return Ok(default);
        }
        match t.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                println!("Please answer yes or no.");
            }
        }
    }
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn run_init_wizard(args: &[String]) -> anyhow::Result<()> {
    let opts = parse_init_options(args)?;

    const CYAN: &str = "\x1b[36m";
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RESET: &str = "\x1b[0m";

    println!(
        "{}========================================================{}",
        CYAN, RESET
    );
    println!(
        "{}                TurbineProxy Config Wizard             {}",
        CYAN, RESET
    );
    println!(
        "{}========================================================{}",
        CYAN, RESET
    );
    println!();

    if opts.output.exists() && !opts.force {
        println!(
            "{}Warning:{} {} already exists.",
            YELLOW,
            RESET,
            opts.output.display()
        );
        let overwrite = prompt_bool("Overwrite existing file", false)?;
        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
    }

    println!("{}Step 1/4:{} Backend settings", CYAN, RESET);
    let backend_type = loop {
        let ans = prompt_with_default("Backend type (mysql|pgsql)", "mysql")?;
        let normalized = ans.to_ascii_lowercase();
        if normalized == "mysql" || normalized == "pgsql" {
            break normalized;
        }
        println!("Please choose mysql or pgsql.");
    };
    let default_addr = if backend_type == "pgsql" {
        "127.0.0.1:5432"
    } else {
        "127.0.0.1:3306"
    };
    let primary_addr = prompt_with_default("Primary backend addr", default_addr)?;
    let db_user = prompt_with_default(
        "Backend user",
        if backend_type == "pgsql" {
            "postgres"
        } else {
            "root"
        },
    )?;
    let db_password = prompt_with_default("Backend password", "")?;
    let db_name = prompt_with_default(
        "Default database",
        if backend_type == "pgsql" {
            "postgres"
        } else {
            "myapp"
        },
    )?;
    println!();

    println!("{}Step 2/4:{} Listener settings", CYAN, RESET);
    let mysql_enabled = if backend_type == "mysql" {
        prompt_bool("Enable MySQL listener", true)?
    } else {
        prompt_bool("Enable MySQL listener", false)?
    };
    let mysql_listen = if mysql_enabled {
        Some(prompt_with_default("MySQL listen addr", "0.0.0.0:3307")?)
    } else {
        None
    };

    let pg_enabled = if backend_type == "pgsql" {
        prompt_bool("Enable PostgreSQL listener", true)?
    } else {
        prompt_bool("Enable PostgreSQL listener", false)?
    };
    let pg_listen = if pg_enabled {
        Some(prompt_with_default(
            "PostgreSQL listen addr",
            "0.0.0.0:5432",
        )?)
    } else {
        None
    };
    println!();

    println!("{}Step 3/4:{} Dashboard and analytics", CYAN, RESET);
    let dashboard_enabled = prompt_bool("Enable dashboard", true)?;
    let dashboard_addr = if dashboard_enabled {
        prompt_with_default("Dashboard listen addr", "0.0.0.0:8080")?
    } else {
        "0.0.0.0:8080".to_string()
    };
    let dashboard_auth = if dashboard_enabled {
        prompt_bool("Enable dashboard auth", false)?
    } else {
        false
    };
    let dashboard_user = if dashboard_auth {
        prompt_with_default("Dashboard username", "admin")?
    } else {
        String::new()
    };
    let dashboard_password = if dashboard_auth {
        prompt_with_default("Dashboard password", "")?
    } else {
        String::new()
    };

    let analytics_enabled = prompt_bool("Enable analytics", true)?;
    let slow_query_ms = prompt_with_default("Slow query threshold (ms)", "100")?;
    let slow_query_ms: u64 = slow_query_ms
        .parse()
        .context("slow_query_ms must be a positive integer")?;
    println!();

    println!("{}Step 4/4:{} Capacity and HA", CYAN, RESET);
    let max_connections: usize = prompt_with_default("Max client connections", "1000")?
        .parse()
        .context("max_connections must be a positive integer")?;
    let pool_size: usize = prompt_with_default("Connection pool size", "20")?
        .parse()
        .context("pool_size must be a positive integer")?;
    let ha_enabled = prompt_bool("Enable HA health checks", true)?;
    println!();

    let mysql_block = if mysql_enabled {
        format!(
            "[mysql]\nenabled     = true\nlisten_addr = \"{}\"\n",
            toml_escape(mysql_listen.as_deref().unwrap_or("0.0.0.0:3307"))
        )
    } else {
        "[mysql]\nenabled = false\n".to_string()
    };

    let pg_block = if pg_enabled {
        format!(
            "[pgsql]\nenabled     = true\nlisten_addr = \"{}\"\nhealth_check_database = \"postgres\"\n",
            toml_escape(pg_listen.as_deref().unwrap_or("0.0.0.0:5432"))
        )
    } else {
        "[pgsql]\nenabled = false\n".to_string()
    };

    let rendered = format!(
        "# Generated by: turbineproxy init\n\n[shared]\nmax_connections = {max_connections}\npool_size       = {pool_size}\n\n[shared.primary]\naddr     = \"{primary_addr}\"\nuser     = \"{db_user}\"\npassword = \"{db_password}\"\ndatabase = \"{db_name}\"\n\n{mysql_block}\n{pg_block}\n[analytics]\nenabled = {analytics_enabled}\ndb_path = \"turbineproxy_analytics.db\"\nslow_query_ms = {slow_query_ms}\nretention_days = 30\n\n[dashboard]\nenabled = {dashboard_enabled}\nlisten_addr = \"{dashboard_addr}\"\nusername = \"{dashboard_user}\"\npassword = \"{dashboard_password}\"\n\n[ha]\nenabled = {ha_enabled}\nhealth_check_interval_secs = 5\nmax_replica_lag_ms = 5000\nprimary_failover_threshold = 3\n",
        max_connections = max_connections,
        pool_size = pool_size,
        primary_addr = toml_escape(&primary_addr),
        db_user = toml_escape(&db_user),
        db_password = toml_escape(&db_password),
        db_name = toml_escape(&db_name),
        mysql_block = mysql_block,
        pg_block = pg_block,
        analytics_enabled = analytics_enabled,
        slow_query_ms = slow_query_ms,
        dashboard_enabled = dashboard_enabled,
        dashboard_addr = toml_escape(&dashboard_addr),
        dashboard_user = toml_escape(&dashboard_user),
        dashboard_password = toml_escape(&dashboard_password),
        ha_enabled = ha_enabled,
    );

    ProxyConfig::from_str(&rendered).context("generated config is invalid")?;

    if let Some(parent) = opts.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
    }
    std::fs::write(&opts.output, rendered)
        .with_context(|| format!("write {}", opts.output.display()))?;

    println!(
        "{}Success:{} config written to {}",
        GREEN,
        RESET,
        opts.output.display()
    );
    println!("Next: run turbineproxy {}", opts.output.display());
    Ok(())
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

    let mut tick = tokio::time::interval(Duration::from_secs(60));
    let mut last_day = Instant::now();

    loop {
        tick.tick().await;

        // --- 1-minute bucket ---
        let snap = throughput.take_snapshot();
        let bucket = (chrono::Utc::now().timestamp() / 60) * 60;
        let ts2 = ts.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || ts2.record_minute(bucket, &snap)).await
        {
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
            if let Err(e) = tokio::task::spawn_blocking(move || ts5.prune(rd)).await {
                log::warn!("Timeseries daily rollup/prune error: {:?}", e);
            }
        }
    }
}
