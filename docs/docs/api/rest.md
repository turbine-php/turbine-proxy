---
sidebar_position: 1
---

# REST API Reference

The TurbineProxy REST API is served on the same port as the dashboard (default: `8080`).

## Authentication

If `[dashboard].username` and `[dashboard].password` are configured, most endpoints require an `X-Auth-Token` header.

**Login:**

```bash
curl -X POST http://localhost:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username": "admin", "password": "secret"}'
# Response: {"token": "abc123..."}
```

**Use the token:**

```bash
curl http://localhost:8080/api/stats \
  -H 'X-Auth-Token: abc123...'
```

---

## Public Endpoints

### `GET /health`

Health check. Returns `503` when the proxy is draining (shutting down).

```json
{"status": "ok"}
```

---

## Metrics & Stats

### `GET /api/stats`

Global proxy counters.

```json
{
  "connections_total": 1024,
  "connections_active": 12,
  "queries_total": 98341,
  "queries_read": 74210,
  "queries_write": 24131,
  "transactions_killed": 0,
  "queries_killed": 2,
  "sqli_blocked": 0,
  "whitelist_blocked": 0
}
```

### `GET /api/capabilities`

Enabled features.

```json
{
  "analytics": true,
  "dashboard": true,
  "ha": true,
  "cluster": false,
  "pgsql": false
}
```

### `GET /metrics`

Prometheus-format metrics. Scrape with Prometheus or compatible tools.

```
# HELP turbineproxy_queries_total Total queries processed
# TYPE turbineproxy_queries_total counter
turbineproxy_queries_total 98341
...
```

### `POST /api/stats/flush`

Force an immediate flush of in-memory analytics to SQLite.

```bash
curl -X POST http://localhost:8080/api/stats/flush
```

---

## Query Analytics

### `GET /api/queries`

Top queries by execution count (from SQLite).

**Query params:**
- `limit` — Number of results (default: 50)

```json
[
  {
    "fingerprint": "SELECT * FROM users WHERE id = ?",
    "count": 4821,
    "total_us": 482100,
    "min_us": 80,
    "max_us": 4200,
    "p95_us": 320,
    "p99_us": 890,
    "last_seen": "2026-05-08T14:23:01Z"
  }
]
```

### `GET /api/slow-queries`

Top queries ranked by P95 latency.

**Query params:**
- `limit` — Number of results (default: 50)

Same response format as `/api/queries`.

### `GET /api/n1`

N+1 detection: query patterns repeated many times within a session.

```json
[
  {
    "fingerprint": "SELECT * FROM tags WHERE post_id = ?",
    "max_count": 47,
    "first_seen": "2026-05-08T14:10:00Z"
  }
]
```

### `GET /api/heatmap`

Query latency distribution over time.

```json
{
  "buckets": ["<1ms", "1-5ms", "5-10ms", "10-50ms", "50-100ms", ">100ms"],
  "data": [
    {"time": "2026-05-08T14:00:00Z", "counts": [120, 340, 80, 12, 2, 0]},
    {"time": "2026-05-08T14:01:00Z", "counts": [115, 360, 75, 10, 1, 0]}
  ]
}
```

### `GET /api/timeseries`

Throughput time series.

**Query params:**
- `resolution` — `minute`, `hour`, or `day` (default: `minute`)
- `limit` — Number of data points

```json
[
  {"time": "2026-05-08T14:00:00Z", "queries": 1420},
  {"time": "2026-05-08T14:01:00Z", "queries": 1385}
]
```

---

## Backends & Pool

### `GET /api/pool`

Connection pool utilization per backend.

```json
[
  {
    "role": "primary",
    "hostgroup": 0,
    "weight": 0,
    "backup": false,
    "idle": 15,
    "in_use": 5,
    "created": 120,
    "reused": 9880,
    "evicted": 3
  },
  {
    "role": "replica",
    "hostgroup": 1,
    "weight": 100,
    "backup": false,
    "idle": 18,
    "in_use": 2,
    "created": 80,
    "reused": 6420,
    "evicted": 1
  }
]
```

### `GET /api/backends`

Per-backend health status.

```json
[
  {
    "role": "primary",
    "hostgroup": 0,
    "addr": "db-primary:3306",
    "healthy": true,
    "lag_ms": 0,
    "consecutive_failures": 0,
    "weight": 0,
    "backup": false
  }
]
```

