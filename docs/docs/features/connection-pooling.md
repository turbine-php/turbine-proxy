---
sidebar_position: 2
---

# Connection Pooling

TurbineProxy maintains persistent connection pools to all backends, dramatically reducing the overhead of MySQL's per-connection handshake.

## How It Works

When a client query arrives:

1. TurbineProxy pops the most recently used idle connection from the pool (LIFO)
2. The query is executed on that backend connection
3. The connection is returned to the pool after the response is forwarded

If no idle connections are available and the pool is not full, a new backend connection is created.

## Configuration

```toml
pool_size                = 20     # Pool size per backend
connection_max_idle_secs = 55     # Evict connections idle longer than this
```

| Key | Default | Description |
|-----|---------|-------------|
| `pool_size` | `20` | Maximum connections per backend. Shared across all clients |
| `connection_max_idle_secs` | `55` | Idle connections older than this are silently discarded. Set below MySQL's `wait_timeout` to avoid `Lost connection` errors |

## Init Connect

Run SQL statements on every new backend connection:

```toml
[primary]
init_connect = [
  "SET NAMES utf8mb4",
  "SET time_zone = '+00:00'",
  "SET SESSION sql_mode = 'STRICT_TRANS_TABLES'"
]
```

Init statements are re-executed automatically when a stale connection is replaced with a new one.

## Pool Statistics

View pool utilization in the dashboard **Pool** tab or via API:

```bash
curl http://localhost:8080/api/pool
```

| Metric | Description |
|--------|-------------|
| `idle` | Connections available for immediate use |
| `in_use` | Connections currently executing a query |
| `created` | Total new TCP connections (lifetime) |
| `reused` | Total pool cache hits (lifetime) |
| `evicted` | Stale connections discarded |

A high `evicted` count relative to `created` indicates connections are being evicted before they can be reused — consider lowering `connection_max_idle_secs`.

## Sticky Connections

Certain operations require routing all subsequent queries in a session to the same backend connection:

- **Transactions**: `BEGIN` → sticky until `COMMIT`/`ROLLBACK`
- **Session variables**: `SET @var = ...` → sticky for rest of session
- **Prepared statements**: bound to the connection that prepared them

Sticky connections are held out of the pool for the duration of the session activity.
