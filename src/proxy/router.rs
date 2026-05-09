//! Query router — read/write splitting, replica selection, and query cache.
//!
//! `Router` encapsulates all decisions about which backend receives a command.
//! It has zero knowledge of the wire protocol; it works with `BackendConnection`
//! trait objects and delegates connection lifecycle to `BackendPool`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::config::BackendConfig;
use crate::protocol::{BackendConnection, BackendResponse, DatabaseProtocol};
use crate::proxy::cache::{is_cacheable, QueryCache};
use crate::proxy::classifier::extract_tables_simple;
use crate::proxy::pool::{BackendPool, ConnectionPool};
use crate::proxy::rewriter::{RewriteOutcome, Rewriter};
use crate::proxy::rules::{Destination, RuleEngine};
use crate::proxy::stmt_shadow::{
    mysql_has_stmt_id, mysql_parse_prepare_ok, mysql_read_stmt_id, mysql_rewrite_prepare_ok,
    mysql_rewrite_stmt_id, MysqlStmtShadow, PgPipelineScan, PgStmtShadow,
};

// ─── Mirror pool manager ──────────────────────────────────────────────────────

/// Lazily creates and caches one `ConnectionPool` per unique `mirror_to` address.
/// Pools are created with a small fixed size (4) and the same credentials as the
/// primary backend.  Mirror queries are fire-and-forget; errors are only logged.
#[derive(Clone)]
struct MirrorManager {
    pools: Arc<RwLock<HashMap<String, Arc<ConnectionPool>>>>,
    protocol: Arc<dyn DatabaseProtocol>,
    /// Template credentials taken from the primary backend config.
    template: BackendConfig,
}

impl MirrorManager {
    fn new(protocol: Arc<dyn DatabaseProtocol>, template: BackendConfig) -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            protocol,
            template,
        }
    }

    async fn get_or_create(&self, addr: &str) -> Arc<ConnectionPool> {
        // Fast path: pool already exists.
        {
            let guard = self.pools.read().await;
            if let Some(p) = guard.get(addr) {
                return p.clone();
            }
        }
        // Slow path: create pool under write lock.
        let mut guard = self.pools.write().await;
        if let Some(p) = guard.get(addr) {
            return p.clone();
        }
        let mut cfg = self.template.clone();
        cfg.addr = addr.to_string();
        let pool = Arc::new(ConnectionPool::with_idle_timeout(
            &cfg,
            4,
            self.protocol.clone(),
            Some(Duration::from_secs(60)),
        ));
        guard.insert(addr.to_string(), pool.clone());
        pool
    }
}

/// Returns `true` when an error looks like a lost / gone-away connection.
/// These errors are safe to retry on a fresh connection because no data was
/// written yet (the original connection was dead before we sent anything).
fn is_connection_lost(e: &anyhow::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    // MySQL error 2006 "MySQL server has gone away"
    // MySQL error 2013 "Lost connection to MySQL server during query"
    // EOF / broken pipe when the OS TCP stack reports the connection is dead
    msg.contains("gone away")
        || msg.contains("lost connection")
        || msg.contains("broken pipe")
        || msg.contains("connection reset")
        || msg.contains("eof")
        || msg.contains("2006")
        || msg.contains("2013")
}

/// Routes queries to the appropriate backend (primary or replica).
/// One `Router` per `ProxyServer` — cloned cheaply via the inner `Arc`s.
///
/// The backend pool is wrapped in `Arc<RwLock<Arc<BackendPool>>>` so it can be
/// hot-swapped atomically without interrupting in-flight queries: existing
/// connections hold their own `Arc` clone and finish normally; new connections
/// pick up the new pool as soon as `reload_pool` stores it.
#[derive(Clone)]
pub struct Router {
    pool: Arc<RwLock<Arc<BackendPool>>>,
    cache: Arc<QueryCache>,
    rules: Arc<RuleEngine>,
    rewriter: Arc<Rewriter>,
    mirror: MirrorManager,
    /// `DatabaseProtocol` used to open a fresh kill-connection when needed.
    protocol: Arc<dyn DatabaseProtocol>,
    /// Config of the primary backend (addr + credentials for kill-connection).
    primary_config: Arc<BackendConfig>,
    /// Per-query timeout; 0 = disabled.
    max_query_time_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Counter incremented each time a query is killed.
    pub queries_killed: Arc<std::sync::atomic::AtomicUsize>,
}

