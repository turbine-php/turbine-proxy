//! Axum route handlers — all return JSON.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::{token_hash, AppState};

#[derive(Deserialize)]
pub struct ProtocolQuery {
    /// mysql | pgsql | postgres | postgresql | auto
    pub protocol: Option<String>,
}

fn normalized_protocol(raw: Option<&str>) -> Option<&'static str> {
    match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Some("auto"),
        "mysql" => Some("mysql"),
        "pgsql" | "postgres" | "postgresql" => Some("pgsql"),
        _ => None,
    }
}

fn resolve_protocol(state: &AppState, raw: Option<&str>) -> Option<&'static str> {
    match normalized_protocol(raw) {
        Some("mysql") => Some("mysql"),
        Some("pgsql") => Some("pgsql"),
        Some("auto") => {
            let cfg = state.proxy_config.read().unwrap();
            let mysql_enabled = cfg.mysql_enabled;
            drop(cfg);
            if mysql_enabled {
                Some("mysql")
            } else if state.pg_proxy_router.is_some() {
                Some("pgsql")
            } else {
                Some("mysql")
            }
        }
        _ => None,
    }
}

// ── /api/login ───────────────────────────────────────────────────────────────

/// Compare two strings in constant time (via SHA-256 hashing to equalise
/// length before the comparison). Prevents timing-based credential leaks.
fn ct_eq_str(a: &str, b: &str) -> bool {
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    ha.as_slice().ct_eq(hb.as_slice()).into()
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub ok: bool,
    pub token: Option<String>,
    pub message: Option<String>,
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> (StatusCode, Json<LoginResponse>) {
    // If no credentials configured, auth is open — return a dummy token
    if state.dashboard_username.is_empty() || state.dashboard_password.is_empty() {
        let token = Uuid::new_v4().to_string();
        state.tokens.lock().unwrap().insert(token_hash(&token));
        return (
            StatusCode::OK,
            Json(LoginResponse {
                ok: true,
                token: Some(token),
                message: None,
            }),
        );
    }

    if ct_eq_str(&body.username, &state.dashboard_username)
        && ct_eq_str(&body.password, &state.dashboard_password)
    {
        let token = Uuid::new_v4().to_string();
        state.tokens.lock().unwrap().insert(token_hash(&token));
        (
            StatusCode::OK,
            Json(LoginResponse {
                ok: true,
                token: Some(token),
                message: None,
            }),
        )
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(LoginResponse {
                ok: false,
                token: None,
                message: Some("Invalid credentials".into()),
            }),
        )
    }
}

// ── /api/logout ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogoutRequest {
    pub token: String,
}

pub async fn logout(
    State(state): State<AppState>,
    Json(body): Json<LogoutRequest>,
) -> Json<serde_json::Value> {
    state
        .tokens
        .lock()
        .unwrap()
        .remove(&token_hash(&body.token));
    Json(serde_json::json!({ "ok": true }))
}

// ── /health ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

pub async fn health(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    use axum::http::StatusCode;
    if state.draining.load(Ordering::Relaxed) {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "draining",
                version: env!("CARGO_PKG_VERSION"),
            }),
        )
    } else {
        (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok",
                version: env!("CARGO_PKG_VERSION"),
            }),
        )
    }
}

// ── /api/stats ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatsResponse {
    connections_total: usize,
    connections_active: usize,
    queries_total: usize,
    queries_read: usize,
    queries_write: usize,
    transactions_killed: usize,
    queries_killed: usize,
    sqli_blocked: usize,
    whitelist_blocked: usize,
    /// Unix timestamp of the last successful config reload (0 = never).
    last_reload_secs: u64,
}

pub async fn stats(
    State(state): State<AppState>,
    Query(params): Query<ProtocolQuery>,
) -> Json<serde_json::Value> {
    let Some(protocol) = resolve_protocol(&state, params.protocol.as_deref()) else {
        return Json(serde_json::json!({
            "ok": false,
            "error": "invalid protocol (use mysql, pgsql, or auto)",
        }));
    };

    let enabled = if protocol == "mysql" {
        let cfg = state.proxy_config.read().unwrap();
        !cfg.listen_addr.trim().is_empty()
    } else {
        state.pg_proxy_router.is_some()
    };

    let m = &state.metrics;
    let data = StatsResponse {
        connections_total: m.connections_total.load(Ordering::Relaxed),
        connections_active: m.connections_active.load(Ordering::Relaxed),
        queries_total: m.queries_total.load(Ordering::Relaxed),
        queries_read: m.queries_read.load(Ordering::Relaxed),
        queries_write: m.queries_write.load(Ordering::Relaxed),
        transactions_killed: m.transactions_killed.load(Ordering::Relaxed),
        queries_killed: state.queries_killed.load(Ordering::Relaxed),
        sqli_blocked: m.sqli_blocked.load(Ordering::Relaxed),
        whitelist_blocked: m.whitelist_blocked.load(Ordering::Relaxed),
        last_reload_secs: state.last_reload_secs.load(Ordering::Relaxed),
    };

    Json(serde_json::json!({
        "ok": true,
        "protocol": protocol,
        "enabled": enabled,
        "data": data,
    }))
}

