//! Integration tests for TurbineProxy.
//!
//! Tests start the proxy binary automatically against a real MySQL instance.
//!
//! # Prerequisites
//! A MySQL-compatible server must be reachable. Set env vars to override defaults:
//!
//! | Variable         | Default     | Description                  |
//! |------------------|-------------|------------------------------|
//! | TEST_MYSQL_HOST  | 127.0.0.1   | MySQL host                   |
//! | TEST_MYSQL_PORT  | 3306        | MySQL port                   |
//! | TEST_MYSQL_USER  | root        | MySQL user                   |
//! | TEST_MYSQL_PASS  | root        | MySQL password               |
//!
//! The database `turbineproxy_test` must exist (created by docker-compose).
//!
//! # Running
//! ```
//! # Start MySQL first (one-time):
//! docker compose up mysql80 -d
//!
//! # Run tests:
//! cargo test --test integration_tests -- --test-threads=1
//! ```
//!
//! Tests are **automatically skipped** when MySQL is unreachable — safe to run anywhere.

use mysql::{prelude::*, Conn, OptsBuilder};
use std::{
    env,
    io::Write as _,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};
use tempfile::NamedTempFile;

// ── Configuration ──────────────────────────────────────────────────────────────

fn mysql_host() -> String {
    env::var("TEST_MYSQL_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}
fn mysql_port() -> u16 {
    env::var("TEST_MYSQL_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3306)
}
fn mysql_user() -> String {
    env::var("TEST_MYSQL_USER").unwrap_or_else(|_| "root".to_string())
}
fn mysql_pass() -> String {
    env::var("TEST_MYSQL_PASS").unwrap_or_else(|_| "root".to_string())
}

/// Port the proxy will listen on during tests (chosen to avoid conflicts).
const PROXY_PORT: u16 = 13307;
const TEST_DB: &str = "turbineproxy_test";

// ── Proxy lifecycle ────────────────────────────────────────────────────────────

/// Holds the proxy process and its temp config file alive for the test run.
struct ProxyProcess {
    _child: Child,
    _config: NamedTempFile,
}

// SAFETY: tests run with --test-threads=1; the struct is only created once
// inside OnceLock::get_or_init and never mutated afterwards.
unsafe impl Sync for ProxyProcess {}

static PROXY: OnceLock<bool> = OnceLock::new();

/// Returns `true` if MySQL is reachable.
fn mysql_available() -> bool {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some(mysql_host()))
        .tcp_port(mysql_port())
        .user(Some(mysql_user()))
        .pass(Some(mysql_pass()))
        .db_name(Some(TEST_DB));
    Conn::new(opts).is_ok()
}

/// Ensures the proxy is started exactly once. Returns `false` when MySQL is
/// unavailable (caller should skip the test).
fn ensure_proxy() -> bool {
    *PROXY.get_or_init(|| {
        if !mysql_available() {
            eprintln!(
                "SKIP: MySQL not reachable at {}:{}. \
                 Start one with: docker compose up mysql80 -d",
                mysql_host(),
                mysql_port()
            );
            return false;
        }
        Box::leak(Box::new(start_proxy()));
        true
    })
}

#[allow(clippy::zombie_processes)]
fn start_proxy() -> ProxyProcess {
    let mut config = NamedTempFile::new().expect("create temp config file");
    write!(
        config,
        r#"listen_addr = "127.0.0.1:{proxy_port}"
max_connections = 100
pool_size = 10

[primary]
addr = "{host}:{port}"
user = "{user}"
password = "{pass}"
database = "{db}"

[analytics]
enabled = false

[dashboard]
enabled = false

[ha]
enabled = false
"#,
        proxy_port = PROXY_PORT,
        host = mysql_host(),
        port = mysql_port(),
        user = mysql_user(),
        pass = mysql_pass(),
        db = TEST_DB,
    )
    .expect("write proxy config");

    let binary = env!("CARGO_BIN_EXE_turbineproxy");
    let child = Command::new(binary)
        .arg(config.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to start turbineproxy binary at {binary}: {e}"));

    // Wait until the proxy accepts connections (up to 10 s).
    for attempt in 0..50 {
        std::thread::sleep(Duration::from_millis(200));
        let opts = OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(PROXY_PORT)
            .user(Some(mysql_user()))
            .pass(Some(mysql_pass()))
            .db_name(Some(TEST_DB));
        if Conn::new(opts).is_ok() {
            eprintln!("proxy ready after ~{}ms", (attempt + 1) * 200);
            return ProxyProcess {
                _child: child,
                _config: config,
            };
        }
    }
    panic!("TurbineProxy did not become ready within 10 s");
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn proxy_conn() -> Conn {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some("127.0.0.1"))
        .tcp_port(PROXY_PORT)
        .user(Some(mysql_user()))
        .pass(Some(mysql_pass()))
        .db_name(Some(TEST_DB));
    Conn::new(opts).expect("connect to TurbineProxy")
}

/// Drops and recreates a table for test isolation.
fn reset_table(conn: &mut Conn, name: &str, ddl: &str) {
    conn.query_drop(format!("DROP TABLE IF EXISTS `{name}`"))
        .unwrap();
    conn.query_drop(ddl).unwrap();
}

/// Skip test unless proxy is ready; prints a clear message when skipped.
macro_rules! require_proxy {
    () => {
        if !ensure_proxy() {
            return;
        }
    };
}

// ── Tests — Basic connectivity ─────────────────────────────────────────────────

#[test]
fn test_basic_arithmetic() {
    require_proxy!();
    let mut c = proxy_conn();
    let result: Vec<i32> = c.query("SELECT 1 + 1").unwrap();
    assert_eq!(result, vec![2]);
}

#[test]
fn test_select_version() {
    require_proxy!();
    let mut c = proxy_conn();
    let version: Option<String> = c.query_first("SELECT VERSION()").unwrap();
    let v = version.expect("VERSION() returned NULL");
    eprintln!("Server version: {v}");
    assert!(!v.is_empty());
}

#[test]
fn test_show_databases() {
    require_proxy!();
    let mut c = proxy_conn();
    let dbs: Vec<String> = c.query("SHOW DATABASES").unwrap();
    assert!(
        dbs.iter()
            .any(|d| d.eq_ignore_ascii_case("information_schema")),
        "information_schema not found in: {dbs:?}"
    );
}

#[test]
fn test_select_current_db() {
    require_proxy!();
    let mut c = proxy_conn();
    let db: Option<String> = c.query_first("SELECT DATABASE()").unwrap();
    assert_eq!(db.as_deref(), Some(TEST_DB));
}

// ── Tests — DML ────────────────────────────────────────────────────────────────

#[test]
fn test_insert_select() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_basic",
        "CREATE TABLE `it_basic` (id INT AUTO_INCREMENT PRIMARY KEY, val VARCHAR(64)) ENGINE=InnoDB",
    );
    c.query_drop("INSERT INTO `it_basic` (val) VALUES ('hello'), ('world')")
        .unwrap();
    let rows: Vec<String> = c.query("SELECT val FROM `it_basic` ORDER BY id").unwrap();
    assert_eq!(rows, vec!["hello", "world"]);
}

#[test]
fn test_update() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_update",
        "CREATE TABLE `it_update` (id INT, val INT) ENGINE=InnoDB",
    );
    c.query_drop("INSERT INTO `it_update` VALUES (1, 10), (2, 20)")
        .unwrap();
    c.query_drop("UPDATE `it_update` SET val = val + 1 WHERE id = 1")
        .unwrap();
    let val: Vec<i32> = c.query("SELECT val FROM `it_update` WHERE id = 1").unwrap();
    assert_eq!(val, vec![11]);
}

