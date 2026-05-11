---
sidebar_position: 4
---

# Explore the Dashboard

TurbineProxy ships with a built-in real-time web dashboard available at `http://localhost:8080`. This tutorial walks through each panel and explains what to look for.

## Accessing the Dashboard

Open your browser at `http://localhost:8080`.

If you configured `[dashboard].username` and `[dashboard].password` in your TOML file, you will be prompted to log in:

```toml
[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
username    = "admin"
password    = "yourpassword"
```

If no credentials are set, the dashboard is accessible without authentication.

## Overview Tab

The **Overview** tab shows a live summary of proxy activity:

| Metric | Description |
|---|---|
| **Total queries** | Cumulative query count since startup |
| **Reads / Writes** | Breakdown of read vs. write queries |
| **Slow queries** | Queries that exceeded `[analytics].slow_query_ms` (default 100ms) |
| **SQL injection blocked** | Queries blocked by the injection protection filter |
| **Active connections** | Current number of connected clients |

The **throughput graph** shows queries-per-minute over time. Spikes are immediately visible.

## Queries Tab

The **Queries** tab shows analytics for every unique query pattern (fingerprint) that has passed through the proxy.

### Top queries by count

The most frequently executed queries, sorted by execution count. This immediately surfaces:
- Queries that benefit most from caching
- N+1 patterns (very high counts for parameterized queries)

### Top queries by P95 latency (slow queries)

Queries sorted by 95th percentile latency. A query at the top of this list is slow for most users, not just in edge cases.

Each row shows:
- **Fingerprint**: The normalized SQL with literals replaced by `?`
- **Count**: How many times this query ran
- **Avg / P95 / P99**: Latency percentiles in milliseconds
- **Last seen**: When it was most recently executed

### Query Heatmap

The heatmap shows query latency distribution over time. The Y-axis is latency buckets; the X-axis is time. A darker cell means more queries fell in that bucket at that time.

Use the heatmap to:
- Spot sudden latency regressions (a row going dark higher up)
- Confirm a fix worked (the high-latency row disappears)

## Backends Tab

Shows the health and pool state for every configured backend:

| Column | Description |
|---|---|
| **Role** | Primary or Replica |
| **Address** | Backend host:port |
| **Healthy** | Green = passing health checks |
| **Lag** | Replica replication lag in milliseconds |
| **Idle / In-use** | Pool connection utilization |

A replica showing **high lag** will be excluded from read routing once it exceeds `[ha].max_replica_lag_ms`.

## N+1 Panel

TurbineProxy detects N+1 query patterns within a single client session. When the same fingerprint is executed more than 5 times in one session, it appears here:

```
⚠ N+1 detected: SELECT * FROM tags WHERE post_id = ? (×47 in session)
```

N+1 patterns are a common ORM anti-pattern that can easily cause 100× more queries than necessary. Use this panel to identify and fix them in your application.

## Config Tab

The **Config** tab shows the currently loaded configuration, including all query rules and rewrite rules. From here you can also trigger a hot reload without restarting:

```bash
# Equivalent API call
curl -X POST http://localhost:8080/api/reload
```

## REST API

All dashboard data is available via the REST API:

```bash
# Summary stats
curl http://localhost:8080/api/stats

# Top queries
curl http://localhost:8080/api/queries

# Slow queries (by P95 latency)
curl http://localhost:8080/api/slow-queries

# Backend health
curl http://localhost:8080/api/backends

# N+1 detections
curl http://localhost:8080/api/n1

# Latency heatmap
curl http://localhost:8080/api/heatmap

# Throughput timeseries
curl http://localhost:8080/api/timeseries
```

## What's Next?

- [Set Up Read/Write Splitting](./read-write-splitting)
- [Tune Connection Pooling](./connection-pooling-tuning)
- [Integrate with Prometheus and Grafana](./prometheus-grafana)
