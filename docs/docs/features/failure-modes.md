---
title: Failure Mode Reference
sidebar_position: 20
---

# Failure Mode Reference

This page documents every failure mode that TurbineProxy handles, what it does in each case, what the client sees, and how to detect it. Teams evaluating TurbineProxy for production use should read this before deployment.

> **Design principle:** TurbineProxy prefers returning an explicit error to the client over silently masking failure or producing incorrect data. The exception is transparent retries on *fresh* idle connections outside of a transaction — where the retry is provably safe.

---

## 1. Primary database unreachable

| Attribute | Detail |
|-----------|--------|
| **Trigger** | Health checker fails to connect or ping the configured primary N times in a row (`primary_failover_threshold`, default `3`) |
| **Proxy action (HA enabled)** | Promotes the healthy replica with the lowest replication lag as the failover primary (`failover_idx`) |
| **Proxy action (HA disabled)** | All writes and reads-on-primary return `ER_LOST_CONNECTION` / connection error |
| **Client sees** | Connection errors on the first N checks; transparent routing to failover backend afterwards |
| **Writes during failover** | Routed to the promoted replica — `WARN` log per session: `[HA] write query routed through failover backend` |
| **Recovery** | When the primary responds to a health check, `failover_idx` is cleared atomically and routing returns to primary. Logged at `INFO` level. |
| **Observability** | `turbineproxy_ha_failover_active` (gauge), `turbineproxy_ha_failover_events_total` (counter), log prefix `[HA]` at `WARN`/`ERROR` level, `consecutive_failures` per backend in `/api/pool` |
| **Config levers** | `ha.health_check_interval_secs`, `ha.primary_failover_threshold` |

**What does NOT happen:** The proxy does not attempt to restart or reconnect to the database. It does not split writes across backends. It does not promote silently — every failover event is logged at `ERROR` level.

---

## 2. Primary unreachable — no healthy replica available

| Attribute | Detail |
|-----------|--------|
| **Trigger** | Primary fails health checks AND all replicas are unhealthy or none are configured |
| **Proxy action** | Logs `[HA] PRIMARY DOWN — no healthy replica available for failover` at `ERROR` level. `failover_idx` remains `-1`. |
| **Client sees** | All write queries return connection errors. Read queries also fail (replica pool is also empty). |
| **Recovery** | Automatic when either primary or a replica becomes reachable on the next health check cycle |
| **Observability** | `[HA]` log at `ERROR`, `turbineproxy_ha_failover_active = 0` despite primary being down — this is the split-brain worst case |

**Operator action:** This state is indistinguishable from "no HA configured" in metrics. Use the log stream as the primary alert source in this scenario.

---

## 3. Replica unhealthy (lag or connection failure)

| Attribute | Detail |
|-----------|--------|
| **Trigger** | Replica lag exceeds `max_replica_lag_ms`, or connection to replica fails |
| **Proxy action** | Marks replica unhealthy (`healthy = false`). Read queries skip it in weighted round-robin. |
| **If all replicas unhealthy** | Read queries fall back to primary silently. Logged at `DEBUG` level. |
| **Client sees** | No error. Read latency may increase if primary becomes the read backend. |
| **Recovery** | Automatic on next health check when lag drops below threshold. Logged: `[HA] Replica [N] addr lag Xms — back in read pool` |
| **Observability** | `turbineproxy_backend_healthy{role="replica"}`, `turbineproxy_replica_lag_seconds`, `consecutive_failures` in `/api/pool` |

---

## 4. Stale Group Replication state (GR cluster unreachable)

| Attribute | Detail |
|-----------|--------|
| **Trigger** | GR monitor (`gr_checker`) cannot reach any configured backend during a poll cycle |
| **Proxy action** | Leaves `gr_primary_idx` unchanged — routing continues to last known GR primary. Logs `WARN`: `[GR] all N backend(s) unreachable during poll — GR primary discovery stalled` |
| **Client sees** | No error — but writes may be going to an outdated primary |
| **Risk** | If the GR PRIMARY changed while the monitor was blind, writes may be rejected by the new primary (MySQL Group Replication will return an error — it does not silently accept writes to a non-primary member) |
| **Recovery** | Automatic on next successful GR poll |
| **Observability** | `[GR] WARN` log per poll failure. No dedicated metric — monitor log stream. |

---

## 5. Backend connection lost — outside a transaction