#[test]
fn test_delete() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_delete",
        "CREATE TABLE `it_delete` (id INT) ENGINE=InnoDB",
    );
    c.query_drop("INSERT INTO `it_delete` VALUES (1), (2), (3)")
        .unwrap();
    c.query_drop("DELETE FROM `it_delete` WHERE id = 2")
        .unwrap();
    let rows: Vec<i32> = c.query("SELECT id FROM `it_delete` ORDER BY id").unwrap();
    assert_eq!(rows, vec![1, 3]);
}

// ── Tests — Transactions ───────────────────────────────────────────────────────

#[test]
fn test_transaction_commit() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_txn_commit",
        "CREATE TABLE `it_txn_commit` (id INT) ENGINE=InnoDB",
    );
    c.query_drop("START TRANSACTION").unwrap();
    c.query_drop("INSERT INTO `it_txn_commit` VALUES (42)")
        .unwrap();
    c.query_drop("COMMIT").unwrap();
    let rows: Vec<i32> = c.query("SELECT id FROM `it_txn_commit`").unwrap();
    assert_eq!(rows, vec![42]);
}

#[test]
fn test_transaction_rollback() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_txn_rollback",
        "CREATE TABLE `it_txn_rollback` (id INT) ENGINE=InnoDB",
    );
    c.query_drop("START TRANSACTION").unwrap();
    c.query_drop("INSERT INTO `it_txn_rollback` VALUES (99)")
        .unwrap();
    c.query_drop("ROLLBACK").unwrap();
    let rows: Vec<i32> = c.query("SELECT id FROM `it_txn_rollback`").unwrap();
    assert!(rows.is_empty(), "ROLLBACK should have removed the row");
}

