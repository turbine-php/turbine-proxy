//! Web dashboard — Axum HTTP server.
//!
//! Serves the React SPA from `dashboard/dist/` in production.
//! In development, the Vite dev server (port 5173) proxies /api/* here.

pub mod grafana;
pub mod mcp;
pub mod prometheus;
pub mod routes;
pub mod routes_config;
pub mod routes_errors;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use sha2::{Digest, Sha256};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::analytics::timeseries::TimeseriesStore;
use crate::analytics::{AnalyticsStorage, Collector};
use crate::config::ConfigStore;
use crate::config::ProxyConfig;
use crate::proxy::app_analytics::AppAnalyticsStore;
use crate::proxy::error_events::ErrorEventStore;
use crate::proxy::heatmap::HeatmapStore;
use crate::proxy::n1::N1Store;
use crate::proxy::pool::BackendPool;
use crate::proxy::regression::RegressionStore;
use crate::proxy::rewriter::Rewriter;
use crate::proxy::router::Router as ProxyRouter;
use crate::proxy::rules::RuleEngine;
use crate::proxy::server::ProxyMetrics;
use crate::proxy::tracer::TracerStore;
use crate::proxy::user_registry::UserRegistry;

/// Role attached to a dashboard session token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRole {
    /// Full access — can read and modify config.
    Admin,
    /// Read-only access — blocked from POST/PUT/DELETE endpoints.
    ReadOnly,
}

/// Entry stored per hashed token.
pub struct TokenEntry {
    /// `None` = never expires.
    pub expires_at: Option<std::time::Instant>,
    pub role: TokenRole,
}

/// In-memory map of hashed token → entry (TTL + role).
pub type TokenStore = Arc<Mutex<HashMap<String, TokenEntry>>>;

/// Per-IP login attempt tracking for rate limiting.
/// Key = IP string, Value = (attempt_count, window_start).
pub type RateLimitStore = Arc<Mutex<HashMap<String, (u32, std::time::Instant)>>>;

/// Hash a raw session token with SHA-256 before storing in memory.
/// A memory dump of the process will not yield usable session tokens.
pub(super) fn token_hash(raw: &str) -> String {
    let hash = Sha256::digest(raw.as_bytes());
    hash.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
        s
    })
}

/// Shared application state injected into every handler.
#[derive(Clone)]
pub struct AppState {
    pub metrics: Arc<ProxyMetrics>,
    pub collector: Arc<Collector>,
    pub storage: Option<Arc<AnalyticsStorage>>,
    pub timeseries: Option<Arc<TimeseriesStore>>,
    pub n1_store: Arc<N1Store>,
    pub pool: Arc<BackendPool>,
    pub user_registry: Arc<UserRegistry>,
    pub rule_engine: Arc<RuleEngine>,
    pub rewriter: Arc<Rewriter>,
    pub tracer_store: Arc<TracerStore>,
    pub app_analytics: Arc<AppAnalyticsStore>,
    pub heatmap: Arc<HeatmapStore>,
    pub regression_store: Arc<RegressionStore>,
    /// Dashboard credentials (empty = auth disabled).
    pub dashboard_username: String,
    pub dashboard_password: String,
    /// Optional read-only credentials (empty = disabled).
    pub dashboard_readonly_username: String,
    pub dashboard_readonly_password: String,
    /// Session token TTL (0 = never expires).
    pub token_ttl_secs: u64,
    /// Max failed login attempts per IP per minute.
    pub login_max_attempts: u32,
    /// Active session tokens (hashed token → entry).
    pub tokens: TokenStore,
    /// Per-IP login attempt tracker for rate limiting.
    pub rate_limits: RateLimitStore,
    /// Path to the config file — used by the reload endpoint.
    pub config_path: String,
    /// The proxy router — used to hot-swap the backend pool via /api/reload/backends.
    pub proxy_router: ProxyRouter,
    /// PostgreSQL proxy router — present when pgsql proxy is enabled.
    pub pg_proxy_router: Option<ProxyRouter>,
    /// The full proxy config — used by /api/reload/backends to rebuild the pool.
    pub proxy_config: Arc<parking_lot::RwLock<ProxyConfig>>,
    /// Unix timestamp of the last successful config reload (0 = never reloaded).
    pub last_reload_secs: Arc<std::sync::atomic::AtomicU64>,
    /// Counter of queries killed by `max_query_time_ms` (from the router).
    pub queries_killed: Arc<std::sync::atomic::AtomicUsize>,
    /// Cluster config (peers + shared secret).  Empty = standalone mode.
    pub cluster: crate::config::ClusterConfig,
    /// Runtime config store — None when analytics is disabled.
    pub config_store: Option<std::sync::Arc<ConfigStore>>,
    /// In-memory error event ring buffer (proxy-captured backend errors).
    pub error_events: std::sync::Arc<ErrorEventStore>,
    /// Graceful shutdown indicator — true after SIGTERM.
    /// The /health endpoint returns 503 while this is set.
    pub draining: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// PostgreSQL backend pool — `None` when PostgreSQL proxy is disabled.
    pub pg_pool: Option<Arc<crate::proxy::pool::BackendPool>>,
    /// Number of active COPY operations on the PostgreSQL proxy (0 when disabled).
    pub pg_copy_active: Arc<std::sync::atomic::AtomicUsize>,
    /// Unix ms of the last config export.  Used to detect unsaved changes.
    pub config_export_ts: Arc<std::sync::atomic::AtomicU64>,
    /// Unix ms of the last config mutation (create/update/delete).  Bumped on every write.
    pub config_mutation_ts: Arc<std::sync::atomic::AtomicU64>,
}

