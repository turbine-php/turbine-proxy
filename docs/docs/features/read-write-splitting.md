---
sidebar_position: 1
---

# Read/Write Splitting

TurbineProxy automatically routes `SELECT` queries to your read replicas and write queries to the primary — with zero application changes.

## How It Works

Every SQL query is classified into one of four categories:

| Category | Examples | Destination |
|---|---|---|
| **Read** | `SELECT`, `SHOW`, `EXPLAIN` | Replica (round-robin) |
| **Write** | `INSERT`, `UPDATE`, `DELETE`, `DDL` | Primary |
| **Transaction** | `BEGIN`, `COMMIT`, `ROLLBACK` | Primary |
| **Other** | `SET`, `USE`, `CALL` | Primary (safe default) |

### Special Cases

- **`SELECT ... FOR UPDATE`** and **`SELECT ... FOR SHARE`** → Primary (locking reads require the primary)
- **Inside a transaction** → All queries go to primary (sticky connection)
- **`SET @var = ...`** → Pins the connection to primary for all subsequent queries in that session (session variable set)
- **Read-Your-Own-Writes** → After a write, reads go to primary for a configurable window (see below)

## Weighted Round-Robin

Replicas are selected using weighted round-robin:

```toml
[[replicas]]
addr   = "replica-1:3306"
weight = 200   # Gets twice as much traffic

[[replicas]]
addr   = "replica-2:3306"
weight = 100   # Half the traffic

[[replicas]]
addr   = "replica-3:3306"
weight = 0     # Disabled (temporarily taken offline)
backup = true  # Only used when all others fail
```

## Read-Your-Own-Writes

Prevent reading stale data after a write by routing subsequent reads to the primary for a configurable window:

```toml
read_your_own_writes_ms = 500  # Route reads to primary for 500ms after write
```

This is useful for workflows where the user writes data and immediately reads it back:

```sql
INSERT INTO orders (user_id, total) VALUES (42, 99.99);
-- Next 500ms of SELECTs go to primary to see the new row
SELECT * FROM orders WHERE user_id = 42;
```

## Transaction Awareness

TurbineProxy tracks transaction state per connection. Once a `BEGIN` or `START TRANSACTION` is issued, all subsequent queries — including reads — go to the same primary backend connection until `COMMIT` or `ROLLBACK`.

This guarantees correctness: reads inside a transaction always see the latest writes within that same transaction.

## Session Variable Pinning

If a client issues session-modifying statements, TurbineProxy pins the connection to the primary:

- `SET @var = ...`
- `SELECT @var := ...`
- `SET @@session.x = ...`
- `SET NAMES <charset>`
- `SET CHARACTER SET <charset>`

Once pinned, the session stays on the same backend connection until the transaction ends or the connection closes.

## Override with Routing Rules

You can force specific queries to specific backends using [query routing rules](../configuration/routing-rules):

```toml
# Force a heavy report query to always use the primary
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*monthly_reports"
destination   = "primary"

# Force a specific user's reads to a dedicated replica
[[query_rules]]
user                  = "analytics"
destination_hostgroup = 2
```

## Monitoring

The dashboard **Overview** tab shows the read/write split ratio in real time. You can also query the REST API:

```bash
curl http://localhost:8080/api/stats | jq '{reads: .queries_read, writes: .queries_write}'
```
