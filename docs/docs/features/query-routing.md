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
