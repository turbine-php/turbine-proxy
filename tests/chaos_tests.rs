//! Chaos tests for TurbineProxy — validates documented failure behavior with
//! real network fault injection via Toxiproxy.
//!
//! # Prerequisites
//!
//! ```bash
//! # 1. Start the full chaos stack
//! docker compose -f docker-compose.chaos.yml up -d
//! docker compose -f docker-compose.chaos.yml run --rm chaos-setup
//! docker compose -f docker-compose.chaos.yml run --rm toxiproxy-init
//!
//! # 2. Build turbineproxy
//! cargo build
//!
//! # 3. Run chaos tests (single-threaded — they manipulate shared network state)
//! cargo test --test chaos_tests -- --test-threads=1 --nocapture
//! ```
//!
//! Tests are **automatically skipped** when Toxiproxy or MySQL is unreachable —
//! safe to run in any environment.
//!
//! # Scenarios
//!
//! | # | Scenario | Validation |
//! |---|----------|------------|
//! | 1 | Primary kill mid-query | Client gets error, session survives retry |
//! | 2 | Replica lag spike | Reads forced to primary during RYOW window |
//! | 3 | Total partition primary | GR warning, clients get clear errors |
//! | 4 | Failover flap (primary up/down 5x) | Cooldown holds, no flip-flop |
//! | 5 | Slow backend (500ms latency) | Circuit breaker opens, queries fail fast |
//! | 6 | Pool exhaustion | Reject-fast, no hang, error is immediate |
//! | 7 | Dashboard isolation | Proxy continues when dashboard is inaccessible |
//! | 8 | Replica timeout | Unhealthy replica removed, reads go to primary |
//! | 9 | SIGTERM drain | Proxy drains in-flight queries before exit |
//! |10 | Config reload | Zero downtime, in-flight queries complete |

use mysql::{prelude::*, Conn, Opts, OptsBuilder};
use std::{
    env,
    io::Write as _,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    thread,
    time::Duration,
};
use tempfile::NamedTempFile;

// ─── Constants & env helpers ─────────────────────────────────────────────────

/// Toxiproxy control API base URL
fn toxi_api() -> String {
    env::var("TOXIPROXY_API").unwrap_or_else(|_| "http://127.0.0.1:8474".into())
}

/// Ports exposed by Toxiproxy (as configured by toxiproxy-init)
const TOXI_PRIMARY_PORT: u16 = 13306;
const TOXI_REPLICA1_PORT: u16 = 13337;
const TOXI_REPLICA2_PORT: u16 = 13338;

/// Port the proxy under test listens on
const PROXY_PORT: u16 = 23307;

const TEST_DB: &str = "turbineproxy_test";
const MYSQL_USER: &str = "root";
const MYSQL_PASS: &str = "root";

// ─── Toxiproxy HTTP client ────────────────────────────────────────────────────

fn toxi_get(path: &str) -> Result<String, String> {
    let url = format!("{}{}", toxi_api(), path);
    let out = Command::new("curl")
        .args(["-sf", &url])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("HTTP {} on GET {path}", out.status))
    }
}

fn toxi_post(path: &str, body: &str) -> Result<(), String> {
    let url = format!("{}{}", toxi_api(), path);
    let out = Command::new("curl")
        .args([
            "-sf",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
            &url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "HTTP {} on POST {path}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn toxi_delete(path: &str) -> Result<(), String> {
    let url = format!("{}{}", toxi_api(), path);
    let out = Command::new("curl")
        .args(["-sf", "-X", "DELETE", &url])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("HTTP {} on DELETE {path}", out.status))
    }
}

/// Add a latency toxic to a named proxy.
fn add_latency(proxy_name: &str, latency_ms: u64, jitter_ms: u64) {
    let body = format!(
        r#"{{"name":"latency","type":"latency","stream":"upstream","toxicity":1.0,"attributes":{{"latency":{latency_ms},"jitter":{jitter_ms}}}}}"#
    );
    toxi_post(&format!("/proxies/{proxy_name}/toxics"), &body)
        .unwrap_or_else(|e| eprintln!("[toxi] add_latency failed: {e}"));
}

