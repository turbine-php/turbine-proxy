---
sidebar_position: 4
---

# Routing Rules Configuration

See [Query Routing](../features/query-routing) for feature documentation and the [Full Reference](./reference#query-routing-rules) for all fields.

## Quick Examples

```toml
# Route by user
[[query_rules]]
user        = "analytics"
destination = "replica"

# Route by pattern
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*reports"
destination   = "primary"

# Route to specific replica
[[query_rules]]
match_pattern         = "(?i)SELECT.*FROM.*archive"
destination_hostgroup = 2

# Canary: 5% to new backend
[[query_rules]]
match_pattern         = "(?i)SELECT.*FROM.*products"
destination_hostgroup = 3
rollout_pct           = 5
```
