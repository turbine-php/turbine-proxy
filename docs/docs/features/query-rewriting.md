---
sidebar_position: 5
---

# Query Rewriting

Transform, limit, or block SQL queries before they reach the backend — without changing your application code.

## Use Cases

- **Cap unbounded queries**: Inject `LIMIT N` to prevent full table scans
- **Add query timeouts**: Inject `MAX_EXECUTION_TIME` optimizer hints
- **Block dangerous queries**: Reject specific patterns entirely
- **Rename tables**: Compatibility aliases during migrations

## Configuration

```toml
[[query_rewrites]]
match_pattern  = "(?i)SELECT .+ FROM exports"
add_limit      = 10000
comment        = "Prevent full table export"
```

## Available Transformations

| Field | Description |
|---|---|
| `replace_with` | Regex replacement (supports `$1`, `$2` backreferences) |
| `add_limit` | Inject `LIMIT N` to unbounded SELECT |
| `add_timeout_ms` | Inject `/*+ MAX_EXECUTION_TIME(N) */` |
| `block` | Reject the query with an error |

## Examples

### Inject LIMIT

```toml
[[query_rewrites]]
match_pattern = "(?i)^SELECT .+ FROM large_table(?!.*LIMIT)"
add_limit     = 5000
comment       = "Cap large_table scans"
```

### Inject Timeout

```toml
[[query_rewrites]]
match_pattern  = "(?i)SELECT.*FROM.*analytics"
add_timeout_ms = 10000
comment        = "Analytics queries max 10s"
```

### Block Pattern

```toml
[[query_rewrites]]
match_pattern = "(?i)DROP TABLE"
block         = true
comment       = "Block DROP TABLE from application connections"
```

### Rename Table (Migration Compatibility)

```toml
[[query_rewrites]]
match_pattern = "(?i)(FROM|JOIN)\\s+old_customers"
replace_with  = "$1 customers"
comment       = "Table was renamed; maintain compatibility"
```

## Order of Evaluation

Rewrite rules are evaluated after routing rules and before the query is sent to the backend. Multiple rewrite rules can match the same query (all are applied in order), except `block` which short-circuits immediately.
