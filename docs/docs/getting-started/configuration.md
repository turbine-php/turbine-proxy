---
sidebar_position: 3
---

# Configuration Overview

TurbineProxy is configured with a single TOML file. Pass the path as the first argument:

```bash
./turbineproxy /etc/turbineproxy/turbineproxy.toml
```

If no argument is given, `./turbineproxy.toml` is used.

## Configuration Formats

TurbineProxy supports two formats in the same file:

### New unified format (recommended)

Use the `[shared]` block to define backend credentials once. Both the MySQL and
PostgreSQL listeners inherit those settings automatically.

```toml
# Shared backend settings — inherited by all protocol listeners
[shared]
max_connections = 1000
pool_size       = 20

[shared.primary]
addr     = "db-primary:5432"
user     = "proxyuser"
password = "secret"
database = "myapp"

# Optional: read replicas
# [[shared.replicas]]
# addr   = "db-replica-1:5432"
# weight = 100

# Optional: per-user access control
# [[shared.users]]
# name         = "readonly"
# password     = "ropass"
# allow_writes = false

# PostgreSQL listener — inherits backend from [shared]
[pgsql]
enabled               = true
listen_addr           = "0.0.0.0:5432"
health_check_database = "postgres"

# MySQL listener — also inherits backend from [shared] (disabled by default)
# [mysql]
# enabled     = true
# listen_addr = "0.0.0.0:3307"

# Analytics, dashboard, HA, security…
[analytics]
enabled      = true
slow_query_ms = 100

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"

[ha]
enabled = true
```

### Legacy MySQL-only format

The original flat format is still fully supported. Defining `[primary]` or a
top-level `listen_addr` activates the MySQL listener automatically.

```toml
listen_addr     = "0.0.0.0:3307"
max_connections = 1000
pool_size       = 20

[primary]
addr     = "db-primary:3306"
user     = "proxyuser"
password = "secret"
database = "myapp"

[[replicas]]
addr   = "db-replica-1:3306"
weight = 100
```

## Hot Reload

Query routing rules, rewrite rules, and backend health thresholds can be reloaded without restart:

```bash
# Via API
curl -X POST http://localhost:8080/api/reload

# Via signal
kill -HUP $(pgrep turbineproxy)
```

A full restart is required for changes to `listen_addr`, `pool_size`, `[shared.primary]` / `[primary]`, or `[[replicas]]`.

## Section Reference

| Section | Purpose |
|---|---|
| `[shared]` | Shared pool settings inherited by all protocol listeners |
| `[shared.primary]` | Shared primary (read-write) backend |
| `[[shared.replicas]]` | Shared read replica backends |
| `[[shared.users]]` | Shared per-user access control |
| `[mysql]` | MySQL listener (new format; use `[primary]` for legacy) |
| `[pgsql]` | PostgreSQL listener and per-protocol overrides |
| `[analytics]` | Query telemetry and slow query log |
| `[dashboard]` | Web UI and REST API |
| `[ha]` | Health checks and automatic failover thresholds |
| `[[query_rules]]` | SQL routing rules |
| `[[query_rewrites]]` | SQL rewriting/blocking rules |
| `[security]` | SQL injection protection, audit log, whitelist |
| `[cluster]` | Multi-instance config sync |
| `[frontend_tls]` | Client→MySQL proxy TLS |
| `[primary]` | Primary backend (legacy MySQL-only format) |
| `[[replicas]]` | Replica backends (legacy MySQL-only format) |

See the [Full Configuration Reference](../configuration/reference) for every available option.
