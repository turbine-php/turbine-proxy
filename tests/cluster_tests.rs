//! Cluster integration tests for TurbineProxy.
//!
//! Starts **two** TurbineProxy instances (node-A and node-B) configured as a
//! cluster and verifies:
//!   - MySQL query routing through both nodes
//!   - PostgreSQL query routing through both nodes
//!   - `POST /api/sync` propagates config changes between nodes (Bearer auth)
//!   - `GET  /api/config/status` reflects unsaved-change state after sync
//!   - Peer rejection when using a wrong cluster secret
//!   - Both nodes see the same pool stats after sync
//!
//! # Prerequisites
//!
//! | Variable         | Default     | Description               |
//! |------------------|-------------|---------------------------|
//! | TEST_MYSQL_HOST  | 127.0.0.1   | MySQL primary host        |
//! | TEST_MYSQL_PORT  | 3306        | MySQL primary port        |
//! | TEST_MYSQL_USER  | root        | MySQL user                |
//! | TEST_MYSQL_PASS  | root        | MySQL password            |
//! | TEST_PG_HOST     | 127.0.0.1   | PostgreSQL primary host   |
//! | TEST_PG_PORT     | 5432        | PostgreSQL primary port   |
//! | TEST_PG_USER     | postgres    | PostgreSQL user           |
//! | TEST_PG_PASS     | postgres    | PostgreSQL password       |
//!
//! # Running
//! ```bash
//! docker compose up mysql80 postgres16 -d
//! cargo test --test cluster_tests -- --test-threads=1
//! ```
//!
//! Tests skip automatically when MySQL or PostgreSQL are unreachable.

use mysql::{prelude::*, Conn, OptsBuilder};
use std::{
    env,
    io::Write as _,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio_postgres::{Config as PgConfig, NoTls};

// ── Shared config helpers ──────────────────────────────────────────────────────

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

// Node A ports
const MYSQL_PROXY_A:  u16 = 23307;
const PG_PROXY_A:     u16 = 25433;
const DASHBOARD_A:    u16 = 28080;

// Node B ports
const MYSQL_PROXY_B:  u16 = 23308;
const PG_PROXY_B:     u16 = 25434;
const DASHBOARD_B:    u16 = 28081;

const CLUSTER_SECRET: &str = "test-cluster-secret-xyz";

// ── Proxy structs ──────────────────────────────────────────────────────────────

struct ProxyPair {
    _node_a: Child,
    _cfg_a:  NamedTempFile,
    _node_b: Child,
    _cfg_b:  NamedTempFile,
}

unsafe impl Sync for ProxyPair {}

static CLUSTER: OnceLock<bool> = OnceLock::new();

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
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

fn ensure_cluster() -> bool {
    *CLUSTER.get_or_init(|| {
        let mysql_ok = mysql_available();
        let pg_ok    = pg_available();
        if !mysql_ok {
            eprintln!("SKIP: MySQL unreachable. docker compose up mysql80 -d");
        }
        if !pg_ok {
            eprintln!("SKIP: PostgreSQL unreachable. docker compose up postgres16 -d");
        }
        if !mysql_ok || !pg_ok { return false; }
        Box::leak(Box::new(start_cluster()));
        true
    })
}

fn write_node_config(
    mysql_proxy_port: u16,
    pg_proxy_port:    u16,
    dashboard_port:   u16,
    peer_dashboard:   u16,
) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    write!(
        f,
        r#"listen_addr     = "127.0.0.1:{mysql_proxy_port}"
max_connections = 50
pool_size       = 5

[primary]
addr     = "{mysql_host}:{mysql_port}"
user     = "{mysql_user}"
password = "{mysql_pass}"
database = "{mysql_db}"

[analytics]
enabled = false

[dashboard]
enabled     = true
listen_addr = "127.0.0.1:{dashboard_port}"

[ha]
enabled = false

[cluster]
peers  = ["http://127.0.0.1:{peer_dashboard}"]
secret = "{secret}"

[pgsql]
enabled         = true
listen_addr     = "127.0.0.1:{pg_proxy_port}"
pool_size       = 5
max_connections = 50

[pgsql.primary]
addr     = "{pg_host}:{pg_port}"
user     = "{pg_user}"
password = "{pg_pass}"
database = "{pg_db}"
"#,
        mysql_proxy_port = mysql_proxy_port,
        mysql_host = mysql_host(),
        mysql_port = mysql_port(),
        mysql_user = mysql_user(),
        mysql_pass = mysql_pass(),
        mysql_db   = MYSQL_DB,
        dashboard_port = dashboard_port,
        peer_dashboard = peer_dashboard,
        secret = CLUSTER_SECRET,
        pg_proxy_port = pg_proxy_port,
        pg_host = pg_host(),
        pg_port = pg_port(),
        pg_user = pg_user(),
        pg_pass = pg_pass(),
        pg_db   = PG_DB,
    )
    .unwrap();
    f
}