// ── /api/capabilities ───────────────────────────────────────────────────────

/// Frontend capability flags so the dashboard can hide irrelevant sections.
///
/// Notes:
/// - MySQL proxy is always enabled in the current architecture.
/// - PostgreSQL proxy is enabled only when `pgsql.enabled=true` and startup succeeded.
/// - Runtime backend CRUD currently exists only for MySQL.
pub async fn capabilities(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.proxy_config.read().unwrap().clone();
    let mysql_enabled = cfg.mysql_enabled;
    let pg_enabled = state.pg_pool.is_some();
    let dashboard_auth_enabled =
        !state.dashboard_username.is_empty() && !state.dashboard_password.is_empty();

    Json(serde_json::json!({
        "mysql_proxy_enabled": mysql_enabled,
        "pgsql_proxy_enabled": pg_enabled,
        "dashboard_auth_enabled": dashboard_auth_enabled,
        "mysql_runtime_backends_supported": true,
        "pgsql_runtime_backends_supported": true,
        "group_replication_enabled": cfg.group_replication.enabled,
    }))
}

// ── /api/queries ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct QueryRow {
    fingerprint: String,
    count: i64,
    avg_us: i64,
    min_us: Option<i64>,
    max_us: Option<i64>,
    p95_us: Option<i64>,
    p99_us: Option<i64>,
    last_seen: Option<String>,
}

#[derive(Default)]
struct QueryAccum {
    count: i64,
    total_us: i64,
    min_us: Option<i64>,
    max_us: Option<i64>,
    p95_us: Option<i64>,
    p99_us: Option<i64>,
    last_seen: Option<String>,
}

