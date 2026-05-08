//! Criterion benchmarks for TurbineProxy.
//!
//! Measures throughput and latency of MySQL and PostgreSQL queries through the
//! proxy, and compares against direct backend connections to quantify overhead.
//!
//! # What is benchmarked
//!
//!  Group                  | Description
//!  -----------------------|------------------------------------------------------
//!  `mysql/direct`         | Queries direct to MySQL (baseline — no proxy)
//!  `mysql/proxy`          | Queries through TurbineProxy MySQL proxy
//!  `mysql/cluster_sync`   | POST /api/sync round-trip to a running node
//!  `pgsql/direct`         | Queries direct to PostgreSQL (baseline)
//!  `pgsql/proxy`          | Queries through TurbineProxy PgSQL proxy
//!  `pgsql/prepared`       | Prepared-statement execute through PgSQL proxy
//!  `pgsql/cluster_sync`   | POST /api/sync round-trip to a running node
//!
//! # Running
//! ```bash
//! # Start backends first:
//! docker compose up mysql80 postgres16 -d
//!
//! # Run all benchmarks (HTML report in target/criterion):
//! cargo bench
//!
//! # Run only a specific group:
//! cargo bench -- mysql
//! cargo bench -- pgsql
//! cargo bench -- cluster
//!
//! # Save a baseline and compare:
//! cargo bench -- --save-baseline main
//! # (make changes)
//! cargo bench -- --baseline main
//! ```
//!
//! Benchmarks are **automatically skipped** when the backend is unreachable —
//! safe to run in CI environments without Docker.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use mysql::{prelude::*, Conn, OptsBuilder};
use std::{
    env,
    io::Write as _,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio::runtime::Runtime;
use tokio_postgres::{Client as PgClient, Config as PgConfig, NoTls};

// ── Config ─────────────────────────────────────────────────────────────────────

fn mysql_host() -> String { env::var("TEST_MYSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()) }
fn mysql_port() -> u16 { env::var("TEST_MYSQL_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(3306) }
fn mysql_user() -> String { env::var("TEST_MYSQL_USER").unwrap_or_else(|_| "root".into()) }
fn mysql_pass() -> String { env::var("TEST_MYSQL_PASS").unwrap_or_else(|_| "root".into()) }

fn pg_host() -> String { env::var("TEST_PG_HOST").unwrap_or_else(|_| "127.0.0.1".into()) }
fn pg_port() -> u16 { env::var("TEST_PG_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(5432) }
fn pg_user() -> String { env::var("TEST_PG_USER").unwrap_or_else(|_| "postgres".into()) }
fn pg_pass() -> String { env::var("TEST_PG_PASS").unwrap_or_else(|_| "postgres".into()) }

const MYSQL_DB: &str = "turbineproxy_test";
const PG_DB:    &str = "turbineproxy_test";

/// MySQL proxy port used for benchmarks (full stack).
const BENCH_MYSQL_PROXY:  u16 = 33307;
/// PgSQL proxy port used for the full-stack proxy (MySQL + PG both required).
const BENCH_PG_PROXY:     u16 = 35433;
/// PgSQL proxy port used for PG-only bench (only PG required).
const BENCH_PG_ONLY_PROXY: u16 = 35434;
/// Dashboard port for cluster sync benchmarks.
const BENCH_DASHBOARD:    u16 = 38080;
const CLUSTER_SECRET:     &str = "bench-secret";

// ── Proxy lifecycle ────────────────────────────────────────────────────────────

struct BenchProxy {
    _child:  Child,
    _config: NamedTempFile,
}

/// Full-stack proxy (MySQL + PG).  Used by `bench_mysql` and `bench_cluster_sync`.
static BENCH_PROXY: OnceLock<Option<BenchProxy>> = OnceLock::new();
/// PG-only proxy.  Used by `bench_pgsql` — does NOT require MySQL to be up.
static PG_ONLY_PROXY: OnceLock<Option<BenchProxy>> = OnceLock::new();

fn rt() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn bench_rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(rt)
}

/// Returns true when this benchmark group should execute for the current CLI filter.
fn should_run_group(group: &str) -> bool {
    let mut saw_filter = false;
    for arg in env::args().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        saw_filter = true;
        if group.contains(&arg) || arg.contains(group) {
            return true;
        }
    }
    !saw_filter
}

fn mysql_available() -> bool {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some(mysql_host()))
        .tcp_port(mysql_port())
        .user(Some(mysql_user()))
        .pass(Some(mysql_pass()))
        .db_name(Some(MYSQL_DB));
    Conn::new(opts).is_ok()
}

fn pg_available() -> bool {
    let mut cfg = PgConfig::new();
    cfg.host(&pg_host())
        .port(pg_port())
        .user(&pg_user())
        .password(pg_pass().as_bytes())
        .dbname(PG_DB)
        .connect_timeout(Duration::from_secs(3));
    rt().block_on(async { cfg.connect(NoTls).await.is_ok() })
}

/// Full-stack proxy — requires MySQL **and** PG.
fn get_proxy() -> Option<&'static BenchProxy> {
    BENCH_PROXY
        .get_or_init(|| {
            if !mysql_available() || !pg_available() {
                eprintln!(
                    "BENCH SKIP (mysql): MySQL or PostgreSQL not reachable — \
                     start with: docker compose up mysql80 postgres16 -d"
                );
                return None;
            }
            Some(start_bench_proxy())
        })
        .as_ref()
}

/// PG-only proxy — requires only PostgreSQL.
fn get_pg_proxy() -> Option<&'static BenchProxy> {
    PG_ONLY_PROXY
        .get_or_init(|| {
            if !pg_available() {
                eprintln!(
                    "BENCH SKIP (pgsql): PostgreSQL not reachable — \
                     start with: docker compose up postgres16 -d"
                );
                return None;
            }
            Some(start_pg_only_proxy())
        })
        .as_ref()
}

fn start_bench_proxy() -> BenchProxy {
    let mut config = NamedTempFile::new().unwrap();
    write!(
        config,
        r#"listen_addr     = "127.0.0.1:{mysql_proxy}"
max_connections = 200
pool_size       = 20

[primary]
addr     = "{mysql_host}:{mysql_port}"
user     = "{mysql_user}"
password = "{mysql_pass}"
database = "{mysql_db}"

[analytics]
enabled = false

[dashboard]
enabled     = true
listen_addr = "127.0.0.1:{dashboard}"

[ha]
enabled = false

[cluster]
peers  = []
secret = "{secret}"

[pgsql]
enabled         = true
listen_addr     = "127.0.0.1:{pg_proxy}"
pool_size       = 20
max_connections = 200

[pgsql.primary]
addr     = "{pg_host}:{pg_port}"
user     = "{pg_user}"
password = "{pg_pass}"
database = "{pg_db}"
"#,
        mysql_proxy = BENCH_MYSQL_PROXY,
        mysql_host  = mysql_host(),
        mysql_port  = mysql_port(),
        mysql_user  = mysql_user(),
        mysql_pass  = mysql_pass(),
        mysql_db    = MYSQL_DB,
        dashboard   = BENCH_DASHBOARD,
        secret      = CLUSTER_SECRET,
        pg_proxy    = BENCH_PG_PROXY,
        pg_host     = pg_host(),
        pg_port     = pg_port(),
        pg_user     = pg_user(),
        pg_pass     = pg_pass(),
        pg_db       = PG_DB,
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_turbineproxy");
    let child = Command::new(binary)
        .arg(config.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn turbineproxy: {e}"));

    // Wait MySQL proxy.
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(200));
        let opts = OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(BENCH_MYSQL_PROXY)
            .user(Some(mysql_user()))
            .pass(Some(mysql_pass()))
            .db_name(Some(MYSQL_DB));
        if Conn::new(opts).is_ok() {
            break;
        }
    }
    // Wait PgSQL proxy.
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(200));
        let mut cfg = PgConfig::new();
        cfg.host("127.0.0.1").port(BENCH_PG_PROXY)
            .user(&pg_user()).password(pg_pass().as_bytes()).dbname(PG_DB)
            .connect_timeout(Duration::from_secs(1));
        if rt().block_on(async { cfg.connect(NoTls).await.is_ok() }) {
            break;
        }
    }
    // Wait dashboard.
    for _ in 0..75 {
        std::thread::sleep(Duration::from_millis(200));
        if rt()
            .block_on(async {
                reqwest::get(format!("http://127.0.0.1:{BENCH_DASHBOARD}/health")).await
            })
            .is_ok()
        {
            break;
        }
    }

    BenchProxy { _child: child, _config: config }
}

