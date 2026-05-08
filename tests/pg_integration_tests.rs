//! PostgreSQL integration tests for TurbineProxy.
//!
//! Tests connect through a running TurbineProxy PgSQL proxy against a real
//! PostgreSQL instance.  A second PgSQL replica is used for read-splitting tests.
//!
//! # Prerequisites
//!
//! | Variable          | Default     | Description                              |
//! |-------------------|-------------|------------------------------------------|
//! | TEST_PG_HOST      | 127.0.0.1   | PostgreSQL primary host                  |
//! | TEST_PG_PORT      | 5432        | PostgreSQL primary port                  |
//! | TEST_PG_REPLICA_PORT | 5433     | PostgreSQL replica port (optional)       |
//! | TEST_PG_USER      | postgres    | PostgreSQL user                          |
//! | TEST_PG_PASS      | postgres    | PostgreSQL password                      |
//!
//! # Running
//! ```bash
//! # Start PostgreSQL (primary + optional replica):
//! docker compose up postgres16 -d
//! docker compose up postgres16-replica -d   # optional, for replica tests
//!
//! # Run tests:
//! cargo test --test pg_integration_tests -- --test-threads=1
//! ```
//!
//! Tests are **automatically skipped** when PostgreSQL is unreachable.

use std::{
    env,
    io::Write as _,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio_postgres::{Client, Config, NoTls};

// ── Configuration ──────────────────────────────────────────────────────────────

fn pg_host() -> String {
    env::var("TEST_PG_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}
fn pg_port() -> u16 {
    env::var("TEST_PG_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5432)
}
fn pg_replica_port() -> Option<u16> {
    env::var("TEST_PG_REPLICA_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
}
fn pg_user() -> String {
    env::var("TEST_PG_USER").unwrap_or_else(|_| "postgres".to_string())
}
fn pg_pass() -> String {
    env::var("TEST_PG_PASS").unwrap_or_else(|_| "postgres".to_string())
}

/// Port the PgSQL proxy will listen on during tests.
const PROXY_PORT: u16 = 15433;
const TEST_DB: &str = "turbineproxy_test";

// ── Proxy lifecycle ────────────────────────────────────────────────────────────

struct ProxyProcess {
    _child: Child,
    _config: NamedTempFile,
}

unsafe impl Sync for ProxyProcess {}

static PROXY: OnceLock<bool> = OnceLock::new();

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Connect directly to PostgreSQL (bypassing the proxy) to verify availability.
fn pg_available() -> bool {
    let mut cfg = Config::new();
    cfg.host(pg_host())
        .port(pg_port())
        .user(pg_user())
        .password(pg_pass().as_bytes())
        .dbname(TEST_DB)
        .connect_timeout(Duration::from_secs(3));
    rt().block_on(async { cfg.connect(NoTls).await.is_ok() })
}

fn ensure_proxy() -> bool {
    *PROXY.get_or_init(|| {
        if !pg_available() {
            eprintln!(
                "SKIP: PostgreSQL not reachable at {}:{}. \
                 Start one with: docker compose up postgres16 -d",
                pg_host(),
                pg_port()
            );
            return false;
        }
        Box::leak(Box::new(start_proxy()));
        true
    })
}

#[allow(clippy::zombie_processes)]
fn start_proxy() -> ProxyProcess {
    let replica_section = if let Some(rport) = pg_replica_port() {
        format!(
            r#"
[[pgsql.replicas]]
addr = "{host}:{rport}"
user = "{user}"
password = "{pass}"
database = "{db}"
"#,
            host = pg_host(),
            rport = rport,
            user = pg_user(),
            pass = pg_pass(),
            db = TEST_DB,
        )
    } else {
        String::new()
    };

    let mut config = NamedTempFile::new().expect("create temp config file");
    write!(
        config,
        r#"listen_addr = "127.0.0.1:13307"
max_connections = 10
pool_size = 5

[primary]
addr = "{mysql_host}:3306"
user = "root"
password = "root"
database = "turbineproxy_test"

[analytics]
enabled = false

[dashboard]
enabled = false

[ha]
enabled = false

[pgsql]
enabled = true
listen_addr = "127.0.0.1:{proxy_port}"
pool_size = 5
max_connections = 100
slow_query_log_ms = 0

[pgsql.primary]
addr = "{host}:{port}"
user = "{user}"
password = "{pass}"
database = "{db}"
{replica_section}
"#,
        mysql_host = pg_host(),
        proxy_port = PROXY_PORT,
        host = pg_host(),
        port = pg_port(),
        user = pg_user(),
        pass = pg_pass(),
        db = TEST_DB,
        replica_section = replica_section,
    )
    .expect("write proxy config");

    let binary = env!("CARGO_BIN_EXE_turbineproxy");
    let child = Command::new(binary)
        .arg(config.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to start turbineproxy: {e}"));

    // Wait until the proxy accepts PgSQL connections (up to 15 s).
    for attempt in 0..75 {
        std::thread::sleep(Duration::from_millis(200));
        let mut cfg = Config::new();
        cfg.host("127.0.0.1")
            .port(PROXY_PORT)
            .user(pg_user())
            .password(pg_pass().as_bytes())
            .dbname(TEST_DB)
            .connect_timeout(Duration::from_secs(1));
        if rt().block_on(async { cfg.connect(NoTls).await.is_ok() }) {
            eprintln!("pg proxy ready after ~{}ms", (attempt + 1) * 200);
            return ProxyProcess {
                _child: child,
                _config: config,
            };
        }
    }
    panic!("TurbineProxy PgSQL did not become ready within 15 s");
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Open a tokio-postgres client through the proxy.
async fn proxy_client() -> Client {
    let mut cfg = Config::new();
    cfg.host("127.0.0.1")
        .port(PROXY_PORT)
        .user(pg_user())
        .password(pg_pass().as_bytes())
        .dbname(TEST_DB)
        .connect_timeout(Duration::from_secs(5));
    let (client, conn) = cfg
        .connect(NoTls)
        .await
        .expect("connect to TurbineProxy PgSQL");
    tokio::spawn(conn);
    client
}

/// Open a direct tokio-postgres client (bypasses proxy — used to verify replica state).
#[allow(dead_code)]
async fn direct_client(port: u16) -> Client {
    let mut cfg = Config::new();
    cfg.host(pg_host())
        .port(port)
        .user(pg_user())
        .password(pg_pass().as_bytes())
        .dbname(TEST_DB)
        .connect_timeout(Duration::from_secs(5));
    let (client, conn) = cfg.connect(NoTls).await.expect("direct pg connect");
    tokio::spawn(conn);
    client
}

macro_rules! require_proxy {
    () => {
        if !ensure_proxy() {
            return;
        }
    };
}

// ── Tests — Basic connectivity ─────────────────────────────────────────────────

#[test]
fn pg_test_arithmetic() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        let row = c.query_one("SELECT 1 + 1 AS r", &[]).await.unwrap();
        let r: i32 = row.get("r");
        assert_eq!(r, 2);
    });
}

#[test]
fn pg_test_select_version() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        let row = c.query_one("SELECT version()", &[]).await.unwrap();
        let v: String = row.get(0);
        assert!(v.contains("PostgreSQL"), "unexpected version: {v}");
    });
}

#[test]
fn pg_test_current_database() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        let row = c.query_one("SELECT current_database()", &[]).await.unwrap();
        let db: String = row.get(0);
        assert_eq!(db, TEST_DB);
    });
}