// ── Tests — Prepared statements ────────────────────────────────────────────────
// NOTE: These may fail until 1.2 (Prepared statements first-class) is implemented.

#[test]
fn test_prepared_insert_select() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_prep",
        "CREATE TABLE `it_prep` (id INT, name VARCHAR(64)) ENGINE=InnoDB",
    );
    c.exec_drop(
        "INSERT INTO `it_prep` (id, name) VALUES (?, ?)",
        (1_i32, "Alice"),
    )
    .unwrap();
    c.exec_drop(
        "INSERT INTO `it_prep` (id, name) VALUES (?, ?)",
        (2_i32, "Bob"),
    )
    .unwrap();
    let names: Vec<String> = c
        .exec("SELECT name FROM `it_prep` WHERE id = ?", (1_i32,))
        .unwrap();
    assert_eq!(names, vec!["Alice"]);
}

#[test]
fn test_prepared_multiple_params() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_prep2",
        "CREATE TABLE `it_prep2` (a INT, b INT, c INT) ENGINE=InnoDB",
    );
    c.exec_drop(
        "INSERT INTO `it_prep2` VALUES (?, ?, ?)",
        (10_i32, 20_i32, 30_i32),
    )
    .unwrap();
    let row: Option<(i32, i32, i32)> = c
        .exec_first("SELECT a, b, c FROM `it_prep2` WHERE a = ?", (10_i32,))
        .unwrap();
    assert_eq!(row, Some((10, 20, 30)));
}

#[test]
fn test_prepared_reuse() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_prep_reuse",
        "CREATE TABLE `it_prep_reuse` (id INT, val VARCHAR(32)) ENGINE=InnoDB",
    );
    // Execute the same prepared statement 5 times — tests stmt ID caching.
    for i in 0..5_i32 {
        c.exec_drop(
            "INSERT INTO `it_prep_reuse` (id, val) VALUES (?, ?)",
            (i, format!("val_{i}")),
        )
        .unwrap();
    }
    let count: Vec<i64> = c.query("SELECT COUNT(*) FROM `it_prep_reuse`").unwrap();
    assert_eq!(count[0], 5);
}

// ── Tests — Charset / encoding ─────────────────────────────────────────────────

#[test]
fn test_utf8mb4_emoji() {
    require_proxy!();
    let mut c = proxy_conn();
    c.query_drop("SET NAMES utf8mb4").unwrap();
    reset_table(
        &mut c,
        "it_unicode",
        "CREATE TABLE `it_unicode` \
         (id INT, val VARCHAR(128) CHARACTER SET utf8mb4) ENGINE=InnoDB",
    );
    let emoji = "Hello 🎉 World 🦀 Rust";
    c.exec_drop("INSERT INTO `it_unicode` (id, val) VALUES (1, ?)", (emoji,))
        .unwrap();
    let result: Option<String> = c
        .exec_first("SELECT val FROM `it_unicode` WHERE id = 1", ())
        .unwrap();
    assert_eq!(result.as_deref(), Some(emoji), "emoji round-trip failed");
}

#[test]
fn test_utf8mb4_cjk() {
    require_proxy!();
    let mut c = proxy_conn();
    c.query_drop("SET NAMES utf8mb4").unwrap();
    reset_table(
        &mut c,
        "it_cjk",
        "CREATE TABLE `it_cjk` \
         (id INT, val VARCHAR(128) CHARACTER SET utf8mb4) ENGINE=InnoDB",
    );
    let cjk = "日本語テスト 中文测试 한국어";
    c.exec_drop("INSERT INTO `it_cjk` (id, val) VALUES (1, ?)", (cjk,))
        .unwrap();
    let result: Option<String> = c
        .exec_first("SELECT val FROM `it_cjk` WHERE id = 1", ())
        .unwrap();
    assert_eq!(result.as_deref(), Some(cjk));
}