/// Start a proxy that only exposes the PgSQL listener.
/// The MySQL section points to a dummy addr so the proxy starts even without MySQL.
fn start_pg_only_proxy() -> BenchProxy {
    let mut config = NamedTempFile::new().unwrap();
    write!(
        config,
        r#"listen_addr     = "127.0.0.1:{mysql_proxy}"
max_connections = 10
pool_size       = 5

[primary]
addr     = "127.0.0.1:1"
user     = "nobody"
password = ""
database = "none"

[analytics]
enabled = false

[dashboard]
enabled = false

[ha]
enabled = false

[cluster]
peers  = []
secret = "{secret}"

[pgsql]
enabled         = true
listen_addr     = "127.0.0.1:{pg_proxy}"
pool_size       = 20
max_connections = 200

[pgsql.primary]
addr     = "{pg_host}:{pg_port}"
user     = "{pg_user}"
password = "{pg_pass}"
database = "{pg_db}"
"#,
        mysql_proxy = BENCH_MYSQL_PROXY + 1000, // port 34307 — not used by any bench
        secret      = CLUSTER_SECRET,
        pg_proxy    = BENCH_PG_ONLY_PROXY,
        pg_host     = pg_host(),
        pg_port     = pg_port(),
        pg_user     = pg_user(),
        pg_pass     = pg_pass(),
        pg_db       = PG_DB,
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_turbineproxy");
    let child = Command::new(binary)
        .arg(config.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn turbineproxy (pg-only): {e}"));

    // Wait for PgSQL proxy to be ready.
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(200));
        let mut cfg = PgConfig::new();
        cfg.host("127.0.0.1").port(BENCH_PG_ONLY_PROXY)
            .user(&pg_user()).password(pg_pass().as_bytes()).dbname(PG_DB)
            .connect_timeout(Duration::from_secs(1));
        if rt().block_on(async { cfg.connect(NoTls).await.is_ok() }) {
            break;
        }
    }

    BenchProxy { _child: child, _config: config }
}

