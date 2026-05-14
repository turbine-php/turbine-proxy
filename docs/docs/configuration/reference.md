---
sidebar_position: 1
---

# Full Configuration Reference

Complete list of every configuration key available in `turbineproxy.toml`.

TurbineProxy supports two configuration formats — see the
[Configuration Overview](../getting-started/configuration) for a comparison.
The sections below document all keys in the **new unified format** (`[shared]`).
Legacy top-level keys (`listen_addr`, `[primary]`, `[[replicas]]`) behave
identically and are noted where relevant.

---

## Shared Backend Settings

Defines backend credentials and pool defaults shared by all protocol listeners.
When a `[mysql]` or `[pgsql]` section does not override a field, the value from
`[shared]` is used.

```toml
[shared]
max_connections          = 1000
pool_size                = 20
auth_cache_ttl_secs      = 300
connection_max_idle_secs = 55
read_your_own_writes_ms  = 0
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_connections` | int | `1000` | Maximum simultaneous client connections. New connections are refused beyond this limit |
| `pool_size` | int | `20` | Backend connection pool size per backend |
| `auth_cache_ttl_secs` | int | `300` | How long to cache successfully authenticated credentials (seconds) |
| `connection_max_idle_secs` | int | `55` | Evict idle backend connections older than this (seconds) |
| `read_your_own_writes_ms` | int | `0` | After a write, route reads to primary for this many milliseconds. `0` = disabled |

> **Legacy equivalent:** these keys can also be set at the top level (outside any `[section]`) when using the MySQL-only flat format.

---

## Shared Primary Backend

```toml
[shared.primary]
addr              = "127.0.0.1:5432"
user              = "proxyuser"
password          = ""
database          = "myapp"
tls_mode          = "off"
tls_ca            = ""
tls_cert          = ""
tls_key           = ""
init_connect      = []
resolution_family = "system"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `addr` | string | — | Backend address as `host:port` |
| `user` | string | — | Database user for backend connections |
| `password` | string | `""` | Database password. Supports `env:VAR`, `file:/path`, or a literal. Literal values are encrypted with AES-256-GCM when `TURBINEPROXY_SECRET_KEY` is set. See [Secret Management](../features/secret-management.md). |
| `database` | string | `""` | Default database to select on connect |
| `tls_mode` | string | `"off"` | Backend TLS: `off`, `required`, `verify-ca`, `verify-identity` |
| `tls_ca` | string | `""` | Path to CA certificate file |
| `tls_cert` | string | `""` | Path to client certificate file |
| `tls_key` | string | `""` | Path to client private key file |
| `init_connect` | array | `[]` | SQL statements to execute on each new backend connection |
| `resolution_family` | string | `"system"` | Address resolution: `system`, `ipv4`, `ipv6` |

> **Legacy equivalent:** `[primary]`

---

## Shared Read Replicas

Define one or more read replicas. TurbineProxy distributes read traffic using weighted round-robin.

```toml
[[shared.replicas]]
addr   = "replica-1:5432"
user   = "proxyuser"
password = ""
database = "myapp"
weight = 100
backup = false
# All other fields same as [shared.primary]
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `weight` | int | `100` | Relative weight for round-robin. Higher = more traffic. Set to `0` to disable |
| `backup` | bool | `false` | If `true`, this replica is only used when all non-backup replicas are down |

> **Legacy equivalent:** `[[replicas]]`

---

## Shared Users

Per-user access control. If no `[[shared.users]]` are defined, TurbineProxy passes credentials directly to the backend (transparent auth).