// ── Tests — NULL handling ──────────────────────────────────────────────────────

#[test]
fn test_null_column() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_null",
        "CREATE TABLE `it_null` (id INT, val VARCHAR(64)) ENGINE=InnoDB",
    );
    c.query_drop("INSERT INTO `it_null` (id, val) VALUES (1, NULL), (2, 'present')")
        .unwrap();
    // Verify NULL row exists and non-NULL row has correct value.
    let null_count: Vec<i64> = c
        .query("SELECT COUNT(*) FROM `it_null` WHERE val IS NULL")
        .unwrap();
    assert_eq!(null_count[0], 1);
    let nonnull: Vec<String> = c
        .query("SELECT val FROM `it_null` WHERE val IS NOT NULL")
        .unwrap();
    assert_eq!(nonnull, vec!["present"]);
}

// ── Tests — Edge cases ─────────────────────────────────────────────────────────

#[test]
fn test_large_result_set() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_large",
        "CREATE TABLE `it_large` \
         (id INT AUTO_INCREMENT PRIMARY KEY, pad CHAR(200)) ENGINE=InnoDB",
    );
    let pad = "x".repeat(200);
    // 10 batches × 100 rows = 1 000 rows
    for _ in 0..10 {
        let vals: String = (0..100)
            .map(|_| format!("('{pad}')"))
            .collect::<Vec<_>>()
            .join(",");
        c.query_drop(format!("INSERT INTO `it_large` (pad) VALUES {vals}"))
            .unwrap();
    }
    let count: Vec<i64> = c.query("SELECT COUNT(*) FROM `it_large`").unwrap();
    assert_eq!(count[0], 1_000);
}

#[test]
fn test_connection_reuse_many_queries() {
    require_proxy!();
    let mut c = proxy_conn();
    // 50 round-trips on the same connection — exercises framing stability.
    for i in 0..50_i32 {
        let result: Vec<i32> = c.query(format!("SELECT {i}")).unwrap();
        assert_eq!(result, vec![i], "round-trip {i} failed");
    }
}

#[test]
fn test_empty_result_set() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_empty",
        "CREATE TABLE `it_empty` (id INT) ENGINE=InnoDB",
    );
    let rows: Vec<i32> = c.query("SELECT id FROM `it_empty`").unwrap();
    assert!(rows.is_empty());
}

#[test]
fn test_show_tables() {
    require_proxy!();
    let mut c = proxy_conn();
    // Just verifies SHOW TABLES doesn't crash.
    let _tables: Vec<String> = c.query("SHOW TABLES").unwrap();
}

#[test]
fn test_show_create_table() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_showcreate",
        "CREATE TABLE `it_showcreate` (id INT PRIMARY KEY, name VARCHAR(64)) ENGINE=InnoDB",
    );
    // SHOW CREATE TABLE returns 2 columns; verify it doesn't error.
    c.query_drop("SHOW CREATE TABLE `it_showcreate`").unwrap();
}

#[test]
fn test_last_insert_id() {
    require_proxy!();
    let mut c = proxy_conn();
    reset_table(
        &mut c,
        "it_lastid",
        "CREATE TABLE `it_lastid` (id INT AUTO_INCREMENT PRIMARY KEY, v INT) ENGINE=InnoDB",
    );
    c.query_drop("INSERT INTO `it_lastid` (v) VALUES (100)")
        .unwrap();
    let id: Vec<u64> = c.query("SELECT LAST_INSERT_ID()").unwrap();
    assert_eq!(id[0], 1, "LAST_INSERT_ID() should be 1 for first insert");
}

#[test]
fn test_multiple_connections_concurrent() {
    require_proxy!();
    // Open 5 connections simultaneously and query each.
    let mut handles = Vec::new();
    for i in 0..5_i32 {
        let h = std::thread::spawn(move || {
            let mut c = proxy_conn();
            let result: Vec<i32> = c.query(format!("SELECT {i}")).unwrap();
            assert_eq!(result, vec![i]);
        });
        handles.push(h);
    }
    for h in handles {
        h.join().expect("thread panicked");
    }
}