### `GET /api/cluster`

MySQL Group Replication member list.

```json
[
  {
    "addr": "db-1:3306",
    "role": "PRIMARY",
    "state": "ONLINE",
    "version": "8.0.36"
  },
  {
    "addr": "db-2:3306",
    "role": "SECONDARY",
    "state": "ONLINE",
    "version": "8.0.36"
  }
]
```

### `POST /api/cluster/actions`

Perform a cluster operation.

**Request body:**

```json
{
  "action": "recheck_health | trigger_failover | clear_failover",
  "force": false
}
```

| Action | Description | `force` |
|---|---|---|
| `recheck_health` | Immediately re-run health checks on all backends | Ignored |
| `trigger_failover` | Promote healthiest replica to primary | Required if primary is still healthy |
| `clear_failover` | Restore writes to the original primary | Ignored |

---

## Users & Sessions

### `GET /api/users`

Active connections per user.

```json
[
  {"username": "app", "active_connections": 12, "queries_total": 48200},
  {"username": "readonly", "active_connections": 4, "queries_total": 12100}
]
```

### `GET /api/transactions`

Currently open transactions.

```json
[
  {
    "connection_id": 42,
    "user": "app",
    "client_addr": "10.0.0.5:52841",
    "backend": "db-primary:3306",
    "duration_ms": 1240,
    "last_query": "SELECT * FROM orders WHERE id = ?"
  }
]
```

---

## Configuration

### `GET /api/query-rules`

List all active routing rules.

### `GET /api/rewrite-rules`

List all active rewrite rules.

### `POST /api/reload`

Hot-reload routing and rewrite rules from the config file (equivalent to `SIGHUP`).

```bash
curl -X POST http://localhost:8080/api/reload
```

### `POST /api/reload/backends`

Hot-swap the backend pool (reconnects to all backends with current config).

---

## Runtime Config CRUD

These endpoints allow managing routing rules, rewrite rules, backends, and users at runtime without editing the config file.

### Routing Rules

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/config/rules` | List all rules |
| `POST` | `/api/config/rules` | Create a rule |
| `PUT` | `/api/config/rules/:id` | Update a rule |
| `DELETE` | `/api/config/rules/:id` | Delete a rule |

### Rewrite Rules

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/config/rewrite-rules` | List all rules |
| `POST` | `/api/config/rewrite-rules` | Create a rule |
| `PUT` | `/api/config/rewrite-rules/:id` | Update a rule |
| `DELETE` | `/api/config/rewrite-rules/:id` | Delete a rule |

### Backends

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/config/backends` | List backends |
| `POST` | `/api/config/backends` | Add a backend |
| `PUT` | `/api/config/backends/:id` | Update a backend |
| `DELETE` | `/api/config/backends/:id` | Remove a backend |

### Users

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/config/users` | List users |
| `POST` | `/api/config/users` | Create a user |
| `PUT` | `/api/config/users/:id` | Update a user |
| `DELETE` | `/api/config/users/:id` | Delete a user |

### Config History & Export

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/config/history` | Config change history |
| `GET` | `/api/config/export` | Export current config as TOML |
| `POST` | `/api/config/import` | Import config from TOML |
| `GET` | `/api/config/status` | Unsaved changes indicator |

---

## Errors

### `GET /api/errors`

Last 1,000 backend errors (ring buffer).

```json
[
  {
    "timestamp": "2026-05-08T14:22:05Z",
    "user": "app",
    "fingerprint": "INSERT INTO orders VALUES (?, ?, ?)",
    "error_code": "1062",
    "message": "Duplicate entry '42' for key 'PRIMARY'"
  }
]
```

### `GET /api/errors/stats`

Error counts grouped by error code.

```json
[
  {"error_code": "1062", "count": 14, "last_seen": "2026-05-08T14:22:05Z"},
  {"error_code": "1205", "count": 2, "last_seen": "2026-05-08T13:10:00Z"}
]
```

---

## TLS Info

### `GET /api/config/tls`

Details about the frontend TLS certificate (if configured).

```json
{
  "enabled": true,
  "subject": "CN=turbineproxy.internal",
  "issuer": "CN=My CA",
  "not_after": "2027-05-08T00:00:00Z"
}
```