| Attribute | Detail |
|-----------|--------|
| **Trigger** | Idle connection in the pool was closed by the backend (MySQL "gone away", TCP reset, EOF) |
| **Proxy action** | Detects the dead connection, discards it, retries the query **once** on a fresh connection |
| **Client sees** | No error. Single transparent retry. |
| **Conditions for retry** | Must be outside a transaction. Error must match `is_connection_lost()` pattern (gone away / lost connection / broken pipe / EOF / 2006 / 2013). |
| **Observability** | `WARN` log: `[pool] replica connection lost, retrying query on fresh connection` or `primary connection lost` |

---

## 6. Backend connection lost — inside a transaction (MySQL)

| Attribute | Detail |
|-----------|--------|
| **Trigger** | The sticky primary connection for an open transaction is dropped mid-flight |
| **Proxy action** | Returns error to client immediately. Clears `tx_conn`. Sets `in_transaction = false`. **No retry.** |
| **Client sees** | MySQL error 2013 / error packet indicating connection lost |
| **Why no retry** | Retrying inside a transaction would re-execute already-committed work. This is never safe without GTID-level replay logic. |
| **Client action** | Client must issue `ROLLBACK` and retry the entire transaction from the beginning |
| **Observability** | Backend error logged at `DEBUG`. Session `tx_conn` is cleared. |

---

## 7. Backend connection lost — inside a transaction (PostgreSQL)

| Attribute | Detail |
|-----------|--------|
| **Trigger** | The sticky primary connection for an open transaction is dropped mid-flight |
| **Proxy action** | Detects connection-lost signals (`broken pipe`, `08006`, `08001`, connection reset). Clears `tx_conn`. Sets `in_transaction = false`. Returns PostgreSQL SQLSTATE `25P02` (in_failed_sql_transaction). |
| **Client sees** | `25P02 in_failed_sql_transaction` error. **The proxy is explicit: the transaction is gone.** |
| **Client action** | Client must issue `ROLLBACK` and retry the transaction |
| **Observability** | `WARN` log: `[pg conn N] backend died mid-tx — clearing tx state, client can retry with ROLLBACK` |

---

## 8. MySQL prepared statement — backend death during COM_STMT_EXECUTE

| Attribute | Detail |
|-----------|--------|
| **Trigger** | `stmt_conn` (the sticky connection that holds open prepared statements) dies |
| **Proxy action** | Re-prepares all open statements on a fresh backend connection. Updates internal stmt_id mapping. Retries the execute on the new connection. |
| **Client sees** | No error if re-prepare succeeds. The client's stmt_id is stable (proxy-level remapping). |
| **When it can fail** | If re-prepare fails (e.g., schema changed, new backend refuses the query), the error is returned to the client normally. |
| **Why this is transparent** | MySQL prepared statements are server-scoped. The proxy maintains a shadow map (`MysqlStmtShadow`) so the client never sees the backend stmt_id — only a stable proxy-assigned id. |
| **Observability** | Re-prepare is logged at `WARN` level. The client sees no indication unless re-prepare itself errors. |

---

## 9. PostgreSQL prepared statement — backend death (PgStmtShadow)

| Attribute | Detail |
|-----------|--------|
| **Trigger** | `stmt_conn` dies with named prepared statements open |
| **Proxy action** | Re-issues `PREPARE` for all tracked named statements on a new backend. Retries the failed pipeline. |
| **When `stmt_conn` is released** | When all named statements are closed (`C` messages) AND no transaction is open — the connection is returned to the pool immediately. |
| **Observability** | `DEBUG` log on stmt_conn release. `WARN` on re-prepare. |

---

## 10. Max connections reached (proxy-level)

| Attribute | Detail |
|-----------|--------|
| **Trigger** | `max_connections` semaphore is exhausted (`try_acquire_owned()` fails) |
| **Proxy action** | Drops the incoming TCP socket immediately. Does not queue. |
| **Client sees** | TCP RST / connection refused |
| **Observability** | `WARN` log: `Max connections reached, rejecting <addr>`. `turbineproxy_connections_active` gauge. |
| **Config** | `max_connections` in `[mysql]` / `[pgsql]` section |

---

## 11. Per-user connection limit exceeded

| Attribute | Detail |
|-----------|--------|
| **Trigger** | User's active connection count exceeds `users[].max_connections` |
| **Proxy action** | Completes the handshake (auth succeeds), then immediately sends MySQL 1040 "Too many connections for this user" and closes |
| **Client sees** | Auth succeeds, then immediate error 1040 |
| **Observability** | `WARN` log per rejection |

---

## 12. Query killed — max_query_time_ms exceeded