/// Build the Axum router.
pub fn build_router(state: AppState) -> Router {
    use axum::routing::{get, post};

    // Public endpoints (no auth required)
    let public = Router::new()
        .route("/health", get(routes::health))
        .route("/api/login", post(routes::login))
        // Cluster sync — uses Bearer secret auth, not session tokens.
        .route("/api/sync", post(routes::cluster_sync))
        // MCP server — Model Context Protocol for AI assistant integration.
        .route("/mcp", post(mcp::handle_mcp))
        .with_state(state.clone());

    // Protected API endpoints
    let protected = Router::new()
        .route("/api/logout", post(routes::logout))
        .route("/api/auth/refresh", post(routes::refresh_token))
        .route("/api/stats", get(routes::stats))
        .route("/api/capabilities", get(routes::capabilities))
        .route("/api/queries", get(routes::queries))
        .route("/api/slow-queries", get(routes::slow_queries))
        .route("/api/n1", get(routes::n1_patterns))
        .route("/api/pool", get(routes::pool_stats))
        .route("/api/users", get(routes::user_stats))
        .route("/api/query-rules", get(routes::query_rules))
        .route("/api/query-rules/reload", post(routes::reload_rules))
        .route("/api/reload", post(routes::reload_config))
        .route("/api/reload/backends", post(routes::reload_backends))
        .route("/api/backends", get(routes::backend_stats))
        .route("/api/cluster", get(routes::cluster_state))
        .route("/api/cluster/actions", post(routes::cluster_action))
        .route("/api/rewrite-rules", get(routes::rewrite_rules))
        .route("/api/transactions", get(routes::transactions))
        .route("/api/analytics", get(routes::analytics))
        .route("/api/heatmap", get(routes::heatmap))
        .route("/api/timeseries", get(routes::timeseries))
        .route("/api/regressions", get(routes::regressions))
        .route("/metrics", get(routes::metrics))
        // ── Runtime config store (Fase 0.5) ─────────────────────────────────
        .route(
            "/api/config/rules",
            get(routes_config::list_config_rules).post(routes_config::create_config_rule),
        )
        .route(
            "/api/config/rules/:id",
            axum::routing::put(routes_config::update_config_rule)
                .delete(routes_config::delete_config_rule),
        )
        .route(
            "/api/config/rewrite-rules",
            get(routes_config::list_config_rewrite_rules)
                .post(routes_config::create_config_rewrite_rule),
        )
        .route(
            "/api/config/rewrite-rules/:id",
            axum::routing::put(routes_config::update_config_rewrite_rule)
                .delete(routes_config::delete_config_rewrite_rule),
        )
        .route(
            "/api/config/backends",
            get(routes_config::list_config_backends).post(routes_config::create_config_backend),
        )
        .route(
            "/api/config/backends/:id",
            axum::routing::put(routes_config::update_config_backend)
                .delete(routes_config::delete_config_backend),
        )
        .route(
            "/api/config/users",
            get(routes_config::list_config_users).post(routes_config::create_config_user),
        )
        .route(
            "/api/config/users/:id",
            axum::routing::put(routes_config::update_config_user)
                .delete(routes_config::delete_config_user),
        )
        .route("/api/config/history", get(routes_config::config_history))
        .route("/api/config/export", get(routes_config::export_config))
        .route("/api/config/import", post(routes_config::import_config))
        // ── Error events (Fase 1.6) ─────────────────────────────────────────
        .route("/api/errors", get(routes_errors::list_errors))
        .route("/api/errors/stats", get(routes_errors::error_stats))
        // ── Stats management (Fase 1.4) ─────────────────────────────────────
        .route("/api/stats/flush", post(routes::flush_stats))
        .route("/api/config/tls", get(routes::tls_cert_info))
        // ── Config unsaved-changes indicator ─────────────────────────────────
        .route("/api/config/status", get(routes_config::config_status))
        // ── Grafana Simple JSON datasource ──────────────────────────────────
        .route("/grafana/", get(grafana::health))
        .route("/grafana/search", post(grafana::search))
        .route("/grafana/query", post(grafana::query))
        .route("/grafana/annotations", post(grafana::annotations))
        .route("/grafana/tag-keys", post(grafana::tag_keys))
        .route("/grafana/tag-values", post(grafana::tag_values))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state);

    // Serve built React app — falls back gracefully if dist/ doesn't exist yet.
    let spa = Router::new()
        .nest_service("/", ServeDir::new("dashboard/dist"))
        .layer(middleware::from_fn(no_cache_html));

    Router::new()
        .merge(public)
        .merge(protected)
        .merge(spa)
        .layer(CorsLayer::new().allow_origin(Any))
}

