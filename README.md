# TurbineProxy

[![CI](https://github.com/turbineproxy/turbineproxy/actions/workflows/ci.yml/badge.svg)](https://github.com/turbineproxy/turbineproxy/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/turbineproxy/turbineproxy/branch/main/graph/badge.svg)](https://codecov.io/gh/turbineproxy/turbineproxy)
[![Crates.io](https://img.shields.io/crates/v/turbineproxy.svg)](https://crates.io/crates/turbineproxy)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)

**High-performance MySQL & PostgreSQL proxy** written in Rust — connection pooling, automatic read/write splitting, GTID-aware consistency guarantees, WAN compression, SQL injection protection, an embedded analytics dashboard, and an MCP server that lets AI assistants reason about your query workload in real time.

> [!WARNING]
> This project is currently under active development.
> Features, APIs, and configuration may change between releases.
> If you find bugs, regressions, or unclear behavior, please open an issue with reproduction steps.

![TurbineProxy Dashboard](docs/static/img/dashboard.png)

```
Client ──TLS──▶ TurbineProxy ──TLS──▶ Primary  (writes + transactions)
                             │
                             ├──────▶ Replica 1 (reads — weighted round-robin)
                             └──────▶ Replica 2 (reads — weighted round-robin)
```

---

## Features

### Connection & Routing

**Read/write splitting** — `SELECT` and `SHOW` go to replicas automatically; writes always go to the primary. Zero application changes.

**Connection pooling** — Persistent per-backend pools with idle eviction, multiplexing, and configurable pool size. Prepared statement connections (`stmt_conn`) are isolated from transaction connections (`tx_conn`) so idle clients do not hold backend slots.

**Query rules** — PCRE regex rules evaluated in order. Route by pattern, normalised fingerprint, username, or schema. Supports weighted hostgroup pinning, query mirroring (fire-and-forget to a canary), and percentage-based traffic rollout.

**GTID-aware Read-Your-Own-Writes** — After a write, the proxy captures the GTID from the primary's OK packet and verifies that a replica has applied it before routing the next read there. If no replica is caught up, the read falls back to the primary automatically. No fixed delays, no application changes.

```toml
[mysql]
gtid_aware_ryow = true
```

**Time-based RYOW** — Simpler fallback: route reads to the primary for a configurable window after any write.

```toml
read_your_own_writes_ms = 500
```

**Fast-forward mode** — Bypass the entire routing pipeline (fingerprinting, query rules, cache, RYOW, N+1 detection, SQL injection scan) and send queries directly to the primary in a single hop. Transaction boundaries are still tracked. Designed for write-only pools, ETL jobs, and message-queue consumers where routing overhead has been profiled as a bottleneck.

```toml
[mysql]
fast_forward = true
```

**Hostgroups** — Assign replicas to numbered groups. Route specific query rules to a specific replica by index. Backup replicas activate automatically when all primary replicas fail.

**Session variable stickiness** — `SET @var = ...` and `SELECT @var :=` pin the session to the same backend connection. The set of sticky statements is replayed if the connection is replaced.

**Query rewriting** — Transform SQL before it reaches the backend: inject `LIMIT N`, append `/*+ MAX_EXECUTION_TIME(N) */`, regex replace, or block outright. All rewrites are visible in the dashboard with hit counters.

---

### Security

**SQL injection protection** — Built-in pattern library covering UNION-based injection, stacked queries, time-delay probes (`SLEEP`, `BENCHMARK`, `pg_sleep`, `WAITFOR DELAY`), out-of-band extraction (`INTO OUTFILE`, `LOAD_FILE`), system command execution (`xp_cmdshell`), encoding evasion (hex, `CHAR()`, URL encoding), boolean-blind patterns, and comment-based obfuscation. Blocked queries return an error packet to the client and increment a Prometheus counter.

```toml
sql_injection_protection = true
```

**Per-user access control** — Define users directly in the proxy with individual passwords, write permissions, and max connection limits. Read-only enforcement happens at the proxy — the backend never sees the rejected write.

```toml
[[shared.users]]
name            = "app_readonly"
password        = "..."
allow_writes    = false
max_connections = 50
```

**Query allowlist** — Restrict execution to a pre-approved set of normalised query fingerprints. Any query not in the list is rejected before reaching the backend.

```toml
query_whitelist = [
  "SELECT * FROM users WHERE id = ?",
]
```

**Audit log** — Append-only NDJSON log of every query: timestamp, user, client IP, SQL text, routing destination, duration (ms), error flag. Designed for logrotate; the proxy re-opens the file on `SIGHUP`.

```toml
audit_log = "/var/log/turbineproxy/audit.log"
```

**TLS — end to end** — Frontend TLS encrypts the client → proxy leg. Backend TLS encrypts the proxy → database leg with `verify-identity` support for RDS / Aurora / Cloud SQL. Configured independently per backend.

```toml
[frontend_tls]
enabled = true
cert    = "/etc/turbineproxy/server.crt"
key     = "/etc/turbineproxy/server.key"

[[shared.replicas]]
tls_mode = "verify-identity"
tls_ca   = "/etc/ssl/certs/rds-ca.pem"
```

**SSL Key Log** — Write TLS session secrets to an [NSS Key Log](https://firefox-source-docs.mozilla.org/security/nss/legacy/key_log_format/index.html) file so Wireshark can decrypt captured traffic. Available on both frontend and backend TLS. **Debug environments only** — never enable in production.

```toml
[frontend_tls]
ssl_keylog_file = "/tmp/sslkeys.log"   # debug only
```

---

### High Availability

**Automatic failover** — Background health checker polls all backends. If the primary fails `N` consecutive checks it is marked unhealthy and writes are rerouted to the best available replica. No restart required.

**Replica lag monitoring** — Replicas exceeding `max_replica_lag_ms` are removed from read rotation until they catch up.

**MySQL Group Replication / InnoDB Cluster** — Background poller queries `performance_schema.replication_group_members` and reroutes writes to the elected GR PRIMARY automatically when the cluster topology changes.

**Galera / Percona XtraDB Cluster** — Health checker reads `wsrep_local_state`; only `SYNCED` (state 4) nodes receive reads.

**PROXY Protocol v1** — Forward real client IPs to backends that support it.

**Multi-node cluster config sync** — When running multiple TurbineProxy instances behind a load balancer, any config reload is pushed to all configured peers atomically via `POST /api/sync`.

---

### Observability

**Query analytics** — Every query is fingerprinted (literals normalised to `?`), counted, timed, and stored in SQLite. The dashboard exposes top-N by count, latency, and error rate with p50/p95/p99 histograms.

**Slow query log** — Queries exceeding a configurable threshold are flagged and surfaced in the analytics panel.

**N+1 detection** — Flags sessions issuing the same fingerprint repeatedly with different parameter values — the classic ORM N+1 anti-pattern.

**Index advisor** — Runs `EXPLAIN` in the background for slow queries and suggests missing indexes when it detects full-table scans.

**Query heatmap** — Time-of-day × day-of-week heatmap of query volume. Useful for capacity planning and identifying unexpected traffic spikes.

**Prometheus metrics** — `GET /metrics` exposes 11 metric families including latency histograms (11 buckets, 1 ms–5 s) with `intent` labels (read/write), per-backend pool gauges, replica lag, and SQL injection block counter. A pre-built Grafana dashboard JSON ships in `dashboard/public/grafana/turbineproxy.json`.

**Real-time dashboard** — React web UI with live pool stats, heatmap, cluster topology, query rules, rewrite rules, backend health, and an inline config editor.

---

### AI Integration — MCP Server

The proxy embeds a [Model Context Protocol](https://modelcontextprotocol.io) server at `POST /mcp`. AI assistants (Claude, GitHub Copilot, GPT-4) can call structured tools to query live proxy data without scraping the dashboard.

```json
{
  "mcpServers": {
    "turbineproxy": { "url": "http://localhost:8080/mcp" }
  }
}
```

| Tool | Returns |
|------|---------|
| `get_pool_stats` | Connection pool utilisation per backend |
| `get_slow_queries` | Top queries by latency with p50/p95/p99 |
| `get_n1_candidates` | Queries flagged as N+1 patterns |
| `get_index_advice` | Index recommendations from EXPLAIN analysis |
| `get_backend_health` | Health, lag, and failover state per backend |
| `get_query_rules` | Active routing rules with hit counters |
| `get_rewrite_rules` | Active rewrite rules with hit counters |

---

### Performance

**WAN compression** — Enable MySQL wire-protocol compression on backend connections to reduce bandwidth. Supports `zlib` (MySQL 5.7+ compatible) and `zstd` (MySQL 8.0.18+ / MariaDB 10.8+, ~3–5× better ratio than zlib). Negotiated at connection time; transparent to the client.

```toml
[[shared.replicas]]
addr        = "wan-replica:3306"
compression = "zstd"   # "off" | "zlib" | "zstd"
```

**Query result cache** — Cache `SELECT` results with a per-rule TTL. Cache invalidation is table-scoped: any write to a table clears all cached results that reference it.

**Timeouts** — Per-connection limits for query time, total transaction time, and transaction idle time. The proxy aborts and recycles the connection when limits are exceeded.

---

### Operations

**Zero-downtime reload** — `SIGHUP` or the dashboard "Reload" button hot-swaps backend config, query rules, rewrite rules, and user definitions without dropping client connections.

**Distribution** — Pre-built static binaries for Linux x86\_64, Linux arm64, macOS Intel, macOS Apple Silicon. Docker image (distroless), Helm chart, AUR, `.deb`, Homebrew formula, systemd unit, and logrotate config.

**Interactive config wizard** — `turbineproxy init` generates a `turbineproxy.toml` interactively.

---

## Quick Start

### One-line Install

```bash
curl -fsSL https://raw.githubusercontent.com/turbineproxy/turbineproxy/main/scripts/install.sh | sh
```

### Manual Setup

```bash
curl -Lo turbineproxy https://github.com/turbineproxy/turbineproxy/releases/latest/download/turbineproxy-x86_64-unknown-linux-musl
chmod +x turbineproxy
turbineproxy init            # interactive config wizard
./turbineproxy               # Dashboard: http://localhost:8080
```

## Docker

```bash
docker run -d \
  -v $(pwd)/turbineproxy.toml:/etc/turbineproxy/turbineproxy.toml:ro \
  -p 3307:3307 -p 8080:8080 \
  ghcr.io/turbineproxy/turbineproxy:latest
```

## Configuration

See [turbineproxy.example.toml](turbineproxy.example.toml) for the full annotated reference or the [configuration docs](https://docs.turbineproxy.com/docs/configuration/reference).

```toml
[shared]
max_connections = 1000
pool_size       = 20

[shared.primary]
addr     = "db-primary:3306"
user     = "proxy"
password = "secret"
database = "myapp"

[[shared.replicas]]
addr        = "db-replica-1:3306"
user        = "proxy"
password    = "secret"
database    = "myapp"
weight      = 100
compression = "zstd"

[mysql]
enabled         = true
listen_addr     = "0.0.0.0:3307"
gtid_aware_ryow = true

[pgsql]
enabled               = true
listen_addr           = "0.0.0.0:5432"
health_check_database = "postgres"

[frontend_tls]
enabled = true
cert    = "/etc/turbineproxy/server.crt"
key     = "/etc/turbineproxy/server.key"

[analytics]
enabled        = true
db_path        = "turbineproxy_analytics.db"
slow_query_ms  = 100
retention_days = 30

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
username    = "admin"
password    = "change-me"

[ha]
enabled                    = true
health_check_interval_secs = 5
max_replica_lag_ms         = 5000
primary_failover_threshold = 3

sql_injection_protection = true
```

## Building from Source

```bash
git clone https://github.com/turbineproxy/turbineproxy
cd turbineproxy
cargo build --release
# target/release/turbineproxy
```

## Testing

```bash
cargo test --bins                                                      # unit tests
docker compose up mysql80 -d && cargo test --test integration_tests   # MySQL integration
docker compose up postgres14 -d && cargo test --test pg_integration_tests  # PG integration
cargo bench -- hot_path                                               # benchmarks
```

## Documentation

Full documentation at **[docs.turbineproxy.com](https://docs.turbineproxy.com)**.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security vulnerabilities: [SECURITY.md](SECURITY.md).

## License

[Apache-2.0](LICENSE-APACHE)


[![CI](https://github.com/turbineproxy/turbineproxy/actions/workflows/ci.yml/badge.svg)](https://github.com/turbineproxy/turbineproxy/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/turbineproxy/turbineproxy/branch/main/graph/badge.svg)](https://codecov.io/gh/turbineproxy/turbineproxy)
[![Crates.io](https://img.shields.io/crates/v/turbineproxy.svg)](https://crates.io/crates/turbineproxy)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)

**High-performance MySQL & PostgreSQL proxy** written in Rust — connection pooling, automatic read/write splitting, GTID-aware read-your-own-writes, WAN compression, SQL injection protection, an embedded analytics dashboard, and an MCP server that lets AI assistants reason about your query workload.

> [!WARNING]
> This project is currently under active development.
> Features, APIs, and configuration may change between releases.
> If you find bugs, regressions, or unclear behavior, please open an issue with reproduction steps.

![TurbineProxy Dashboard](docs/static/img/dashboard.png)

```
Client ──TLS──▶ TurbineProxy ──TLS──▶ Primary  (writes + transactions)
                            │
                            ├──────▶ Replica 1 (reads — weighted round-robin)
                            └──────▶ Replica 2 (reads — weighted round-robin)
```

---

## What's New

### GTID-aware Read-Your-Own-Writes (P2)

After a write, the proxy records the GTID returned by the primary and, on the next read, verifies that at least one replica has applied that transaction before routing the read there. If no replica is caught up yet, the read falls back to the primary — guaranteeing consistency without a fixed delay.

```toml
[mysql]
gtid_aware_ryow = true   # default: false
```

No application changes needed. Works transparently with `aurora_replica_lag_ms` and standard GTID-based replication. Falls back gracefully on topologies without GTID.

### WAN Compression — zlib & zstd (P2)

Enable MySQL wire-protocol compression on backend connections to reduce bandwidth on high-latency or metered links. Supports both the classic `zlib` (MySQL 5.7+ compatible) and the modern `zstd` algorithm (MySQL 8.0.18+, MariaDB 10.8+, ~3–5× better ratio).

```toml
[[shared.replicas]]
addr        = "wan-replica:3306"
compression = "zstd"   # "off" | "zlib" | "zstd"
```

Compression is negotiated per-backend at connection time; the client-facing side is unaffected.

### MCP Server — AI-Native Database Observability (P2)

TurbineProxy embeds an [MCP (Model Context Protocol)](https://modelcontextprotocol.io) server so AI assistants (Claude, Copilot, GPT-4) can query live proxy data via structured tools — no scraping dashboards, no manual copy-paste.

```
POST /mcp  (JSON-RPC 2.0)
```

Available tools:

| Tool | Description |
|------|-------------|
| `get_pool_stats` | Connection pool utilisation per backend |
| `get_slow_queries` | Top slow queries with p50/p95/p99 latencies |
| `get_n1_candidates` | Queries flagged as N+1 patterns |
| `get_index_advice` | Index recommendations from EXPLAIN analysis |
| `get_backend_health` | Health, lag, and failover state of every backend |
| `get_query_rules` | Active routing rules with hit counters |
| `get_rewrite_rules` | Active rewrite rules with hit counters |

Enable in `turbineproxy.toml` (on by default when the dashboard is enabled):

```toml
[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"

[dashboard.mcp]
enabled = true
```

Example — ask your AI assistant: *"Which queries are causing the most replica lag right now?"* and it will call `get_slow_queries` + `get_backend_health` and reason over live data.

### Fast-Forward Mode — Zero-Overhead Passthrough (P3)

For workloads where routing intelligence is not needed (uniform writes, message queues, batch jobs), fast-forward mode bypasses the entire processing pipeline — no fingerprinting, no query rules, no cache, no RYOW checks, no N+1 detection, no SQL injection scan — and sends queries directly to the primary with a single hop.

```toml
[mysql]
fast_forward = true   # default: false
```

Transaction boundaries (`BEGIN` / `COMMIT` / `ROLLBACK`) are still tracked correctly so sticky connections work as expected. All other sessions running without `fast_forward` are unaffected.

> **When to use:** dedicated pools for write-only services, ETL pipelines, or any path where you have already profiled and confirmed that proxy logic is the bottleneck.

### SSL Keylog — TLS Traffic Debugging (P3)

For enterprise security teams and advanced debugging, TurbineProxy can write TLS session secrets to a file in [NSS Key Log Format](https://firefox-source-docs.mozilla.org/security/nss/legacy/key_log_format/index.html). Wireshark and other tools can load this file to decrypt captured traffic.

```toml
# Client → Proxy TLS
[frontend_tls]
enabled         = true
cert            = "/etc/turbineproxy/server.crt"
key             = "/etc/turbineproxy/server.key"
ssl_keylog_file = "/var/log/turbineproxy/sslkeys.log"  # NSS Key Log

# Proxy → Backend TLS
[[shared.replicas]]
addr            = "replica:3306"
tls_mode        = "verify-identity"
ssl_keylog_file = "/var/log/turbineproxy/backend-sslkeys.log"
```

> [!WARNING]
> TLS key logs contain session secrets that allow full decryption of captured traffic.
> **Never enable this in production.** Restrict the file with `chmod 600`, write to a tmpfs, and delete it immediately after the debugging session. Rotate SIGHUP clears no existing secrets — restart the proxy to start a clean log.

---

## Feature Overview

| Category | Features |
|----------|----------|
| **Protocols** | MySQL 8.0+, MariaDB 10.6+, PostgreSQL 14+ |
| **Routing** | Auto read/write split, query rules (regex/digest/user/schema), hostgroup pinning, weighted round-robin, backup replicas |
| **Consistency** | Time-based RYOW, GTID-aware RYOW, sticky connections for user variables and prepared statements |
| **Pooling** | Per-backend pool, idle eviction, multiplexing, stmt_conn isolation from tx_conn |
| **Compression** | zlib (MySQL 5.7+), zstd (MySQL 8.0.18+) on backend connections |
| **Performance** | Fast-forward mode (zero-overhead passthrough), result cache with TTL, query rewriting (LIMIT injection, timeout hints) |
| **TLS** | Frontend TLS (client → proxy), backend TLS (proxy → DB), verify-identity for RDS/Cloud SQL, NSS Key Log for debugging |
| **Security** | SQL injection protection (UNION, stacked queries, SLEEP, BENCHMARK, INTO OUTFILE, xp_cmdshell, hex evasion…), per-user rules, read-only enforcement, query allowlist, append-only audit log |
| **HA** | Health checks, lag monitoring, automatic failover, Group Replication / InnoDB Cluster awareness, Galera check, PROXY Protocol v1, multi-node cluster config sync |
| **Observability** | Prometheus metrics (10 metric families + histograms), Grafana dashboard JSON, query heatmap, N+1 detector, index advisor, slow query log, per-query tracer |
| **Operations** | Zero-downtime reload (SIGHUP / dashboard), Helm chart, Docker (distroless), AUR / deb / Homebrew packages, systemd unit, logrotate config |
| **AI / Automation** | Embedded MCP server (7 tools) for AI assistant integration |

---

## Quick Start

### One-line Install

```bash
curl -fsSL https://raw.githubusercontent.com/turbineproxy/turbineproxy/main/scripts/install.sh | sh
```

Install a specific release tag:

```bash
curl -fsSL https://raw.githubusercontent.com/turbineproxy/turbineproxy/main/scripts/install.sh | sh -s -- v0.1.0
```

### Interactive Config Wizard

```bash
turbineproxy init
turbineproxy init --output ./deploy/turbineproxy.toml
```

### Manual Setup

```bash
# 1. Download the latest binary (Linux x86_64)
curl -Lo turbineproxy https://github.com/turbineproxy/turbineproxy/releases/latest/download/turbineproxy-x86_64-unknown-linux-musl
chmod +x turbineproxy

# 2. Create a minimal config
cat > turbineproxy.toml << 'EOF'
[shared]
max_connections = 1000
pool_size       = 20

[shared.primary]
addr     = "127.0.0.1:3306"
user     = "root"
password = "secret"
database = "myapp"

[mysql]
enabled     = true
listen_addr = "0.0.0.0:3307"

[pgsql]
enabled = false

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
EOF

# 3. Run
./turbineproxy --config turbineproxy.toml
# Dashboard: http://localhost:8080
# MySQL:     localhost:3307
```

## Docker

```bash
docker run -d \
  -v $(pwd)/turbineproxy.toml:/etc/turbineproxy/turbineproxy.toml:ro \
  -p 3307:3307 -p 8080:8080 \
  ghcr.io/turbineproxy/turbineproxy:latest
```

---

## Configuration

See [turbineproxy.example.toml](turbineproxy.example.toml) for the full annotated reference. A typical production setup:

```toml
[shared]
max_connections = 1000
pool_size       = 20

[shared.primary]
addr     = "db-primary:3306"
user     = "proxy"
password = "secret"
database = "myapp"
# TLS to RDS/Aurora/Cloud SQL:
# tls_mode = "verify-identity"
# ssl_keylog_file = ""   # keep empty in production

[[shared.replicas]]
addr        = "db-replica-1:3306"
user        = "proxy"
password    = "secret"
database    = "myapp"
weight      = 100
compression = "zstd"    # WAN compression

[mysql]
enabled          = true
listen_addr      = "0.0.0.0:3307"
gtid_aware_ryow  = true    # consistency after writes
# fast_forward   = false   # enable only for write-only pools

[pgsql]
enabled               = true
listen_addr           = "0.0.0.0:5432"
health_check_database = "postgres"

[frontend_tls]
enabled = true
cert    = "/etc/turbineproxy/server.crt"
key     = "/etc/turbineproxy/server.key"
# ssl_keylog_file = ""   # keep empty in production

[analytics]
enabled        = true
db_path        = "turbineproxy_analytics.db"
slow_query_ms  = 100
retention_days = 30

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
username    = "admin"
password    = "change-me"

[ha]
enabled                    = true
health_check_interval_secs = 5
max_replica_lag_ms         = 5000
primary_failover_threshold = 3

# SQL injection protection (recommended):
sql_injection_protection = true

# Append-only audit log (optional):
# audit_log = "/var/log/turbineproxy/audit.log"
```

---

## Security

TurbineProxy applies multiple layers of protection:

### SQL Injection Detection

A built-in pattern library (`src/proxy/security.rs`) inspects every inbound query before it reaches the backend. Patterns cover:

- Classic UNION-based injection (`UNION SELECT`, `UNION ALL SELECT`)
- Stacked queries (`;` followed by DML/DDL)
- Time-delay probes (`SLEEP()`, `BENCHMARK()`, `pg_sleep()`, `WAITFOR DELAY`)
- Out-of-band extraction (`INTO OUTFILE`, `INTO DUMPFILE`, `LOAD_FILE()`)
- System command execution (`xp_cmdshell`, `exec()`)
- Encoding evasion (hex literals, `CHAR()` sequences, URL-encoded payloads)
- Boolean-based blind patterns (`1=1`, `1=0`, `'a'='a'`)
- Comment-based obfuscation (`/**/`, `-- -`, `#`)

Blocked queries return a MySQL/PostgreSQL error packet to the client and increment the `turbineproxy_sqli_blocked_total` Prometheus counter. Enable with:

```toml
sql_injection_protection = true
```

### Per-User Access Control

```toml
[[shared.users]]
name            = "app_readonly"
password        = "..."
allow_writes    = false   # DML returns ERR 1290
max_connections = 50

[[shared.users]]
name            = "app_rw"
password        = "..."
allow_writes    = true
max_connections = 200
```

Read-only enforcement is applied at the proxy level — the backend never sees the write.

### Query Allowlist

For high-security environments, restrict execution to a pre-approved set of query fingerprints:

```toml
query_whitelist = [
  "SELECT * FROM users WHERE id = ?",
  "INSERT INTO events (user_id, event) VALUES (?, ?)",
]
```

Any query whose normalised fingerprint is not in the list is rejected before reaching the backend.

### Audit Log

Append-only NDJSON log recording every query: timestamp, user, client IP, SQL text, routing destination, duration (ms), and error flag. Designed for logrotate (proxy re-opens on SIGHUP):

```toml
audit_log = "/var/log/turbineproxy/audit.log"
```

### TLS

- **Frontend (client → proxy):** configure `[frontend_tls]` with a certificate and key. The proxy advertises `CLIENT_SSL` only when TLS is configured.
- **Backend (proxy → database):** set `tls_mode` per backend (`required`, `verify-ca`, `verify-identity`). `verify-identity` validates the hostname against the certificate — use this for RDS / Cloud SQL / Aurora.
- **SSL Key Log:** available for debugging only — see the [SSL Keylog](#ssl-keylog--tls-traffic-debugging-p3) section above for security considerations.

### Credential Handling

- Passwords in `turbineproxy.toml` are never logged.
- The dashboard authentication credential is separate from database credentials.
- SHA-1 and SHA-256 auth tokens are pre-computed at startup and cached with a configurable TTL (`auth_cache_ttl_secs`). Plaintext passwords are not held in memory after the cache is warm.

### Responsible Disclosure

Security vulnerabilities should be reported to **security@turbineproxy.com** — see [SECURITY.md](SECURITY.md). Do not open public issues for security bugs.

---

## MCP Server

The embedded MCP server exposes proxy intelligence to AI coding assistants and automation tools via a simple JSON-RPC 2.0 interface.

**Endpoint:** `POST /mcp` (same port as the dashboard)

**Authentication:** same `username`/`password` as the dashboard (HTTP Basic when set).

**Tools:**

```jsonc
// List available tools
{ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }

// Call a tool
{
  "jsonrpc": "2.0", "id": 2,
  "method": "tools/call",
  "params": { "name": "get_slow_queries", "arguments": { "limit": 10 } }
}
```

| Tool | Key fields returned |
|------|---------------------|
| `get_pool_stats` | `primary`, `replicas[]` — idle, in_use, created, evicted |
| `get_slow_queries` | `fingerprint`, `count`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms` |
| `get_n1_candidates` | `fingerprint`, `call_count`, `distinct_params`, `pattern_score` |
| `get_index_advice` | `table`, `column`, `query_sample`, `estimated_rows`, `suggestion` |
| `get_backend_health` | `addr`, `role`, `healthy`, `lag_ms`, `consecutive_failures` |
| `get_query_rules` | `match_pattern`, `destination`, `hit_count`, `last_match_secs` |
| `get_rewrite_rules` | `match_pattern`, `operation`, `hit_count`, `last_match_secs` |

**VS Code / Claude Desktop integration** — add to your `mcp.json` / `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "turbineproxy": {
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

---

## Prometheus Metrics

`GET http://localhost:8080/metrics` — Prometheus text exposition format v0.0.4.

Key metric families:

| Metric | Type | Labels |
|--------|------|--------|
| `turbineproxy_build_info` | gauge | `version` |
| `turbineproxy_connections_total` | counter | — |
| `turbineproxy_connections_active` | gauge | — |
| `turbineproxy_queries_total` | counter | `intent` (read/write/other) |
| `turbineproxy_query_duration_seconds` | histogram | `intent` — 11 buckets 1ms→5s |
| `turbineproxy_pool_connections` | gauge | `backend`, `role`, `state` |
| `turbineproxy_pool_connections_created_total` | counter | `backend`, `role` |
| `turbineproxy_pool_connections_evicted_total` | counter | `backend`, `role` |
| `turbineproxy_replica_lag_seconds` | gauge | `backend` |
| `turbineproxy_backend_healthy` | gauge | `backend`, `role` |
| `turbineproxy_sqli_blocked_total` | counter | — |

A pre-built Grafana dashboard JSON is available at `dashboard/public/grafana/turbineproxy.json` — import it directly into Grafana.

---

## Building from Source

```bash
git clone https://github.com/turbineproxy/turbineproxy
cd turbineproxy
cargo build --release
# Binary: target/release/turbineproxy
```

Cross-compile for Linux musl (static binary):

```bash
cross build --release --target x86_64-unknown-linux-musl
```

## Testing

```bash
# Unit tests (no database needed)
cargo test --bins

# Integration tests (requires Docker)
docker compose up mysql80 -d
cargo test --test integration_tests -- --test-threads=1

# PostgreSQL integration tests
docker compose up postgres14 -d
cargo test --test pg_integration_tests -- --test-threads=1

# Benchmarks
cargo bench -- hot_path
```

---

## Roadmap

| Priority | Feature | Status |
|----------|---------|--------|
| 🔴 P1 | Runtime config reload via SQLite (ProxySQL-compatible admin interface) | Planned |
| 🔴 P1 | Protocol-level session variable tracking (OK packet `SERVER_SESSION_STATE_CHANGED`) | Planned |
| 🔴 P1 | `COM_CHANGE_USER` and `COM_STMT_SEND_LONG_DATA` full coverage | Planned |
| 🟡 P2 | GTID-aware Read-Your-Own-Writes | ✅ Done |
| 🟡 P2 | zlib / zstd wire compression | ✅ Done |
| 🟡 P2 | Embedded MCP server (7 tools) | ✅ Done |
| 🟢 P3 | Fast-forward mode (zero-overhead passthrough) | ✅ Done |
| 🟢 P3 | SSL Key Log (NSS format, Wireshark-compatible) | ✅ Done |

---

## Documentation

Full documentation at **[docs.turbineproxy.com](https://docs.turbineproxy.com)**.

- [Getting Started](https://docs.turbineproxy.com/docs/getting-started)
- [Configuration Reference](https://docs.turbineproxy.com/docs/configuration/reference)
- [Security Guide](https://docs.turbineproxy.com/docs/features/security)
- [Dashboard & Metrics](https://docs.turbineproxy.com/docs/dashboard)
- [Migration from ProxySQL](https://docs.turbineproxy.com/docs/getting-started/from-proxysql)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see [SECURITY.md](SECURITY.md).

## License

Licensed under [Apache-2.0](LICENSE-APACHE).
