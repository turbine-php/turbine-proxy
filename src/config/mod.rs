//! Configuration for TurbineProxy, loaded from TOML.
#![allow(unused)]

pub mod store;
pub use store::ConfigStore;

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    #[serde(skip)]
    pub mysql_enabled: bool,

    /// Address the proxy listens on (e.g., "0.0.0.0:3307")
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,

    /// Maximum concurrent client connections
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,

    /// Connection pool size per backend
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,

    /// Primary (read-write) MySQL backend
    pub primary: BackendConfig,

    /// Replica (read-only) MySQL backends
    #[serde(default)]
    pub replicas: Vec<BackendConfig>,

    /// Analytics configuration
    #[serde(default)]
    pub analytics: AnalyticsConfig,

    /// Dashboard configuration
    #[serde(default)]
    pub dashboard: DashboardConfig,

    /// High-availability configuration
    #[serde(default)]
    pub ha: HaConfig,

    /// TLS for incoming client connections (frontend TLS).
    #[serde(default)]
    pub frontend_tls: FrontendTlsConfig,

    /// Per-user proxy access rules.
    /// If empty, the proxy accepts any username/password without verification
    /// (backward-compatible open mode). When non-empty, only listed users can connect.
    #[serde(default)]
    pub users: Vec<UserConfig>,

    /// Credential cache TTL in seconds.
    /// Verified credentials are cached to avoid re-hashing on every reconnect.
    #[serde(default = "default_auth_cache_ttl")]
    pub auth_cache_ttl_secs: u64,

    /// Maximum idle time (seconds) before a pooled backend connection is considered
    /// stale and discarded. Should be less than MySQL's `wait_timeout`.
    /// Default: 55s (safe under the common 60s managed-DB idle timeout).
    /// Set to 0 to disable eviction (not recommended for production).
    #[serde(default = "default_connection_max_idle_secs")]
    pub connection_max_idle_secs: u64,

    /// Configurable query routing rules.
    /// Rules are evaluated in declaration order. The first match wins.
    /// If no rule matches, the built-in read/write splitting heuristic applies.
    #[serde(default)]
    pub query_rules: Vec<QueryRuleConfig>,

    /// MySQL Group Replication / InnoDB Cluster awareness.
    /// When enabled, the proxy automatically tracks which member is the current
    /// PRIMARY and re-routes writes accordingly — no restart required.
    #[serde(default)]
    pub group_replication: GroupReplicationConfig,

    /// Multi-instance config sync (TurbineProxy Cluster).
    /// When non-empty `peers` are configured, every config reload on this node
    /// is pushed to all peers so they stay in sync without a restart.
    #[serde(default)]
    pub cluster: ClusterConfig,

    /// Query rewriting rules.
    /// Applied to every COM_QUERY before routing. The first matching rule wins.
    /// Rewrites can: regex-substitute SQL text, force a LIMIT, inject a query
    /// timeout hint, or block the query entirely.
    #[serde(default)]
    pub rewrite_rules: Vec<QueryRewriteConfig>,

    /// Maximum duration (milliseconds) for an open transaction before the proxy
    /// aborts it with an error. 0 = disabled (default).
    /// Use this as a safety net against forgotten BEGIN or slow ORM transactions.
    #[serde(default)]
    pub max_transaction_time_ms: u64,

    /// Maximum duration (milliseconds) for a single query before the proxy
    /// issues a `KILL QUERY <thread_id>` on the backend and returns an error to
    /// the client. 0 = disabled (default).
    /// Unlike `max_transaction_time_ms`, this kills the individual query at the
    /// MySQL level — the backend connection is returned to the pool after the kill.
    /// Uses `tokio::time::timeout` — zero overhead when disabled.
    #[serde(default)]
    pub max_query_time_ms: u64,

    /// Maximum idle time (milliseconds) for an open transaction.
    /// If a transaction is open but no query arrives within this window, the
    /// proxy aborts the transaction with an error. 0 = disabled (default).
    #[serde(default)]
    pub max_transaction_idle_ms: u64,

    /// Controls how `SELECT VERSION()` / `SELECT @@version` is handled.
    /// When `true` (default), the proxy responds directly with the backend's
    /// version string captured at connect time — avoids an extra round-trip.
    /// When `false`, the query is forwarded to the backend unchanged.
    #[serde(default = "default_true")]
    pub select_version_forwarding: bool,

    /// Read-Your-Own-Writes window in milliseconds.
    ///
    /// After any successful write (INSERT/UPDATE/DELETE/REPLACE), subsequent
    /// read queries from the same client connection are routed to the **primary**
    /// for this many milliseconds before reverting to replica routing.
    ///
    /// This prevents a client from reading stale data immediately after a write
    /// due to replication lag. 0 = disabled (default).
    ///
    /// Typical values: 200–1000 ms (cover replication lag on fast networks).
    /// Equivalent to ProxySQL's `mysql-default_query_delay` + hostgroup rules,
    /// but automatic and per-connection.
    #[serde(default)]
    pub read_your_own_writes_ms: u64,

    /// Whitelist of normalised query fingerprints that are allowed to execute.
    /// When non-empty, any query whose fingerprint is NOT in this list is
    /// rejected with an error (allowlist / whitelist mode).
    /// Fingerprints use the same normalisation as the N+1 detector and the
    /// slow-query log (literals replaced with `?`, whitespace collapsed).
    /// Leave empty (default) to disable whitelist enforcement.
    #[serde(default)]
    pub query_whitelist: Vec<String>,

    /// SQL injection detection: when true, queries matching known injection
    /// patterns are rejected before reaching the backend.
    #[serde(default)]
    pub sql_injection_protection: bool,

    /// Audit log file path.  When non-empty, every query (user, client IP,
    /// SQL text, routing decision, duration ms) is appended as NDJSON.
    /// Rotate externally with `logrotate`; the proxy re-opens the file on SIGHUP.
    #[serde(default)]
    pub audit_log: String,

    /// PROXY Protocol v1 support.
    /// When enabled, the proxy reads the PROXY header from each incoming TCP
    /// connection to extract the real client IP (for use behind HAProxy/AWS NLB).
    #[serde(default)]
    pub proxy_protocol: ProxyProtocolConfig,

    /// Graceful shutdown timeout in seconds.
    /// After receiving SIGTERM, the proxy stops accepting new connections and
    /// waits up to this many seconds for active connections to finish.
    /// 0 = exit immediately without draining.  Default: 30.
    #[serde(default = "default_shutdown_timeout_secs")]
    pub shutdown_timeout_secs: u64,

    /// Per-connection consecutive error limit.
    /// If a client triggers this many consecutive errors within
    /// `client_error_window_secs`, the proxy closes the connection.
    /// 0 = disabled (default).
    #[serde(default)]
    pub client_error_limit: u32,

    /// Rolling window (seconds) for counting consecutive client errors.
    /// Default: 60.  Ignored when `client_error_limit` is 0.
    #[serde(default = "default_client_error_window")]
    pub client_error_window_secs: u64,

    /// When `true`, log the raw parameter bytes (hex) of every `COM_STMT_EXECUTE`
    /// packet to the slow-query log.  Disabled by default (can be verbose).
    /// Useful during development / debugging to inspect prepared-statement bindings.
    #[serde(default)]
    pub log_prepared_params: bool,

    /// PostgreSQL proxy configuration.
    /// When `pgsql.enabled = true`, the proxy also listens on a PostgreSQL port
    /// and acts as a protocol-transparent proxy for PostgreSQL clients.
    #[serde(default)]
    pub pgsql: PgsqlConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SharedConfig {
    pub listen_addr: Option<String>,
    pub max_connections: Option<usize>,
    pub pool_size: Option<usize>,
    pub primary: Option<BackendConfig>,
    #[serde(default)]
    pub replicas: Vec<BackendConfig>,
    #[serde(default)]
    pub users: Vec<UserConfig>,
    pub auth_cache_ttl_secs: Option<u64>,
    pub connection_max_idle_secs: Option<u64>,
    pub read_your_own_writes_ms: Option<u64>,
    #[serde(default)]
    pub query_whitelist: Vec<String>,
    pub sql_injection_protection: Option<bool>,
    pub audit_log: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawProxyConfig {
    pub shared: Option<SharedConfig>,

    pub listen_addr: Option<String>,
    pub max_connections: Option<usize>,
    pub pool_size: Option<usize>,
    pub primary: Option<BackendConfig>,
    #[serde(default)]
    pub replicas: Vec<BackendConfig>,

    #[serde(default)]
    pub analytics: AnalyticsConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub ha: HaConfig,
    #[serde(default)]
    pub frontend_tls: FrontendTlsConfig,
    #[serde(default)]
    pub users: Vec<UserConfig>,

    pub auth_cache_ttl_secs: Option<u64>,
    pub connection_max_idle_secs: Option<u64>,

    #[serde(default)]
    pub query_rules: Vec<QueryRuleConfig>,
    #[serde(default)]
    pub group_replication: GroupReplicationConfig,
    #[serde(default)]
    pub cluster: ClusterConfig,
    #[serde(default)]
    pub rewrite_rules: Vec<QueryRewriteConfig>,

    #[serde(default)]
    pub max_transaction_time_ms: u64,
    #[serde(default)]
    pub max_query_time_ms: u64,
    #[serde(default)]
    pub max_transaction_idle_ms: u64,
    #[serde(default = "default_true")]
    pub select_version_forwarding: bool,

    pub read_your_own_writes_ms: Option<u64>,
    #[serde(default)]
    pub query_whitelist: Vec<String>,
    pub sql_injection_protection: Option<bool>,
    pub audit_log: Option<String>,

    #[serde(default)]
    pub proxy_protocol: ProxyProtocolConfig,
    #[serde(default = "default_shutdown_timeout_secs")]
    pub shutdown_timeout_secs: u64,
    #[serde(default)]
    pub client_error_limit: u32,
    #[serde(default = "default_client_error_window")]
    pub client_error_window_secs: u64,
    #[serde(default)]
    pub log_prepared_params: bool,

    #[serde(default)]
    pub pgsql: PgsqlConfig,
}

/// Routing destination for a query rule.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuleDestination {
    /// Fall through to the built-in heuristic (default).
    #[default]
    Any,
    /// Always route to the primary (read-write) backend.
    Primary,
    /// Always route to a replica (read-only) backend.
    Replica,
}