/// Middleware: reject unauthenticated requests to protected endpoints when
/// auth is enabled (username + password are non-empty in config).
async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Response {
    // If no credentials configured, allow everything
    if state.dashboard_username.is_empty() || state.dashboard_password.is_empty() {
        return next.run(req).await;
    }

    let token = headers
        .get("X-Auth-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let hashed = token_hash(token);
    let now = std::time::Instant::now();
    let role = {
        let store = state.tokens.lock();
        store.get(&hashed).and_then(|entry| {
            // Reject expired tokens
            if entry.expires_at.is_some_and(|exp| exp <= now) {
                None
            } else {
                Some(entry.role)
            }
        })
    };

    match role {
        None => {
            state
                .metrics
                .dashboard_auth_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
        }
        Some(TokenRole::ReadOnly) => {
            // Read-only tokens are blocked from all mutating requests except logout.
            let is_mutating = !matches!(
                req.method(),
                &axum::http::Method::GET | &axum::http::Method::HEAD | &axum::http::Method::OPTIONS
            );
            let is_logout = req.uri().path() == "/api/logout";
            if is_mutating && !is_logout {
                return (
                    StatusCode::FORBIDDEN,
                    "Admin access required for write operations",
                )
                    .into_response();
            }
            next.run(req).await
        }
        Some(TokenRole::Admin) => next.run(req).await,
    }
}

/// Middleware: add `Cache-Control: no-store` to HTML responses.
async fn no_cache_html(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let is_html = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("text/html"))
        .unwrap_or(false);
    if is_html {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    resp
}

/// Start the dashboard server on the given address.
pub async fn run(addr: &str, state: AppState) -> anyhow::Result<()> {
    // Spawn token + rate-limit sweeper (every 60 s)
    {
        let tokens = state.tokens.clone();
        let rate_limits = state.rate_limits.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = std::time::Instant::now();
                // Evict expired tokens
                tokens
                    .lock()
                    .retain(|_, entry| entry.expires_at.map(|exp| exp > now).unwrap_or(true));
                // Evict rate-limit windows older than 5 min
                let cutoff = now
                    .checked_sub(std::time::Duration::from_secs(300))
                    .unwrap_or(now);
                rate_limits
                    .lock()
                    .retain(|_, (_, window_start)| *window_start > cutoff);
            }
        });
    }

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::info!("Dashboard listening on http://{}", addr);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
