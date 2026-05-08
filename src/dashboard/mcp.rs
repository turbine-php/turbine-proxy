//! MCP (Model Context Protocol) server for TurbineProxy.
//!
//! Exposes proxy analytics, rules, and diagnostics to AI assistants via the
//! JSON-RPC 2.0-based MCP protocol over a single HTTP `POST /mcp` endpoint.
//!
//! Spec reference: <https://modelcontextprotocol.io/specification>
//!
//! Supported methods:
//!  - `initialize`      → server capabilities
//!  - `tools/list`      → list all available tools
//!  - `tools/call`      → invoke a tool by name

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

use crate::proxy::histogram::BUCKET_BOUNDS;

use super::AppState;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Approximate percentile (in ms) from a cumulative histogram snapshot.
/// Returns `null` when no observations have been recorded.
fn approx_percentile_ms(counts: &[u64; 12], count: u64, pct: f64) -> Option<f64> {
    if count == 0 {
        return None;
    }
    let target = (pct / 100.0 * count as f64).ceil() as u64;
    for (i, &c) in counts[..11].iter().enumerate() {
        if c >= target {
            return Some(BUCKET_BOUNDS[i] * 1000.0);
        }
    }
    // Fell in +Inf bucket — use last finite bound as best estimate.
    Some(*BUCKET_BOUNDS.last().unwrap() * 1000.0)
}

// ── MCP error codes (JSON-RPC 2.0) ──────────────────────────────────────────

const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;
const ERR_INTERNAL: i64 = -32603;

// ── Entry-point ──────────────────────────────────────────────────────────────

/// Handle all MCP requests (`POST /mcp`).
///
/// The endpoint is intentionally unauthenticated: it lives behind the same
/// optional dashboard auth as all other public-facing routes, and MCP clients
/// typically run on localhost.  Add a bearer token check in the route
/// registration if you need stronger isolation.
pub async fn handle_mcp(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> impl IntoResponse {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = req
        .get("params")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    let result = dispatch(&state, &method, &params).await;
    match result {
        Ok(body) => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": body
            });
            (StatusCode::OK, Json(resp))
        }
        Err((code, msg)) => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": msg }
            });
            (StatusCode::OK, Json(resp))
        }
    }
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

async fn dispatch(state: &AppState, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(mcp_initialize()),
        "tools/list" => Ok(mcp_tools_list()),
        "tools/call" => mcp_tools_call(state, params).await,
        _ => Err((
            ERR_METHOD_NOT_FOUND,
            format!("Method not found: {}", method),
        )),
    }
}

// ── initialize ───────────────────────────────────────────────────────────────

fn mcp_initialize() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "turbineproxy",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

// ── tools/list ───────────────────────────────────────────────────────────────

fn mcp_tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "turbineproxy_stats",
                "description": "Return overall proxy runtime statistics: connection counts, query counts, latency percentiles, and security counters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "turbineproxy_top_queries",
                "description": "Return the top N query fingerprints ranked by execution count or P95 latency.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "Maximum number of results (default 20)." },
                        "order": { "type": "string", "enum": ["count", "p95"], "description": "Sort field (default 'count')." }
                    }
                }
            },
            {
                "name": "turbineproxy_n1_patterns",
                "description": "Return detected N+1 query patterns. Each entry includes the query fingerprint, the number of distinct connections that triggered it, and the maximum repetition count in a single connection.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "turbineproxy_regressions",
                "description": "Return active and recently resolved query regression alerts (latency spikes, hot-key reads, etc.).",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "turbineproxy_backends",
                "description": "Return the current pool status: primary and replica counts, health, and per-backend query counters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "turbineproxy_query_rules",
                "description": "Return the list of active query routing / rewrite rules stored in the runtime config store.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "turbineproxy_errors",
                "description": "Return the most recent backend error events captured by the proxy.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "Maximum number of events to return (default 20)." }
                    }
                }
            }
        ]
    })
}

// ── tools/call ───────────────────────────────────────────────────────────────

async fn mcp_tools_call(state: &AppState, params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (ERR_INVALID_PARAMS, "Missing 'name' field".to_string()))?;

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    let content = match name {
        "turbineproxy_stats" => tool_stats(state),
        "turbineproxy_top_queries" => tool_top_queries(state, &args),
        "turbineproxy_n1_patterns" => tool_n1_patterns(state),
        "turbineproxy_regressions" => tool_regressions(state),
        "turbineproxy_backends" => tool_backends(state).await,
        "turbineproxy_query_rules" => tool_query_rules(state),
        "turbineproxy_errors" => tool_errors(state, &args),
        _ => return Err((ERR_METHOD_NOT_FOUND, format!("Unknown tool: {}", name))),
    };

    // MCP tools/call response wraps content in an array of "content" blocks.
    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": serde_json::to_string_pretty(&content?).unwrap_or_default()
            }
        ]
    }))
}

// ── Tool implementations ──────────────────────────────────────────────────────

