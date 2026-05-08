//! Route handlers for runtime config management (Fase 0.5).
//!
//! All mutations: persist to SQLite → reload in-memory engine → log to config_changes.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;

use crate::config::store::{BackendRow, ChangeRow, RewriteRuleRow, RuleRow, UserRow};

#[derive(Deserialize)]
pub struct BackendProtocolQuery {
    /// mysql | pgsql | postgres | postgresql
    pub protocol: Option<String>,
}

/// Body accepted by POST /api/config/backends (host + port separate, no id)
#[derive(Deserialize)]
pub struct CreateBackendBody {
    pub host: String,
    pub port: u16,
    pub role: String,
    #[serde(default = "default_weight")]
    pub weight: i64,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub database: Option<String>,
}
fn default_weight() -> i64 {
    100
}
use crate::dashboard::AppState;

fn backend_protocol(raw: Option<&str>) -> Result<&'static str, String> {
    match raw.unwrap_or("mysql").trim().to_ascii_lowercase().as_str() {
        "" | "mysql" => Ok("mysql"),
        "pgsql" | "postgres" | "postgresql" => Ok("pgsql"),
        other => Err(format!("invalid protocol '{other}' (use mysql or pgsql)")),
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn ok() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

fn err(msg: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "ok": false, "error": msg.to_string() })),
    )
}

/// Bump the config mutation timestamp so the unsaved-changes badge appears.
fn bump_mutation(s: &crate::dashboard::AppState) {
    s.config_mutation_ts.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
}

// ─── Query Rules CRUD ─────────────────────────────────────────────────────────

pub async fn list_config_rules(State(s): State<AppState>) -> Json<Vec<RuleRow>> {
    let store = s.config_store.as_ref().unwrap();
    Json(store.list_rules().unwrap_or_default())
}