```toml
[[shared.users]]
name                   = "app"
password               = "apppass"
allow_writes           = true
max_connections        = 0
default_schema         = ""
transaction_isolation  = ""
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `name` | string | — | Database username |
| `password` | string | — | Password. Supports `env:VAR`, `file:/path`, or a literal. Hashed in memory, never logged. See [Secret Management](../features/secret-management.md). |
| `allow_writes` | bool | `true` | If `false`, only `SELECT`, `SHOW`, `EXPLAIN` are permitted |
| `max_connections` | int | `0` | Per-user connection limit. `0` = unlimited |
| `default_schema` | string | `""` | Automatically issue `USE <schema>` on connect |
| `transaction_isolation` | string | `""` | Override isolation level: `READ-UNCOMMITTED`, `READ-COMMITTED`, `REPEATABLE-READ`, `SERIALIZABLE` |

> **Legacy equivalent:** `[[users]]`

---

## MySQL Listener

Enables the MySQL protocol listener. In the unified format this section is
**required** to start the MySQL listener; it is not started implicitly.
In the legacy flat format, the MySQL listener starts automatically whenever
`[primary]` or a top-level `listen_addr` is present.

```toml
[mysql]
enabled     = true
listen_addr = "0.0.0.0:3307"
# All other fields inherit from [shared] when not set here.
# [mysql.primary], [[mysql.replicas]], [[mysql.users]] are also supported.
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Start the MySQL proxy listener |
| `listen_addr` | string | `"0.0.0.0:3307"` | TCP address for the MySQL listener |
| `server_version` | string | `"8.0.36-TurbineProxy"` | Version string sent to clients in the initial handshake. Override when frameworks or ORMs require a specific version (e.g. `"5.7.44-aurora"`) |

---

## PostgreSQL Listener

```toml
[pgsql]
enabled                    = true
listen_addr                = "0.0.0.0:5432"
health_check_database      = "postgres"
pool_size                  = 20
max_connections            = 0
connection_max_idle_secs   = 55
read_your_own_writes_ms    = 0
health_check_interval_secs   = 10
max_replica_lag_ms           = 5000
primary_failover_threshold   = 3
failover_cooldown_secs       = 30
failover_min_recovery_checks = 3
ssl_cert                     = ""
ssl_key                      = ""
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable the PostgreSQL proxy listener |
| `listen_addr` | string | `"0.0.0.0:5433"` | TCP address for the PostgreSQL listener |
| `health_check_database` | string | `"postgres"` | Database used exclusively for backend health probes (`SELECT 1`, `pg_is_in_recovery()`). Client sessions may connect to any database; probes are always anchored to this control DB. Must exist on every backend |
| `pool_size` | int | inherited | Override pool size for PostgreSQL connections |
| `max_connections` | int | inherited | Override max client connections for PostgreSQL |
| `connection_max_idle_secs` | int | inherited | Override idle connection eviction timeout |
| `read_your_own_writes_ms` | int | inherited | After a write, route reads to primary for this many ms |
| `health_check_interval_secs` | int | `10` | How often to probe backends (seconds) |
| `max_replica_lag_ms` | int | `5000` | Replicas lagging more than this are marked unhealthy |
| `primary_failover_threshold` | int | `3` | Consecutive failed health checks before promoting a replica |
| `failover_cooldown_secs` | int | `30` | Seconds to hold failover after primary recovery (flap protection) |
| `failover_min_recovery_checks` | int | `3` | Consecutive OK checks before clearing failover |
| `ssl_cert` | string | `""` | Path to PEM certificate for client→proxy TLS |
| `ssl_key` | string | `""` | Path to PEM private key for client→proxy TLS |
| `server_version` | string | `"16.0"` | PostgreSQL version string sent to clients at startup. Override when migrating to/from Aurora, Cloud SQL, etc. (e.g. `"15.4-aurora"`) |

Backend, replica, and user overrides follow the same pattern as `[shared]`:

```toml
# [pgsql.primary]
# addr     = "pg-primary:5432"
# user     = "postgres"
# password = ""
# database = "myapp"

# [[pgsql.replicas]]
# addr   = "pg-replica-1:5432"
# weight = 100

