---
sidebar_position: 5
---

# Read/Write Splitting in Practice

Read/write splitting routes `SELECT` queries to your read replicas and write queries (`INSERT`, `UPDATE`, `DELETE`, DDL) to the primary. This tutorial shows you how to configure replicas and verify the split is working.

## Prerequisites

- TurbineProxy running (see [Install and Run](./install-and-run))
- At least one MySQL replica streaming from the primary

## How TurbineProxy Classifies Queries

TurbineProxy inspects every SQL statement before routing it:

| Query type | Examples | Destination |
|---|---|---|
| **Read** | `SELECT`, `SHOW`, `EXPLAIN` | Replica (round-robin by weight) |
| **Write** | `INSERT`, `UPDATE`, `DELETE`, DDL | Primary |
| **Transaction** | `BEGIN`, `COMMIT`, `ROLLBACK` | Primary |
| **Session control** | `SET`, `USE`, `CALL` | Primary (safe default) |

### Special Cases

- `SELECT ... FOR UPDATE` and `SELECT ... FOR SHARE` — go to the **primary** (locking reads need primary)
- Queries inside an open transaction (`BEGIN` ... `COMMIT`) — all go to the **primary**
- `SET @var = ...` — pins the session to the **primary** for all subsequent queries until the connection is closed

## Step 1: Add a Read Replica

Open your `turbineproxy.toml` and add one or more `[[replicas]]` entries:

```toml
listen_addr = "0.0.0.0:3307"

[primary]
addr     = "db-primary:3306"
user     = "proxyuser"
password = "yourpassword"
database = "myapp"

[[replicas]]
addr     = "db-replica-1:3306"
user     = "proxyuser"
password = "yourpassword"
database = "myapp"
weight   = 100
```

Restart TurbineProxy to apply the change.

## Step 2: Add Multiple Replicas with Weighted Load Balancing

When you have several replicas with different hardware, use `weight` to control traffic distribution:

```toml
[[replicas]]
addr   = "db-replica-1:3306"
weight = 200   # Receives twice as much read traffic

[[replicas]]
addr   = "db-replica-2:3306"
weight = 100   # Receives half the traffic of replica-1

[[replicas]]
addr   = "db-replica-3:3306"
weight = 0     # Disabled — set weight > 0 to re-enable

[[replicas]]
addr   = "db-replica-dr:3306"
backup = true  # Only used when all non-backup replicas are unhealthy
```

## Step 3: Verify Routing

Check the read/write split in real time:

```bash
curl http://localhost:8080/api/stats | jq '{reads: .queries_read, writes: .queries_write}'
```

Or open the dashboard at `http://localhost:8080` and watch the **Overview** tab as your application runs. The reads vs. writes counters update live.

## Step 4: Read-Your-Own-Writes

By default, a write followed immediately by a read may return stale data if the replica has not yet replicated the write. To prevent this, configure a window during which reads go to the primary:

```toml
read_your_own_writes_ms = 500
```

After any write, all `SELECT` queries for that session go to the primary for 500 ms. After the window expires, reads resume going to replicas.

This is useful for user-facing workflows like "create an order, then display it immediately".

## Step 5: Override Routing for Specific Queries

Some queries need to go to the primary even if they look like reads — for example, reports that need real-time data:

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*monthly_reports"
destination   = "primary"
comment       = "Reports need fresh data — always use primary"
```

Or force all queries from a specific user to replicas:

```toml
[[query_rules]]
user        = "analytics_user"
destination = "replica"
comment     = "Analytics user always reads from replica"
```

Rules are evaluated in order; the first match wins. See [Query Routing and Rate Limiting](./rate-limiting-and-routing) for the full rule reference.

## Checking Replica Health

TurbineProxy excludes replicas that are lagging too far behind the primary. The default threshold is 5 seconds:

```toml
[ha]
enabled            = true
max_replica_lag_ms = 2000  # Exclude replicas lagging more than 2 seconds
```

Check current replica health:

```bash
curl http://localhost:8080/api/backends | jq '.[] | {role, addr, healthy, lag_ms}'
```

## What's Next?

- [Tune Connection Pooling](./connection-pooling-tuning)
- [Query Routing and Rate Limiting](./rate-limiting-and-routing)
- [High Availability and Automatic Failover](./high-availability)
