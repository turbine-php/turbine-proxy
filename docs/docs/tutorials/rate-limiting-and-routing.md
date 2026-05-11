---
sidebar_position: 8
---

# Query Routing and Rate Limiting

Query rules (`[[query_rules]]`) control where each query is sent and how much traffic is allowed through. This tutorial covers destination routing, rate limiting, canary rollouts, shadow traffic, and safe rule testing with dry run.

## The Query Rule Structure

```toml
[[query_rules]]
# Matching (all conditions must match; omit to match all queries)
match_pattern       = ""    # PCRE regex on raw SQL
match_digest        = ""    # PCRE regex on normalized SQL (fingerprint)
user                = ""    # Exact username
schema              = ""    # Exact schema/database name

# Actions (any combination)
destination         = ""    # "primary" or "replica"
destination_hostgroup = 0   # 0 = primary, 1..N = specific replica
qps_limit           = 0     # Max queries/second (token bucket); 0 = no limit
rollout_pct         = 100   # Percentage of matching queries to which this rule applies
dry_run             = false # Log the match; do not change behavior
mirror_to           = ""    # Shadow destination: "primary" or "replica"
fast_forward        = false # Skip remaining rules; apply immediately
comment             = ""    # Label shown in logs and dashboard
```

Rules are evaluated in order. The first rule that **matches** and is **not a dry_run** applies. Dry-run rules log the match but never change routing.

## Routing by Destination

### Force reads to primary

```toml
[[query_rules]]
match_pattern = "(?i)SELECT .* FROM audit_log"
destination   = "primary"
comment       = "Audit log reads must see the latest data"
```

### Force writes to primary (default — but explicit is clearer)

```toml
[[query_rules]]
user        = "batch_import"
destination = "primary"
comment     = "Batch import user always writes to primary"
```

### Route to a specific replica (by hostgroup index)

Hostgroup `0` is always the primary. Replicas are numbered `1`, `2`, `3`... in the order they appear in the config:

```toml
[[query_rules]]
match_digest         = "SELECT .* FROM monthly_summary"
destination_hostgroup = 2     # Always use the second replica
comment              = "Monthly reports land on dedicated analytics replica"
```

## Rate Limiting (QPS Limit)

Limit how many queries per second a pattern can generate. Excess queries are rejected immediately with an error:

```toml
[[query_rules]]
match_digest = "SELECT .* FROM products"
qps_limit    = 500
comment      = "Cap product catalog reads at 500 QPS"
```

```toml
[[query_rules]]
user      = "public_api"
qps_limit = 100
comment   = "Public API user capped at 100 QPS"
```

The rate limiter uses a **token bucket** algorithm. Each second, the bucket refills to `qps_limit` tokens. Each query consumes one token. When the bucket is empty, queries are rejected immediately.

## Canary Rollouts with rollout_pct

Apply a rule only to a percentage of matching traffic. Use this to gradually roll out a destination change:

```toml
# Phase 1: Route 5% of reads to the new replica
[[query_rules]]
match_digest = "SELECT .* FROM products"
destination  = "replica"
rollout_pct  = 5
comment      = "Canary: 5% to new replica"

# Phase 2: Once you're confident, increase to 50%, then 100%
```

When `rollout_pct = 50`, approximately 50% of matching queries go to this rule's destination; the other 50% fall through to the next matching rule.

## Shadow Traffic with mirror_to

Mirror a copy of every matching query to a secondary destination. The original query still goes to its normal destination, and the client receives the real response. The mirrored copy is executed fire-and-forget:

```toml
[[query_rules]]
match_digest = "SELECT .* FROM orders"
mirror_to    = "replica"
comment      = "Shadow reads to new replica for validation"
```

Use mirroring to:
- Test a new replica under real production traffic without risk
- Validate a schema migration by running queries against the new and old schema simultaneously

## Dry Run Mode

Test a rule without changing any routing behavior. TurbineProxy logs every time the rule would have matched:

```toml
[[query_rules]]
match_digest = "SELECT .* FROM sessions"
destination  = "primary"
dry_run      = true
comment      = "Testing: how many session reads would go to primary?"
```

Check the logs or dashboard to see how many queries matched. Then remove `dry_run = true` when you are confident in the rule. See [How-To: Dry Run](./howto-query-rules-dry-run) for a detailed walkthrough.

## Matching: match_pattern vs match_digest

| Field | Input | Use when |
|---|---|---|
| `match_pattern` | Raw SQL text (with literals) | You need to match specific values: `WHERE id = 42` |
| `match_digest` | Normalized SQL fingerprint (literals replaced with `?`) | You want to match a query pattern regardless of parameter values |

Example: both rules match the same query type

```toml
# Matches only queries with status='pending'
[[query_rules]]
match_pattern = "(?i)SELECT .* FROM orders WHERE status='pending'"
qps_limit     = 50

# Matches all "SELECT ... FROM orders WHERE status = ?" regardless of value
[[query_rules]]
match_digest  = "SELECT .* FROM orders WHERE status = \\?"
destination   = "replica"
```

## Applying Changes

Query rules can be reloaded without restarting:

```bash
curl -X POST http://localhost:8080/api/reload
```

Or send `SIGHUP` to the process:

```bash
kill -HUP $(pgrep turbineproxy)
```

## What's Next?

- [High Availability and Automatic Failover](./high-availability)
- [How-To: Test Rules Safely with Dry Run](./howto-query-rules-dry-run)
- [How-To: Hot Reload Configuration](./howto-hot-reload)