impl Router {
    pub fn new(
        pool: Arc<BackendPool>,
        rules: Arc<RuleEngine>,
        rewriter: Arc<Rewriter>,
        protocol: Arc<dyn DatabaseProtocol>,
    ) -> Self {
        let template = pool.primary.config.clone();
        let primary_config = Arc::new(pool.primary.config.clone());
        Self {
            pool: Arc::new(RwLock::new(pool)),
            cache: Arc::new(QueryCache::with_defaults()),
            rules,
            rewriter,
            mirror: MirrorManager::new(protocol.clone(), template),
            protocol,
            primary_config,
            max_query_time_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            queries_killed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Update the per-query timeout at runtime (call after config reload).
    pub fn set_max_query_time_ms(&self, ms: u64) {
        self.max_query_time_ms
            .store(ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically swap in a new backend pool.
    /// In-flight queries finish on the old pool; new queries use the new pool.
    pub async fn reload_pool(&self, new_pool: Arc<BackendPool>) {
        *self.pool.write().await = new_pool;
    }

    /// Get a snapshot of the current pool (cheap Arc clone).
    pub async fn pool(&self) -> Arc<BackendPool> {
        self.pool.read().await.clone()
    }

    /// Execute a query with the per-query timeout enforced.
    ///
    /// If the timeout fires:
    /// 1. The timed-out connection is dropped (not returned to pool).
    /// 2. A background task opens a fresh connection and sends
    ///    `KILL QUERY <thread_id>` to clean up the server-side cursor.
    /// 3. Returns `Err` with a 1969 / ER_QUERY_TIMEOUT-style message.
    async fn execute_timed(
        &self,
        conn: &mut Box<dyn BackendConnection>,
        sql: &[u8],
        timeout_ms: u64,
    ) -> anyhow::Result<BackendResponse> {
        use std::sync::atomic::Ordering;
        use tokio::time::{timeout, Duration};

        let fut = conn.execute_query(sql);
        match timeout(Duration::from_millis(timeout_ms), fut).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(anyhow::anyhow!("{}", e)),
            Err(_elapsed) => {
                // Query timed out — fire KILL QUERY asynchronously.
                if let Some(thread_id) = conn.backend_conn_id() {
                    let protocol = self.protocol.clone();
                    let cfg = self.primary_config.as_ref().clone();
                    let killed = self.queries_killed.clone();
                    tokio::spawn(async move {
                        match protocol.connect_backend(&cfg).await {
                            Ok(mut kill_conn) => {
                                let kill_sql = format!("KILL QUERY {}", thread_id);
                                if let Err(e) = kill_conn.execute_query(kill_sql.as_bytes()).await {
                                    log::warn!("[kill] KILL QUERY {} failed: {}", thread_id, e);
                                } else {
                                    killed.fetch_add(1, Ordering::Relaxed);
                                    log::info!("[kill] KILL QUERY {} sent — query exceeded max_query_time_ms", thread_id);
                                }
                            }
                            Err(e) => {
                                log::warn!("[kill] failed to open kill-connection: {}", e);
                            }
                        }
                    });
                }
                Err(anyhow::anyhow!(
                    "Query killed: exceeded max_query_time_ms ({}ms)",
                    timeout_ms
                ))
            }
        }
    }

    /// Route a COM_QUERY to the correct backend and return the buffered response.
    ///
    /// Returns `true` when the **first matching rule** for this query has
    /// `fast_forward = true`. Used by `handle_connection` to bypass the full
    /// routing / analytics pipeline for specific hot-path query patterns.
    pub async fn is_fast_forward_rule(&self, sql: &str, user: &str, schema: &str) -> bool {
        self.rules
            .match_query(sql, user, schema)
            .await
            .map(|m| m.fast_forward)
            .unwrap_or(false)
    }

    /// Routing order:
    /// 1. Active transaction      → sticky primary, cache bypassed, no retry
    /// 2. Query rules             → explicit `destination` overrides heuristic
    /// 3. Heuristic               → `use_replica` flag (SELECT → replica)
    ///
    /// **Retry:** if the backend returns a connection-lost error (MySQL gone
    /// away) the dead connection is discarded and the query is retried once on
    /// a fresh connection. Transactions are never retried.
    pub async fn route_query(
        &self,
        sql: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        use_replica: bool,
        user: &str,
        schema: &str,
    ) -> anyhow::Result<BackendResponse> {
        self.route_query_inner(
            sql,
            tx_conn,
            in_transaction,
            use_replica,
            user,
            schema,
            &[],
            None,
        )
        .await
    }

    /// PostgreSQL helper: routes query using a database-scoped backend pool.
    #[allow(clippy::too_many_arguments)]
    pub async fn route_query_with_database(
        &self,
        sql: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        use_replica: bool,
        user: &str,
        schema: &str,
        database: &str,
    ) -> anyhow::Result<BackendResponse> {
        self.route_query_inner(
            sql,
            tx_conn,
            in_transaction,
            use_replica,
            user,
            schema,
            &[],
            Some(database),
        )
        .await
    }

    /// Like `route_query` but replays `session_init_sqls` on a newly-acquired
    /// sticky connection before executing the actual query.  Used for
    /// re-applying session variables (SET NAMES, SET @var, etc.) when the
    /// backend connection is replaced after the session has already set them.
    #[allow(clippy::too_many_arguments)]
    pub async fn route_query_with_session_vars(
        &self,
        sql: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        use_replica: bool,
        user: &str,
        schema: &str,
        session_init_sqls: &[String],
    ) -> anyhow::Result<BackendResponse> {
        self.route_query_inner(
            sql,
            tx_conn,
            in_transaction,
            use_replica,
            user,
            schema,
            session_init_sqls,
            None,
        )
        .await
    }

    /// PostgreSQL helper: session-vars variant with database-scoped pooling.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub async fn route_query_with_session_vars_and_database(
        &self,
        sql: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        use_replica: bool,
        user: &str,
        schema: &str,
        session_init_sqls: &[String],
        database: &str,
    ) -> anyhow::Result<BackendResponse> {
        self.route_query_inner(
            sql,
            tx_conn,
            in_transaction,
            use_replica,
            user,
            schema,
            session_init_sqls,
            Some(database),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn route_query_inner(
        &self,
        sql: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        use_replica: bool,
        user: &str,
        schema: &str,
        session_init_sqls: &[String],
        database: Option<&str>,
    ) -> anyhow::Result<BackendResponse> {
        let pool = self.pool.read().await.clone();
        // Transaction: sticky primary, no cache, no retry.
        if in_transaction {
            let is_new = tx_conn.is_none();
            if is_new {
                *tx_conn = Some(pool.get_primary_for_database(database).await?);
                // Re-apply session variables on the fresh connection.
                if !session_init_sqls.is_empty() {
                    let conn = tx_conn.as_mut().unwrap();
                    for init_sql in session_init_sqls {
                        if let Err(e) = conn.execute_query(init_sql.as_bytes()).await {
                            log::warn!("[session-vars] failed to replay '{}': {}", init_sql, e);
                        }
                    }
                }
            }
            return tx_conn
                .as_mut()
                .unwrap()
                .execute_query(sql)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e));
        }

        let sql_str = std::str::from_utf8(sql).unwrap_or("");

        // Per-query timeout (0 = disabled).
        let timeout_ms = self
            .max_query_time_ms
            .load(std::sync::atomic::Ordering::Relaxed);

        // ── Step 1: query rewriting ───────────────────────────────────────────
        // Rewrites run before routing so that rules and the cache see the
        // final SQL.  We use Cow-style logic: only allocate when necessary.
        let rewritten_sql: String;
        let effective_sql: &str = if !self.rewriter.is_empty() {
            match self.rewriter.apply(sql_str) {
                RewriteOutcome::Blocked(msg) => {
                    return Err(anyhow::anyhow!("{}", msg));
                }
                RewriteOutcome::Rewritten(s) => {
                    if s != sql_str {
                        log::debug!("[rewrite] SQL rewritten: {} → {}", sql_str, s);
                    }
                    rewritten_sql = s;
                    &rewritten_sql
                }
                RewriteOutcome::Unchanged => sql_str,
            }
        } else {
            sql_str
        };
        let effective_bytes: Vec<u8>;
        let sql_bytes_to_use: &[u8] = if effective_sql.len() != sql_str.len()
            || !std::ptr::eq(effective_sql.as_ptr(), sql_str.as_ptr())
        {
            effective_bytes = effective_sql.as_bytes().to_vec();
            &effective_bytes
        } else {
            sql
        };
        let scoped_cache_key = if let Some(db) = database {
            format!("[db:{}] {}", db, effective_sql)
        } else {
            effective_sql.to_string()
        };

        // ── Step 2: explicit query rules ──────────────────────────────────────
        // Rules override the heuristic. `Destination::Any` falls through.
        let rule_match = self.rules.match_query(effective_sql, user, schema).await;

        // Rate-limit: fail fast before allocating any backend connection.
        if let Some(ref m) = rule_match {
            if m.rate_limited {
                return Err(anyhow::anyhow!(
                    "Too many requests: query rate limit exceeded"
                ));
            }
        }

        // Hostgroup routing — bypass the replica/primary path entirely.
        if let Some(ref m) = rule_match {
            if let Destination::Hostgroup(hg) = m.destination {
                let cache_ok = m.cache_ttl_secs > 0;
                if cache_ok && is_cacheable(effective_sql) {
                    if let Some(cached) = self.cache.get(&scoped_cache_key).await {
                        return Ok(BackendResponse {
                            bytes: cached,
                            affected_rows: None,
                            is_error: false,
                            session_changes: vec![],
                            write_gtid: None,
                        });
                    }
                }
                let (mut conn, put_idx) = pool.get_hostgroup_for_database(hg, database).await?;
                let response = if timeout_ms > 0 {
                    self.execute_timed(&mut conn, sql_bytes_to_use, timeout_ms)
                        .await?
                } else {
                    conn.execute_query(sql_bytes_to_use)
                        .await
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                };
                if put_idx == usize::MAX {
                    pool.put_primary_for_database(conn, database).await;
                } else {
                    pool.put_replica_for_database(conn, put_idx, database).await;
                }
                if cache_ok && !response.is_error {
                    self.cache
                        .put(&scoped_cache_key, response.bytes.clone())
                        .await;
                } else if !response.is_error && hg == 0 {
                    let tables = extract_tables_simple(effective_sql);
                    self.cache.invalidate_tables(&tables).await;
                }
                return Ok(response);
            }
        }

        let (effective_replica, cache_allowed) = if let Some(ref m) = rule_match {
            match m.destination {
                Destination::Primary => (false, false),
                Destination::Replica => (true, m.cache_ttl_secs > 0),
                Destination::Any => (use_replica, true),
                Destination::Hostgroup(_) => unreachable!(), // handled above
            }
        } else {
            (use_replica, true)
        };

        // Cache hit — avoid backend round-trip entirely.
        if effective_replica && cache_allowed && is_cacheable(effective_sql) {
            if let Some(cached) = self.cache.get(&scoped_cache_key).await {
                return Ok(BackendResponse {
                    bytes: cached,
                    affected_rows: None,
                    is_error: false,
                    session_changes: vec![],
                    write_gtid: None,
                });
            }
        }

        if effective_replica {
            let (mut conn, replica_idx) = pool.get_replica_for_database(database).await?;
            let response = match if timeout_ms > 0 {
                self.execute_timed(&mut conn, sql_bytes_to_use, timeout_ms)
                    .await
            } else {
                conn.execute_query(sql_bytes_to_use)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            } {
                Ok(r) => r,
                Err(e) => {
                    // Dead replica connection — retry once on a fresh one.
                    if is_connection_lost(&e) {
                        log::warn!(
                            "[pool] replica connection lost, retrying query on fresh connection"
                        );
                        drop(conn); // discard dead connection (not returned to pool)
                        let (mut fresh, fresh_idx) =
                            pool.get_replica_for_database(database).await?;
                        let r = fresh
                            .execute_query(sql_bytes_to_use)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e))?;
                        if fresh_idx == usize::MAX {
                            pool.put_primary_for_database(fresh, database).await;
                        } else {
                            pool.put_replica_for_database(fresh, fresh_idx, database)
                                .await;
                        }
                        return Ok(r);
                    }
                    return Err(e);
                }
            };
            if replica_idx == usize::MAX {
                pool.put_primary_for_database(conn, database).await;
            } else {
                pool.put_replica_for_database(conn, replica_idx, database)
                    .await;
            }
            if cache_allowed && !response.is_error {
                self.cache
                    .put(&scoped_cache_key, response.bytes.clone())
                    .await;
            }
            if let Some(ref m) = rule_match {
                if let Some(ref addr) = m.mirror_to {
                    self.fire_mirror(addr, effective_sql).await;
                }
            }
            Ok(response)
        } else {
            // Write path — execute on primary, then invalidate affected tables.
            let mut conn = pool.get_primary_for_database(database).await?;
            let response = match if timeout_ms > 0 {
                self.execute_timed(&mut conn, sql_bytes_to_use, timeout_ms)
                    .await
            } else {
                conn.execute_query(sql_bytes_to_use)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            } {
                Ok(r) => r,
                Err(e) => {
                    if is_connection_lost(&e) {
                        log::warn!(
                            "[pool] primary connection lost, retrying query on fresh connection"
                        );
                        drop(conn);
                        let mut fresh = pool.get_primary_for_database(database).await?;
                        let r = fresh
                            .execute_query(sql_bytes_to_use)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e))?;
                        pool.put_primary_for_database(fresh, database).await;
                        return Ok(r);
                    }
                    return Err(e);
                }
            };
            pool.put_primary_for_database(conn, database).await;
            if !response.is_error {
                let tables = extract_tables_simple(effective_sql);
                self.cache.invalidate_tables(&tables).await;
            }
            if let Some(ref m) = rule_match {
                if let Some(ref addr) = m.mirror_to {
                    self.fire_mirror(addr, effective_sql).await;
                }
            }
            Ok(response)
        }
    }

    /// Fire-and-forget: send `sql` to a mirror backend asynchronously.
    /// The caller never waits for the mirror response; errors are only logged.
    async fn fire_mirror(&self, addr: &str, sql: &str) {
        let pool = self.mirror.get_or_create(addr).await;
        let sql = sql.to_string();
        tokio::spawn(async move {
            match pool.get().await {
                Ok(mut conn) => {
                    if let Err(e) = conn.execute_query(sql.as_bytes()).await {
                        log::debug!("[mirror] query error: {}", e);
                    } else {
                        pool.put(conn).await;
                    }
                }
                Err(e) => {
                    log::debug!("[mirror] connection error: {}", e);
                }
            }
        });
    }

    /// Return a primary connection to the pool (e.g. on client disconnect).
    pub async fn put_primary(&self, conn: Box<dyn BackendConnection>) {
        self.pool.read().await.put_primary(conn).await;
    }

    /// Database-aware return path for PostgreSQL sessions.
    pub async fn put_primary_for_database(&self, conn: Box<dyn BackendConnection>, database: &str) {
        self.pool
            .read()
            .await
            .put_primary_for_database(conn, Some(database))
            .await;
    }

    // ─── Sticky-hint routing ──────────────────────────────────────────────────

    /// Execute `sql` on a per-session sticky replica connection.
    ///
    /// On the first call the router acquires a replica (falling back to
    /// primary when no replica is available) and stores both the connection
    /// and its pool index in `sticky_conn` / `sticky_idx`.  Subsequent calls
    /// reuse the same backend for read consistency within the hint window.
    ///
    /// Returns the backend response, or an error if the query fails.
    /// The caller must eventually call [`put_replica`] to return the
    /// connection to the pool and avoid leaking the `borrowed` counter.
    pub async fn route_sticky_query(
        &self,
        sql: &[u8],
        sticky_conn: &mut Option<Box<dyn BackendConnection>>,
        sticky_idx: &mut usize,
    ) -> anyhow::Result<BackendResponse> {
        if sticky_conn.is_none() {
            let pool = self.pool.read().await;
            let (conn, idx) = pool.get_replica_for_database(None).await?;
            *sticky_conn = Some(conn);
            *sticky_idx = idx;
            log::debug!("[sticky] acquired backend (replica_idx={})", idx);
        }
        sticky_conn
            .as_mut()
            .unwrap()
            .execute_query(sql)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Return a sticky replica connection to the appropriate pool.
    ///
    /// When `idx == usize::MAX` the connection is returned to the primary
    /// pool (it was acquired via fallback when no replica was available).
    pub async fn put_replica(&self, conn: Box<dyn BackendConnection>, idx: usize) {
        let pool = self.pool.read().await;
        if idx == usize::MAX {
            pool.put_primary_for_database(conn, None).await;
        } else {
            pool.put_replica_for_database(conn, idx, None).await;
        }
    }

    ///
    /// Used for COM_INIT_DB and any non-query, non-stmt command that must
    /// always go to the primary.
    pub async fn route_raw(
        &self,
        packet: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
    ) -> anyhow::Result<BackendResponse> {
        self.route_raw_inner(packet, tx_conn, in_transaction, None)
            .await
    }

    /// PostgreSQL helper: route raw packets using database-scoped pooling.
    #[allow(dead_code)]
    pub async fn route_raw_with_database(
        &self,
        packet: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        database: &str,
    ) -> anyhow::Result<BackendResponse> {
        self.route_raw_inner(packet, tx_conn, in_transaction, Some(database))
            .await
    }

    async fn route_raw_inner(
        &self,
        packet: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        database: Option<&str>,
    ) -> anyhow::Result<BackendResponse> {
        let pool = self.pool.read().await.clone();
        if in_transaction {
            if tx_conn.is_none() {
                *tx_conn = Some(pool.get_primary_for_database(database).await?);
            }
            return tx_conn
                .as_mut()
                .unwrap()
                .send_raw(packet)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e));
        }

        let mut conn = pool.get_primary_for_database(database).await?;
        let response = conn
            .send_raw(packet)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        pool.put_primary_for_database(conn, database).await;
        Ok(response)
    }

    /// Route a prepared-statement command through the session-sticky stmt connection.
    ///
    /// `stmt_conn` is acquired on the first call and kept alive until the caller
    /// drops it.  All COM_STMT_* commands for a given client session MUST use
    /// the same backend connection because stmt_ids are per-connection on the
    /// MySQL backend side.
    ///
    /// When in a transaction, the existing `tx_conn` is reused instead so that
    /// prepared statements inside a transaction see the same session state.
    ///
    /// **Retry:** COM_STMT_PREPARE is retried on a fresh connection if the current
    /// stmt_conn has gone away.  COM_STMT_EXECUTE is NOT retried because the
    /// prepared statement ID is invalidated when the connection dies.
    #[allow(dead_code)]
    pub async fn route_stmt(
        &self,
        packet: &[u8],
        stmt_conn: &mut Option<Box<dyn BackendConnection>>,
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
    ) -> anyhow::Result<BackendResponse> {
        use crate::protocol::mysql::command as cmd;
        let pool = self.pool.read().await.clone();

        // Inside a transaction: use the transaction's sticky connection.
        if in_transaction {
            if tx_conn.is_none() {
                *tx_conn = Some(pool.get_primary().await?);
            }
            return tx_conn
                .as_mut()
                .unwrap()
                .send_raw(packet)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e));
        }

        // Outside a transaction: use (or lazily acquire) the stmt-sticky conn.
        if stmt_conn.is_none() {
            *stmt_conn = Some(pool.get_primary().await?);
        }

        let result = stmt_conn
            .as_mut()
            .unwrap()
            .send_raw(packet)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e));

        match result {
            Ok(r) => Ok(r),
            Err(e) if is_connection_lost(&e) => {
                // Only retry COM_STMT_PREPARE — we can re-prepare on a fresh conn.
                // COM_STMT_EXECUTE cannot be retried because the stmt_id is gone.
                if packet.first().copied() == Some(cmd::COM_STMT_PREPARE) {
                    log::warn!(
                        "[pool] stmt connection lost during PREPARE, retrying on fresh connection"
                    );
                    // Discard the dead connection and open a fresh one.
                    *stmt_conn = Some(pool.get_primary().await?);
                    stmt_conn
                        .as_mut()
                        .unwrap()
                        .send_raw(packet)
                        .await
                        .map_err(|e| anyhow::anyhow!("{}", e))
                } else {
                    // For EXECUTE / CLOSE / RESET: propagate the error.
                    // The session will re-prepare on the next client request.
                    *stmt_conn = None; // force a fresh conn on next use
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ─── Fast-forward mode ────────────────────────────────────────────────────
    // ──────────────────────────────────────────────────────────────────────────

    /// Send SQL directly to the primary backend, skipping all routing logic
    /// (rewriting, query rules, cache, RYOW, replica selection, fingerprinting).
    ///
    /// Used by the `fast_forward = true` code-path in `handle_connection`.  The
    /// caller is responsible for tracking transaction state and passing
    /// `in_transaction` correctly; connection management still uses the shared
    /// pool so idle-timeout eviction and pool-size limits apply as normal.
    pub async fn route_fast(
        &self,
        sql: &[u8],
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
    ) -> anyhow::Result<BackendResponse> {
        let pool = self.pool.read().await.clone();
        if in_transaction {
            // Acquire a sticky primary connection if we don't have one yet.
            if tx_conn.is_none() {
                *tx_conn = Some(pool.get_primary().await?);
            }
            return tx_conn
                .as_mut()
                .unwrap()
                .execute_query(sql)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e));
        }
        // Non-transactional: borrow a connection, execute, return to pool.
        let mut conn = pool.get_primary().await?;
        let resp = conn
            .execute_query(sql)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        pool.put_primary(conn).await;
        Ok(resp)
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ─── GTID-aware RYOW ─────────────────────────────────────────────────────
    // ──────────────────────────────────────────────────────────────────────────

    /// Check whether *any* available replica has applied `write_gtid`.
    ///
    /// Issues `SELECT GTID_SUBSET(?, @@global.gtid_executed)` (non-blocking) on
    /// one replica from the pool.  Returns `true` when the replica is up-to-date
    /// and the read can safely be routed there; `false` when the replica is
    /// lagging or when the pool has no healthy replicas (fall back to primary).
    ///
    /// Errors are treated as `false` (safe fallback).
    pub async fn check_replica_has_gtid(&self, write_gtid: &str) -> bool {
        let pool = self.pool.read().await.clone();
        // If no replicas are configured, there's nothing to check.
        if pool.replicas.is_empty() {
            return false;
        }
        // Get a replica connection for the GTID check.
        let result: anyhow::Result<bool> = (async {
            let (mut conn, idx) = pool.get_replica_for_database(None).await?;
            // Use GTID_SUBSET to check without blocking: returns 1 when
            // write_gtid is a subset of the replica's executed GTID set.
            let sql = format!(
                "SELECT GTID_SUBSET('{}', @@global.gtid_executed)",
                write_gtid.replace('\'', "''") // minimal escaping for single-quotes in GTID
            );
            let resp = conn
                .execute_query(sql.as_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            pool.put_replica_for_database(conn, idx, None).await;
            // A successful result-set response (not an ERR packet) contains a
            // result row.  The first data row starts at byte 9+ in the raw buffer.
            // We look for the byte '1' (0x31) anywhere after the column definition.
            // This is a fast-path heuristic; it works for the 1-column/1-row response
            // that GTID_SUBSET returns.
            if resp.is_error {
                return Ok(false);
            }
            // Search for a row containing "1" (GTID_SUBSET = 1 = replica up-to-date).
            // Row data packets start after column-count + N column defs + EOF.
            // For a simple 1-column result, the first row data starts around byte 20.
            Ok(resp.bytes.windows(1).any(|b| b == b"1"))
        })
        .await;
        result.unwrap_or(false)
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ─── PostgreSQL statement shadowing ───────────────────────────────────────
    // ──────────────────────────────────────────────────────────────────────────

    /// Route a PG extended-query pipeline with transparent re-prepare on
    /// backend connection death.
    ///
    /// The caller must already have called `pg_shadow.apply_scan(&scan)` so
    /// the shadow map reflects the current pipeline's Parse/Close messages.
    ///
    /// On backend death the proxy:
    /// 1. Acquires a fresh `stmt_conn` from the pool.
    /// 2. Re-issues `Parse + Sync` for every named statement in `pg_shadow`
    ///    **except** those that are being freshly prepared by `raw` itself
    ///    (determined via `scan.parses`).
    /// 3. Replays `raw` on the fresh backend.
    #[allow(dead_code)]
    pub async fn route_stmt_pg_shadow(
        &self,
        raw: &[u8],
        scan: &PgPipelineScan,
        shadow: &PgStmtShadow,
        stmt_conn: &mut Option<Box<dyn BackendConnection>>,
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
    ) -> anyhow::Result<BackendResponse> {
        self.route_stmt_pg_shadow_inner(raw, scan, shadow, stmt_conn, tx_conn, in_transaction, None)
            .await
    }

    /// PostgreSQL helper: route extended protocol packets using DB-scoped pools.
    #[allow(clippy::too_many_arguments)]
    pub async fn route_stmt_pg_shadow_with_database(
        &self,
        raw: &[u8],
        scan: &PgPipelineScan,
        shadow: &PgStmtShadow,
        stmt_conn: &mut Option<Box<dyn BackendConnection>>,
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        database: &str,
    ) -> anyhow::Result<BackendResponse> {
        self.route_stmt_pg_shadow_inner(
            raw,
            scan,
            shadow,
            stmt_conn,
            tx_conn,
            in_transaction,
            Some(database),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn route_stmt_pg_shadow_inner(
        &self,
        raw: &[u8],
        scan: &PgPipelineScan,
        shadow: &PgStmtShadow,
        stmt_conn: &mut Option<Box<dyn BackendConnection>>,
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
        database: Option<&str>,
    ) -> anyhow::Result<BackendResponse> {
        let pool = self.pool.read().await.clone();

        if in_transaction {
            if tx_conn.is_none() {
                *tx_conn = Some(pool.get_primary_for_database(database).await?);
            }
            return tx_conn
                .as_mut()
                .unwrap()
                .send_raw(raw)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e));
        }

        if stmt_conn.is_none() {
            *stmt_conn = Some(pool.get_primary_for_database(database).await?);
        }

        let result = stmt_conn
            .as_mut()
            .unwrap()
            .send_raw(raw)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e));

        match result {
            Ok(r) => Ok(r),
            Err(e) if is_connection_lost(&e) => {
                log::warn!(
                    "[pg shadow] stmt_conn lost — re-preparing {} stmts on fresh backend",
                    shadow.open_count()
                );

                let mut fresh = pool.get_primary_for_database(database).await?;

                // Names being freshly prepared by this pipeline (backend will
                // create them via the P messages in `raw`; we must not duplicate).
                let newly_prepared: std::collections::HashSet<String> = scan
                    .parses
                    .iter()
                    .filter(|p| !p.name.is_empty())
                    .map(|p| p.name.clone())
                    .collect();

                // Re-prepare every previously-tracked statement that is NOT in
                // the current pipeline.  Errors are best-effort / logged only.
                let reprepare_list = shadow.build_reprepare_for(&newly_prepared);
                for reprepare_bytes in reprepare_list {
                    if let Err(re) = fresh.send_raw(&reprepare_bytes).await {
                        log::warn!("[pg shadow] re-prepare failed (best effort): {}", re);
                    }
                }

                // Replay the original pipeline on the fresh backend.
                let retry = fresh
                    .send_raw(raw)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e));
                *stmt_conn = Some(fresh);
                retry
            }
            Err(e) => Err(e),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // ─── MySQL statement shadowing ────────────────────────────────────────────
    // ──────────────────────────────────────────────────────────────────────────

    /// Route a MySQL prepared-statement command with proxy-level stmt_id
    /// remapping and transparent re-prepare on backend connection death.
    ///
    /// ## PREPARE
    /// Routes the raw packet, parses the backend's assigned stmt_id from the
    /// response, registers a stable proxy_id in `shadow`, and rewrites the
    /// response before it reaches the client.
    ///
    /// ## EXECUTE / CLOSE / RESET / FETCH / SEND_LONG_DATA
    /// Reads `proxy_id` from the packet, maps to `backend_id`, rewrites the
    /// packet, and routes it.  On EXECUTE connection-loss, re-prepares all
    /// open statements on a fresh backend and retries.
    ///
    /// ## CLOSE
    /// After routing, removes the statement from `shadow` and returns the
    /// connection to the pool when all statements are closed (done by the caller
    /// checking `shadow.is_empty()`).
    pub async fn route_stmt_mysql_shadow(
        &self,
        raw: &[u8],
        shadow: &mut MysqlStmtShadow,
        stmt_conn: &mut Option<Box<dyn BackendConnection>>,
        tx_conn: &mut Option<Box<dyn BackendConnection>>,
        in_transaction: bool,
    ) -> anyhow::Result<BackendResponse> {
        use crate::protocol::mysql::command as cmd;

        let pool = self.pool.read().await.clone();

        let cmd_byte = match raw.first().copied() {
            Some(b) => b,
            None => return Err(anyhow::anyhow!("[mysql shadow] empty stmt packet")),
        };

        // Ensure the sticky connection exists.
        if in_transaction {
            if tx_conn.is_none() {
                *tx_conn = Some(pool.get_primary().await?);
            }
        } else if stmt_conn.is_none() {
            *stmt_conn = Some(pool.get_primary().await?);
        }

        // Helper macro — borrows the correct connection without holding across await.
        macro_rules! active_conn {
            () => {
                if in_transaction {
                    tx_conn.as_mut().unwrap()
                } else {
                    stmt_conn.as_mut().unwrap()
                }
            };
        }

        match cmd_byte {
            // ── COM_STMT_PREPARE ─────────────────────────────────────────────
            cmd::COM_STMT_PREPARE => {
                let query = raw[1..].to_vec();

                let result = active_conn!()
                    .send_raw(raw)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e));

                match result {
                    Ok(mut response) => {
                        if let Some((backend_id, num_columns, num_params)) =
                            mysql_parse_prepare_ok(&response.bytes)
                        {
                            let proxy_id =
                                shadow.register(query, num_params, num_columns, backend_id);
                            mysql_rewrite_prepare_ok(&mut response.bytes, proxy_id);
                            log::debug!(
                                "[mysql shadow] PREPARE proxy_id={} backend_id={}",
                                proxy_id,
                                backend_id
                            );
                        }
                        Ok(response)
                    }
                    Err(e) if is_connection_lost(&e) && !in_transaction => {
                        // Re-try on fresh connection (no state committed yet).
                        log::warn!(
                            "[mysql shadow] PREPARE: connection lost, retrying on fresh connection"
                        );
                        *stmt_conn = Some(pool.get_primary().await?);
                        let mut response = stmt_conn
                            .as_mut()
                            .unwrap()
                            .send_raw(raw)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e))?;
                        if let Some((backend_id, num_columns, num_params)) =
                            mysql_parse_prepare_ok(&response.bytes)
                        {
                            let proxy_id = shadow.register(
                                raw[1..].to_vec(),
                                num_params,
                                num_columns,
                                backend_id,
                            );
                            mysql_rewrite_prepare_ok(&mut response.bytes, proxy_id);
                        }
                        Ok(response)
                    }
                    Err(e) => Err(e),
                }
            }

            // ── COM_STMT_EXECUTE / CLOSE / RESET / FETCH / SEND_LONG_DATA ───
            b if mysql_has_stmt_id(b) => {
                let proxy_id = match mysql_read_stmt_id(raw) {
                    Some(id) => id,
                    None => {
                        return active_conn!()
                            .send_raw(raw)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e));
                    }
                };

                // Map proxy → backend stmt_id (fall back to pass-through for
                // stmts prepared before shadow was enabled).
                let backend_id = shadow.backend_id(proxy_id).unwrap_or(proxy_id);
                let rewritten = mysql_rewrite_stmt_id(raw, backend_id);

                let result = active_conn!()
                    .send_raw(&rewritten)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e));

                // CLOSE: remove from shadow regardless of success.
                if cmd_byte == cmd::COM_STMT_CLOSE {
                    shadow.remove(proxy_id);
                }

                match result {
                    Ok(r) => Ok(r),

                    // On EXECUTE connection-loss: re-prepare all open stmts and retry.
                    Err(e)
                        if cmd_byte == cmd::COM_STMT_EXECUTE
                            && is_connection_lost(&e)
                            && !in_transaction =>
                    {
                        log::warn!(
                            "[mysql shadow] EXECUTE: connection lost — re-preparing {} stmts on fresh backend",
                            shadow.open_count()
                        );

                        let mut fresh = pool.get_primary().await?;

                        // Collect owned reprepare packets before any await.
                        let jobs = shadow.reprepare_jobs();
                        let mut new_ids = std::collections::HashMap::new();
                        for (pid, prep) in &jobs {
                            match fresh.send_raw(prep).await {
                                Ok(resp) => {
                                    if let Some((new_bid, ..)) = mysql_parse_prepare_ok(&resp.bytes)
                                    {
                                        new_ids.insert(*pid, new_bid);
                                        log::debug!(
                                            "[mysql shadow] re-prepared proxy_id={} new_backend_id={}",
                                            pid, new_bid
                                        );
                                    }
                                }
                                Err(re) => {
                                    log::warn!(
                                        "[mysql shadow] re-prepare proxy_id={} failed: {}",
                                        pid,
                                        re
                                    );
                                }
                            }
                        }
                        shadow.update_backend_ids(&new_ids);
                        *stmt_conn = Some(fresh);

                        // Retry EXECUTE with updated backend_id.
                        let new_backend_id = shadow.backend_id(proxy_id).unwrap_or(backend_id);
                        let retry_packet = mysql_rewrite_stmt_id(raw, new_backend_id);
                        stmt_conn
                            .as_mut()
                            .unwrap()
                            .send_raw(&retry_packet)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e))
                    }

                    Err(e) => Err(e),
                }
            }

            // ── Everything else — forward as-is ─────────────────────────────
            _ => active_conn!()
                .send_raw(raw)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e)),
        }
    }
}
