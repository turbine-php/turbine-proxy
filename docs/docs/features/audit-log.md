---
sidebar_position: 10
---

# Audit Log

TurbineProxy can write an immutable append-only audit log of all queries.

## Configuration

```toml
[security]
audit_log = "/var/log/turbineproxy/audit.ndjson"
```

## Format

Each line is a JSON object (NDJSON):

```json
{"timestamp":"2026-05-08T14:22:01Z","user":"app","client_ip":"10.0.0.5","fingerprint":"INSERT INTO orders (user_id, total) VALUES (?, ?)","affected_rows":1,"duration_ms":2.4,"backend":"db-primary:3306"}
```

## Fields

| Field | Description |
|---|---|
| `timestamp` | ISO 8601 UTC timestamp |
| `user` | MySQL username |
| `client_ip` | Client IP address (real IP if PROXY Protocol enabled) |
| `fingerprint` | Normalized SQL (literals replaced with `?`) |
| `affected_rows` | Rows affected (writes only) |
| `duration_ms` | Query execution time in milliseconds |
| `backend` | Backend address that executed the query |

## Log Rotation

The audit log file is opened in append mode. Use `logrotate` with `copytruncate` to rotate without restarting TurbineProxy:

```
/var/log/turbineproxy/audit.ndjson {
    daily
    rotate 90
    compress
    copytruncate
    missingok
    notifempty
}
```