/// A single query routing rule.
///
/// A rule matches when **all** non-empty filter fields agree:
/// `match_pattern` (regex on raw SQL), `match_digest` (exact fingerprint),
/// `user`, `schema`. The first matching rule wins.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryRuleConfig {
    /// PCRE-compatible regex matched against the raw SQL text.
    /// Compiled once at startup — zero overhead on the hot path.
    pub match_pattern: Option<String>,

    /// Exact fingerprint (normalised query) to match.
    /// Takes precedence over `match_pattern` when both are set.
    pub match_digest: Option<String>,

    /// Restrict rule to a specific MySQL user (empty = all users).
    #[serde(default)]
    pub user: String,

    /// Restrict rule to a specific schema/database (empty = all schemas).
    #[serde(default)]
    pub schema: String,

    /// Where to send the query: `primary`, `replica`, or `any` (heuristic).
    #[serde(default)]
    pub destination: RuleDestination,

    /// Override cache TTL in seconds. `0` disables caching for this rule.
    /// Ignored when `destination = "primary"` (writes are never cached).
    #[serde(default)]
    pub cache_ttl_secs: u64,

    /// Human-readable description shown in the dashboard.
    #[serde(default)]
    pub comment: String,

    /// Mirror queries to this backend address (fire-and-forget).
    /// The client always receives the response from the real destination;
    /// the mirror receives the same query asynchronously and its response
    /// is discarded. Useful for load testing and canary validation.
    pub mirror_to: Option<String>,

    /// Route to a specific backend by hostgroup index.
    /// `0` = primary, `1` = first replica, `2` = second replica, etc.
    /// Takes precedence over `destination` when set.
    pub destination_hostgroup: Option<u32>,

    /// Incremental rollout percentage (1–100).
    /// Only this percentage of matching queries are routed via this rule;
    /// the rest fall through to the next matching rule or the heuristic.
    /// Useful for canary traffic splitting (e.g. `rollout_pct = 10` sends
    /// 10 % of matching queries to `destination`).
    pub rollout_pct: Option<u8>,
}