fn mysql_direct() -> Conn {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some(mysql_host()))
        .tcp_port(mysql_port())
        .user(Some(mysql_user()))
        .pass(Some(mysql_pass()))
        .db_name(Some(MYSQL_DB));
    Conn::new(opts).expect("direct MySQL connect")
}

fn mysql_proxy() -> Conn {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some("127.0.0.1"))
        .tcp_port(BENCH_MYSQL_PROXY)
        .user(Some(mysql_user()))
        .pass(Some(mysql_pass()))
        .db_name(Some(MYSQL_DB));
    Conn::new(opts).expect("proxy MySQL connect")
}

// ── PgSQL connection helpers ───────────────────────────────────────────────────

async fn pg_direct() -> PgClient {
    let mut cfg = PgConfig::new();
    cfg.host(&pg_host()).port(pg_port())
        .user(&pg_user()).password(pg_pass().as_bytes()).dbname(PG_DB)
        .connect_timeout(Duration::from_secs(5));
    let (c, conn) = cfg.connect(NoTls).await.expect("direct pg connect");
    tokio::spawn(conn);
    c
}

/// Connect through the full-stack proxy (MySQL + PG both required).
async fn pg_proxy_client() -> PgClient {
    let mut cfg = PgConfig::new();
    cfg.host("127.0.0.1").port(BENCH_PG_PROXY)
        .user(&pg_user()).password(pg_pass().as_bytes()).dbname(PG_DB)
        .connect_timeout(Duration::from_secs(5));
    let (c, conn) = cfg.connect(NoTls).await.expect("proxy pg connect");
    tokio::spawn(conn);
    c
}

/// Connect through the PG-only proxy (only PG required).
async fn pg_proxy_only_client() -> PgClient {
    let mut cfg = PgConfig::new();
    cfg.host("127.0.0.1").port(BENCH_PG_ONLY_PROXY)
        .user(&pg_user()).password(pg_pass().as_bytes()).dbname(PG_DB)
        .connect_timeout(Duration::from_secs(5));
    let (c, conn) = cfg.connect(NoTls).await.expect("pg-only proxy connect");
    tokio::spawn(conn);
    c
}