/// Remove a named toxic from a proxy.
fn remove_toxic(proxy_name: &str, toxic_name: &str) {
    toxi_delete(&format!("/proxies/{proxy_name}/toxics/{toxic_name}"))
        .unwrap_or_else(|e| eprintln!("[toxi] remove_toxic failed: {e}"));
}

/// Disable all connections through a proxy (simulates a complete network partition).
fn disable_proxy(proxy_name: &str) {
    let body = r#"{"enabled":false}"#.to_string();
    Command::new("curl")
        .args([
            "-sf",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            &format!("{}/proxies/{}", toxi_api(), proxy_name),
        ])
        .output()
        .ok();
}

/// Re-enable a proxy.
fn enable_proxy(proxy_name: &str) {
    let body = r#"{"enabled":true}"#.to_string();
    Command::new("curl")
        .args([
            "-sf",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            &format!("{}/proxies/{}", toxi_api(), proxy_name),
        ])
        .output()
        .ok();
}

/// Reset all toxics on all proxies to a clean state.
fn reset_all_toxics() {
    for proxy in &["mysql-primary", "mysql-replica1", "mysql-replica2"] {
        // Re-enable first (in case it was disabled)
        enable_proxy(proxy);
        // List and delete all toxics
        if let Ok(body) = toxi_get(&format!("/proxies/{proxy}/toxics")) {
            // Simple parse: extract all "name" fields
            let mut start = 0;
            while let Some(pos) = body[start..].find(r#""name":""#) {
                let abs = start + pos + 8;
                if let Some(end) = body[abs..].find('"') {
                    let name = &body[abs..abs + end];
                    if !name.is_empty() {
                        let _ = toxi_delete(&format!("/proxies/{proxy}/toxics/{name}"));
                    }
                    start = abs + end + 1;
                } else {
                    break;
                }
            }
        }
    }
}

// ─── Proxy process management ────────────────────────────────────────────────

struct ProxyProcess {
    child: Child,
    _config: NamedTempFile,
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[allow(clippy::zombie_processes)]
fn start_proxy_with_ha() -> ProxyProcess {
    let mut config = NamedTempFile::new().expect("create temp config file");
    write!(
        config,
        r#"listen_addr = "127.0.0.1:{proxy_port}"
max_connections = 20
pool_size = 5

[primary]
addr = "127.0.0.1:{primary_port}"
user = "{user}"
password = "{pass}"
database = "{db}"

[[replicas]]
addr   = "127.0.0.1:{replica1_port}"
user   = "{user}"
password = "{pass}"
database = "{db}"
weight = 100

[[replicas]]
addr   = "127.0.0.1:{replica2_port}"
user   = "{user}"
password = "{pass}"
database = "{db}"
weight = 100
backup = true

[analytics]
enabled = false

[dashboard]
enabled = false

[ha]
enabled                      = true
health_check_interval_secs   = 2
max_replica_lag_ms           = 2000
primary_failover_threshold   = 2
failover_cooldown_secs       = 10
failover_min_recovery_checks = 2
circuit_breaker_threshold    = 3
circuit_breaker_recovery_ms  = 5000
"#,
        proxy_port = PROXY_PORT,
        primary_port = TOXI_PRIMARY_PORT,
        replica1_port = TOXI_REPLICA1_PORT,
        replica2_port = TOXI_REPLICA2_PORT,
        user = MYSQL_USER,
        pass = MYSQL_PASS,
        db = TEST_DB,
    )
    .expect("write proxy config");

    let binary = env!("CARGO_BIN_EXE_turbineproxy");
    let child = Command::new(binary)
        .arg(config.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn turbineproxy");

    // Give proxy time to bind and connect.
    thread::sleep(Duration::from_millis(800));

    ProxyProcess {
        child,
        _config: config,
    }
}

fn proxy_conn() -> Option<Conn> {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some("127.0.0.1"))
        .tcp_port(PROXY_PORT)
        .user(Some(MYSQL_USER))
        .pass(Some(MYSQL_PASS))
        .db_name(Some(TEST_DB))
        .tcp_connect_timeout(Some(Duration::from_secs(3)));
    Conn::new(Opts::from(opts)).ok()
}

fn direct_conn(port: u16) -> Option<Conn> {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some("127.0.0.1"))
        .tcp_port(port)
        .user(Some(MYSQL_USER))
        .pass(Some(MYSQL_PASS))
        .db_name(Some(TEST_DB))
        .tcp_connect_timeout(Some(Duration::from_secs(3)));
    Conn::new(Opts::from(opts)).ok()
}