#[test]
fn pg_test_current_user() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        let row = c.query_one("SELECT current_user", &[]).await.unwrap();
        let u: String = row.get(0);
        assert_eq!(u, pg_user());
    });
}

// ── Tests — DML ────────────────────────────────────────────────────────────────

#[test]
fn pg_test_insert_select() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_basic RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute(
            "INSERT INTO it_basic (val) VALUES ($1), ($2)",
            &[&"hello", &"world"],
        )
        .await
        .unwrap();
        let rows = c
            .query("SELECT val FROM it_basic ORDER BY id", &[])
            .await
            .unwrap();
        let vals: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        assert_eq!(vals, vec!["hello", "world"]);
    });
}

#[test]
fn pg_test_update() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_txn RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute("INSERT INTO it_txn VALUES (1, 10)", &[])
            .await
            .unwrap();
        c.execute("UPDATE it_txn SET val = val + 1 WHERE id = 1", &[])
            .await
            .unwrap();
        let row = c
            .query_one("SELECT val FROM it_txn WHERE id = 1", &[])
            .await
            .unwrap();
        let v: i32 = row.get(0);
        assert_eq!(v, 11);
    });
}

#[test]
fn pg_test_delete() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_txn RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute("INSERT INTO it_txn VALUES (1, 1), (2, 2), (3, 3)", &[])
            .await
            .unwrap();
        c.execute("DELETE FROM it_txn WHERE id = 2", &[])
            .await
            .unwrap();
        let rows = c
            .query("SELECT id FROM it_txn ORDER BY id", &[])
            .await
            .unwrap();
        let ids: Vec<i32> = rows.iter().map(|r| r.get(0)).collect();
        assert_eq!(ids, vec![1, 3]);
    });
}