/// TLS mode for backend connections.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// No TLS — plain TCP (default).
    #[default]
    Off,
    /// Encrypt but do not verify the server certificate.
    Required,
    /// Verify the server certificate against `tls_ca` (or the Mozilla root store
    /// if unset), but do not check the hostname.
    VerifyCa,
    /// Verify the server certificate **and** that the hostname matches the cert.
    /// Use this when connecting to AWS RDS / Aurora / Cloud SQL.
    VerifyIdentity,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    /// Address of the MySQL server (e.g., "127.0.0.1:3306")
    pub addr: String,

    /// MySQL username
    #[serde(default)]
    pub user: String,

    /// MySQL password
    #[serde(default)]
    pub password: String,

    /// Default database
    pub database: Option<String>,

    /// TLS mode. Default: off (plain TCP).
    #[serde(default)]
    pub tls_mode: TlsMode,

    /// Path to a PEM-encoded CA certificate bundle.
    /// Used for `verify-ca` and `verify-identity` modes.
    /// If unset, the Mozilla root store (webpki-roots) is used.
    pub tls_ca: Option<String>,

    /// Path to a PEM-encoded client certificate (mutual TLS, optional).
    pub tls_cert: Option<String>,

    /// Path to a PEM-encoded client private key (mutual TLS, optional).
    pub tls_key: Option<String>,

    /// Relative weight for load balancing among replicas (default: 100).
    /// A replica with `weight = 200` receives twice as many queries as one
    /// with `weight = 100`. Ignored for the primary.
    #[serde(default = "default_backend_weight")]
    pub weight: u32,

    /// If true, this replica is used only when all non-backup replicas are
    /// unhealthy (last-resort fallback). Default: false.
    #[serde(default)]
    pub backup: bool,

    /// SQL statements to execute immediately after opening a new backend connection.
    /// Applied before the connection is added to the pool (equivalent to ProxySQL's
    /// `init_connect`). Example: `["SET NAMES utf8mb4", "SET SESSION sql_mode = ''"]`.
    #[serde(default)]
    pub init_connect: Vec<String>,

    /// Address family to use when resolving backend hostnames.
    /// `"system"` (default) — let the OS choose (normal DNS resolution).
    /// `"ipv4"` — force IPv4; `"ipv6"` — force IPv6.
    /// Prevents IPv4↔IPv6 flapping on dual-stack networks (AWS, GCP).
    #[serde(default = "default_resolution_family")]
    pub resolution_family: String,
}