// ── Benchmark groups ───────────────────────────────────────────────────────────

fn bench_mysql(c: &mut Criterion) {
    if !should_run_group("mysql") {
        return;
    }
    if get_proxy().is_none() { return; }

    let mut group = c.benchmark_group("mysql");
    group.throughput(Throughput::Elements(1));

    // ── SELECT 1 ──────────────────────────────────────────────────────────────
    let mut direct = mysql_direct();
    group.bench_function("direct/select_1", |b| {
        b.iter(|| {
            let _: Vec<i32> = direct.query("SELECT 1").unwrap();
        });
    });

    let mut proxy = mysql_proxy();
    group.bench_function("proxy/select_1", |b| {
        b.iter(|| {
            let _: Vec<i32> = proxy.query("SELECT 1").unwrap();
        });
    });

    // ── SELECT now() ──────────────────────────────────────────────────────────
    let mut direct = mysql_direct();
    group.bench_function("direct/select_now", |b| {
        b.iter(|| {
            let _: Vec<String> = direct.query("SELECT NOW()").unwrap();
        });
    });

    let mut proxy = mysql_proxy();
    group.bench_function("proxy/select_now", |b| {
        b.iter(|| {
            let _: Vec<String> = proxy.query("SELECT NOW()").unwrap();
        });
    });

    // ── Insert + select (DML round-trip) ──────────────────────────────────────
    direct
        .query_drop(
            "CREATE TABLE IF NOT EXISTS bench_mysql \
             (id INT AUTO_INCREMENT PRIMARY KEY, v INT) ENGINE=InnoDB",
        )
        .unwrap();

    let mut direct = mysql_direct();
    group.bench_function("direct/insert_select", |b| {
        b.iter(|| {
            direct.query_drop("INSERT INTO bench_mysql (v) VALUES (1)").unwrap();
            let _: Vec<i32> = direct.query("SELECT COUNT(*) FROM bench_mysql").unwrap();
        });
    });

    let mut proxy = mysql_proxy();
    group.bench_function("proxy/insert_select", |b| {
        b.iter(|| {
            proxy.query_drop("INSERT INTO bench_mysql (v) VALUES (1)").unwrap();
            let _: Vec<i32> = proxy.query("SELECT COUNT(*) FROM bench_mysql").unwrap();
        });
    });

    // ── Prepared statement ────────────────────────────────────────────────────
    let mut direct = mysql_direct();
    group.bench_function("direct/prepared_select", |b| {
        b.iter(|| {
            let _: Vec<i32> = direct.exec("SELECT ?", (1_i32,)).unwrap();
        });
    });

    let mut proxy = mysql_proxy();
    group.bench_function("proxy/prepared_select", |b| {
        b.iter(|| {
            let _: Vec<i32> = proxy.exec("SELECT ?", (1_i32,)).unwrap();
        });
    });

    // ── Transaction (BEGIN + INSERT + COMMIT) ─────────────────────────────────
    let mut direct = mysql_direct();
    group.bench_function("direct/transaction", |b| {
        b.iter(|| {
            direct.query_drop("START TRANSACTION").unwrap();
            direct.query_drop("INSERT INTO bench_mysql (v) VALUES (99)").unwrap();
            direct.query_drop("ROLLBACK").unwrap();
        });
    });

    let mut proxy = mysql_proxy();
    group.bench_function("proxy/transaction", |b| {
        b.iter(|| {
            proxy.query_drop("START TRANSACTION").unwrap();
            proxy.query_drop("INSERT INTO bench_mysql (v) VALUES (99)").unwrap();
            proxy.query_drop("ROLLBACK").unwrap();
        });
    });

    // ── Batch SELECT with varying result sizes ────────────────────────────────
    for n in [1usize, 10, 100] {
        let mut direct = mysql_direct();
        group.bench_with_input(BenchmarkId::new("direct/select_n_rows", n), &n, |b, &n| {
            b.iter(|| {
                let _: Vec<i32> = direct
                    .query(format!(
                        "SELECT seq FROM (SELECT 1 AS seq UNION ALL \
                         SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 \
                         UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 \
                         UNION ALL SELECT 9 UNION ALL SELECT 10) t LIMIT {n}"
                    ))
                    .unwrap();
            });
        });

        let mut proxy = mysql_proxy();
        group.bench_with_input(BenchmarkId::new("proxy/select_n_rows", n), &n, |b, &n| {
            b.iter(|| {
                let _: Vec<i32> = proxy
                    .query(format!(
                        "SELECT seq FROM (SELECT 1 AS seq UNION ALL \
                         SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 \
                         UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 \
                         UNION ALL SELECT 9 UNION ALL SELECT 10) t LIMIT {n}"
                    ))
                    .unwrap();
            });
        });
    }

    group.finish();
}