pub async fn create_config_rule(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(row): Json<RuleRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    let id = store.create_rule(&row, &ip).map_err(err)?;
    apply_rules(&s).await.map_err(err)?;
    bump_mutation(&s);
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub async fn update_config_rule(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(row): Json<RuleRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    store.update_rule(id, &row, &ip).map_err(err)?;
    apply_rules(&s).await.map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

pub async fn delete_config_rule(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    store.delete_rule(id, &ip).map_err(err)?;
    apply_rules(&s).await.map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

// ─── Rewrite Rules CRUD ───────────────────────────────────────────────────────

pub async fn list_config_rewrite_rules(State(s): State<AppState>) -> Json<Vec<RewriteRuleRow>> {
    let store = s.config_store.as_ref().unwrap();
    Json(store.list_rewrite_rules().unwrap_or_default())
}

pub async fn create_config_rewrite_rule(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(row): Json<RewriteRuleRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    // Validate regex before persisting.
    if let Err(e) = regex::Regex::new(&row.match_pattern) {
        return Err(err(format!("Invalid regex: {e}")));
    }
    let ip = client_ip(&headers);
    let id = store.create_rewrite_rule(&row, &ip).map_err(err)?;
    apply_rewrite_rules(&s).map_err(err)?;
    bump_mutation(&s);
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub async fn update_config_rewrite_rule(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(row): Json<RewriteRuleRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    if let Err(e) = regex::Regex::new(&row.match_pattern) {
        return Err(err(format!("Invalid regex: {e}")));
    }
    let ip = client_ip(&headers);
    store.update_rewrite_rule(id, &row, &ip).map_err(err)?;
    apply_rewrite_rules(&s).map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

pub async fn delete_config_rewrite_rule(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    store.delete_rewrite_rule(id, &ip).map_err(err)?;
    apply_rewrite_rules(&s).map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

// ─── Backends CRUD ────────────────────────────────────────────────────────────

pub async fn list_config_backends(
    State(s): State<AppState>,
    Query(params): Query<BackendProtocolQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let protocol = backend_protocol(params.protocol.as_deref()).map_err(err)?;
    let rows = store
        .list_backends_by_protocol(protocol)
        .unwrap_or_default();
    // Mask passwords before returning.
    let masked: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id, "addr": r.addr, "user": r.user,
                "password": "***",
                "database": r.database, "role": r.role,
                "weight": r.weight, "backup": r.backup,
                "tls_mode": r.tls_mode, "enabled": r.enabled,
            })
        })
        .collect();
    Ok(Json(serde_json::json!(masked)))
}

pub async fn create_config_backend(
    State(s): State<AppState>,
    Query(params): Query<BackendProtocolQuery>,
    headers: HeaderMap,
    Json(body): Json<CreateBackendBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let protocol = backend_protocol(params.protocol.as_deref()).map_err(err)?;
    let ip = client_ip(&headers);
    let row = BackendRow {
        id: 0,
        addr: format!("{}:{}", body.host, body.port),
        user: body.user,
        password: body.password,
        database: body.database,
        role: body.role,
        weight: body.weight,
        backup: false,
        tls_mode: "off".into(),
        enabled: true,
    };
    let id = store
        .create_backend_with_protocol(&row, &ip, protocol)
        .map_err(err)?;
    apply_backends_for_protocol(&s, protocol)
        .await
        .map_err(err)?;
    bump_mutation(&s);
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub async fn update_config_backend(
    State(s): State<AppState>,
    Query(params): Query<BackendProtocolQuery>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(row): Json<BackendRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let protocol = backend_protocol(params.protocol.as_deref()).map_err(err)?;
    let ip = client_ip(&headers);
    store
        .update_backend_with_protocol(id, &row, &ip, protocol)
        .map_err(err)?;
    apply_backends_for_protocol(&s, protocol)
        .await
        .map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

pub async fn delete_config_backend(
    State(s): State<AppState>,
    Query(params): Query<BackendProtocolQuery>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let protocol = backend_protocol(params.protocol.as_deref()).map_err(err)?;
    let ip = client_ip(&headers);
    store
        .delete_backend_with_protocol(id, &ip, protocol)
        .map_err(err)?;
    apply_backends_for_protocol(&s, protocol)
        .await
        .map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

// ─── Users CRUD ───────────────────────────────────────────────────────────────

pub async fn list_config_users(State(s): State<AppState>) -> Json<serde_json::Value> {
    let store = s.config_store.as_ref().unwrap();
    let rows = store.list_users().unwrap_or_default();
    let masked: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id, "name": r.name, "password": "***",
                "allow_writes": r.allow_writes,
                "max_connections": r.max_connections,
                "enabled": r.enabled,
            })
        })
        .collect();
    Json(serde_json::json!(masked))
}

pub async fn create_config_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(row): Json<UserRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    let id = store.create_user(&row, &ip).map_err(err)?;
    bump_mutation(&s);
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub async fn update_config_user(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(row): Json<UserRow>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    store.update_user(id, &row, &ip).map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

pub async fn delete_config_user(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);
    store.delete_user(id, &ip).map_err(err)?;
    bump_mutation(&s);
    Ok(ok())
}

// ─── Config History ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct HistoryParams {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    50
}

pub async fn config_history(
    State(s): State<AppState>,
    Query(params): Query<HistoryParams>,
) -> Json<Vec<ChangeRow>> {
    let store = s.config_store.as_ref().unwrap();
    Json(store.list_changes(params.limit).unwrap_or_default())
}

// ─── Import (TOML) ────────────────────────────────────────────────────────────