/// Per-user proxy access configuration.
/// Users listed here are verified before any backend connection is established.
#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    /// MySQL username presented by the client.
    pub name: String,

    /// Plaintext password — stored in memory as hashed; never logged.
    pub password: String,

    /// If false, the user can only execute read (SELECT/SHOW/EXPLAIN) queries.
    /// Writes (INSERT/UPDATE/DELETE/DDL) are rejected at the proxy layer.
    #[serde(default = "default_true")]
    pub allow_writes: bool,

    /// Maximum simultaneous connections for this user (0 = unlimited).
    #[serde(default)]
    pub max_connections: usize,

    /// Default database to `USE` when a client doesn't specify one at connect
    /// time.  The proxy injects `USE \`<schema>\`` as the first query of each
    /// session.  Empty string = no default (server default applies).
    #[serde(default)]
    pub default_schema: String,

    /// Per-user transaction isolation level.
    /// When non-empty, the proxy injects
    /// `SET SESSION TRANSACTION ISOLATION LEVEL <value>` at session start.
    /// Valid values: READ-UNCOMMITTED, READ-COMMITTED, REPEATABLE-READ, SERIALIZABLE.
    /// Empty = use the server's default (no injection).
    #[serde(default)]
    pub transaction_isolation: String,
}

/// PROXY Protocol v1 configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProxyProtocolConfig {
    /// Accept the PROXY Protocol v1 header from client connections.
    /// Enable this when the proxy sits behind HAProxy, AWS NLB, or any load
    /// balancer that sends PROXY headers so the real client IP is preserved.
    #[serde(default)]
    pub enabled: bool,
}

/// TLS for accepting client connections.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FrontendTlsConfig {
    /// Enable TLS on the proxy's listen socket.
    #[serde(default)]
    pub enabled: bool,

    /// Path to a PEM-encoded server certificate.
    #[serde(default)]
    pub cert: String,

    /// Path to a PEM-encoded server private key.
    #[serde(default)]
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsConfig {
    /// Enable query analytics
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Path to SQLite database for analytics storage
    #[serde(default = "default_analytics_path")]
    pub db_path: String,

    /// Slow query threshold in milliseconds
    #[serde(default = "default_slow_query_ms")]
    pub slow_query_ms: u64,

    /// How many days of time-series data to retain (default 30).
    /// Rows older than this are pruned by the background rollup task.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: default_analytics_path(),
            slow_query_ms: default_slow_query_ms(),
            retention_days: default_retention_days(),
        }
    }
}

fn default_retention_days() -> u32 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashboardConfig {
    /// Enable the web dashboard
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Dashboard listen address
    #[serde(default = "default_dashboard_addr")]
    pub listen_addr: String,

    /// Dashboard admin username. If empty, auth is disabled.
    #[serde(default)]
    pub username: String,

    /// Dashboard admin password. If empty, auth is disabled.
    #[serde(default)]
    pub password: String,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_addr: default_dashboard_addr(),
            username: String::new(),
            password: String::new(),
        }
    }
}

/// A single query rewriting rule.
///
/// Rules are evaluated in declaration order. The **first** matching rule wins —
/// the rest are skipped.  Rewrites are applied before routing so that query
/// rules and the cache see the rewritten SQL.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryRewriteConfig {
    /// PCRE-compatible regex that must match the raw SQL text.
    pub match_pattern: String,

    /// Regex replacement string applied to the whole SQL.
    /// Supports `$1`, `$2` … back-references from `match_pattern` capture groups.
    /// Leave unset (or set to `""`) to skip substitution.
    pub replace_with: Option<String>,

    /// Inject `LIMIT N` at the end of the query when no `LIMIT` clause is
    /// already present.  Only applied to SELECT statements.
    pub add_limit: Option<u32>,

    /// Inject `/*+ MAX_EXECUTION_TIME(N) */` after the `SELECT` keyword.
    /// N is in milliseconds, matching the MySQL hint semantics.
    /// Only applied to SELECT statements that don't already carry the hint.
    pub add_timeout_ms: Option<u64>,

    /// If `true`, block the query and return an error to the client.
    /// Takes precedence over all other rewrite operations.
    #[serde(default)]
    pub block: bool,

    /// Human-readable description shown in the dashboard.
    #[serde(default)]
    pub comment: String,
}

/// MySQL Group Replication / InnoDB Cluster configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupReplicationConfig {
    /// Enable Group Replication monitoring and automatic primary re-routing.
    #[serde(default)]
    pub enabled: bool,

    /// How often to poll `performance_schema.replication_group_members` (seconds).
    #[serde(default = "default_gr_interval")]
    pub check_interval_secs: u64,
}

