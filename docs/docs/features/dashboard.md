---
sidebar_position: 7
---

# Dashboard

TurbineProxy includes a real-time web dashboard built with React. It provides visibility into query performance, backend health, active connections, and cluster state — with no external monitoring stack required.

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

The dashboard uses token-based authentication (`X-Auth-Token` header). The token is valid for the session duration.

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