fn bench_pgsql(c: &mut Criterion) {
    if !should_run_group("pgsql") {
        return;
    }
    // PG bench only needs PostgreSQL — MySQL is not required.
    if get_pg_proxy().is_none() { return; }

    let mut group = c.benchmark_group("pgsql");
    group.throughput(Throughput::Elements(1));

    let rt = bench_rt();

    // ── SELECT 1 ──────────────────────────────────────────────────────────────
    let direct = rt.block_on(pg_direct());
    group.bench_function("direct/select_1", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = direct.query_one("SELECT 1 AS n", &[]).await.unwrap();
        });
    });

    let proxy = rt.block_on(pg_proxy_only_client());
    group.bench_function("proxy/select_1", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = proxy.query_one("SELECT 1 AS n", &[]).await.unwrap();
        });
    });

    // ── SELECT now() ──────────────────────────────────────────────────────────
    let direct = rt.block_on(pg_direct());
    group.bench_function("direct/select_now", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = direct.query_one("SELECT now()", &[]).await.unwrap();
        });
    });

    let proxy = rt.block_on(pg_proxy_only_client());
    group.bench_function("proxy/select_now", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = proxy.query_one("SELECT now()", &[]).await.unwrap();
        });
    });

    // ── Parameterized query ───────────────────────────────────────────────────
    let direct = rt.block_on(pg_direct());
    group.bench_function("direct/parameterized", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = direct
                .query_one("SELECT $1::int + $2::int AS r", &[&10_i32, &20_i32])
                .await
                .unwrap();
        });
    });

    let proxy = rt.block_on(pg_proxy_only_client());
    group.bench_function("proxy/parameterized", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = proxy
                .query_one("SELECT $1::int + $2::int AS r", &[&10_i32, &20_i32])
                .await
                .unwrap();
        });
    });

    // ── Prepared statement (reused handle) ────────────────────────────────────
    let direct = rt.block_on(async {
        let c = pg_direct().await;
        let stmt = c.prepare("SELECT $1::int AS n").await.unwrap();
        (c, stmt)
    });
    group.bench_function("direct/prepared_reuse", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = direct.0.query_one(&direct.1, &[&42_i32]).await.unwrap();
        });
    });

    let proxy_prepared = rt.block_on(async {
        let c = pg_proxy_only_client().await;
        let stmt = c.prepare("SELECT $1::int AS n").await.unwrap();
        (c, stmt)
    });
    group.bench_function("proxy/prepared_reuse", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _row = proxy_prepared.0.query_one(&proxy_prepared.1, &[&42_i32]).await.unwrap();
        });
    });

    // ── Transaction (BEGIN + INSERT + ROLLBACK) ───────────────────────────────
    let direct = rt.block_on(async {
        let c = pg_direct().await;
        c.execute(
            "CREATE TABLE IF NOT EXISTS bench_pgsql \
             (id SERIAL PRIMARY KEY, v INT)",
            &[],
        )
        .await
        .unwrap();
        c
    });
    group.bench_function("direct/transaction", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            direct
                .batch_execute("BEGIN; INSERT INTO bench_pgsql (v) VALUES (99); ROLLBACK;")
                .await
                .unwrap();
        });
    });

    let proxy = rt.block_on(pg_proxy_only_client());
    group.bench_function("proxy/transaction", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            // Use simple query protocol here to avoid prepared statement cache
            // mismatches when backend sessions are recycled by the proxy.
            proxy
                .batch_execute("BEGIN; INSERT INTO bench_pgsql (v) VALUES (99); ROLLBACK;")
                .await
                .unwrap();
        });
    });

    // ── Batch SELECT with varying result sizes ────────────────────────────────
    for n in [1usize, 10, 100] {
        let direct = rt.block_on(pg_direct());
        group.bench_with_input(BenchmarkId::new("direct/select_n_rows", n), &n, |b, &n| {
            b.to_async(bench_rt())
            .iter(|| async {
                let _rows = direct
                    .query(
                        &format!("SELECT generate_series(1, {n}) AS s"),
                        &[],
                    )
                    .await
                    .unwrap();
            });
        });

        let proxy = rt.block_on(pg_proxy_only_client());
        group.bench_with_input(BenchmarkId::new("proxy/select_n_rows", n), &n, |b, &n| {
            b.to_async(bench_rt())
            .iter(|| async {
                let _rows = proxy
                    .query(
                        &format!("SELECT generate_series(1, {n}) AS s"),
                        &[],
                    )
                    .await
                    .unwrap();
            });
        });
    }

    group.finish();
}