fn opt_min(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

fn opt_max(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

fn max_last_seen(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y > x { y } else { x }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn upsert_acc(
    acc: &mut HashMap<String, QueryAccum>,
    fingerprint: String,
    count: i64,
    total_us: i64,
    min_us: Option<i64>,
    max_us: Option<i64>,
    p95_us: Option<i64>,
    p99_us: Option<i64>,
    last_seen: Option<String>,
) {
    let e = acc.entry(fingerprint).or_default();
    e.count += count;
    e.total_us += total_us;
    e.min_us = opt_min(e.min_us, min_us);
    e.max_us = opt_max(e.max_us, max_us);
    e.p95_us = opt_max(e.p95_us, p95_us);
    e.p99_us = opt_max(e.p99_us, p99_us);
    e.last_seen = max_last_seen(e.last_seen.take(), last_seen);
}

async fn collect_query_rows(state: &AppState, by_p95: bool) -> Vec<QueryRow> {
    let mut acc: HashMap<String, QueryAccum> = HashMap::new();

    if let Some(storage) = &state.storage {
        let s = Arc::clone(storage);
        let stored = tokio::task::spawn_blocking(move || {
            if by_p95 {
                s.get_top_by_p95(200)
            } else {
                s.get_top_by_count(200)
            }
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();

        for r in stored {
            upsert_acc(
                &mut acc,
                r.fingerprint,
                r.count,
                r.total_us,
                r.min_us,
                r.max_us,
                r.p95_us,
                r.p99_us,
                r.last_seen,
            );
        }
    }

    for s in state.collector.get_stats().await {
        upsert_acc(
            &mut acc,
            s.fingerprint.clone(),
            s.count as i64,
            s.total_us as i64,
            Some(s.min_us as i64),
            Some(s.max_us as i64),
            Some(s.p95().as_micros() as i64),
            Some(s.p99().as_micros() as i64),
            Some(s.last_seen.to_rfc3339()),
        );
    }

    let mut rows: Vec<QueryRow> = acc
        .into_iter()
        .map(|(fingerprint, a)| QueryRow {
            fingerprint,
            count: a.count,
            avg_us: if a.count > 0 { a.total_us / a.count } else { 0 },
            min_us: a.min_us,
            max_us: a.max_us,
            p95_us: a.p95_us,
            p99_us: a.p99_us,
            last_seen: a.last_seen,
        })
        .collect();

    if by_p95 {
        rows.sort_by(|a, b| b.p95_us.unwrap_or(0).cmp(&a.p95_us.unwrap_or(0)));
    } else {
        rows.sort_by(|a, b| b.count.cmp(&a.count));
    }
    rows.truncate(50);
    rows
}

pub async fn queries(
    State(state): State<AppState>,
    Query(params): Query<ProtocolQuery>,
) -> Json<serde_json::Value> {
    let Some(protocol) = resolve_protocol(&state, params.protocol.as_deref()) else {
        return Json(serde_json::json!({
            "ok": false,
            "error": "invalid protocol (use mysql, pgsql, or auto)",
        }));
    };
    let enabled = if protocol == "mysql" {
        let cfg = state.proxy_config.read().unwrap();
        !cfg.listen_addr.trim().is_empty()
    } else {
        state.pg_proxy_router.is_some()
    };

    if protocol == "pgsql" && state.pg_proxy_router.is_none() {
        return Json(serde_json::json!({
            "ok": true,
            "protocol": protocol,
            "enabled": enabled,
            "data": Vec::<QueryRow>::new(),
        }));
    }

    let rows = collect_query_rows(&state, false).await;
    Json(serde_json::json!({
        "ok": true,
        "protocol": protocol,
        "enabled": enabled,
        "data": rows,
    }))
}

// ── /api/slow-queries ────────────────────────────────────────────────────────

pub async fn slow_queries(
    State(state): State<AppState>,
    Query(params): Query<ProtocolQuery>,
) -> Json<serde_json::Value> {
    let Some(protocol) = resolve_protocol(&state, params.protocol.as_deref()) else {
        return Json(serde_json::json!({
            "ok": false,
            "error": "invalid protocol (use mysql, pgsql, or auto)",
        }));
    };
    let enabled = if protocol == "mysql" {
        let cfg = state.proxy_config.read().unwrap();
        !cfg.listen_addr.trim().is_empty()
    } else {
        state.pg_proxy_router.is_some()
    };

    if protocol == "pgsql" && state.pg_proxy_router.is_none() {
        return Json(serde_json::json!({
            "ok": true,
            "protocol": protocol,
            "enabled": enabled,
            "data": Vec::<QueryRow>::new(),
        }));
    }

    let rows = collect_query_rows(&state, true).await;
    Json(serde_json::json!({
        "ok": true,
        "protocol": protocol,
        "enabled": enabled,
        "data": rows,
    }))
}

// ── /api/n1 ──────────────────────────────────────────────────────────────────

pub async fn n1_patterns(State(state): State<AppState>) -> Json<Vec<crate::proxy::n1::N1Pattern>> {
    Json(state.n1_store.get_all())
}

// ── /api/pool ────────────────────────────────────────────────────────────────

pub async fn pool_stats(
    State(state): State<AppState>,
    Query(params): Query<ProtocolQuery>,
) -> Json<serde_json::Value> {
    let Some(protocol) = resolve_protocol(&state, params.protocol.as_deref()) else {
        return Json(serde_json::json!({
            "ok": false,
            "error": "invalid protocol (use mysql, pgsql, or auto)",
        }));
    };

    if protocol == "mysql" {
        let (pool, backends) = tokio::join!(state.pool.pool_stats(), state.pool.backend_stats());
        return Json(serde_json::json!({
            "ok": true,
            "protocol": "mysql",
            "enabled": true,
            "pool": pool,
            "backends": backends,
            "copy_active": 0,
        }));
    }

    let copy_active = state.pg_copy_active.load(Ordering::Relaxed);
    match state.pg_proxy_router.clone() {
        Some(router) => {
            let pool = router.pool().await;
            let (stats, backends) = tokio::join!(pool.pool_stats(), pool.backend_stats());
            Json(serde_json::json!({
                "ok": true,
                "protocol": "pgsql",
                "enabled": true,
                "pool": stats,
                "backends": backends,
                "copy_active": copy_active,
            }))
        }
        None => Json(serde_json::json!({
            "ok": true,
            "protocol": "pgsql",
            "enabled": false,
            "pool": serde_json::Value::Null,
            "backends": Vec::<serde_json::Value>::new(),
            "copy_active": copy_active,
        })),
    }
}

// ── /api/users ───────────────────────────────────────────────────────────────

pub async fn user_stats(State(state): State<AppState>) -> Json<Vec<UserStatRow>> {
    let snapshot = state.user_registry.snapshot().await;
    Json(
        snapshot
            .into_iter()
            .map(|(username, stats)| UserStatRow {
                username,
                connections_active: stats.connections_active,
                connections_total: stats.connections_total,
                queries_total: stats.queries_total,
                last_seen: stats.last_seen,
                allow_writes: stats.allow_writes,
            })
            .collect(),
    )
}

#[derive(serde::Serialize)]
pub struct UserStatRow {
    pub username: String,
    pub connections_active: usize,
    pub connections_total: usize,
    pub queries_total: usize,
    pub last_seen: Option<String>,
    pub allow_writes: bool,
}

// ── /api/query-rules ─────────────────────────────────────────────────────────

pub async fn query_rules(
    State(state): State<AppState>,
) -> Json<Vec<crate::proxy::rules::RuleStats>> {
    Json(state.rule_engine.snapshot().await)
}

// ── /api/query-rules/reload ───────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct ReloadResponse {
    ok: bool,
    message: String,
}

pub async fn reload_rules(State(state): State<AppState>) -> Json<ReloadResponse> {
    match state.rule_engine.reload_from_file().await {
        Ok(()) => Json(ReloadResponse {
            ok: true,
            message: "rules reloaded".to_string(),
        }),
        Err(e) => Json(ReloadResponse {
            ok: false,
            message: e.to_string(),
        }),
    }
}

// ── /api/reload ──────────────────────────────────────────────────────────────

/// Full config reload — reloads both query_rules and rewrite_rules atomically.
/// Equivalent to sending SIGHUP to the process, exposed as an HTTP endpoint.
pub async fn reload_config(State(state): State<AppState>) -> Json<ReloadResponse> {
    let r1 = state.rule_engine.reload_from_file().await;
    let r2 = state.rewriter.reload_from_file().await;

    let ok = r1.is_ok() && r2.is_ok();
    let message = match (r1, r2) {
        (Ok(()), Ok(())) => {
            // Update the last-reloaded timestamp in shared state.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            state.last_reload_secs.store(now, Ordering::Relaxed);

            // Push rules reload to cluster peers.
            if !state.cluster.peers.is_empty() && !state.cluster.secret.is_empty() {
                let toml_text = std::fs::read_to_string(&state.config_path).unwrap_or_default();
                push_config_to_peers(
                    state.cluster.peers.clone(),
                    state.cluster.secret.clone(),
                    toml_text,
                );
            }

            "query_rules and rewrite_rules reloaded".to_string()
        }
        (Err(e), Ok(())) => format!("rewrite_rules ok; query_rules failed: {}", e),
        (Ok(()), Err(e)) => format!("query_rules ok; rewrite_rules failed: {}", e),
        (Err(e1), Err(e2)) => format!("both failed — rules: {}; rewriter: {}", e1, e2),
    };

    Json(ReloadResponse { ok, message })
}

/// Reload backends (primary + replicas) from the config file without restarting.
/// In-flight queries finish on the old pool; new queries use the refreshed pool.
pub async fn reload_backends(State(state): State<AppState>) -> Json<ReloadResponse> {
    // Re-read the config file so we pick up address / credential changes.
    let config_path = &state.config_path;
    let result: anyhow::Result<crate::config::ProxyConfig> = (|| {
        let text = std::fs::read_to_string(config_path)
            .map_err(|e| anyhow::anyhow!("read {}: {}", config_path, e))?;
        let cfg: crate::config::ProxyConfig =
            toml::from_str(&text).map_err(|e| anyhow::anyhow!("parse {}: {}", config_path, e))?;
        Ok(cfg)
    })();

    match result {
        Err(e) => Json(ReloadResponse {
            ok: false,
            message: format!("config read error: {}", e),
        }),
        Ok(new_cfg) => {
            let primary_addr = new_cfg.primary.addr.clone();
            let replica_count = new_cfg.replicas.len();

            // Swap the proxy_config so SIGHUP and future reloads see the latest.
            *state.proxy_config.write().unwrap() = new_cfg.clone();

            // Hot-swap the backend pool inside the router.
            let idle_timeout = if new_cfg.connection_max_idle_secs == 0 {
                None
            } else {
                Some(std::time::Duration::from_secs(
                    new_cfg.connection_max_idle_secs,
                ))
            };
            let new_pool = Arc::new(crate::proxy::pool::BackendPool::with_options(
                &new_cfg.primary,
                &new_cfg.replicas,
                new_cfg.pool_size,
                // Re-use the protocol from the existing pool (it's Arc-cloned).
                state.pool.primary.protocol.clone(),
                idle_timeout,
                new_cfg.ha.circuit_breaker_threshold,
                new_cfg.ha.circuit_breaker_recovery_ms,
                new_cfg.pool_wait_queue_size,
                new_cfg.pool_wait_timeout_ms,
            ));
            state.proxy_router.reload_pool(new_pool).await;

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            state
                .last_reload_secs
                .store(now, std::sync::atomic::Ordering::Relaxed);

            log::info!(
                "[reload] backends reloaded: primary={} replicas={}",
                primary_addr,
                replica_count
            );

            // Push to cluster peers (fire-and-forget; failures are logged, not returned).
            if !state.cluster.peers.is_empty() && !state.cluster.secret.is_empty() {
                let toml_text = std::fs::read_to_string(&state.config_path).unwrap_or_default();
                push_config_to_peers(
                    state.cluster.peers.clone(),
                    state.cluster.secret.clone(),
                    toml_text,
                );
            }

            Json(ReloadResponse {
                ok: true,
                message: format!(
                    "backends reloaded — primary={} replicas={}",
                    primary_addr, replica_count
                ),
            })
        }
    }
}

// ── /api/sync  (TurbineProxy Cluster — peer config sync) ──────────────────────

/// Request body accepted by `POST /api/sync`.
/// Peers send the raw TOML config text so the receiving node can apply it
/// without re-reading the local disk (useful when config lives in a secret
/// manager or is passed via environment variable).
#[derive(Deserialize)]
pub struct SyncRequest {
    pub config_toml: String,
}

/// Handler for inbound cluster sync requests from peer nodes.
///
/// Authentication: `Authorization: Bearer <secret>` header.
/// The secret must match `cluster.secret` in this node's config.
pub async fn cluster_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SyncRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    // ── Authentication ────────────────────────────────────────────────────────
    let secret = &state.cluster.secret;
    if secret.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "message": "cluster sync disabled on this node"})),
        );
    }
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if provided != secret.as_str() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"ok": false, "message": "invalid cluster secret"})),
        );
    }

    // ── Parse and apply the incoming config ───────────────────────────────────
    let new_cfg: crate::config::ProxyConfig = match toml::from_str(&body.config_toml) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"ok": false, "message": format!("config parse error: {}", e)}),
                ),
            );
        }
    };

    let primary_addr = new_cfg.primary.addr.clone();
    let replica_count = new_cfg.replicas.len();

    *state.proxy_config.write().unwrap() = new_cfg.clone();

    let idle_timeout = if new_cfg.connection_max_idle_secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(
            new_cfg.connection_max_idle_secs,
        ))
    };
    let new_pool = Arc::new(crate::proxy::pool::BackendPool::with_options(
        &new_cfg.primary,
        &new_cfg.replicas,
        new_cfg.pool_size,
        state.pool.primary.protocol.clone(),
        idle_timeout,
        new_cfg.ha.circuit_breaker_threshold,
        new_cfg.ha.circuit_breaker_recovery_ms,
        new_cfg.pool_wait_queue_size,
        new_cfg.pool_wait_timeout_ms,
    ));
    state.proxy_router.reload_pool(new_pool).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    state.last_reload_secs.store(now, Ordering::Relaxed);

    log::info!(
        "[cluster] sync applied from peer — primary={} replicas={}",
        primary_addr,
        replica_count,
    );

    let pg_enabled = state.pg_pool.is_some();
    let pg_pool_size = if let Some(ref pool) = state.pg_pool {
        let stats = pool.pool_stats().await;
        stats.primary_idle + stats.primary_in_use
    } else {
        0
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "message": format!("applied — primary={} replicas={}", primary_addr, replica_count),
            "pg_proxy_enabled": pg_enabled,
            "pg_pool_size": pg_pool_size,
        })),
    )
}

