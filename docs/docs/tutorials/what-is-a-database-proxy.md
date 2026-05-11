---
sidebar_position: 1
---

# What Is a Database Proxy — and Why Use One?

Before running TurbineProxy, it helps to understand what a database proxy does, the problems it solves, and when you should use one.

## The Problem

Your application connects directly to a single database server:

```
App → MySQL (127.0.0.1:3306)
```

This works fine at small scale, but as traffic grows you hit several walls:

| Problem | Symptom |
|---|---|
| **Too many connections** | MySQL's `max_connections` is exhausted; new connections fail |
| **Read bottleneck** | All `SELECT` queries hit the same server even when replicas exist |
| **No visibility** | You can't see which queries are slow without expensive profiling |
| **No safety net** | A badly written query can take down the whole database |
| **Credentials in config files** | Passwords stored in plaintext across many application servers |

## What a Database Proxy Does

A database proxy sits between your application and the database, transparently intercepting all SQL traffic:

```
App → TurbineProxy (127.0.0.1:3307) → MySQL primary (127.0.0.1:3306)
                                     ↘ MySQL replica-1 (127.0.0.1:3308)
                                     ↘ MySQL replica-2 (127.0.0.1:3309)
```

The application uses the **same MySQL driver** and sees TurbineProxy as if it were a normal MySQL server. No application code changes are needed.

## What TurbineProxy Adds

### Read/Write Splitting
`SELECT` queries go to replicas. `INSERT`, `UPDATE`, `DELETE`, and DDL go to the primary. This happens automatically based on SQL classification — your app doesn't need to manage two connection strings.

### Connection Pooling
TurbineProxy maintains a small, persistent pool of connections to each backend (default: 20 per backend). Hundreds of application connections share those pool connections, dramatically reducing MySQL's connection overhead.

### Query Analytics
Every query is fingerprinted, timed, and stored. You get P95/P99 latency, slow query detection, N+1 pattern alerts, and a throughput heatmap — all visible in the built-in dashboard.

### Query Routing and Rewriting
Route specific queries to specific backends (e.g., send analytics to a dedicated replica). Inject `LIMIT`, add timeouts, or block dangerous patterns — without touching application code.

### High Availability
TurbineProxy monitors all backends and can automatically promote a replica to primary if the primary becomes unavailable.

### Security
AES-256-GCM encryption for credentials stored in SQLite, SQL injection protection, per-user access control, and an audit log.

## TurbineProxy vs. ProxySQL

Both tools solve the same class of problems. Key differences:

| Feature | TurbineProxy | ProxySQL |
|---|---|---|
| Language | Rust | C++ |
| PostgreSQL support | ✓ | Partial (added in v2.x, experimental) |
| Built-in dashboard | ✓ | ✗ (needs external tools) |
| Prometheus metrics | ✓ | via exporter |
| SQL injection protection | ✓ (dedicated module) | via query rules (manual patterns) |
| Credential encryption | ✓ (AES-256-GCM, `enc:` prefix) | ✗ (plaintext in SQLite) |
| Configuration | TOML file | MySQL-style CLI (admin port 6032) |
| Binary size | ~12 MB | ~50 MB |
| License | Apache-2.0 | GPL-3.0 |

## What's Next?

Now that you understand the concept, install TurbineProxy and run it in front of your database in under 5 minutes:

→ [Install and Run TurbineProxy](./install-and-run)