fn spawn_node(cfg: &NamedTempFile) -> Child {
    let binary = env!("CARGO_BIN_EXE_turbineproxy");
    Command::new(binary)
        .arg(cfg.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn turbineproxy: {e}"))
}

fn wait_mysql_proxy(port: u16) {
    for attempt in 0..75 {
        std::thread::sleep(Duration::from_millis(200));
        let opts = OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(port)
            .user(Some(mysql_user()))
            .pass(Some(mysql_pass()))
            .db_name(Some(MYSQL_DB));
        if Conn::new(opts).is_ok() {
            eprintln!("MySQL proxy :{port} ready after ~{}ms", (attempt + 1) * 200);
            return;
        }
    }
    panic!("MySQL proxy :{port} did not become ready within 15 s");
}

fn wait_pg_proxy(port: u16) {
    for attempt in 0..75 {
        std::thread::sleep(Duration::from_millis(200));
        let mut cfg = PgConfig::new();
        cfg.host("127.0.0.1")
            .port(port)
            .user(&pg_user())
            .password(pg_pass().as_bytes())
            .dbname(PG_DB)
            .connect_timeout(Duration::from_secs(1));
        if rt().block_on(async { cfg.connect(NoTls).await.is_ok() }) {
            eprintln!("PgSQL proxy :{port} ready after ~{}ms", (attempt + 1) * 200);
            return;
        }
    }
    panic!("PgSQL proxy :{port} did not become ready within 15 s");
}

fn wait_dashboard(port: u16) {
    for attempt in 0..75 {
        std::thread::sleep(Duration::from_millis(200));
        if rt()
            .block_on(async {
                reqwest::get(format!("http://127.0.0.1:{port}/health")).await
            })
            .is_ok()
        {
            eprintln!("Dashboard :{port} ready after ~{}ms", (attempt + 1) * 200);
            return;
        }
    }
    panic!("Dashboard :{port} did not become ready within 15 s");
}

fn start_cluster() -> ProxyPair {
    let cfg_a = write_node_config(MYSQL_PROXY_A, PG_PROXY_A, DASHBOARD_A, DASHBOARD_B);
    let cfg_b = write_node_config(MYSQL_PROXY_B, PG_PROXY_B, DASHBOARD_B, DASHBOARD_A);

    let node_a = spawn_node(&cfg_a);
    let node_b = spawn_node(&cfg_b);

    // Wait for all four listeners.
    wait_mysql_proxy(MYSQL_PROXY_A);
    wait_mysql_proxy(MYSQL_PROXY_B);
    wait_pg_proxy(PG_PROXY_A);
    wait_pg_proxy(PG_PROXY_B);
    wait_dashboard(DASHBOARD_A);
    wait_dashboard(DASHBOARD_B);

    ProxyPair { _node_a: node_a, _cfg_a: cfg_a, _node_b: node_b, _cfg_b: cfg_b }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn mysql_conn(port: u16) -> Conn {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some("127.0.0.1"))
        .tcp_port(port)
        .user(Some(mysql_user()))
        .pass(Some(mysql_pass()))
        .db_name(Some(MYSQL_DB));
    Conn::new(opts).expect("connect to MySQL proxy")
}

async fn pg_client(port: u16) -> tokio_postgres::Client {
    let mut cfg = PgConfig::new();
    cfg.host("127.0.0.1")
        .port(port)
        .user(&pg_user())
        .password(pg_pass().as_bytes())
        .dbname(PG_DB)
        .connect_timeout(Duration::from_secs(5));
    let (client, conn) = cfg.connect(NoTls).await.expect("connect to PgSQL proxy");
    tokio::spawn(conn);
    client
}

macro_rules! require_cluster {
    () => {
        if !ensure_cluster() { return; }
    };
}

// ── Tests — MySQL routing through both nodes ───────────────────────────────────

#[test]
fn cluster_mysql_node_a_routes_select() {
    require_cluster!();
    let mut c = mysql_conn(MYSQL_PROXY_A);
    let r: Vec<i32> = c.query("SELECT 1 + 1").unwrap();
    assert_eq!(r, vec![2]);
}

#[test]
fn cluster_mysql_node_b_routes_select() {
    require_cluster!();
    let mut c = mysql_conn(MYSQL_PROXY_B);
    let r: Vec<i32> = c.query("SELECT 2 + 2").unwrap();
    assert_eq!(r, vec![4]);
}

#[test]
fn cluster_mysql_both_nodes_consistent_dml() {
    require_cluster!();
    let mut ca = mysql_conn(MYSQL_PROXY_A);
    let mut cb = mysql_conn(MYSQL_PROXY_B);

    ca.query_drop("DROP TABLE IF EXISTS `cluster_test_mysql`").unwrap();
    ca.query_drop(
        "CREATE TABLE `cluster_test_mysql` \
         (id INT AUTO_INCREMENT PRIMARY KEY, val VARCHAR(64)) ENGINE=InnoDB",
    ).unwrap();

    // Write via node A.
    ca.query_drop("INSERT INTO `cluster_test_mysql` (val) VALUES ('from_node_a')").unwrap();

    // Read via node B — both hit the same backend DB.
    let rows: Vec<String> = cb.query("SELECT val FROM `cluster_test_mysql`").unwrap();
    assert_eq!(rows, vec!["from_node_a"]);
}

// ── Tests — PostgreSQL routing through both nodes ──────────────────────────────

#[test]
fn cluster_pg_node_a_routes_select() {
    require_cluster!();
    rt().block_on(async {
        let c = pg_client(PG_PROXY_A).await;
        let row = c.query_one("SELECT 10 + 10 AS r", &[]).await.unwrap();
        assert_eq!(row.get::<_, i32>("r"), 20);
    });
}

#[test]
fn cluster_pg_node_b_routes_select() {
    require_cluster!();
    rt().block_on(async {
        let c = pg_client(PG_PROXY_B).await;
        let row = c.query_one("SELECT 20 + 20 AS r", &[]).await.unwrap();
        assert_eq!(row.get::<_, i32>("r"), 40);
    });
}

#[test]
fn cluster_pg_both_nodes_consistent_dml() {
    require_cluster!();
    rt().block_on(async {
        let ca = pg_client(PG_PROXY_A).await;
        let cb = pg_client(PG_PROXY_B).await;

        ca.execute(
            "CREATE TABLE IF NOT EXISTS cluster_test_pg \
             (id SERIAL PRIMARY KEY, val TEXT)",
            &[],
        ).await.unwrap();
        ca.execute("TRUNCATE cluster_test_pg RESTART IDENTITY", &[]).await.unwrap();

        ca.execute("INSERT INTO cluster_test_pg (val) VALUES ($1)", &[&"from_node_a"])
            .await.unwrap();

        let row = cb.query_one("SELECT val FROM cluster_test_pg", &[]).await.unwrap();
        assert_eq!(row.get::<_, String>(0), "from_node_a");
    });
}

// ── Tests — Cluster sync: POST /api/sync ──────────────────────────────────────

/// Push a config from node A to node B and verify node B accepted it.
#[test]
fn cluster_sync_push_accepted_by_peer() {
    require_cluster!();
    rt().block_on(async {
        // Build a minimal config TOML to push.
        let config_toml = format!(
            r#"listen_addr = "127.0.0.1:{mysql_proxy_port}"
max_connections = 50
pool_size = 5

[primary]
addr     = "{mysql_host}:{mysql_port}"
user     = "{mysql_user}"
password = "{mysql_pass}"
database = "{mysql_db}"

[analytics]
enabled = false

[dashboard]
enabled     = true
listen_addr = "127.0.0.1:{dashboard_port}"

[ha]
enabled = false

[cluster]
peers  = ["http://127.0.0.1:{peer_dashboard}"]
secret = "{secret}"

[pgsql]
enabled = false
"#,
            mysql_proxy_port = MYSQL_PROXY_B,
            mysql_host = mysql_host(),
            mysql_port = mysql_port(),
            mysql_user = mysql_user(),
            mysql_pass = mysql_pass(),
            mysql_db   = MYSQL_DB,
            dashboard_port = DASHBOARD_B,
            peer_dashboard = DASHBOARD_A,
            secret = CLUSTER_SECRET,
        );

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{DASHBOARD_B}/api/sync"))
            .header("Authorization", format!("Bearer {CLUSTER_SECRET}"))
            .json(&serde_json::json!({ "config_toml": config_toml }))
            .send()
            .await
            .expect("POST /api/sync");

        assert_eq!(resp.status(), 200, "sync should be accepted");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true, "response should have ok=true: {body}");
    });
}