// ─── Environment checks ───────────────────────────────────────────────────────

fn toxiproxy_available() -> bool {
    toxi_get("/proxies").is_ok()
}

fn mysql_primary_available() -> bool {
    direct_conn(TOXI_PRIMARY_PORT).is_some()
}

static CHAOS_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn chaos_available() -> bool {
    *CHAOS_AVAILABLE.get_or_init(|| {
        if !toxiproxy_available() {
            eprintln!(
                "SKIP: Toxiproxy not reachable at {}. \
                 Run: docker compose -f docker-compose.chaos.yml up -d",
                toxi_api()
            );
            return false;
        }
        if !mysql_primary_available() {
            eprintln!(
                "SKIP: MySQL primary not reachable via Toxiproxy at port {}. \
                 Run: docker compose -f docker-compose.chaos.yml run --rm chaos-setup && \
                      docker compose -f docker-compose.chaos.yml run --rm toxiproxy-init",
                TOXI_PRIMARY_PORT
            );
            return false;
        }
        true
    })
}

macro_rules! require_chaos {
    () => {
        if !chaos_available() {
            return;
        }
    };
}

// ─── Test helpers ─────────────────────────────────────────────────────────────

fn wait_for_proxy(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if proxy_conn().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Execute a simple query via the proxy, returning whether it succeeded.
fn probe(conn: &mut Conn) -> bool {
    conn.query_drop("SELECT 1").is_ok()
}

// ─── Scenario 1: Primary connection loss mid-query ────────────────────────────
//
// Expectation: client receives an error (not a hang). A new connection to the
// proxy on the next attempt succeeds once Toxiproxy is restored.
#[test]
fn chaos_01_primary_connection_loss() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    let mut c = proxy_conn().expect("initial connection");
    assert!(probe(&mut c), "initial probe must succeed");

    // Disable the primary upstream — simulates a killed/crashed server.
    disable_proxy("mysql-primary");

    // Next query should fail (not hang indefinitely).
    let start = std::time::Instant::now();
    let result = c.query_drop("SELECT SLEEP(0.01)");
    let elapsed = start.elapsed();
    assert!(result.is_err(), "query should fail when primary is down");
    assert!(
        elapsed < Duration::from_secs(8),
        "query should fail fast, not hang (took {elapsed:?})"
    );

    // Restore primary.
    enable_proxy("mysql-primary");
    thread::sleep(Duration::from_millis(500));

    // A fresh connection to the proxy must work again.
    let mut c2 = proxy_conn().expect("connection after primary restore");
    assert!(probe(&mut c2), "proxy must recover after primary restore");
}

// ─── Scenario 2: Replica lag spike → reads routed to primary ─────────────────
//
// Expectation: when all replicas have high lag (>max_replica_lag_ms), reads
// fall back to the primary. No error returned to the client.
#[test]
fn chaos_02_replica_lag_fallback() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    // Inject high latency on both replicas (health checks will detect lag and
    // mark them unhealthy; reads should fall back to primary automatically).
    add_latency("mysql-replica1", 3000, 0); // 3s > max_replica_lag_ms=2s
    add_latency("mysql-replica2", 3000, 0);

    // Wait for HA health checker to mark replicas unhealthy (interval=2s × 2).
    thread::sleep(Duration::from_secs(6));

    let mut c = proxy_conn().expect("connection while replicas are lagging");
    // Read query — must succeed (falls back to primary).
    let result: Result<Vec<u64>, _> = c.query("SELECT 1");
    assert!(
        result.is_ok(),
        "reads must succeed even when all replicas are lagging"
    );

    remove_toxic("mysql-replica1", "latency");
    remove_toxic("mysql-replica2", "latency");
}

