---
sidebar_position: 17
---

# How-To: Read/Write Splitting with Stored Procedures

`CALL` statements go to the primary by default — TurbineProxy cannot inspect the SQL inside a procedure at runtime, so it plays it safe. This how-to shows you how to route read-only procedures to replicas and keep mixed procedures on the primary.

## Default Behavior

| Statement | Default destination |
|---|---|
| `SELECT`, `SHOW`, `EXPLAIN` | Replica |
| `INSERT`, `UPDATE`, `DELETE`, DDL | Primary |
| `CALL any_procedure()` | **Primary** |

Because a `CALL` could do anything — reads, writes, or both — TurbineProxy routes all calls to the primary unless you tell it otherwise.

## Creating a Query Rule — Dashboard or TOML

You can create query rules in two ways:

### Option 1: Dashboard (recommended for quick iteration)

Open the dashboard at `http://localhost:8080`, go to **Query Rules** and click **Add Rule**. Fill in:

| Field | Value |
|---|---|
| Match Pattern | `(?i)CALL\s+proc_relatorio` |
| Destination | `replica` |
| Comment | `proc_relatorio is read-only` |

Click **Save** and the rule is active immediately — no file edit or reload needed. The dashboard also shows a **matches** counter per rule so you can confirm it's firing.

### Option 2: TOML config

Add the rule to `turbineproxy.toml` and reload:

```toml
[[query_rules]]
match_pattern = "(?i)CALL\\s+proc_relatorio"
destination   = "replica"
comment       = "proc_relatorio is read-only — route to replica"
```

```bash
curl -X POST http://localhost:8080/api/reload
```

## Routing a Read-Only Procedure to a Replica

If you know a procedure only runs `SELECT` statements, create a rule (via dashboard or TOML) pointing it to a replica:

```toml
[[query_rules]]
match_pattern = "(?i)CALL\\s+proc_relatorio"
destination   = "replica"
comment       = "proc_relatorio is read-only — route to replica"
```

From now on, every `CALL proc_relatorio(...)` goes to a replica. All other `CALL` statements still go to the primary.

## Matching Multiple Read-Only Procedures

Use a regex alternation to cover a group of known safe procedures:

```toml
[[query_rules]]
match_pattern = "(?i)CALL\\s+(get_report|list_orders|summary_by_month)"
destination   = "replica"
comment       = "Read-only reporting procedures — send to replica"
```

Or match by naming convention — for example, if all read-only procedures start with `rpt_`:

```toml
[[query_rules]]
match_pattern = "(?i)CALL\\s+rpt_"
destination   = "replica"
comment       = "All rpt_* procedures are read-only"
```

## Procedures That Mix Reads and Writes

Leave them as-is. They'll continue going to the primary, which is the correct behavior.

```toml
# No rule needed — CALL proc_process_order() goes to primary automatically
```

If a procedure starts read-only but is later modified to include writes, you don't need to touch TurbineProxy — the absence of a rule means it keeps going to the primary.

## Testing Before Enabling

Before routing to a replica in production, enable `dry_run` first. The rule will match and count hits but won't change routing:

**Via dashboard:** add the rule with the **Dry Run** checkbox ticked. The **Query Rules** table shows a live **matches** counter per rule — watch it grow as procedures are called. When you're satisfied only the right procedures are matching, untick **Dry Run** and save.

**Via TOML:**

```toml
[[query_rules]]
match_pattern = "(?i)CALL\\s+proc_relatorio"
destination   = "replica"
dry_run       = true
comment       = "DRY RUN: verify proc_relatorio routing"
```

```bash
curl -X POST http://localhost:8080/api/reload
```

Check the dashboard **Query Rules** tab for the matches counter. Once confirmed, remove `dry_run = true` and reload again.

## Summary

| Scenario | Config needed |
|---|---|
| Procedure with only SELECTs | Add `[[query_rules]]` with `destination = "replica"` |
| Procedure with writes or mixed | Nothing — goes to primary by default |
| Procedure that changes over time | No rule; primary is always safe |

## What's Next?

- [Query Routing and Rate Limiting](./rate-limiting-and-routing)
- [How-To: Test Query Rules Safely with Dry Run](./howto-query-rules-dry-run)
