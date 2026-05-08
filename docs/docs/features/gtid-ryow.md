---
sidebar_position: 3
---

# GTID-aware Read-Your-Own-Writes

TurbineProxy can guarantee that a client always reads its own writes — even when reads are routed to replicas — without relying on a fixed time delay.

## The Problem

With standard read/write splitting, a write goes to the primary and subsequent reads go to a replica. If the replica hasn't applied the write yet, the client sees stale data:

```
INSERT INTO orders VALUES (...);    → primary ✅
SELECT * FROM orders WHERE id = ?;  → replica ❌ (not yet replicated)
```

The naive fix — routing all reads to the primary for N milliseconds after a write — works but wastes replica capacity when replication is fast.

## How GTID-aware RYOW Works

When `gtid_aware_ryow = true`:

1. After every write, TurbineProxy extracts the GTID from the primary's OK packet (`SESSION_TRACK_GTIDS`).
2. Before the next read, the proxy runs `SELECT GTID_SUBSET(?, @@global.gtid_executed)` on an available replica.
3. If the replica has applied the GTID, the read is routed there normally.
4. If the replica is still catching up, the read falls back to the primary.

```
INSERT INTO orders VALUES (...);
  → primary returns: gtid = "a1b2c3d4:17"

SELECT * FROM orders WHERE id = ?;
  → proxy checks replica: has_gtid("a1b2c3d4:17") → true
  → read routed to replica ✅

SELECT * FROM orders WHERE id = ?;  (another client, right after failover)
  → proxy checks replica: has_gtid("a1b2c3d4:17") → false
  → read falls back to primary ✅ (safe)
```

The GTID is cleared as soon as a replica confirms it has been applied. All subsequent reads use replicas freely.

## Configuration

```toml
[mysql]
enabled         = true
listen_addr     = "0.0.0.0:3307"
gtid_aware_ryow = true   # default: false
```

GTID-aware RYOW is independent of (and compatible with) the time-based `read_your_own_writes_ms`. You can enable both; the proxy falls back to primary if either condition is active.

## Requirements

- MySQL 8.0+ or MariaDB 10.5+ with GTID mode enabled (`gtid_mode = ON`)
- The proxy user must have `REPLICATION CLIENT` privilege (for `@@global.gtid_executed`)
- At least one healthy replica in the pool

## Fallback Behaviour

If the topology does not support GTID (e.g. statement-based replication without GTID mode), the check always returns `false` and reads fall back to the primary. There is no error — the proxy degrades gracefully to primary-reads-only for the affected session.

## Performance Impact

The GTID check is a single lightweight query per session after a write. On a LAN replica the check completes in under 1 ms. The check is only issued when:

- `gtid_aware_ryow = true`
- The query intent is `Read`
- The session has a pending write GTID

Sessions with no recent writes bypass the check entirely.

## Comparison with Time-Based RYOW

| | Time-based (`read_your_own_writes_ms`) | GTID-aware |
|---|---|---|
| Consistency guarantee | Probabilistic (lag can exceed the window) | Exact |
| Replica utilisation | Reads go to primary for the full window | Reads go to replica as soon as it catches up |
| MySQL version | Any | 8.0+ / MariaDB 10.5+ with GTID enabled |
| Configuration | Single integer | Single boolean |
