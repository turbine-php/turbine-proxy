---
sidebar_position: 6
---

# Connection Pooling Tuning

TurbineProxy maintains persistent connection pools to all backends. This tutorial explains how the pool works, how to size it correctly, and how to interpret pool metrics.

## How the Pool Works

When a client query arrives:

1. TurbineProxy pops the most recently used **idle** connection from the pool (LIFO order)
2. Executes the query on that backend connection
3. Returns the connection to the pool after the response is forwarded

If no idle connections are available and the pool is not full, a new backend connection is created on demand. If the pool is full, the query waits until a connection becomes available.

The key insight is that **many application connections share a small set of backend connections**. A typical deployment runs 500 app connections through a pool of 20 backend connections, because most app connections are idle at any given moment.

## Default Configuration

```toml
pool_size                = 20    # Connections per backend
connection_max_idle_secs = 55    # Evict idle connections older than this
```

Both keys can be set at the top level (legacy format) or inside `[shared]` (unified format):

```toml
[shared]
pool_size                = 20
connection_max_idle_secs = 55
```

## Sizing the Pool

### How many connections does one pool support?

A pool of 20 backend connections can serve hundreds of application connections as long as the average query duration is short. Use this rough formula:

```
throughput (qps) = pool_size × (1000 / avg_query_ms)
```

For example, with `pool_size = 20` and an average query of 5ms:

```
20 × (1000 / 5) = 4,000 queries/second
```

If you are seeing connection wait latency in the dashboard, increase `pool_size`.

### Avoid setting pool_size too high

Each backend connection consumes a MySQL thread and memory on the database server. Setting `pool_size = 500` is rarely better than `pool_size = 50` — at some point you'll be bottlenecked by MySQL itself, not the number of connections.

Start at the default of 20 and increase in increments of 10 while monitoring MySQL's `Threads_running` metric.

### connection_max_idle_secs

MySQL has a server-side `wait_timeout` (default: 8 hours). If TurbineProxy holds an idle connection for longer than `wait_timeout`, MySQL closes it and the next query on that connection fails with `MySQL server has gone away`.

Set `connection_max_idle_secs` **lower** than MySQL's `wait_timeout`:

```toml
# If MySQL wait_timeout = 3600 (1 hour), set:
connection_max_idle_secs = 3500
```

The default of `55` seconds is a safe value for most MySQL configurations.

## Init Connect Statements

Run SQL statements automatically on every new backend connection:

```toml
[primary]
addr         = "db-primary:3306"
user         = "proxyuser"
password     = "yourpassword"
database     = "myapp"
init_connect = [
  "SET NAMES utf8mb4",
  "SET time_zone = '+00:00'",
  "SET SESSION sql_mode = 'STRICT_TRANS_TABLES'"
]
```

Init statements are replayed automatically when a stale connection is replaced — you never have inconsistent session state.

## Monitoring Pool Utilization

### Dashboard

Open the **Backends** tab in the dashboard. Each backend card shows:

- **Idle**: Connections in the pool waiting to be used
- **In-use**: Connections currently executing a query

### REST API

```bash
curl http://localhost:8080/api/pool
```

Response fields:

| Field | Description |
|-------|-------------|
| `idle` | Connections available for immediate use |
| `in_use` | Connections currently executing a query |
| `created` | Total new TCP connections (lifetime counter) |
| `reused` | Total pool hits (lifetime counter) |
| `evicted` | Stale connections discarded (lifetime counter) |

### Interpreting the Metrics

**High `in_use` / low `idle`**: The pool is saturated. Increase `pool_size` or optimize slow queries that are holding connections for too long.

**High `evicted` relative to `created`**: Connections are being evicted before they can be reused. Lower `connection_max_idle_secs` or increase application query frequency.

**High `created` / low `reused`**: Pool churn — connections are created and immediately evicted. Check for connection spikes or excessively short idle timeouts.

## Sticky Connections and Pool Impact

Some operations hold a backend connection for the entire duration of a client session:

- **Open transactions**: `BEGIN` pins the session to a single backend connection until `COMMIT` or `ROLLBACK`
- **Session variables**: `SET @var = ...` pins to primary for the rest of that connection
- **Prepared statements**: bound to the connection that created them

Sticky connections are counted as `in_use` while held. If your application runs many long transactions, consider increasing `pool_size` proportionally.

## Max Client Connections

Separately from pool size, you can cap the number of simultaneous **client** connections:

```toml
max_connections = 1000
```

New client connections beyond this limit are refused immediately with an error. The default is 1000.

## Per-User Connection Limits

Limit specific users to a maximum number of simultaneous connections:

```toml
[[users]]
name            = "reporting"
password        = "ropass"
allow_writes    = false
max_connections = 10   # This user can hold at most 10 connections
```

## What's Next?

- [Query Rewriting: Add Limits and Timeouts](./query-rewriting)
- [Query Routing and Rate Limiting](./rate-limiting-and-routing)