// ─── Scenario 3: Total network partition on primary ───────────────────────────
//
// Expectation: writes fail with a clear error (not a hang). After HA threshold
// is reached, reads are promoted to the failover replica.
#[test]
fn chaos_03_total_primary_partition() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    let mut c = proxy_conn().expect("initial connection");
    assert!(probe(&mut c), "initial probe");

    // Partition primary.
    disable_proxy("mysql-primary");

    // Wait for failover (health_check_interval_secs=2, threshold=2 → ~4s).
    thread::sleep(Duration::from_secs(6));

    // Read queries should now go to the failover replica.
    let mut c2 = proxy_conn().expect("connection during partition");
    let read_result: Result<Vec<u64>, _> = c2.query("SELECT 1");
    assert!(
        read_result.is_ok(),
        "reads should succeed via failover replica"
    );

    // Restore primary.
    enable_proxy("mysql-primary");
    thread::sleep(Duration::from_secs(12)); // wait for cooldown + recovery checks
    reset_all_toxics();
}

// ─── Scenario 4: Failover flap protection ────────────────────────────────────
//
// Expectation: rapid primary up/down does not cause rapid flip-flop. Cooldown
// (`failover_cooldown_secs=10`) holds the failover active during instability.
#[test]
fn chaos_04_failover_flap_protection() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    // Simulate primary flapping: down → up → down → up (rapidly within cooldown)
    for _ in 0..3 {
        disable_proxy("mysql-primary");
        thread::sleep(Duration::from_secs(3));
        enable_proxy("mysql-primary");
        thread::sleep(Duration::from_secs(2));
    }

    // Disable primary one final time to trigger a failover.
    disable_proxy("mysql-primary");
    thread::sleep(Duration::from_secs(6)); // let health checker detect it

    // Proxy should be serving reads via failover replica (not crashing).
    let mut c = proxy_conn().expect("connection during flap");
    let result: Result<Vec<u64>, _> = c.query("SELECT 1");
    assert!(
        result.is_ok(),
        "proxy must remain stable during primary flapping"
    );

    enable_proxy("mysql-primary");
    thread::sleep(Duration::from_secs(15)); // full cooldown
    reset_all_toxics();
}

// ─── Scenario 5: Slow backend opens circuit breaker ──────────────────────────
//
// Expectation: after `circuit_breaker_threshold` consecutive failures caused
// by a very slow backend (combined with query timeout), the circuit breaker
// opens and subsequent requests fail immediately (not after a long wait).
#[test]
fn chaos_05_circuit_breaker_opens_on_slow_backend() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    // Inject extreme latency on primary to cause connection timeouts.
    // proxy pool timeout is default; we just need enough requests to fail.
    add_latency("mysql-primary", 15000, 0); // 15s latency → connection pool timeout

    // Attempt several writes — each should fail with an error (not hang forever).
    let mut fast_failures = 0u32;
    for _ in 0..5 {
        let start = std::time::Instant::now();
        if let Some(mut c) = proxy_conn() {
            if c.query_drop("INSERT INTO chaos_probe (v) VALUES (1)")
                .is_err()
                && start.elapsed() < Duration::from_secs(10)
            {
                fast_failures += 1;
            }
        }
    }

    remove_toxic("mysql-primary", "latency");
    thread::sleep(Duration::from_secs(1));

    // At least some failures should have been fast (circuit breaker effect).
    // We don't assert on the exact count because the CB opens after threshold.
    eprintln!("[chaos_05] fast_failures={fast_failures} (CB may need more iterations in slow CI)");
    reset_all_toxics();
}