/// Fire-and-forget: push the current config TOML to all configured peers.
/// Failures are logged but never propagate to the caller.
pub fn push_config_to_peers(peers: Vec<String>, secret: String, config_toml: String) {
    if peers.is_empty() || secret.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[cluster] failed to build HTTP client: {}", e);
                return;
            }
        };
        for peer in &peers {
            let url = format!("{}/api/sync", peer.trim_end_matches('/'));
            match client
                .post(&url)
                .header("Authorization", format!("Bearer {}", secret))
                .header("Content-Type", "application/json")
                .body(serde_json::json!({"config_toml": config_toml}).to_string())
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    log::info!("[cluster] synced config to peer {}", peer);
                }
                Ok(resp) => {
                    log::warn!(
                        "[cluster] peer {} rejected sync — status {}",
                        peer,
                        resp.status(),
                    );
                }
                Err(e) => {
                    log::warn!("[cluster] failed to reach peer {}: {}", peer, e);
                }
            }
        }
    });
}

// ── /api/backends ─────────────────────────────────────────────────────────────

pub async fn backend_stats(
    State(state): State<AppState>,
    Query(params): Query<ProtocolQuery>,
) -> Json<Vec<crate::proxy::pool::BackendStat>> {
    let Some(protocol) = resolve_protocol(&state, params.protocol.as_deref()) else {
        return Json(Vec::new());
    };

    if protocol == "mysql" {
        return Json(state.pool.backend_stats().await);
    }

    match state.pg_proxy_router.clone() {
        Some(router) => {
            let pool = router.pool().await;
            Json(pool.backend_stats().await)
        }
        None => Json(Vec::new()),
    }
}