impl Default for GroupReplicationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            check_interval_secs: default_gr_interval(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HaConfig {
    /// Enable active health checks and failover.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// How often to run health checks (seconds).
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,

    /// Replica replication lag above this threshold (ms) is considered unhealthy.
    #[serde(default = "default_max_lag_ms")]
    pub max_replica_lag_ms: u64,

    /// Consecutive primary failures before promoting a replica as failover.
    #[serde(default = "default_failover_threshold")]
    pub primary_failover_threshold: u32,

    /// Enable Galera / Percona XtraDB Cluster node-state checks.
    ///
    /// When `true`, the health checker queries `SHOW GLOBAL STATUS LIKE 'wsrep_local_state'`
    /// on every node and marks a node unhealthy unless `wsrep_local_state = 4` (SYNCED).
    /// This prevents routing reads to nodes that are joining, leaving, or desynced.
    ///
    /// Safe to enable on standard asynchronous replication setups — the check is skipped
    /// gracefully if `wsrep_local_state` is not present (non-Galera servers return an
    /// empty result set).
    ///
    /// Default: false (disabled).
    #[serde(default)]
    pub galera_check: bool,
}

impl Default for HaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            health_check_interval_secs: default_health_interval(),
            max_replica_lag_ms: default_max_lag_ms(),
            primary_failover_threshold: default_failover_threshold(),
            galera_check: false,
        }
    }
}

// ─── ClusterConfig ────────────────────────────────────────────────────────────

/// TurbineProxy Cluster — multi-instance config synchronisation.
///
/// When `peers` is non-empty and `secret` is set, every config reload on this
/// node (via SIGHUP, `POST /api/reload`, or `POST /api/reload/backends`) is
/// pushed to every peer's `POST /api/sync` endpoint.  Peers validate the
/// request with the shared `secret` and apply the config atomically.
///
/// All instances must share the same `secret`.  Traffic is not redirected —
/// each node still maintains its own connections and health state.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ClusterConfig {
    /// Dashboard base URLs of peer nodes (e.g. `["http://node2:8080", "http://node3:8080"]`).
    /// The local node should NOT include itself.
    #[serde(default)]
    pub peers: Vec<String>,

    /// Shared secret used to authenticate cluster sync requests.
    /// Must match on all nodes.  Empty string disables cluster sync even
    /// if peers are listed (safe default — requires explicit opt-in).
    #[serde(default)]
    pub secret: String,
}

fn default_auth_cache_ttl() -> u64 {
    300 // 5 minutes
}

fn default_connection_max_idle_secs() -> u64 {
    55 // safe margin below MySQL's common 60s managed-DB wait_timeout
}

fn default_backend_weight() -> u32 {
    100
}

fn default_listen_addr() -> String {
    "0.0.0.0:3307".to_string()
}

fn default_max_connections() -> usize {
    1000
}

fn default_pool_size() -> usize {
    20
}

fn default_true() -> bool {
    true
}

fn default_analytics_path() -> String {
    "turbineproxy_analytics.db".to_string()
}

fn default_slow_query_ms() -> u64 {
    100
}

fn default_dashboard_addr() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_health_interval() -> u64 {
    5
}
fn default_max_lag_ms() -> u64 {
    5000
}
fn default_failover_threshold() -> u32 {
    3
}
fn default_patroni_port() -> u16 {
    8008
}
fn default_gr_interval() -> u64 {
    5
}
fn default_shutdown_timeout_secs() -> u64 {
    30
}
fn default_client_error_window() -> u64 {
    60
}
fn default_resolution_family() -> String {
    "system".to_string()
}
fn default_pgsql_listen_addr() -> String {
    "0.0.0.0:5433".to_string()
}
fn default_pgsql_pool_size() -> usize {
    10
}
fn default_pgsql_health_database() -> String {
    "postgres".to_string()
}

