---
sidebar_position: 6
---

# SQL Injection Protection

TurbineProxy can detect and block common SQL injection patterns before they reach the backend.

## Enabling

```toml
[security]
sql_injection_protection = true
```

When enabled, every query is checked against a library of injection patterns. Blocked queries receive an error response; no traffic reaches the backend.

## Monitoring

The `/api/stats` endpoint includes a `sqli_blocked` counter. The dashboard Overview panel displays this count in real time.

## Query Whitelist

For maximum security, you can define a whitelist of allowed query fingerprints. All queries not on the whitelist are rejected:

```toml
[security]
query_whitelist = [
  "SELECT id, name FROM users WHERE id = ?",
  "INSERT INTO orders (user_id, total, status) VALUES (?, ?, ?)",
  "UPDATE orders SET status = ? WHERE id = ? AND user_id = ?"
]
```

Fingerprints use the same normalization as analytics (literals → `?`). Use the dashboard **Queries** tab to find the correct fingerprints for your application's queries.