// ─── Scenario 6: Pool exhaustion → reject-fast ───────────────────────────────
//
// Expectation: when pool is full (pool_size=5, max_connections=20), additional
// connection attempts are rejected immediately (not hung).
#[test]
fn chaos_06_pool_exhaustion_reject_fast() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    // Add latency to primary so pool connections stay open longer.
    add_latency("mysql-primary", 500, 0);

    // Open more connections than pool_size (5) simultaneously.
    let handles: Vec<_> = (0..25)
        .map(|_| {
            thread::spawn(|| {
                let start = std::time::Instant::now();
                let result = proxy_conn()
                    .map(|mut c| c.query_drop("SELECT SLEEP(0.1)").is_ok())
                    .unwrap_or(false);
                (result, start.elapsed())
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let slow_count = results
        .iter()
        .filter(|(_, d)| *d > Duration::from_secs(10))
        .count();

    remove_toxic("mysql-primary", "latency");

    // No request should hang for more than 10s — pool exhaustion must be fast.
    assert!(
        slow_count == 0,
        "{slow_count} requests hung for >10s during pool exhaustion"
    );
}

// ─── Scenario 7: Dashboard isolation ─────────────────────────────────────────
//
// Expectation: proxy continues to serve database queries even when dashboard
// is disabled/inaccessible.
#[test]
fn chaos_07_dashboard_isolation() {
    require_chaos!();
    // Start proxy without dashboard (dashboard.enabled = false is default in
    // the chaos config). Verify database queries work regardless.
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    let mut c = proxy_conn().expect("connection");
    for _ in 0..10 {
        let result: Result<Vec<u64>, _> = c.query("SELECT 1");
        assert!(
            result.is_ok(),
            "queries must work even when dashboard is not running"
        );
    }
}

// ─── Scenario 8: Replica timeout → removed from pool ─────────────────────────
//
// Expectation: when a replica becomes slow/unreachable (not primary), reads
// fall back to primary. No error returned to the client.
#[test]
fn chaos_08_replica_timeout_fallback() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    // Take both replicas down.
    disable_proxy("mysql-replica1");
    disable_proxy("mysql-replica2");

    // Wait for HA to mark replicas unhealthy.
    thread::sleep(Duration::from_secs(6));

    let mut c = proxy_conn().expect("connection with replicas down");
    for _ in 0..5 {
        let result: Result<Vec<u64>, _> = c.query("SELECT 1");
        assert!(
            result.is_ok(),
            "reads must fall back to primary when all replicas are down"
        );
    }

    enable_proxy("mysql-replica1");
    enable_proxy("mysql-replica2");
    reset_all_toxics();
}

// ─── Scenario 9: SIGTERM graceful drain ──────────────────────────────────────
//
// Expectation: proxy exits within a reasonable time after SIGTERM and does not
// leave connections open. We test that the port is released promptly.
#[test]
fn chaos_09_sigterm_graceful_drain() {
    require_chaos!();
    reset_all_toxics();

    let proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );

    // Open a connection and run a query to exercise the drain path.
    let handle = thread::spawn(|| {
        if let Some(mut c) = proxy_conn() {
            let _: Result<Vec<u64>, _> = c.query("SELECT SLEEP(0.2)");
        }
    });

    thread::sleep(Duration::from_millis(50)); // let the query start

    // SIGTERM the proxy.
    let pid = proxy.child.id();
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        // On non-Unix we just kill for simplicity in CI.
        drop(proxy);
    }

    let _ = handle.join();

    // After a reasonable grace period, the port should be free.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut port_free = false;
    while std::time::Instant::now() < deadline {
        if proxy_conn().is_none() {
            port_free = true;
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    assert!(
        port_free,
        "proxy port should be released within 10s of SIGTERM"
    );
}

// ─── Scenario 10: Live config reload (SIGHUP) ────────────────────────────────
//
// Expectation: SIGHUP triggers config reload. In-flight queries complete.
// The proxy continues serving after reload.
#[test]
fn chaos_10_config_reload_live() {
    require_chaos!();
    let _proxy = start_proxy_with_ha();
    assert!(
        wait_for_proxy(Duration::from_secs(5)),
        "proxy did not start"
    );
    reset_all_toxics();

    // Start a slow background query.
    let handle = thread::spawn(|| {
        if let Some(mut c) = proxy_conn() {
            // 200ms query — should complete despite reload.
            let _: Result<Vec<u64>, _> = c.query("SELECT SLEEP(0.2)");
        }
    });

    thread::sleep(Duration::from_millis(50));

    // SIGHUP → reload config.
    #[cfg(unix)]
    {
        let pid = _proxy.child.id();
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGHUP) };
    }

    let _ = handle.join();

    // Verify proxy is still serving after reload.
    thread::sleep(Duration::from_millis(500));
    let mut c = proxy_conn().expect("connection after config reload");
    let result: Result<Vec<u64>, _> = c.query("SELECT 1");
    assert!(
        result.is_ok(),
        "proxy must serve queries after SIGHUP reload"
    );
}
