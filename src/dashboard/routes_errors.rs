//! Error event REST endpoints.
//!
//! `GET /api/errors?limit=N&code=X&protocol=postgres`  — list recent error events
//! `GET /api/errors/stats`                              — counts by category (1h / 24h / 7d)

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;

use crate::dashboard::AppState;

fn normalize_protocol(raw: Option<&str>) -> Result<Option<&'static str>, String> {
    match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(None),
        "mysql" => Ok(Some("mysql")),
        "pgsql" | "postgres" | "postgresql" => Ok(Some("postgres")),
        other => Err(format!("invalid protocol '{other}' (use mysql, pgsql, or auto)")),
    }
}

#[derive(Deserialize)]
pub struct ListErrorsParams {
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Optional filter by error code (exact match).
    pub code: Option<u16>,
    /// Optional filter by category string (e.g. "SYNTAX", "AUTH").
    pub category: Option<String>,
    /// Optional filter by protocol (`"mysql"` or `"postgres"`).
    pub protocol: Option<String>,
}

fn default_limit() -> usize { 100 }

/// `GET /api/errors` — return most recent error events.
pub async fn list_errors(
    State(state): State<AppState>,
    Query(params): Query<ListErrorsParams>,
) -> Json<serde_json::Value> {
    let protocol = match normalize_protocol(params.protocol.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            return Json(serde_json::json!({ "ok": false, "error": e }));
        }
    };

    let mut events = state.error_events.list_filtered(params.limit.min(1_000), protocol);

    if let Some(code) = params.code {
        events.retain(|e| e.code == code);
    }
    if let Some(ref cat) = params.category {
        let cat_up = cat.to_uppercase();
        events.retain(|e| e.category == cat_up);
    }

    Json(serde_json::json!({
        "ok": true,
        "protocol": protocol.unwrap_or("all"),
        "events": events,
        "count": events.len(),
    }))
}

/// `GET /api/errors/stats` — aggregated counts (1h / 24h / 7d).
pub async fn error_stats(
    State(state): State<AppState>,
    Query(params): Query<ListErrorsParams>,
) -> Json<serde_json::Value> {
    let protocol = match normalize_protocol(params.protocol.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            return Json(serde_json::json!({ "ok": false, "error": e }));
        }
    };

    Json(serde_json::json!({
        "ok": true,
        "protocol": protocol.unwrap_or("all"),
        "data": state.error_events.stats_filtered(protocol),
    }))
}