fn bench_cluster_sync(c: &mut Criterion) {
    if !should_run_group("cluster") {
        return;
    }
    if get_proxy().is_none() { return; }

    let mut group = c.benchmark_group("cluster");
    group.throughput(Throughput::Elements(1));

    // Build a minimal valid config to push.
    let config_toml = format!(
        r#"listen_addr = "127.0.0.1:{mysql_proxy}"
max_connections = 200
pool_size = 20

[primary]
addr     = "{mysql_host}:{mysql_port}"
user     = "{mysql_user}"
password = "{mysql_pass}"
database = "{mysql_db}"

[analytics]
enabled = false

[dashboard]
enabled     = true
listen_addr = "127.0.0.1:{dashboard}"

[ha]
enabled = false

[cluster]
peers  = []
secret = "{secret}"

[pgsql]
enabled = false
"#,
        mysql_proxy = BENCH_MYSQL_PROXY,
        mysql_host  = mysql_host(),
        mysql_port  = mysql_port(),
        mysql_user  = mysql_user(),
        mysql_pass  = mysql_pass(),
        mysql_db    = MYSQL_DB,
        dashboard   = BENCH_DASHBOARD,
        secret      = CLUSTER_SECRET,
    );

    let client = reqwest::Client::new();

    // ── POST /api/sync latency ────────────────────────────────────────────────
    group.bench_function("sync/post_valid", |b| {
        b.to_async(bench_rt())
        .iter(|| {
            let client = &client;
            let config_toml = &config_toml;
            async move {
                let _resp = client
                    .post(format!("http://127.0.0.1:{BENCH_DASHBOARD}/api/sync"))
                    .header("Authorization", format!("Bearer {CLUSTER_SECRET}"))
                    .json(&serde_json::json!({ "config_toml": config_toml }))
                    .send()
                    .await
                    .unwrap();
            }
        });
    });

    // ── GET /health latency ───────────────────────────────────────────────────
    group.bench_function("dashboard/get_health", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _resp =
                reqwest::get(format!("http://127.0.0.1:{BENCH_DASHBOARD}/health"))
                    .await
                    .unwrap();
        });
    });

    // ── GET /api/postgres/pool latency ────────────────────────────────────────
    group.bench_function("dashboard/get_pg_pool", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _resp =
                reqwest::get(format!("http://127.0.0.1:{BENCH_DASHBOARD}/api/postgres/pool"))
                    .await
                    .unwrap();
        });
    });

    // ── GET /api/config/status latency ───────────────────────────────────────
    group.bench_function("dashboard/get_config_status", |b| {
        b.to_async(bench_rt())
        .iter(|| async {
            let _resp = reqwest::get(format!(
                "http://127.0.0.1:{BENCH_DASHBOARD}/api/config/status"
            ))
            .await
            .unwrap();
        });
    });

    group.finish();
}

fn bench_config() -> Criterion {
    // Local quick mode: much faster feedback while tuning.
    // Usage: BENCH_FAST=1 cargo bench -- pgsql
    if env::var("BENCH_FAST").ok().as_deref() == Some("1") {
        return Criterion::default()
            .measurement_time(Duration::from_secs(2))
            .warm_up_time(Duration::from_secs(1))
            .sample_size(10);
    }

    Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(3))
        .sample_size(50)
}

criterion_group!(
    name    = benches;
    config  = bench_config();
    targets = bench_mysql, bench_pgsql, bench_cluster_sync
);
criterion_main!(benches);