| Attribute | Detail |
|-----------|--------|
| **Trigger** | A query runs longer than `max_query_time_ms` |
| **Proxy action** | Times out the frontend wait. Drops the connection to the backend (does not return to pool). Asynchronously sends `KILL QUERY <thread_id>` to clean up the server cursor. |
| **Client sees** | Error: "Query killed: exceeded max_query_time_ms (Nms)" |
| **What is NOT done** | The proxy does not roll back any server-side work. KILL QUERY is best-effort. |
| **Observability** | `turbineproxy_queries_killed_total` counter (via `/api/stats`). `INFO` log on successful kill. |

---

## 13. Transaction killed — max_transaction_time_ms exceeded

| Attribute | Detail |
|-----------|--------|
| **Trigger** | An open transaction exceeds `max_transaction_time_ms` wall-clock time |
| **Proxy action** | Sends error packet to client and closes the session. Clears `tx_conn`. |
| **Client sees** | Error and disconnection |
| **Observability** | `transactions_killed` counter in `/api/stats`. |

---

## 14. Client error limit — client_error_limit exceeded

| Attribute | Detail |
|-----------|--------|
| **Trigger** | Client sends N queries that produce errors within a `client_error_window_secs` window |
| **Proxy action** | Closes the connection after the Nth error |
| **Client sees** | Queries return errors normally until disconnection on the Nth |
| **Purpose** | Defence against misconfigured clients flooding the proxy with bad queries |
| **Observability** | `WARN` log: `[conn N] client error limit reached (N/N errors in Ns window) — closing` |

---

## 15. SQL injection blocked

| Attribute | Detail |
|-----------|--------|
| **Trigger** | Query matches a pattern in the SQL injection detection layer (when `sql_injection_protection = true`) |
| **Proxy action** | Rejects query, logs event, pushes to error event ring buffer |
| **Client sees** | Error 42501 "Query blocked: potential SQL injection detected" |
| **Observability** | `turbineproxy_sqli_blocked_total`, error event in dashboard, `WARN` log with client IP and user |

---

## Summary: degradation matrix

| What fails | HA enabled | Client sees | Writes continue | Reads continue |
|-----------|-----------|-------------|-----------------|----------------|
| Primary (replicas healthy) | Yes | Transparent (after failover) | ✅ via failover replica | ✅ replicas |
| Primary (no healthy replica) | Yes | Connection errors | ❌ | ❌ |
| Primary (HA disabled) | No | Connection errors immediately | ❌ | ❌ |
| One replica down | — | Nothing | ✅ primary | ✅ other replicas |
| All replicas down | — | Nothing (silently falls back) | ✅ primary | ✅ primary (fallback) |
| GR poll blind | — | Nothing (stale routing) | ⚠️ may hit wrong primary | ✅ |
| Backend connection lost (no tx) | — | Nothing (transparent retry) | ✅ | ✅ |
| Backend connection lost (in tx) | — | Error, must retry tx | ❌ client must retry | n/a |
| Max connections | — | TCP RST | ❌ | ❌ |
| Query timeout | — | Error on that query | ✅ others | ✅ others |
| Transaction timeout | — | Error + disconnect | ❌ session killed | ❌ session killed |

---

## Log prefixes reference

| Prefix | Level | Meaning |
|--------|-------|---------|
| `[HA]` | WARN/ERROR/INFO | Health checker event |
| `[HA] FAILOVER:` | ERROR | Failover triggered |
| `[HA] Primary ... recovered` | INFO | Failover cleared |
| `[HA] write query routed through failover backend` | WARN | Write during active failover |
| `[HA/Galera]` | WARN | Galera node state check |
| `[GR]` | INFO/WARN | Group Replication monitor |
| `[pool]` | WARN | Connection pool retry |
| `[pg conn N] backend died mid-tx` | WARN | PG transaction backend death |
| `[conn N] client error limit reached` | WARN | Client disconnected for errors |
| `[kill]` | INFO/WARN | Query kill (KILL QUERY sent) |

---

## Alerting recommendations

| Metric / log | Alert condition | Severity |
|-------------|-----------------|----------|
| `turbineproxy_ha_failover_active == 1` | Anytime | Page |
| `turbineproxy_ha_failover_events_total` increases | Rate > 1 per 5 min | Page |
| `turbineproxy_backend_healthy{role="primary"} == 0` | Anytime | Page |
| `turbineproxy_backend_healthy{role="replica"} == 0` for all replicas | Anytime | Alert |
| `turbineproxy_replica_lag_seconds > 30` | Sustained | Alert |
| `[GR] WARN` log events | Rate > 3 per interval | Alert |
| `turbineproxy_connections_active / max_connections > 0.9` | Sustained | Alert |