# [[pgsql.users]]
# name         = "app"
# password     = "secret"
# allow_writes = true
```

---

---

## Per-Connection Timeouts

```toml
max_transaction_time_ms  = 0
max_query_time_ms        = 0
max_transaction_idle_ms  = 0
select_version_forwarding = true
shutdown_timeout_secs    = 30
client_error_limit       = 0
log_prepared_params      = false
proxy_protocol           = false
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_transaction_time_ms` | int | `0` | Abort transactions running longer than this (ms). `0` = disabled |
| `max_query_time_ms` | int | `0` | Kill queries running longer than this via `KILL QUERY <id>` (ms). `0` = disabled |
| `max_transaction_idle_ms` | int | `0` | Abort transactions idle for this long (ms). `0` = disabled |
| `select_version_forwarding` | bool | `true` | Respond to `SELECT VERSION()` locally without a backend round-trip |
| `shutdown_timeout_secs` | int | `30` | On SIGTERM, wait up to this many seconds for in-flight queries to finish |
| `client_error_limit` | int | `0` | Disconnect a client after N consecutive backend errors. `0` = disabled |
| `log_prepared_params` | bool | `false` | Log prepared statement parameter bytes (hex) in slow query log |
| `proxy_protocol` | bool | `false` | Enable PROXY Protocol v1 (for HAProxy, AWS NLB). Extracts real client IP |

---

## Query Routing Rules

Route specific SQL patterns to specific backends. Rules are evaluated in order; the first match wins.

```toml
[[query_rules]]
match_pattern        = ""
match_digest         = ""
user                 = ""
schema               = ""
destination          = "replica"
destination_hostgroup = -1
cache_ttl_secs       = 0
mirror_to            = -1
rollout_pct          = 100
dry_run              = false
qps_limit            = 0
fast_forward         = false
comment              = ""
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `match_pattern` | string | `""` | PCRE regex matched against the raw SQL string |
| `match_digest` | string | `""` | Exact match against the normalized fingerprint (literals replaced with `?`) |
| `user` | string | `""` | Restrict this rule to a specific MySQL user. Empty = any user |
| `schema` | string | `""` | Restrict this rule to a specific database. Empty = any schema |
| `destination` | string | `"replica"` | Route to: `primary`, `replica`, or `any` (heuristic) |
| `destination_hostgroup` | int | `-1` | Route to a specific backend index: `0` = primary, `1`…`N` = replicas. `-1` = use `destination` field |
| `cache_ttl_secs` | int | `0` | Cache query result for this many seconds. `0` = disabled |
| `mirror_to` | int | `-1` | Fire-and-forget shadow copy to another hostgroup. `-1` = disabled |
| `rollout_pct` | int | `100` | Percentage of matching traffic to apply this rule (1–100). For canary rollouts |
| `dry_run` | bool | `false` | Log the match but skip routing — the query falls through to the next rule or the default heuristic. Use for safely testing new rules in production |
| `qps_limit` | int | `0` | Maximum queries per second for this rule (token bucket). Excess queries are rejected immediately with an error. `0` = unlimited |
| `fast_forward` | bool | `false` | Bypass the full routing, analytics, and security pipeline for matching queries. More surgical than the global `fast_forward` listener option |
| `comment` | string | `""` | Human-readable description shown in the dashboard |

**Examples:**

```toml
# Force all reporting queries to primary
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*reports"
destination   = "primary"
comment       = "Reports require fresh data"

# Route by fingerprint to replica
[[query_rules]]
match_digest = "SELECT id, name FROM users WHERE status = ?"
destination  = "replica"

# Read-only user always goes to replica
[[query_rules]]
user        = "readonly"
destination = "replica"

# Canary: send 10% of heavy analytics to replica 2
[[query_rules]]
match_pattern         = "(?i)SELECT.*FROM.*analytics"
destination_hostgroup = 2
rollout_pct           = 10
```

---

## Query Rewriting Rules

Transform, limit, or block SQL queries before they reach the backend.

```toml
[[query_rewrites]]
match_pattern = ""
replace_with  = ""
add_limit     = 0
add_timeout_ms = 0
block         = false
comment       = ""
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `match_pattern` | string | — | **Required.** PCRE regex matched against the raw SQL |
| `replace_with` | string | `""` | Replacement string. Supports `$1`, `$2` backreferences |
| `add_limit` | int | `0` | Inject `LIMIT N` to unbounded SELECT queries. `0` = disabled |
| `add_timeout_ms` | int | `0` | Inject `/*+ MAX_EXECUTION_TIME(N) */` optimizer hint. `0` = disabled |
| `block` | bool | `false` | Reject this query with an error instead of executing it |
| `comment` | string | `""` | Human-readable description |

**Examples:**

```toml
# Block schema dumps
[[query_rewrites]]
match_pattern = "(?i)SELECT.*FROM.*information_schema"
block         = true
comment       = "Block information_schema queries from app"