pub async fn import_config(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = s.config_store.as_ref().unwrap();
    let ip = client_ip(&headers);

    // Parse TOML as a ProxyConfig — reuse the existing config types.
    let cfg: crate::config::ProxyConfig =
        toml::from_str(&body).map_err(|e| err(format!("TOML parse error: {e}")))?;

    // Validate all regex patterns before touching the DB.
    for r in &cfg.rewrite_rules {
        regex::Regex::new(&r.match_pattern)
            .map_err(|e| err(format!("Invalid regex {:?}: {e}", r.match_pattern)))?;
    }

    // Replace everything in SQLite.
    store
        .replace_all(
            &cfg.query_rules,
            &cfg.rewrite_rules,
            &cfg.primary,
            &cfg.replicas,
            &cfg.users,
            &ip,
        )
        .map_err(err)?;

    // Hot-reload in-memory engines.
    apply_rules(&s).await.map_err(err)?;
    apply_rewrite_rules(&s).map_err(err)?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "rules": cfg.query_rules.len(),
        "rewrite_rules": cfg.rewrite_rules.len(),
        "backends": 1 + cfg.replicas.len(),
        "users": cfg.users.len(),
    })))
}

// ─── Export (TOML) ────────────────────────────────────────────────────────────

pub async fn export_config(State(s): State<AppState>) -> (StatusCode, String) {
    // Mark the export timestamp
    s.config_export_ts.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    let store = s.config_store.as_ref().unwrap();

    let rules = store.list_rules().unwrap_or_default();
    let rw_rules = store.list_rewrite_rules().unwrap_or_default();
    let backends = store.list_backends().unwrap_or_default();
    let users = store.list_users().unwrap_or_default();

    let mut out = String::from("# TurbineProxy runtime config export\n\n");

    for r in &rules {
        out.push_str("[[query_rules]]\n");
        if let Some(p) = &r.match_pattern {
            out.push_str(&format!("match_pattern = {p:?}\n"));
        }
        if let Some(d) = &r.match_digest {
            out.push_str(&format!("match_digest = {d:?}\n"));
        }
        if !r.user.is_empty() {
            out.push_str(&format!("user = {:?}\n", r.user));
        }
        out.push_str(&format!("destination = {:?}\n", r.destination));
        if !r.comment.is_empty() {
            out.push_str(&format!("comment = {:?}\n", r.comment));
        }
        out.push('\n');
    }

    for r in &rw_rules {
        out.push_str("[[rewrite_rules]]\n");
        out.push_str(&format!("match_pattern = {:?}\n", r.match_pattern));
        if let Some(v) = &r.replace_with {
            out.push_str(&format!("replace_with = {v:?}\n"));
        }
        if let Some(v) = r.add_limit {
            out.push_str(&format!("add_limit = {v}\n"));
        }
        if let Some(v) = r.add_timeout_ms {
            out.push_str(&format!("add_timeout_ms = {v}\n"));
        }
        if r.block {
            out.push_str("block = true\n");
        }
        if !r.comment.is_empty() {
            out.push_str(&format!("comment = {:?}\n", r.comment));
        }
        out.push('\n');
    }

    for b in &backends {
        if b.role == "primary" {
            out.push_str("[primary]\n");
        } else {
            out.push_str("[[replicas]]\n");
        }
        out.push_str(&format!("addr = {:?}\n", b.addr));
        out.push_str(&format!("user = {:?}\n", b.user));
        out.push_str("password = \"***\"  # passwords are not exported for security\n");
        if b.backup {
            out.push_str("backup = true\n");
        }
        if b.weight != 100 {
            out.push_str(&format!("weight = {}\n", b.weight));
        }
        out.push('\n');
    }

    for u in &users {
        out.push_str("[[users]]\n");
        out.push_str(&format!("name = {:?}\n", u.name));
        out.push_str("password = \"***\"  # passwords are not exported for security\n");
        if !u.allow_writes {
            out.push_str("allow_writes = false\n");
        }
        if u.max_connections > 0 {
            out.push_str(&format!("max_connections = {}\n", u.max_connections));
        }
        out.push('\n');
    }

    (StatusCode::OK, out)
}

// ─── Config status (unsaved-changes indicator) ────────────────────────────────