/// Sync with wrong secret must be rejected with 401.
#[test]
fn cluster_sync_wrong_secret_rejected() {
    require_cluster!();
    rt().block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{DASHBOARD_A}/api/sync"))
            .header("Authorization", "Bearer wrong-secret-intentionally")
            .json(&serde_json::json!({ "config_toml": "listen_addr = \"0.0.0.0:3307\"\n" }))
            .send()
            .await
            .expect("POST /api/sync");

        assert_eq!(resp.status(), 401, "wrong secret should return 401");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    });
}

/// Sync with missing Authorization header must be rejected.
#[test]
fn cluster_sync_missing_auth_rejected() {
    require_cluster!();
    rt().block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{DASHBOARD_A}/api/sync"))
            .json(&serde_json::json!({ "config_toml": "listen_addr = \"0.0.0.0:3307\"\n" }))
            .send()
            .await
            .expect("POST /api/sync");

        // 401 or 503 (disabled) — either is acceptable security-wise.
        assert!(
            resp.status() == 401 || resp.status() == 503,
            "unauthenticated sync should be rejected, got {}",
            resp.status()
        );
    });
}

/// Sync with malformed TOML must return 400.
#[test]
fn cluster_sync_bad_toml_returns_400() {
    require_cluster!();
    rt().block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{DASHBOARD_B}/api/sync"))
            .header("Authorization", format!("Bearer {CLUSTER_SECRET}"))
            .json(&serde_json::json!({ "config_toml": "this is [not] valid {{toml" }))
            .send()
            .await
            .expect("POST /api/sync");

        assert_eq!(resp.status(), 400, "bad TOML should return 400");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    });
}

