---
sidebar_position: 16
---

# How-To: Migrate from ProxySQL

This guide helps you migrate from ProxySQL to TurbineProxy. The core concepts are the same (backends, query routing, connection pooling), but the configuration format and some behaviors differ. This how-to maps ProxySQL concepts to their TurbineProxy equivalents.

## Conceptual Mapping

| ProxySQL concept | TurbineProxy equivalent |
|---|---|
| `mysql_servers` (hostgroup 0) | `[primary]` or `[shared.primary]` |
| `mysql_servers` (hostgroup 1+) | `[[replicas]]` or `[[shared.replicas]]` |
| `mysql_users` | `[[users]]` or `[[shared.users]]` |
| `mysql_query_rules` | `[[query_rules]]` |
| Query digest / `match_digest` | `match_digest` (same concept) |
| `mysql_variables.max_connections` | `max_connections` |
| `mysql_variables.connection_max_age_ms` | `connection_max_idle_secs` (seconds) |
| Admin interface (`6032`) | Dashboard REST API (`:8080`) |
| `LOAD ... TO RUNTIME` + `SAVE ... TO DISK` | `curl -X POST http://localhost:8080/api/reload` |
| Stats tables (`stats_mysql_*`) | `/api/stats`, `/api/queries`, `/api/pool`, `/api/backends` |
| Prometheus exporter (plugin) | Built-in `/metrics` endpoint |

## Configuration Migration

### ProxySQL: Backends

ProxySQL uses `mysql_servers`:

```sql
INSERT INTO mysql_servers (hostgroup_id, hostname, port, weight) VALUES
  (0, 'db-primary', 3306, 1),
  (1, 'db-replica-1', 3306, 100),
  (1, 'db-replica-2', 3306, 100);
```

TurbineProxy equivalent (`turbineproxy.toml`):

```toml
[primary]
addr     = "db-primary:3306"
user     = "proxyuser"
password = "yourpassword"
database = "myapp"

[[replicas]]
addr   = "db-replica-1:3306"
weight = 100

[[replicas]]
addr   = "db-replica-2:3306"
weight = 100
```

### ProxySQL: Users

ProxySQL:

```sql
INSERT INTO mysql_users (username, password, default_hostgroup, max_connections)
VALUES ('app', 'apppass', 0, 500);
```

TurbineProxy:

```toml
[[users]]
name            = "app"
password        = "apppass"
allow_writes    = true
max_connections = 500
```

### ProxySQL: Query Rules

ProxySQL uses integer `rule_id` ordering and a mix of regex and digest fields:

```sql
INSERT INTO mysql_query_rules (rule_id, active, match_digest, destination_hostgroup, apply) VALUES
  (10, 1, '^SELECT', 1, 1),
  (20, 1, '^SELECT .* FOR UPDATE', 0, 1);
```

TurbineProxy uses TOML array order (first match wins) and the `destination` field:

```toml
# SELECT FOR UPDATE → primary (must come first — more specific)
[[query_rules]]
match_pattern = "(?i)SELECT.*FOR UPDATE"
destination   = "primary"
comment       = "Locking reads go to primary"

# All other SELECTs → replica
[[query_rules]]
match_digest  = "^SELECT"
destination   = "replica"
comment       = "General reads go to replica"
```

> **Note:** TurbineProxy performs read/write splitting automatically — you don't need explicit rules to send `INSERT`/`UPDATE`/`DELETE` to the primary. Rules are only needed when you want to override the default behavior.

### ProxySQL: max_connections

```ini
# ProxySQL admin
SET mysql-max_connections=1000;
LOAD MYSQL VARIABLES TO RUNTIME;
```

TurbineProxy:

```toml
max_connections = 1000
```

### ProxySQL: connection_max_age_ms

```ini
SET mysql-connection_max_age_ms = 3600000;
```

TurbineProxy (in seconds):

```toml
connection_max_idle_secs = 3500
```

## Key Differences to Be Aware Of

### 1. Configuration is File-Based, Not SQL-Based

ProxySQL stores configuration in an SQLite database and applies changes via `mysql -h 127.0.0.1 -P 6032`. TurbineProxy uses a TOML file. Changes are applied by editing the file and calling the reload API or sending `SIGHUP`.

### 2. No Separate Admin Port

ProxySQL exposes an admin interface on port `6032`. TurbineProxy's equivalent is the dashboard REST API on port `8080`. There is no separate admin SQL interface.

### 3. Read/Write Splitting is Automatic

In ProxySQL, you must write explicit query rules to route `SELECT` to replicas. TurbineProxy auto-classifies queries — `SELECT`, `SHOW`, and `EXPLAIN` go to replicas by default; writes go to the primary. You only write rules for exceptions.

### 4. No "Sharding" or Multi-Schema Routing

TurbineProxy does not implement ProxySQL's multiplexing query rules for sharding across different backends based on schema name. TurbineProxy routes to one primary + N replicas per listener.

### 5. Multiplexing / Connection Multiplexing

ProxySQL uses connection multiplexing (multiple client connections share a single backend connection simultaneously, using a per-query channel model). TurbineProxy uses a connection pool (a client holds a backend connection for the duration of a query, then returns it). The behavior is similar under normal workloads; the implementation differs.

## Migration Checklist

1. **Export your current ProxySQL config**: `SELECT * FROM mysql_servers\G`, `SELECT * FROM mysql_users\G`, `SELECT * FROM mysql_query_rules\G`
2. **Translate backends** to `[primary]` + `[[replicas]]` in `turbineproxy.toml`
3. **Translate users** to `[[users]]`
4. **Translate query rules** — only rules that override automatic read/write split are needed
5. **Test in staging** with the same application traffic (use `dry_run = true` to verify rules match expected queries)
6. **Verify pooling config**: convert `connection_max_age_ms` to `connection_max_idle_secs`, set `pool_size` to match ProxySQL's connection pool size
7. **Update your application** to connect to TurbineProxy's port (`3307` for MySQL) instead of ProxySQL's port (typically `6033`)
8. **Enable the dashboard** and verify read/write counts, latency metrics, and backend health
9. **Cutover**: update DNS or load balancer to point to TurbineProxy; monitor for 10–15 minutes

## Getting Help

- Docs: [https://turbineproxy.dev/docs](https://turbineproxy.dev/docs)
- GitHub Issues: [https://github.com/turbine-php/turbine-proxy/issues](https://github.com/turbine-php/turbine-proxy/issues)