#[test]
fn pg_test_null_handling() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_basic RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute(
            "INSERT INTO it_basic (val) VALUES (NULL), ($1)",
            &[&"present"],
        )
        .await
        .unwrap();
        let null_count: i64 = c
            .query_one("SELECT COUNT(*) FROM it_basic WHERE val IS NULL", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(null_count, 1);
        let nonnull: Vec<Option<String>> = c
            .query("SELECT val FROM it_basic WHERE val IS NOT NULL", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(nonnull, vec![Some("present".to_string())]);
    });
}

// ── Tests — Type coverage ──────────────────────────────────────────────────────

#[test]
fn pg_test_types_roundtrip() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_types RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute(
            "INSERT INTO it_types (i_col, f_col, t_col, b_col) VALUES ($1, $2, $3, $4)",
            &[&42_i32, &std::f64::consts::PI, &"hello 🦀", &true],
        )
        .await
        .unwrap();
        let row = c
            .query_one("SELECT i_col, f_col, t_col, b_col FROM it_types", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<_, i32>(0), 42);
        let f: f64 = row.get(1);
        assert!((f - std::f64::consts::PI).abs() < 1e-9);
        assert_eq!(row.get::<_, String>(2), "hello 🦀");
        assert!(row.get::<_, bool>(3));
    });
}

#[test]
fn pg_test_unicode() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_basic RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        let s = "日本語テスト 🎉 中文测试 한국어";
        c.execute("INSERT INTO it_basic (val) VALUES ($1)", &[&s])
            .await
            .unwrap();
        let row = c.query_one("SELECT val FROM it_basic", &[]).await.unwrap();
        assert_eq!(row.get::<_, String>(0), s);
    });
}

// ── Tests — Transactions ───────────────────────────────────────────────────────

#[test]
fn pg_test_transaction_commit() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_txn RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute("BEGIN", &[]).await.unwrap();
        c.execute("INSERT INTO it_txn VALUES (42, 999)", &[])
            .await
            .unwrap();
        c.execute("COMMIT", &[]).await.unwrap();
        let row = c
            .query_one("SELECT val FROM it_txn WHERE id = 42", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<_, i32>(0), 999);
    });
}

#[test]
fn pg_test_transaction_rollback() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_txn RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute("BEGIN", &[]).await.unwrap();
        c.execute("INSERT INTO it_txn VALUES (99, 0)", &[])
            .await
            .unwrap();
        c.execute("ROLLBACK", &[]).await.unwrap();
        let count: i64 = c
            .query_one("SELECT COUNT(*) FROM it_txn WHERE id = 99", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 0, "ROLLBACK must not persist the row");
    });
}

#[test]
fn pg_test_transaction_savepoint() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_txn RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute("BEGIN", &[]).await.unwrap();
        c.execute("INSERT INTO it_txn VALUES (1, 10)", &[])
            .await
            .unwrap();
        c.execute("SAVEPOINT sp1", &[]).await.unwrap();
        c.execute("INSERT INTO it_txn VALUES (2, 20)", &[])
            .await
            .unwrap();
        c.execute("ROLLBACK TO SAVEPOINT sp1", &[]).await.unwrap();
        c.execute("COMMIT", &[]).await.unwrap();
        let count: i64 = c
            .query_one("SELECT COUNT(*) FROM it_txn", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 1, "only row before savepoint should survive");
    });
}

// ── Tests — Prepared statements ────────────────────────────────────────────────

#[test]
fn pg_test_prepared_statement() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_basic RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        let stmt = c
            .prepare("INSERT INTO it_basic (val) VALUES ($1)")
            .await
            .unwrap();
        for i in 0..5 {
            c.execute(&stmt, &[&format!("item_{i}")]).await.unwrap();
        }
        let count: i64 = c
            .query_one("SELECT COUNT(*) FROM it_basic", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 5);
    });
}

#[test]
fn pg_test_prepared_parameterized_select() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_txn RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        c.execute("INSERT INTO it_txn VALUES (1, 100), (2, 200)", &[])
            .await
            .unwrap();
        let stmt = c
            .prepare("SELECT val FROM it_txn WHERE id = $1")
            .await
            .unwrap();
        let row = c.query_one(&stmt, &[&2_i32]).await.unwrap();
        assert_eq!(row.get::<_, i32>(0), 200);
    });
}

// ── Tests — Large result set ───────────────────────────────────────────────────