fn tool_stats(state: &AppState) -> Result<Value, (i64, String)> {
    let m = &state.metrics;
    let (r_counts, _, r_count) = m.read_hist.snapshot();
    let (w_counts, _, w_count) = m.write_hist.snapshot();
    Ok(json!({
        "connections": {
            "total": m.connections_total.load(Ordering::Relaxed),
            "active": m.connections_active.load(Ordering::Relaxed)
        },
        "queries": {
            "total":  m.queries_total.load(Ordering::Relaxed),
            "read":   m.queries_read.load(Ordering::Relaxed),
            "write":  m.queries_write.load(Ordering::Relaxed),
            "killed": state.queries_killed.load(Ordering::Relaxed)
        },
        "transactions_killed": m.transactions_killed.load(Ordering::Relaxed),
        "security": {
            "sqli_blocked":      m.sqli_blocked.load(Ordering::Relaxed),
            "whitelist_blocked": m.whitelist_blocked.load(Ordering::Relaxed)
        },
        "latency_read_ms": {
            "p50": approx_percentile_ms(&r_counts, r_count, 50.0),
            "p95": approx_percentile_ms(&r_counts, r_count, 95.0),
            "p99": approx_percentile_ms(&r_counts, r_count, 99.0)
        },
        "latency_write_ms": {
            "p50": approx_percentile_ms(&w_counts, w_count, 50.0),
            "p95": approx_percentile_ms(&w_counts, w_count, 95.0),
            "p99": approx_percentile_ms(&w_counts, w_count, 99.0)
        }
    }))
}

fn tool_top_queries(state: &AppState, args: &Value) -> Result<Value, (i64, String)> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let order = args
        .get("order")
        .and_then(|v| v.as_str())
        .unwrap_or("count");

    let storage = match &state.storage {
        Some(s) => s,
        None => return Ok(json!([])), // analytics disabled
    };

    let rows = if order == "p95" {
        storage.get_top_by_p95(limit)
    } else {
        storage.get_top_by_count(limit)
    };

    match rows {
        Ok(rows) => Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "fingerprint": r.fingerprint,
                    "count": r.count,
                    "total_ms": r.total_us / 1000,
                    "min_ms":   r.min_us.map(|v| v / 1000),
                    "max_ms":   r.max_us.map(|v| v / 1000),
                    "p95_ms":   r.p95_us.map(|v| v / 1000),
                    "p99_ms":   r.p99_us.map(|v| v / 1000),
                    "last_seen": r.last_seen
                })
            })
            .collect()),
        Err(e) => Err((ERR_INTERNAL, e.to_string())),
    }
}

fn tool_n1_patterns(state: &AppState) -> Result<Value, (i64, String)> {
    let patterns = state.n1_store.get_all();
    Ok(patterns
        .iter()
        .map(|p| {
            json!({
                "fingerprint":  p.fingerprint,
                "connections":  p.connections,
                "max_per_conn": p.max_per_conn,
                "last_seen":    p.last_seen
            })
        })
        .collect())
}

fn tool_regressions(state: &AppState) -> Result<Value, (i64, String)> {
    let alerts = state.regression_store.snapshot();
    Ok(alerts
        .iter()
        .map(|a| {
            json!({
                "id":            a.id,
                "fingerprint":   a.fingerprint,
                "detected_at_ms": a.detected_at_ms,
                "resolved":      a.resolved,
                "details":       format!("{:?}", a.details)
            })
        })
        .collect())
}

async fn tool_backends(state: &AppState) -> Result<Value, (i64, String)> {
    let pool = state.pool.clone();
    let primary_addr = pool.primary.config.addr.clone();
    let replica_count = pool.replicas.len();

    let primary = json!({
        "addr": primary_addr,
        "role": "primary"
    });

    let replicas: Vec<Value> = pool
        .replicas
        .iter()
        .map(|r| {
            json!({
                "addr": r.config.addr,
                "role": "replica"
            })
        })
        .collect();

    Ok(json!({
        "replica_count": replica_count,
        "primary": primary,
        "replicas":  replicas
    }))
}

fn tool_query_rules(state: &AppState) -> Result<Value, (i64, String)> {
    let store = match &state.config_store {
        Some(s) => s,
        None => return Ok(json!([])),
    };
    store
        .list_rules()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    json!({
                        "id":           r.id,
                        "priority":     r.priority,
                        "match_pattern": r.match_pattern,
                        "match_digest": r.match_digest,
                        "destination":  r.destination,
                        "destination_hostgroup": r.destination_hostgroup,
                        "cache_ttl_secs": r.cache_ttl_secs,
                        "rollout_pct":  r.rollout_pct,
                        "enabled":      r.enabled
                    })
                })
                .collect()
        })
        .map_err(|e| (ERR_INTERNAL, e.to_string()))
}

fn tool_errors(state: &AppState, args: &Value) -> Result<Value, (i64, String)> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    let events = state.error_events.list(limit);
    Ok(events
        .iter()
        .map(|e| {
            json!({
                "ts":           e.ts,
                "code":         e.code,
                "category":     e.category,
                "message":      e.message,
                "fingerprint":  e.fingerprint,
                "backend_addr": e.backend_addr,
                "client_ip":    e.client_ip,
                "user":         e.user,
                "duration_ms":  e.duration_ms,
                "protocol":     e.protocol
            })
        })
        .collect())
}
