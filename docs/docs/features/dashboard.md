---
sidebar_position: 7
---

# Dashboard

TurbineProxy includes a real-time web dashboard built with React. It provides visibility into query performance, backend health, active connections, and cluster state — with no external monitoring stack required.

![TurbineProxy Dashboard](/img/dashboard.png)

## Accessing the Dashboard

By default, the dashboard is available at `http://localhost:8080`.

```toml
[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
```

### Authentication

To enable login:

```toml
[dashboard]
username = "admin"
password = "strongpassword"
```

The dashboard uses token-based authentication (`X-Auth-Token` header).

#### Session Tokens

Tokens are UUIDs hashed with SHA-256 before storage — a process memory dump does not yield usable tokens. Tokens expire after `token_ttl_secs` (default 24 h). A background task sweeps expired tokens every 60 seconds.

```toml
[dashboard]
token_ttl_secs = 86400   # 24 h (default); 0 = tokens never expire
```

#### Token Refresh

Call `POST /api/auth/refresh` to renew a token without re-entering credentials. The old token is atomically revoked before the new one is issued, preventing replay of the old value.

```http
POST /api/auth/refresh
X-Auth-Token: <current-token>
Content-Type: application/json

{ "token": "<current-token>" }
```

Response:

```json
{ "ok": true, "token": "<new-token>", "message": null }
```

Returns `401 Unauthorized` if the token is missing, invalid, or already expired.

#### Logout

Call `POST /api/auth/logout` to explicitly invalidate the session token:

```http
POST /api/auth/logout
X-Auth-Token: <token>
Content-Type: application/json

{ "token": "<token>" }
```

Both readonly and admin tokens can call this endpoint.

#### Auth Failure Monitoring

Every failed authentication event — wrong password at login, invalid or expired token in the request middleware, and invalid/expired token presented to `/api/auth/refresh` — increments `turbineproxy_dashboard_auth_failures_total`. Monitor this counter in Prometheus / Grafana to detect brute-force attempts.

#### Read-Only Role

Create a second user with dashboard visibility but no write access. Read-only users can view all panels but POST/PUT/DELETE endpoints return `403 Forbidden`.

```toml
[dashboard]
readonly_username = "viewer"
readonly_password = "viewpass"
```

#### Login Rate Limiting

Failed login attempts are counted per source IP. After `login_max_attempts` failures within 60 seconds the endpoint returns `429 Too Many Requests`. The window resets automatically.

```toml
[dashboard]
login_max_attempts = 5   # default
```

## Panels

### Overview

Real-time counters updated every few seconds:

- **Total connections** (cumulative and active)
- **Total queries** (cumulative)
- **Read / Write split** — ratio of reads to writes
- **Active connections** count
- **Queries killed** — by timeout enforcement
- **SQL injection blocks** — blocked queries

### Queries

Two views for query analysis:

**Top by Count** — Most frequently executed queries, with count, average latency, and last seen timestamp.

**Slow Queries (P95)** — Queries ranked by P95 latency. Each row shows:
- Query fingerprint (normalized SQL)
- Execution count
- Min / Avg / Max / P95 / P99 latency
- Last execution timestamp

### Latency Heatmap

A time-bucketed heatmap showing query latency distribution over time. Each cell represents the count of queries in a latency bucket during a time window.

- X axis: time (minutes)
- Y axis: latency buckets (< 1ms, 1–5ms, 5–10ms, 10–50ms, 50–100ms, > 100ms)
- Color intensity: query volume

The heatmap uses the active color theme (light/dark).

### Throughput

Time-series chart of queries per minute. Available at three resolutions:
- Per minute (last 2 hours)
- Per hour (last 7 days)
- Per day (last 30 days)

### N+1 Detection

Lists query patterns that were repeated many times within a single session — a common ORM anti-pattern.

Each entry shows:
- Fingerprint
- Maximum repetition count observed
- Recommendation

### Pool

Connection pool utilization per backend:

| Column | Description |
|---|---|
| Role | `primary` or `replica` |
| Hostgroup | Backend index (0 = primary) |
| Weight | Configured round-robin weight |
| Idle | Connections available in pool |
| In Use | Connections currently checked out |
| Created | Total new TCP connections (lifetime) |
| Reused | Total pool cache hits (lifetime) |
| Evicted | Stale connections discarded |

### Backends

Per-backend health status:

| Column | Description |
|---|---|
| Role | `primary` or `replica` |
| Address | `host:port` |
| Healthy | Current health state |
| Lag | Replication lag in ms |
| Failures | Consecutive failed health checks |
| Backup | Whether this is a last-resort backup |

### Cluster

MySQL Group Replication or HA cluster view:

- Lists all cluster members with role (PRIMARY/SECONDARY), state, and version
- Shows current effective primary
- Provides operational buttons: **Recheck Health**, **Trigger Failover**, **Clear Failover**

Failover actions require confirmation. Forcing failover when the primary is healthy requires explicit force confirmation.

### Errors

Ring buffer of the last 1,000 backend errors:

- Error timestamp
- Client user
- Query fingerprint
- Error code and message

Also shows error statistics (counts by error code, error rate over time).

### Config

Runtime configuration management:

- **Routing Rules** — View, create, update, and delete query routing rules without restart
- **Rewrite Rules** — View, create, update, and delete query rewriting rules
- **Backends** — View and manage backend connections
- **Users** — View and manage user access control

Changes take effect immediately. A reload button is available to apply config file changes.

### Transactions

Active long-running transactions:

- Client IP and user
- Transaction duration
- Current backend connection
- Last query executed

### Users

Active connections per user:

- Username
- Active connection count
- Total queries issued

## Internationalization

The dashboard supports multiple languages:

| Language | Code |
|---|---|
| English | `en` |
| Portuguese | `pt` |
| Spanish | `es` |
| French | `fr` |
| German | `de` |
| Chinese | `zh` |
| Polish | `pl` |

Language can be selected from the top-right language selector.

## Theme

Light and dark themes are available via the toggle in the top-right corner. The dashboard respects the system's `prefers-color-scheme` setting on first load.

## Frontend Dev Server

During development, run the Vite dev server for hot-reloading:

```bash
cd dashboard
npm run dev
```

The dev server auto-detects the backend port from `turbineproxy.toml`. Override with:

```bash
VITE_API_ORIGIN=http://localhost:9090 npm run dev
```