/// `GET /api/config/status` — returns `{ modified: bool }`.
///
/// `modified = true` means at least one write (create/update/delete) has been
/// performed since the last export, i.e. there are in-DB changes not yet
/// reflected in `turbineproxy.toml`.
pub async fn config_status(State(s): State<AppState>) -> Json<serde_json::Value> {
    let export_ts = s
        .config_export_ts
        .load(std::sync::atomic::Ordering::Relaxed);
    let mutation_ts = s
        .config_mutation_ts
        .load(std::sync::atomic::Ordering::Relaxed);
    Json(serde_json::json!({ "modified": mutation_ts > export_ts }))
}

// ─── In-process hot-reload helpers ───────────────────────────────────────────

async fn apply_rules(s: &AppState) -> anyhow::Result<()> {
    let store = s.config_store.as_ref().unwrap();
    let rules = store.active_query_rules()?;
    s.rule_engine.reload_from_slice(&rules).await
}

fn apply_rewrite_rules(s: &AppState) -> anyhow::Result<()> {
    let store = s.config_store.as_ref().unwrap();
    let rules = store.active_rewrite_rules()?;
    s.rewriter.reload_from_slice(&rules)
}

async fn apply_pg_backends(s: &AppState) -> anyhow::Result<()> {
    let store = s
        .config_store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("config store unavailable"))?;

    let pg_router = s
        .pg_proxy_router
        .clone()
        .ok_or_else(|| anyhow::anyhow!("postgresql proxy is not enabled"))?;

    let (primary, replicas) = store.active_backends_for_protocol("pgsql")?;
    let primary = primary.ok_or_else(|| anyhow::anyhow!("pgsql requires one primary backend"))?;

    let (pg_pool_size, pg_idle_secs) = {
        let mut cfg = s.proxy_config.write().unwrap();
        if !cfg.pgsql.enabled {
            return Err(anyhow::anyhow!("pgsql.enabled is false in current config"));
        }
        cfg.pgsql.primary = Some(primary.clone());
        cfg.pgsql.replicas = replicas.clone();
        (cfg.pgsql.pool_size, cfg.pgsql.connection_max_idle_secs)
    };

    let current_pool = pg_router.pool().await;
    let protocol = current_pool.primary.protocol.clone();
    let idle_timeout = if pg_idle_secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(pg_idle_secs))
    };

    let new_pool = std::sync::Arc::new(crate::proxy::pool::BackendPool::with_idle_timeout(
        &primary,
        &replicas,
        pg_pool_size,
        protocol,
        idle_timeout,
    ));
    pg_router.reload_pool(new_pool).await;

    Ok(())
}

async fn apply_mysql_backends(s: &AppState) -> anyhow::Result<()> {
    let store = s
        .config_store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("config store unavailable"))?;

    let (primary, replicas) = store.active_backends_for_protocol("mysql")?;
    let primary = primary.ok_or_else(|| anyhow::anyhow!("mysql requires one primary backend"))?;

    let (pool_size, idle_secs) = {
        let mut cfg = s.proxy_config.write().unwrap();
        cfg.primary = primary.clone();
        cfg.replicas = replicas.clone();
        (cfg.pool_size, cfg.connection_max_idle_secs)
    };

    let current_pool = s.proxy_router.pool().await;
    let protocol = current_pool.primary.protocol.clone();
    let idle_timeout = if idle_secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(idle_secs))
    };

    let new_pool = std::sync::Arc::new(crate::proxy::pool::BackendPool::with_idle_timeout(
        &primary,
        &replicas,
        pool_size,
        protocol,
        idle_timeout,
    ));
    s.proxy_router.reload_pool(new_pool).await;
    Ok(())
}

async fn apply_backends_for_protocol(s: &AppState, protocol: &str) -> anyhow::Result<()> {
    match protocol {
        "mysql" => apply_mysql_backends(s).await,
        "pgsql" => apply_pg_backends(s).await,
        _ => Err(anyhow::anyhow!("unsupported protocol: {protocol}")),
    }
}