// ── /api/cluster ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ClusterMemberView {
    pub addr: String,
    pub role: String,
    pub state: String,
    pub version: Option<String>,
    pub healthy: Option<bool>,
    pub lag_ms: Option<u64>,
    pub consecutive_failures: Option<u32>,
}

#[derive(Serialize)]
pub struct ClusterProtocolView {
    pub protocol: &'static str,
    /// mysql: standalone|group_replication; pgsql: standalone|ha
    pub mode: &'static str,
    pub enabled: bool,
    pub primary_addr: Option<String>,
    pub failover_active: bool,
    pub patroni_check: Option<bool>,
    pub members: Vec<ClusterMemberView>,
}

#[derive(Serialize)]
pub struct ClusterStateResponse {
    pub ok: bool,
    pub protocol: String,
    pub mysql: Option<ClusterProtocolView>,
    pub pgsql: Option<ClusterProtocolView>,
}

pub async fn cluster_state(
    State(state): State<AppState>,
    Query(params): Query<ProtocolQuery>,
) -> Json<ClusterStateResponse> {
    let requested = normalized_protocol(params.protocol.as_deref()).unwrap_or("auto");
    let cfg = state.proxy_config.read().unwrap().clone();

    let mysql_enabled = !cfg.listen_addr.trim().is_empty();
    let pg_enabled = state.pg_proxy_router.is_some();

    let mysql_view = async {
        let members = state.pool.gr_members.lock().await.clone();
        let mode = if members.is_empty() {
            "standalone"
        } else {
            "group_replication"
        };
        let primary_addr = members
            .iter()
            .find(|m| m.role == "PRIMARY" && m.state == "ONLINE")
            .map(|m| m.addr.clone())
            .or_else(|| Some(state.pool.primary_addr()));
        let failover_active = state.pool.failover_idx.load(Ordering::Relaxed) >= 0;
        let members = members
            .into_iter()
            .map(|m| ClusterMemberView {
                addr: m.addr,
                role: m.role,
                state: m.state,
                version: if m.version.is_empty() {
                    None
                } else {
                    Some(m.version)
                },
                healthy: None,
                lag_ms: None,
                consecutive_failures: None,
            })
            .collect();

        ClusterProtocolView {
            protocol: "mysql",
            mode,
            enabled: mysql_enabled,
            primary_addr,
            failover_active,
            patroni_check: None,
            members,
        }
    };

    let pg_view = async {
        if let Some(router) = state.pg_proxy_router.clone() {
            let pool = router.pool().await;
            let backends = pool.backend_stats().await;
            let failover_active = pool.failover_idx.load(Ordering::Relaxed) >= 0;
            let primary_addr = Some(pool.primary_addr());
            let mode = if backends.iter().any(|b| b.role == "replica") || failover_active {
                "ha"
            } else {
                "standalone"
            };

            let members = backends
                .into_iter()
                .map(|b| ClusterMemberView {
                    addr: b.addr,
                    role: b.role.to_uppercase(),
                    state: if b.healthy {
                        "ONLINE".to_string()
                    } else {
                        "UNHEALTHY".to_string()
                    },
                    version: None,
                    healthy: Some(b.healthy),
                    lag_ms: if b.role == "replica" {
                        Some(b.lag_ms)
                    } else {
                        None
                    },
                    consecutive_failures: Some(b.consecutive_failures),
                })
                .collect();

            ClusterProtocolView {
                protocol: "pgsql",
                mode,
                enabled: pg_enabled,
                primary_addr,
                failover_active,
                patroni_check: Some(cfg.pgsql.patroni_check),
                members,
            }
        } else {
            ClusterProtocolView {
                protocol: "pgsql",
                mode: "standalone",
                enabled: false,
                primary_addr: None,
                failover_active: false,
                patroni_check: Some(cfg.pgsql.patroni_check),
                members: Vec::new(),
            }
        }
    };

    let (mysql_v, pg_v) = tokio::join!(mysql_view, pg_view);

    let (mysql, pgsql) = match requested {
        "mysql" => (Some(mysql_v), None),
        "pgsql" => (None, Some(pg_v)),
        _ => (Some(mysql_v), Some(pg_v)),
    };

    Json(ClusterStateResponse {
        ok: true,
        protocol: requested.to_string(),
        mysql,
        pgsql,
    })
}