/// PostgreSQL proxy configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PgsqlConfig {
    /// Enable the PostgreSQL proxy listener (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Address the PostgreSQL proxy listens on (default: "0.0.0.0:5433").
    #[serde(default = "default_pgsql_listen_addr")]
    pub listen_addr: String,

    /// Primary (read-write) PostgreSQL backend.
    /// Required when `enabled = true`.
    pub primary: Option<BackendConfig>,

    /// Read-only PostgreSQL replica backends.
    #[serde(default)]
    pub replicas: Vec<BackendConfig>,

    /// Connection pool size per PostgreSQL backend (default: 10).
    #[serde(default = "default_pgsql_pool_size")]
    pub pool_size: usize,

    /// Maximum concurrent PostgreSQL client connections (0 = no limit).
    #[serde(default)]
    pub max_connections: usize,

    /// Per-user PostgreSQL access rules.
    /// When empty, the proxy accepts any username/password without verification.
    #[serde(default)]
    pub users: Vec<UserConfig>,

    /// Maximum idle time (seconds) before a pooled PostgreSQL backend connection
    /// is discarded. Default: 55. Set to 0 to disable.
    #[serde(default = "default_connection_max_idle_secs")]
    pub connection_max_idle_secs: u64,

    /// After any write, route subsequent reads to the primary for this many ms.
    /// 0 = disabled (default). Same semantics as the MySQL `read_your_own_writes_ms`.
    #[serde(default)]
    pub read_your_own_writes_ms: u64,

    /// Active health check interval in seconds (default: 10).
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,

    /// Replica lag above this threshold (ms) is treated as unhealthy (default: 5000).
    #[serde(default = "default_max_lag_ms")]
    pub max_replica_lag_ms: u64,

    /// Consecutive primary check failures before failover (default: 3).
    #[serde(default = "default_failover_threshold")]
    pub primary_failover_threshold: u32,

    /// Database used exclusively for backend health probes (`SELECT 1`,
    /// `pg_is_in_recovery()`). This lets client sessions use any database while
    /// keeping probes anchored to a known control DB.
    #[serde(default = "default_pgsql_health_database")]
    pub health_check_database: String,

    // ── Security ──────────────────────────────────────────────────────────────
    /// Enforce fingerprint allowlist.  When `true`, queries not in
    /// `query_whitelist` are rejected with SQLSTATE 42501. Default: false.
    #[serde(default)]
    pub whitelist_mode: bool,

    /// Allowed query fingerprints.  Each entry is a normalised SQL string
    /// where literals are replaced with `$1`, `$2` etc.  Ignored when
    /// `whitelist_mode = false`.
    #[serde(default)]
    pub query_whitelist: Vec<String>,

    /// SQL injection detection: when true, queries matching known injection
    /// patterns are rejected before reaching the backend.
    #[serde(default)]
    pub sql_injection_protection: bool,

    /// Path for the immutable NDJSON audit log.  Empty = disabled.
    #[serde(default)]
    pub audit_log: String,

    // ── Observability ─────────────────────────────────────────────────────────
    /// Queries that take longer than this (ms) are emitted to the slow query
    /// log (via the shared `Collector`).  0 = disabled.
    #[serde(default)]
    pub slow_query_log_ms: u64,

    // ── Patroni ───────────────────────────────────────────────────────────────
    /// Use Patroni REST API in addition to `pg_is_in_recovery()` to determine
    /// the role of each backend.  Default: false.
    #[serde(default)]
    pub patroni_check: bool,

    /// Port of the Patroni REST API on each backend host.  Default: 8008.
    #[serde(default = "default_patroni_port")]
    pub patroni_api_port: u16,

    // ── Frontend TLS ──────────────────────────────────────────────────────────
    /// Path to the PEM certificate used for TLS on the PostgreSQL listener.
    /// Empty = TLS disabled (clients that send SSLRequest receive `N`).
    #[serde(default)]
    pub ssl_cert: String,

    /// Path to the PEM private key matching `ssl_cert`.
    #[serde(default)]
    pub ssl_key: String,
}

