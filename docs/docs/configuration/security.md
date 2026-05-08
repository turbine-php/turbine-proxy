---
sidebar_position: 10
---

# Security Configuration

See the [Full Configuration Reference](./reference#security) and [SQL Injection Protection](../features/sql-injection-protection) for details.

```toml
[security]
sql_injection_protection = true
audit_log               = "/var/log/turbineproxy/audit.ndjson"
query_whitelist         = []
```
