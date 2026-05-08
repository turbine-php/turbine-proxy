---
sidebar_position: 13
---

# Fast-Forward Mode

Fast-forward mode sends every query directly to the primary backend, bypassing the entire proxy processing pipeline. It is an opt-in performance optimisation for dedicated pools where routing intelligence is not needed.

## What Gets Bypassed

When `fast_forward = true`, the following are **skipped** for every `COM_QUERY`:

- Query fingerprinting and normalisation
- Query routing rules (`[[query_rules]]`)
- Query rewrite rules (`[[rewrite_rules]]`)
- Result cache
- Read-Your-Own-Writes checks (time-based and GTID-aware)
- N+1 detection
- SQL injection protection
- Query analytics instrumentation
- Slow query logging

The following still apply:

- Connection authentication and per-user rules
- Transaction boundary tracking (`BEGIN` / `COMMIT` / `ROLLBACK`) — sticky `tx_conn` works correctly
- `metrics.queries_total` counter — the dashboard shows query volume
- TLS and compression — configured as normal on the backend

## Configuration

```toml
[mysql]
enabled      = true
listen_addr  = "0.0.0.0:3307"
fast_forward = true   # default: false
```

`fast_forward` is a listener-level option. A single TurbineProxy instance can run one fast-forward listener and one normal listener on different ports simultaneously:

```toml
# Normal listener — full routing + analytics
[mysql]
enabled     = true
listen_addr = "0.0.0.0:3307"

# Fast-forward listener — dedicated write pool
# (run a second turbineproxy instance with a separate config)
```

## Use Cases

**Write-only application pools** — Background workers, job queues, and event ingestion pipelines that only write and never read benefit from the reduced per-query overhead.

**ETL and batch imports** — Large bulk-load sessions where the proxy overhead per query is measurable.

**Message queue consumers** — Services that `INSERT` at high frequency with no routing requirements.

**Benchmarking** — Isolate pure connection-pool throughput from routing overhead.

## What It Is Not

Fast-forward is **not** a replacement for normal operation. It trades observability and safety for throughput. If you need:

- Read/write splitting → use normal mode
- SQL injection protection → use normal mode
- Slow query detection → use normal mode
- Query routing rules → use normal mode

## Transaction Behaviour

Even in fast-forward mode, the proxy tracks transaction state from the SQL text:

| Statement | Effect |
|-----------|--------|
| `BEGIN` / `START TRANSACTION` | Sets `in_transaction = true`; subsequent queries use the same backend connection |
| `COMMIT` | Executes on the sticky connection, then releases it back to the pool |
| `ROLLBACK` | Same as COMMIT — connection released |
| Any other query in a transaction | Routed to the same connection opened by `BEGIN` |

This means multi-statement transactions work correctly — you do not need to manage connection affinity in your application.