impl ProxyConfig {
    fn from_raw(raw: RawProxyConfig) -> anyhow::Result<Self> {
        let mysql_enabled =
            raw.primary.is_some() || raw.listen_addr.is_some() || !raw.replicas.is_empty();

        let shared = raw.shared.unwrap_or_default();

        let listen_addr = raw
            .listen_addr
            .or(shared.listen_addr)
            .unwrap_or_else(default_listen_addr);
        let max_connections = raw
            .max_connections
            .or(shared.max_connections)
            .unwrap_or_else(default_max_connections);
        let pool_size = raw
            .pool_size
            .or(shared.pool_size)
            .unwrap_or_else(default_pool_size);

        let primary = raw
            .primary
            .or(shared.primary)
            .or_else(|| raw.pgsql.primary.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                "missing backend config: define [primary], [shared.primary], or [pgsql.primary]"
            )
            })?;

        let replicas = if !raw.replicas.is_empty() {
            raw.replicas
        } else if !shared.replicas.is_empty() {
            shared.replicas
        } else if !raw.pgsql.replicas.is_empty() {
            raw.pgsql.replicas.clone()
        } else {
            Vec::new()
        };

        let users = if !raw.users.is_empty() {
            raw.users
        } else if !shared.users.is_empty() {
            shared.users
        } else if !raw.pgsql.users.is_empty() {
            raw.pgsql.users.clone()
        } else {
            Vec::new()
        };

        let auth_cache_ttl_secs = raw
            .auth_cache_ttl_secs
            .or(shared.auth_cache_ttl_secs)
            .unwrap_or_else(default_auth_cache_ttl);
        let connection_max_idle_secs = raw
            .connection_max_idle_secs
            .or(shared.connection_max_idle_secs)
            .unwrap_or_else(default_connection_max_idle_secs);
        let read_your_own_writes_ms = raw
            .read_your_own_writes_ms
            .or(shared.read_your_own_writes_ms)
            .unwrap_or_default();

        let query_whitelist = if !raw.query_whitelist.is_empty() {
            raw.query_whitelist
        } else {
            shared.query_whitelist
        };
        let sql_injection_protection = raw
            .sql_injection_protection
            .or(shared.sql_injection_protection)
            .unwrap_or(false);
        let audit_log = raw.audit_log.or(shared.audit_log).unwrap_or_default();

        Ok(Self {
            mysql_enabled,
            listen_addr,
            max_connections,
            pool_size,
            primary,
            replicas,
            analytics: raw.analytics,
            dashboard: raw.dashboard,
            ha: raw.ha,
            frontend_tls: raw.frontend_tls,
            users,
            auth_cache_ttl_secs,
            connection_max_idle_secs,
            query_rules: raw.query_rules,
            group_replication: raw.group_replication,
            cluster: raw.cluster,
            rewrite_rules: raw.rewrite_rules,
            max_transaction_time_ms: raw.max_transaction_time_ms,
            max_query_time_ms: raw.max_query_time_ms,
            max_transaction_idle_ms: raw.max_transaction_idle_ms,
            select_version_forwarding: raw.select_version_forwarding,
            read_your_own_writes_ms,
            query_whitelist,
            sql_injection_protection,
            audit_log,
            proxy_protocol: raw.proxy_protocol,
            shutdown_timeout_secs: raw.shutdown_timeout_secs,
            client_error_limit: raw.client_error_limit,
            client_error_window_secs: raw.client_error_window_secs,
            log_prepared_params: raw.log_prepared_params,
            pgsql: raw.pgsql,
        })
    }

    /// Load configuration from a TOML file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_str(&content)
    }

    /// Load from a TOML string.
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        let raw: RawProxyConfig = toml::from_str(s)?;
        Self::from_raw(raw)
    }

    /// Resolve the effective PostgreSQL config by inheriting shared values from
    /// the top-level config when the PG section leaves them unset/defaulted.
    pub fn resolved_pgsql(&self) -> PgsqlConfig {
        let mut pg = self.pgsql.clone();

        if pg.primary.is_none() {
            pg.primary = Some(self.primary.clone());
        }
        if pg.replicas.is_empty() {
            pg.replicas = self.replicas.clone();
        }
        if pg.users.is_empty() {
            pg.users = self.users.clone();
        }

        if pg.pool_size == default_pgsql_pool_size() {
            pg.pool_size = self.pool_size;
        }
        if pg.max_connections == 0 {
            pg.max_connections = self.max_connections;
        }
        if pg.connection_max_idle_secs == default_connection_max_idle_secs() {
            pg.connection_max_idle_secs = self.connection_max_idle_secs;
        }
        if pg.read_your_own_writes_ms == 0 {
            pg.read_your_own_writes_ms = self.read_your_own_writes_ms;
        }

        if pg.health_check_interval_secs == default_health_interval() {
            pg.health_check_interval_secs = self.ha.health_check_interval_secs;
        }
        if pg.max_replica_lag_ms == default_max_lag_ms() {
            pg.max_replica_lag_ms = self.ha.max_replica_lag_ms;
        }
        if pg.primary_failover_threshold == default_failover_threshold() {
            pg.primary_failover_threshold = self.ha.primary_failover_threshold;
        }

        if pg.query_whitelist.is_empty() {
            pg.query_whitelist = self.query_whitelist.clone();
        }
        if !pg.sql_injection_protection {
            pg.sql_injection_protection = self.sql_injection_protection;
        }
        if pg.audit_log.is_empty() {
            pg.audit_log = self.audit_log.clone();
        }

        pg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: minimal valid TOML ─────────────────────────────────────────────

    fn minimal_toml(primary_addr: &str) -> String {
        format!(
            r#"
[primary]
addr = "{}"
user = "root"
password = "secret"
"#,
            primary_addr
        )
    }

    // ── from_str: valid configs ────────────────────────────────────────────────

    #[test]
    fn test_parse_minimal_config() {
        let cfg = ProxyConfig::from_str(&minimal_toml("127.0.0.1:3306")).unwrap();
        assert_eq!(cfg.primary.addr, "127.0.0.1:3306");
        assert_eq!(cfg.primary.user, "root");
    }

    #[test]
    fn test_defaults_applied() {
        let cfg = ProxyConfig::from_str(&minimal_toml("127.0.0.1:3306")).unwrap();
        assert_eq!(cfg.listen_addr, "0.0.0.0:3307");
        assert_eq!(cfg.max_connections, 1000);
        assert_eq!(cfg.pool_size, 20);
        assert_eq!(cfg.shutdown_timeout_secs, 30);
        assert!(cfg.analytics.enabled);
        assert_eq!(cfg.analytics.slow_query_ms, 100);
    }

    #[test]
    fn test_parse_with_replicas() {
        let toml = r#"
[primary]
addr = "10.0.0.1:3306"
user = "rw"
password = "pass"

[[replicas]]
addr = "10.0.0.2:3306"
user = "ro"
password = "pass"

[[replicas]]
addr = "10.0.0.3:3306"
user = "ro"
password = "pass"
"#;
        let cfg = ProxyConfig::from_str(toml).unwrap();
        assert_eq!(cfg.replicas.len(), 2);
        assert_eq!(cfg.replicas[0].addr, "10.0.0.2:3306");
        assert_eq!(cfg.replicas[1].addr, "10.0.0.3:3306");
    }

    #[test]
    fn test_parse_listen_addr_override() {
        let toml = r#"
listen_addr = "0.0.0.0:13307"

[primary]
addr = "127.0.0.1:3306"
user = "root"
password = "secret"
"#;
        let cfg = ProxyConfig::from_str(toml).unwrap();
        assert_eq!(cfg.listen_addr, "0.0.0.0:13307");
    }

    #[test]
    fn test_parse_analytics_override() {
        let toml = format!(
            "{}\n[analytics]\nenabled = false\nslow_query_ms = 500",
            minimal_toml("127.0.0.1:3306")
        );
        let cfg = ProxyConfig::from_str(&toml).unwrap();
        assert!(!cfg.analytics.enabled);
        assert_eq!(cfg.analytics.slow_query_ms, 500);
    }

    #[test]
    fn test_parse_users() {
        let toml = r#"
[primary]
addr = "127.0.0.1:3306"
user = "root"
password = "secret"

[[users]]
name = "app"
password = "apppass"
allow_writes = true

[[users]]
name = "reader"
password = "readpass"
allow_writes = false
"#;
        let cfg = ProxyConfig::from_str(toml).unwrap();
        assert_eq!(cfg.users.len(), 2);
        assert_eq!(cfg.users[0].name, "app");
        assert!(cfg.users[0].allow_writes);
        assert_eq!(cfg.users[1].name, "reader");
        assert!(!cfg.users[1].allow_writes);
    }

    #[test]
    fn test_parse_tls_mode() {
        let toml = r#"
[primary]
addr = "rds.example.com:3306"
user = "admin"
password = "pw"
tls_mode = "verify-identity"
"#;
        let cfg = ProxyConfig::from_str(toml).unwrap();
        assert_eq!(cfg.primary.tls_mode, TlsMode::VerifyIdentity);
    }

    #[test]
    fn test_parse_query_rules() {
        let toml = r#"
[primary]
addr = "127.0.0.1:3306"
user = "root"
password = "secret"

[[query_rules]]
match_pattern = "^SELECT"
destination = "replica"
comment = "reads to replica"

[[query_rules]]
match_digest = "SELECT * FROM heavy_table WHERE id = ?"
destination = "primary"
"#;
        let cfg = ProxyConfig::from_str(toml).unwrap();
        assert_eq!(cfg.query_rules.len(), 2);
        assert_eq!(cfg.query_rules[0].destination, RuleDestination::Replica);
        assert_eq!(cfg.query_rules[1].destination, RuleDestination::Primary);
    }

    // ── from_str: invalid configs ─────────────────────────────────────────────

    #[test]
    fn test_parse_missing_primary_fails() {
        let toml = r#"
listen_addr = "0.0.0.0:3307"
max_connections = 100
"#;
        assert!(
            ProxyConfig::from_str(toml).is_err(),
            "missing [primary] should fail"
        );
    }

    #[test]
    fn test_parse_invalid_toml_fails() {
        assert!(ProxyConfig::from_str("this is not toml {{{{").is_err());
    }

    #[test]
    fn test_parse_unknown_tls_mode_fails() {
        let toml = r#"
[primary]
addr = "127.0.0.1:3306"
user = "root"
password = "secret"
tls_mode = "invalid-mode"
"#;
        assert!(ProxyConfig::from_str(toml).is_err());
    }

    // ── Shared config inheritance ──────────────────────────────────────────────

    #[test]
    fn test_shared_pool_size_inherited() {
        let toml = r#"
[shared]
pool_size = 50

[primary]
addr = "127.0.0.1:3306"
user = "root"
password = "secret"
"#;
        let cfg = ProxyConfig::from_str(toml).unwrap();
        assert_eq!(cfg.pool_size, 50);
    }

    // ── Backend defaults ──────────────────────────────────────────────────────

    #[test]
    fn test_backend_default_weight() {
        let cfg = ProxyConfig::from_str(&minimal_toml("127.0.0.1:3306")).unwrap();
        assert_eq!(cfg.primary.weight, 100);
    }

    #[test]
    fn test_backend_default_resolution_family() {
        let cfg = ProxyConfig::from_str(&minimal_toml("127.0.0.1:3306")).unwrap();
        assert_eq!(cfg.primary.resolution_family, "system");
    }
}