# Cap unbounded exports
[[query_rewrites]]
match_pattern = "(?i)SELECT .+ FROM exports"
add_limit     = 10000
comment       = "Prevent full table scans on exports"

# Rewrite legacy table name
[[query_rewrites]]
match_pattern = "(?i)(FROM|JOIN)\\s+old_users"
replace_with  = "$1 users"
comment       = "Compatibility alias"
```

---

## Analytics

```toml
[analytics]
enabled        = true
db_path        = "turbineproxy_analytics.db"
slow_query_ms  = 100
retention_days = 30
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable query telemetry and analytics storage |
| `db_path` | string | `"turbineproxy_analytics.db"` | Path to the SQLite database file |
| `slow_query_ms` | int | `100` | Queries slower than this are logged as slow queries (ms) |
| `retention_days` | int | `30` | Analytics data older than this is pruned automatically |

---

## Dashboard

```toml
[dashboard]
enabled            = true
listen_addr        = "0.0.0.0:8080"
username           = ""
password           = ""
readonly_username  = ""
readonly_password  = ""
token_ttl_secs     = 86400
login_max_attempts = 5
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable the web dashboard and REST API |
| `listen_addr` | string | `"0.0.0.0:8080"` | TCP address for the dashboard HTTP server |
| `username` | string | `""` | Dashboard admin login username. Empty = no authentication |
| `password` | string | `""` | Dashboard admin login password. Empty = no authentication |
| `readonly_username` | string | `""` | Optional read-only user. Empty = disabled. Gets dashboard visibility without write access |
| `readonly_password` | string | `""` | Password for the read-only user |
| `token_ttl_secs` | int | `86400` | Session token lifetime in seconds. `0` = never expires. Default is 24 hours |
| `login_max_attempts` | int | `5` | Maximum failed login attempts per source IP per minute before returning HTTP 429. `0` = disabled |

---

## High Availability

```toml
[ha]
enabled                      = true
health_check_interval_secs   = 5
max_replica_lag_ms           = 5000
primary_failover_threshold   = 3
failover_cooldown_secs       = 30
failover_min_recovery_checks = 3
galera_check                 = false
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable backend health checking and automatic failover |
| `health_check_interval_secs` | int | `5` | How often to check backend health (seconds) |
| `max_replica_lag_ms` | int | `5000` | Replicas lagging more than this are marked unhealthy and excluded from routing |
| `primary_failover_threshold` | int | `3` | Consecutive failed health checks before promoting a replica to primary |
| `failover_cooldown_secs` | int | `30` | After primary recovery, keep failover active for this many seconds before clearing. Prevents flapping. `0` = clear immediately (legacy behaviour) |
| `failover_min_recovery_checks` | int | `3` | Consecutive successful health checks required before clearing a failover. Symmetric to `primary_failover_threshold` |
| `galera_check` | bool | `false` | Enable Galera/Percona XtraDB Cluster `wsrep_local_state` health checks |

---

## Security

```toml
sql_injection_protection = false
audit_log               = ""
query_whitelist         = []
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `sql_injection_protection` | bool | `false` | Detect and block SQL injection patterns before query reaches backend |
| `audit_log` | string | `""` | Path to append-only NDJSON audit log file. Empty = disabled |
| `query_whitelist` | array | `[]` | List of allowed query fingerprints. All others are rejected. Empty = all allowed |

---

## Frontend TLS (Client → Proxy)

Enable TLS between your application and TurbineProxy:

```toml
[frontend_tls]
enabled  = true
cert     = "/etc/turbineproxy/proxy.crt"
key      = "/etc/turbineproxy/proxy.key"
ca       = ""
require  = false
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable TLS on the proxy listener |
| `cert` | string | — | Path to server certificate |
| `key` | string | — | Path to server private key |
| `ca` | string | `""` | Path to CA for mutual TLS (mTLS). Empty = no client cert required |
| `require` | bool | `false` | Reject clients that do not present a valid certificate |

---

## Cluster Sync

Synchronize configuration changes across multiple TurbineProxy instances:

```toml
[cluster]
enabled = true
peers   = ["proxy-2:9090", "proxy-3:9090"]
secret  = "shared-secret-key"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable cluster config sync |
| `peers` | array | `[]` | List of peer addresses to sync with |
| `secret` | string | `""` | Shared secret for peer authentication |