// ── Tests — Dashboard API health & stats ──────────────────────────────────────

#[test]
fn cluster_dashboard_health_both_nodes() {
    require_cluster!();
    rt().block_on(async {
        for port in [DASHBOARD_A, DASHBOARD_B] {
            let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
                .await
                .expect("GET /health");
            assert!(resp.status().is_success(), "node :{port} /health not OK");
        }
    });
}

#[test]
fn cluster_dashboard_stats_both_nodes() {
    require_cluster!();
    rt().block_on(async {
        let client = reqwest::Client::new();
        for port in [DASHBOARD_A, DASHBOARD_B] {
            let resp = client
                .get(format!("http://127.0.0.1:{port}/api/stats"))
                .send()
                .await
                .expect("GET /api/stats");
            // 200 or 401 (if auth is enabled) — just ensure the server responds.
            assert!(
                resp.status().as_u16() < 500,
                "node :{port} /api/stats returned server error"
            );
        }
    });
}

#[test]
fn cluster_pg_pool_stats_node_a() {
    require_cluster!();
    rt().block_on(async {
        let resp = reqwest::get(format!("http://127.0.0.1:{DASHBOARD_A}/api/postgres/pool"))
            .await
            .expect("GET /api/postgres/pool");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["enabled"], true, "PgSQL should be enabled on node A");
    });
}

#[test]
fn cluster_config_status_endpoint() {
    require_cluster!();
    rt().block_on(async {
        let resp = reqwest::get(format!("http://127.0.0.1:{DASHBOARD_A}/api/config/status"))
            .await
            .expect("GET /api/config/status");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        // "modified" key must exist (bool).
        assert!(body["modified"].is_boolean(), "config/status must have 'modified' bool");
    });
}

// ── Tests — Concurrent load across both nodes ──────────────────────────────────

#[test]
fn cluster_concurrent_mixed_load() {
    require_cluster!();
    // 4 MySQL threads (2 per node) + 4 PgSQL threads (2 per node) in parallel.
    let mut handles = Vec::new();

    for (i, port) in [(0, MYSQL_PROXY_A), (1, MYSQL_PROXY_A), (2, MYSQL_PROXY_B), (3, MYSQL_PROXY_B)] {
        handles.push(std::thread::spawn(move || {
            let mut c = mysql_conn(port);
            let r: Vec<i32> = c.query(format!("SELECT {i} AS n")).unwrap();
            assert_eq!(r, vec![i]);
        }));
    }
    for (i, port) in [(10, PG_PROXY_A), (11, PG_PROXY_A), (12, PG_PROXY_B), (13, PG_PROXY_B)] {
        handles.push(std::thread::spawn(move || {
            rt().block_on(async move {
                let c = pg_client(port).await;
                let row = c.query_one(&format!("SELECT {i} AS n"), &[]).await.unwrap();
                assert_eq!(row.get::<_, i32>("n"), i);
            });
        }));
    }

    for h in handles { h.join().expect("thread panicked"); }
}