#[test]
fn pg_test_large_result_set() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("TRUNCATE it_large RESTART IDENTITY CASCADE", &[])
            .await
            .unwrap();
        // 100 rows with 200-char padding
        let pad = "x".repeat(200);
        for _ in 0..100 {
            c.execute("INSERT INTO it_large (pad) VALUES ($1)", &[&pad])
                .await
                .unwrap();
        }
        let count: i64 = c
            .query_one("SELECT COUNT(*) FROM it_large", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 100);
    });
}

// ── Tests — Many round-trips (framing stability) ───────────────────────────────

#[test]
fn pg_test_many_round_trips() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        for i in 0_i32..50 {
            let row = c.query_one(&format!("SELECT {i} AS n"), &[]).await.unwrap();
            assert_eq!(row.get::<_, i32>("n"), i);
        }
    });
}

// ── Tests — Concurrent connections ────────────────────────────────────────────

#[test]
fn pg_test_concurrent_connections() {
    require_proxy!();
    let handles: Vec<_> = (0_i32..5)
        .map(|i| {
            std::thread::spawn(move || {
                rt().block_on(async move {
                    let c = proxy_client().await;
                    let row = c.query_one(&format!("SELECT {i} AS n"), &[]).await.unwrap();
                    assert_eq!(row.get::<_, i32>("n"), i);
                });
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panicked");
    }
}

// ── Tests — Session / search_path ─────────────────────────────────────────────

#[test]
fn pg_test_set_search_path() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("SET search_path TO public", &[]).await.unwrap();
        let row = c.query_one("SHOW search_path", &[]).await.unwrap();
        let sp: String = row.get(0);
        assert!(
            sp.contains("public"),
            "search_path should contain public: {sp}"
        );
    });
}

#[test]
fn pg_test_set_application_name() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        c.execute("SET application_name TO 'turbineproxy_test'", &[])
            .await
            .unwrap();
        let row = c.query_one("SHOW application_name", &[]).await.unwrap();
        let name: String = row.get(0);
        assert_eq!(name, "turbineproxy_test");
    });
}

// ── Tests — Meta-command equivalents (SQL translations) ────────────────────────

#[test]
fn pg_test_list_tables_via_proxy_sql() {
    require_proxy!();
    // Equivalent to psql \dt — proxy translates to information_schema query
    rt().block_on(async {
        let c = proxy_client().await;
        let rows = c
            .query(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' ORDER BY table_name",
                &[],
            )
            .await
            .unwrap();
        let tables: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        assert!(
            tables.contains(&"it_basic".to_string()),
            "it_basic should exist: {tables:?}"
        );
    });
}

#[test]
fn pg_test_list_schemas() {
    require_proxy!();
    rt().block_on(async {
        let c = proxy_client().await;
        let rows = c
            .query(
                "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
                &[],
            )
            .await
            .unwrap();
        let schemas: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        assert!(
            schemas.contains(&"public".to_string()),
            "public schema missing"
        );
        assert!(schemas.contains(&"information_schema".to_string()));
    });
}

// ── Tests — Read-from-replica (requires postgres16-replica) ────────────────────

#[test]
fn pg_test_replica_is_in_recovery() {
    // Only run when a replica port is configured.
    let Some(rport) = pg_replica_port() else {
        return;
    };
    rt().block_on(async {
        let mut cfg = Config::new();
        cfg.host(pg_host())
            .port(rport)
            .user(pg_user())
            .password(pg_pass().as_bytes())
            .dbname("postgres")
            .connect_timeout(Duration::from_secs(5));
        let (c, conn) = cfg
            .connect(NoTls)
            .await
            .expect("direct pg connect (postgres db)");
        tokio::spawn(conn);
        let row = c
            .query_one("SELECT pg_is_in_recovery()", &[])
            .await
            .unwrap();
        let in_recovery: bool = row.get(0);
        assert!(
            in_recovery,
            "replica should report pg_is_in_recovery = true"
        );
    });
}

#[test]
fn pg_test_primary_not_in_recovery() {
    rt().block_on(async {
        // Connect to the always-present "postgres" db so this test works on any
        // backend regardless of whether turbineproxy_test has been created yet.
        let mut cfg = Config::new();
        cfg.host(pg_host())
            .port(pg_port())
            .user(pg_user())
            .password(pg_pass().as_bytes())
            .dbname("postgres")
            .connect_timeout(Duration::from_secs(5));
        let (c, conn) = cfg
            .connect(NoTls)
            .await
            .expect("direct pg connect (postgres db)");
        tokio::spawn(conn);
        let row = c
            .query_one("SELECT pg_is_in_recovery()", &[])
            .await
            .unwrap();
        let in_recovery: bool = row.get(0);
        assert!(
            !in_recovery,
            "primary should report pg_is_in_recovery = false"
        );
    });
}
