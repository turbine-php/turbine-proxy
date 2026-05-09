---
sidebar_position: 4
---

# Query Routing Rules

Route specific SQL patterns to specific backends using PCRE regex rules. Rules are evaluated in order; the first match wins.

## Basic Usage

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*reports"
destination   = "primary"
comment       = "Reporting queries need fresh data"
```

## Rule Fields

See the [Query Routing Rules configuration reference](../configuration/routing-rules) for all available fields.

## Routing Destinations

| Destination | Behavior |
|---|---|
| `"primary"` | Always routes to the primary backend |
| `"replica"` | Always routes to a replica (round-robin by weight) |
| `"any"` | Use the default heuristic (read/write classification) |

For fine-grained control, use `destination_hostgroup`:

| Index | Backend |
|---|---|
| `0` | Primary |
| `1` | First replica |
| `2` | Second replica |
| `N` | Nth replica |

## Matching Strategies

### By Pattern (PCRE Regex)

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*orders.*WHERE.*created_at"
destination   = "replica"
```

### By Fingerprint

Match the normalized fingerprint (all literals replaced with `?`):

```toml
[[query_rules]]
match_digest = "SELECT id, name FROM products WHERE category_id = ?"
destination  = "replica"
```

### By User

```toml
[[query_rules]]
user        = "analytics"
destination = "replica"
comment     = "Analytics user always reads from replica"
```

### By Schema

```toml
[[query_rules]]
schema      = "reporting_db"
destination = "primary"
```

## Canary Rollouts

Gradually migrate traffic to a new backend:

```toml
[[query_rules]]
match_pattern         = "(?i)SELECT.*FROM.*catalog"
destination_hostgroup = 2     # New replica
rollout_pct           = 10    # 10% of matching queries
comment               = "Testing new catalog replica"
```

## Query Mirroring

Fire-and-forget shadow copy to another backend (for testing):

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*users"
destination   = "replica"
mirror_to     = 2        # Shadow copy to hostgroup 2
```

## Hot Reload

Rules can be reloaded without restart:

```bash
curl -X POST http://localhost:8080/api/reload
```

Or send `SIGHUP` to the process. The config file is re-read and rules are recompiled atomically.

---

## Dry Run

Test a new rule in production without affecting traffic. The rule matches and is logged, but the query falls through to the next rule or the default heuristic:

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*new_feature"
destination   = "replica"
dry_run       = true
comment       = "Validating rule before enabling"
```

Dry-run hits are visible in the dashboard rule statistics.

---

## Rate Limiting (QPS Limit)

Cap the number of queries per second that a rule will forward to the backend. Implemented as a **token bucket** — short bursts up to `qps_limit` tokens are absorbed instantly; sustained traffic above the limit is rejected immediately with an error:

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*analytics"
destination   = "replica"
qps_limit     = 20
comment       = "Limit analytics queries to 20 QPS"
```

Set to `0` (default) for unlimited.

---

## Per-Rule Fast Forward

Bypass the routing, analytics, and security pipeline for queries matching a specific rule. More surgical than the [global `fast_forward` listener option](fast-forward):

```toml
[[query_rules]]
match_pattern = "(?i)^SELECT 1$"
destination   = "primary"
fast_forward  = true
comment       = "Health-check ping — zero-overhead fast path"
```

Only queries that match this rule are fast-forwarded; all other queries continue through the full pipeline. See [Fast-Forward Mode](fast-forward#per-rule-fast-forward) for what is bypassed.
