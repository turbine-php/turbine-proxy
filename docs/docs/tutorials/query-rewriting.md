---
sidebar_position: 7
---

# Query Rewriting: Add Limits, Timeouts, and Blocks

Query rewriting lets you transform SQL statements before they reach the backend — without modifying your application. This tutorial covers the four transformations available: `add_limit`, `add_timeout_ms`, `replace_with`, and `block`.

## When to Use Query Rewriting

| Problem | Solution |
|---|---|
| App does `SELECT * FROM large_table` with no LIMIT | Inject `LIMIT N` automatically |
| A slow report query can run indefinitely | Add `MAX_EXECUTION_TIME` hint |
| Table was renamed; old code still uses old name | Rewrite the table reference transparently |
| Application should never run `DROP TABLE` | Block the pattern entirely |

## Configuration Structure

```toml
[[query_rewrites]]
match_pattern  = "PCRE regex here"
# One or more transformations:
add_limit      = 0
add_timeout_ms = 0
replace_with   = ""
block          = false
comment        = ""
```

Rules are evaluated **after** routing and **before** the query is sent to the backend. Multiple rewrite rules can match the same query — all are applied in sequence — except `block`, which short-circuits immediately.

## Injection 1: Add LIMIT

Cap the number of rows returned by unbounded `SELECT` queries:

```toml
[[query_rewrites]]
match_pattern = "(?i)^SELECT .+ FROM large_table(?!.*\\bLIMIT\\b)"
add_limit     = 5000
comment       = "Prevent full scan of large_table"
```

The regex `(?!.*\\bLIMIT\\b)` is a negative lookahead — it matches only queries that don't already have a `LIMIT` clause, so you don't double-inject.

If the query already has `LIMIT 10` and your rule sets `add_limit = 5000`, TurbineProxy injects `LIMIT 5000` **only** when no `LIMIT` is already present.

Another common pattern — limit all exports regardless of existing LIMIT:

```toml
[[query_rewrites]]
match_pattern = "(?i)SELECT .+ FROM exports"
add_limit     = 10000
comment       = "Cap export queries at 10k rows"
```

## Injection 2: Add Query Timeout

Inject a `MAX_EXECUTION_TIME` optimizer hint to kill queries that run too long:

```toml
[[query_rewrites]]
match_pattern  = "(?i)SELECT.*FROM.*analytics"
add_timeout_ms = 10000
comment        = "Kill analytics queries after 10 seconds"
```

The injected SQL becomes:

```sql
SELECT /*+ MAX_EXECUTION_TIME(10000) */ * FROM analytics WHERE ...
```

> **Note:** `MAX_EXECUTION_TIME` is a MySQL 5.7.4+ feature. It is a hint, not a hard guarantee — the server may kill the query slightly after the timeout.

## Transformation 3: Rewrite (Replace)

Use a PCRE regex with backreferences to rewrite SQL:

```toml
[[query_rewrites]]
match_pattern = "(?i)(FROM|JOIN)\\s+old_customers"
replace_with  = "$1 customers"
comment       = "Table was renamed; maintain backward compatibility"
```

Before reaching the backend, the query:

```sql
SELECT * FROM old_customers WHERE region = 'EU'
```

becomes:

```sql
SELECT * FROM customers WHERE region = 'EU'
```

Another example — stripping a query comment your ORM always adds:

```toml
[[query_rewrites]]
match_pattern = "/\\*\\s*orm:.*?\\*/"
replace_with  = ""
comment       = "Remove ORM comment noise"
```

## Transformation 4: Block

Reject a query entirely. The client receives an error and the query never reaches the backend:

```toml
[[query_rewrites]]
match_pattern = "(?i)DROP\\s+TABLE"
block         = true
comment       = "Applications must never drop tables directly"
```

```toml
[[query_rewrites]]
match_pattern = "(?i)SELECT.*FROM.*information_schema"
block         = true
comment       = "Block schema introspection from app connections"
```

A blocked query returns a MySQL error:

```
ERROR 1045 (28000): Query blocked by TurbineProxy rewrite rule: <comment>
```

## Combining Rules

Rules are applied in the order they appear in the config file. You can stack multiple rules for the same query:

```toml
# First: cap the rows
[[query_rewrites]]
match_pattern = "(?i)SELECT .+ FROM reports"
add_limit     = 1000

# Second: also add a timeout
[[query_rewrites]]
match_pattern  = "(?i)SELECT .+ FROM reports"
add_timeout_ms = 5000
```

## Hot Reload

Rewrite rules can be reloaded without restarting:

```bash
curl -X POST http://localhost:8080/api/reload
```

Or send `SIGHUP`:

```bash
kill -HUP $(pgrep turbineproxy)
```

## What's Next?

- [Query Routing and Rate Limiting](./rate-limiting-and-routing)
- [How-To: Test Rules Safely with Dry Run](./howto-query-rules-dry-run)