#[derive(Deserialize)]
pub struct ClusterActionRequest {
    pub protocol: String,
    pub action: String,
    /// Set to true to bypass the safety guard that prevents trigger_failover
    /// when the current primary is still healthy.
    #[serde(default)]
    pub force: bool,
}

pub async fn cluster_action(
    State(state): State<AppState>,
    Json(body): Json<ClusterActionRequest>,
) -> Json<serde_json::Value> {
    let protocol = match normalized_protocol(Some(body.protocol.as_str())) {
        Some("mysql") => "mysql",
        Some("pgsql") => "pgsql",
        _ => {
            return Json(serde_json::json!({
                "ok": false,
                "error": "invalid protocol (use mysql or pgsql)",
            }));
        }
    };

    let action = body.action.trim().to_ascii_lowercase();

    async fn recheck_pool_health(pool: std::sync::Arc<crate::proxy::pool::BackendPool>) {
        let timeout = std::time::Duration::from_secs(3);

        let primary_ok = tokio::time::timeout(timeout, async {
            match pool
                .primary
                .protocol
                .connect_backend(&pool.primary.config)
                .await
            {
                Ok(mut conn) => conn.ping().await.is_ok(),
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false);

        if primary_ok {
            pool.primary_health.healthy.store(true, Ordering::Relaxed);
            pool.primary_health
                .consecutive_failures
                .store(0, Ordering::Relaxed);
        } else {
            pool.primary_health.healthy.store(false, Ordering::Relaxed);
            pool.primary_health
                .consecutive_failures
                .fetch_add(1, Ordering::Relaxed);
        }

        for (i, replica) in pool.replicas.iter().enumerate() {
            let ok = tokio::time::timeout(timeout, async {
                match replica.protocol.connect_backend(&replica.config).await {
                    Ok(mut conn) => conn.ping().await.is_ok(),
                    Err(_) => false,
                }
            })
            .await
            .unwrap_or(false);

            if ok {
                pool.replica_health[i]
                    .healthy
                    .store(true, Ordering::Relaxed);
                pool.replica_health[i]
                    .consecutive_failures
                    .store(0, Ordering::Relaxed);
            } else {
                pool.replica_health[i]
                    .healthy
                    .store(false, Ordering::Relaxed);
                pool.replica_health[i]
                    .consecutive_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn trigger_failover(
        pool: std::sync::Arc<crate::proxy::pool::BackendPool>,
        force: bool,
    ) -> Result<String, String> {
        // Safety guard: refuse to failover if the primary is still healthy
        // unless the caller explicitly sets force = true.
        if !force && pool.primary_health.healthy.load(Ordering::Relaxed) {
            return Err("primary is currently healthy — set force:true to override".to_string());
        }

        let candidate = pool
            .replicas
            .iter()
            .enumerate()
            .filter(|(i, _)| pool.replica_health[*i].healthy.load(Ordering::Relaxed))
            .min_by_key(|(i, _)| pool.replica_health[*i].lag_ms.load(Ordering::Relaxed));

        match candidate {
            Some((idx, r)) => {
                pool.failover_idx.store(idx as i64, Ordering::Relaxed);
                Ok(format!(
                    "failover set to replica [{}] {}",
                    idx, r.config.addr
                ))
            }
            None => Err("no healthy replica available for failover".to_string()),
        }
    }

    let force = body.force;
    let result_msg = if protocol == "mysql" {
        let pool = state.pool.clone();
        match action.as_str() {
            "recheck_health" => {
                recheck_pool_health(pool).await;
                "health recheck completed".to_string()
            }
            "trigger_failover" => match trigger_failover(pool, force).await {
                Ok(msg) => msg,
                Err(e) => return Json(serde_json::json!({ "ok": false, "error": e })),
            },
            "clear_failover" => {
                pool.failover_idx.store(-1, Ordering::Relaxed);
                "failover cleared".to_string()
            }
            _ => {
                return Json(serde_json::json!({
                    "ok": false,
                    "error": "invalid action (use recheck_health, trigger_failover, clear_failover)",
                }))
            }
        }
    } else {
        let Some(router) = state.pg_proxy_router.clone() else {
            return Json(serde_json::json!({ "ok": false, "error": "pgsql proxy is disabled" }));
        };
        let pool = router.pool().await;
        match action.as_str() {
            "recheck_health" => {
                recheck_pool_health(pool).await;
                "health recheck completed".to_string()
            }
            "trigger_failover" => match trigger_failover(pool, force).await {
                Ok(msg) => msg,
                Err(e) => return Json(serde_json::json!({ "ok": false, "error": e })),
            },
            "clear_failover" => {
                pool.failover_idx.store(-1, Ordering::Relaxed);
                "failover cleared".to_string()
            }
            _ => {
                return Json(serde_json::json!({
                    "ok": false,
                    "error": "invalid action (use recheck_health, trigger_failover, clear_failover)",
                }))
            }
        }
    };

    Json(serde_json::json!({
        "ok": true,
        "protocol": protocol,
        "action": action,
        "message": result_msg,
    }))
}

// ── /api/rewrite-rules ────────────────────────────────────────────────────────

pub async fn rewrite_rules(
    State(state): State<AppState>,
) -> Json<Vec<crate::proxy::rewriter::RewriteRuleStat>> {
    Json(state.rewriter.snapshot())
}

// ── /metrics (Prometheus text exposition) ────────────────────────────────────

pub async fn metrics(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    let body = crate::dashboard::prometheus::render(&state.metrics, &state.pool).await;
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

// ── /api/transactions ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TransactionQuery {
    /// Maximum number of traces to return (default 50, max 500).
    limit: Option<usize>,
    /// Filter to traces matching a specific transaction fingerprint.
    fingerprint: Option<String>,
}

#[derive(Serialize)]
pub struct TransactionsResponse {
    traces: Vec<crate::proxy::tracer::TransactionTrace>,
    fingerprints: Vec<FingerprintStat>,
}

#[derive(Serialize)]
pub struct FingerprintStat {
    fingerprint: String,
    count: usize,
}

pub async fn transactions(
    State(state): State<AppState>,
    Query(params): Query<TransactionQuery>,
) -> Json<TransactionsResponse> {
    let limit = params.limit.unwrap_or(50).min(500);
    let fp = params.fingerprint.as_deref();
    let traces = state.tracer_store.snapshot(limit, fp);
    let fingerprints = state
        .tracer_store
        .fingerprint_counts()
        .into_iter()
        .map(|(fingerprint, count)| FingerprintStat { fingerprint, count })
        .collect();
    Json(TransactionsResponse {
        traces,
        fingerprints,
    })
}

// ── /api/analytics ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AnalyticsResponse {
    users: Vec<crate::proxy::app_analytics::DimEntry>,
    ips: Vec<crate::proxy::app_analytics::DimEntry>,
    apps: Vec<crate::proxy::app_analytics::DimEntry>,
}

pub async fn analytics(State(state): State<AppState>) -> Json<AnalyticsResponse> {
    let (users, ips, apps) = tokio::join!(
        state.app_analytics.snapshot_users(),
        state.app_analytics.snapshot_ips(),
        state.app_analytics.snapshot_apps(),
    );
    Json(AnalyticsResponse { users, ips, apps })
}

// ── /api/heatmap ─────────────────────────────────────────────────────────────

pub async fn heatmap(
    State(state): State<AppState>,
) -> Json<crate::proxy::heatmap::HeatmapSnapshot> {
    Json(state.heatmap.snapshot())
}
// ── /api/timeseries ────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TimeseriesQuery {
    #[serde(default = "default_ts_resolution")]
    resolution: String,
    #[serde(default = "default_ts_limit")]
    limit: usize,
}

fn default_ts_resolution() -> String {
    "1h".to_string()
}
fn default_ts_limit() -> usize {
    168
}

#[derive(Serialize)]
pub struct TimeseriesResponse {
    resolution: String,
    points: Vec<crate::analytics::timeseries::TsPoint>,
}

pub async fn timeseries(
    State(state): State<AppState>,
    Query(q): Query<TimeseriesQuery>,
) -> Json<TimeseriesResponse> {
    let resolution = match q.resolution.as_str() {
        "1m" | "1h" | "1d" => q.resolution.clone(),
        _ => "1h".to_string(),
    };
    let points = state
        .timeseries
        .as_ref()
        .and_then(|ts| ts.query(&resolution, q.limit).ok())
        .unwrap_or_default();
    Json(TimeseriesResponse { resolution, points })
}

// ── /api/regressions ──────────────────────────────────────────────────────────

pub async fn regressions(
    State(state): State<AppState>,
) -> Json<Vec<crate::proxy::regression::RegressionAlert>> {
    Json(state.regression_store.snapshot())
}

// ── POST /api/stats/flush ─────────────────────────────────────────────────────

/// Reset the rolling query/error counters shown on the Overview dashboard.
/// Useful after maintenance windows to start fresh baselines.
/// Does NOT reset connection counters (connections_total, connections_active).
pub async fn flush_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    let m = &state.metrics;
    m.queries_total.store(0, Ordering::Relaxed);
    m.queries_read.store(0, Ordering::Relaxed);
    m.queries_write.store(0, Ordering::Relaxed);
    m.sqli_blocked.store(0, Ordering::Relaxed);
    m.whitelist_blocked.store(0, Ordering::Relaxed);
    m.transactions_killed.store(0, Ordering::Relaxed);
    log::info!("Stats flushed via dashboard API");
    Json(serde_json::json!({ "ok": true, "message": "Stats flushed" }))
}

// ── GET /api/config/tls ────────────────────────────────────────────────────────

/// Return information about the configured frontend TLS certificate.
/// Returns `{ "enabled": false }` when TLS is not configured.
pub async fn tls_cert_info(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.proxy_config.read().unwrap();
    let tls = &config.frontend_tls;
    if !tls.enabled || tls.cert.is_empty() {
        return Json(serde_json::json!({ "enabled": false }));
    }
    let cert_path = tls.cert.clone();
    let key_path = tls.key.clone();
    drop(config); // release lock before I/O
    let readable = std::fs::metadata(&cert_path).is_ok();
    Json(serde_json::json!({
        "enabled": true,
        "cert_path": cert_path,
        "key_path": key_path,
        "readable": readable,
    }))
}
