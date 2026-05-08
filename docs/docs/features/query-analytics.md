---
sidebar_position: 3
---

# Query Analytics

TurbineProxy records telemetry for every query that passes through it. No setup required — analytics are enabled by default.

## What Is Collected

For each unique query pattern (fingerprint), TurbineProxy tracks:

| Metric | Description |
|---|---|
| **Count** | Total executions |
| **Total time** | Sum of all execution durations |
| **Min / Max latency** | Fastest and slowest execution |
| **P95 / P99 latency** | 95th and 99th percentile latency |
| **Last seen** | Timestamp of most recent execution |

## Query Fingerprinting

All literal values in SQL are normalized to `?` before storing. This groups semantically identical queries regardless of their parameters:

```sql
-- These three queries share one fingerprint:
SELECT * FROM users WHERE id = 1
SELECT * FROM users WHERE id = 42
SELECT * FROM users WHERE id = 999

-- Fingerprint:
SELECT * FROM users WHERE id = ?
```

Fingerprinting covers:
- Single-quoted strings: `'value'` → `?`
- Double-quoted strings: `"value"` → `?`
- Integers, floats, hex, scientific notation: `42`, `3.14`, `0xFF`, `1e6` → `?`
- Preserves identifiers like column names and table aliases

## Slow Query Log

Queries slower than `[analytics].slow_query_ms` (default: 100ms) are logged to stderr with microsecond precision:

```
Slow query (243.1ms): SELECT * FROM orders WHERE user_id = ? AND status = ?
```

Configure the threshold:

```toml
[analytics]
slow_query_ms = 50   # Log queries slower than 50ms
```

## Storage

Analytics are stored in a SQLite database (default: `turbineproxy_analytics.db`). The architecture is designed to never block the query hot path:

```
Query executes
     │
     ▼
try_send() → bounded channel (10k capacity, non-blocking)
     │
     ▼ (background task)
Aggregation loop → in-memory HashMap
     │
     ▼ (every 30 seconds)
Flush to SQLite (ON CONFLICT DO UPDATE)
```

If the channel is full (extremely high throughput), events are silently dropped rather than slowing down queries.

### Schema

```sql
CREATE TABLE query_stats (
    fingerprint_hash INTEGER PRIMARY KEY,
    fingerprint      TEXT    NOT NULL,
    count            INTEGER NOT NULL,
    total_us         INTEGER NOT NULL,
    min_us           INTEGER,
    max_us           INTEGER,
    p95_us           INTEGER,
    p99_us           INTEGER,
    last_seen        TEXT,
    updated_at       TEXT
);
```

## Data Retention

Old analytics data is pruned automatically:

```toml
[analytics]
retention_days = 30   # Keep 30 days of history (default)
```

## Accessing Analytics

### Dashboard

Open the **Queries** tab in the dashboard to see:
- Top queries by execution count
- Top queries by P95 latency (slow queries)
- Latency heatmap over time
- Throughput timeseries (queries/minute)

### REST API

```bash
# Top queries by count
curl http://localhost:8080/api/queries

# Top queries by p95 latency (slow query list)
curl http://localhost:8080/api/slow-queries

# Latency heatmap
curl http://localhost:8080/api/heatmap

# Throughput timeseries
curl http://localhost:8080/api/timeseries
```

### Force Flush

Force an immediate flush of in-memory stats to SQLite:

```bash
curl -X POST http://localhost:8080/api/stats/flush
```

## N+1 Detection

TurbineProxy detects repeated identical queries within a single client session — a common ORM anti-pattern:

- **Warn threshold**: 5 identical queries in one session
- **Hot key threshold**: 30 identical queries (reported to regression store)

Example alert in the dashboard **N+1** panel:

```
⚠ N+1 detected: SELECT * FROM tags WHERE post_id = ? (×47 in session)
```

Access via API:

```bash
curl http://localhost:8080/api/n1
```

## Performance Regression Detection

TurbineProxy tracks query performance over time and alerts when a previously fast query becomes significantly slower. View regression alerts in the dashboard or via:

```bash
# (exposed via /api/stats summary)
```

## Disabling Analytics

```toml
[analytics]
enabled = false
```

When disabled, no fingerprinting, logging, or SQLite writes occur. The hot path overhead is zero.
