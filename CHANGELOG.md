# Changelog

All notable changes to TurbineProxy are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versioning follows [Semantic Versioning](https://semver.org/).

---

## [0.3.0] - 2026-05-09

### Features

- **`server_version` per listener** — Configure the version string sent to clients in the
  initial handshake independently per port. `[mysql]` defaults to `"8.0.36-TurbineProxy"`;
  `[pgsql]` defaults to `"16.0"`. Useful when migrating to/from Aurora, Cloud SQL, or
  MariaDB without changing applications that depend on the version string.

- **Per-rule `fast_forward`** — In addition to the global listener-level fast-forward mode,
  individual `[[query_rules]]` now support `fast_forward = true`. Only queries matching
  that rule bypass the full pipeline (fingerprinting, routing, cache, RYOW, N+1 detection,
  SQL injection protection, analytics). All other traffic retains full observability.

- **Per-rule `qps_limit`** — Rate-limit the number of queries per second a rule forwards to
  the backend using a token bucket. Short bursts are absorbed immediately; sustained traffic
  above the limit is rejected with an error. `0` or omit = unlimited.

- **Per-rule `dry_run`** — Mark a rule as `dry_run = true` to log matches without applying
  routing. Match statistics appear normally in the dashboard. Enables safe rule validation
  in production before going live.

- **Per-backend `max_connections`** — Cap the maximum number of open connections to each
  primary or replica independently via the `max_connections` field in `[[shared.replicas]]`
  or `[[shared.primary]]`.

- **PROXY Protocol v2** — Full support for the binary PROXY Protocol v2 format in
  `[mysql.proxy_protocol]` and `[pgsql.proxy_protocol]`, in addition to the existing v1
  (text) support.

### Performance

- **Lookup table in query classifier** — Query intent classification (`SELECT`, `INSERT`,
  `UPDATE`, …) now uses a compile-time 256-entry uppercase lookup table, eliminating
  per-query heap allocations in the hot path.

### Dashboard

- Query rules form now includes a **Fast forward** toggle alongside QPS limit and Dry run.
- Rules table has a new **Fast-fwd** column with a visual indicator.

### Documentation

- README: feature table updated to reflect all new capabilities.
- `reference.md`: `dry_run`, `qps_limit`, `fast_forward` added to `[[query_rules]]`;
  `server_version` added to `[mysql]` and `[pgsql]`.
- `query-routing.md`: new sections — Dry Run, Rate Limiting, and Per-Rule Fast Forward.
- `fast-forward.md`: new Per-Rule Fast Forward section with a comparison table
  (global vs. per-rule).

---

## [0.2.0] - 2026-04-28

### Features

- **Fast-forward mode** — Listener-level option to bypass the full proxy pipeline for
  dedicated write-only or high-throughput pools (`fast_forward = true` under `[mysql]`).
- **SSL key log** — Export TLS session keys for backend connections to a file for
  Wireshark/decryption workflows (`ssl_keylog_file`).
- **zstd compression** — Backend connection compression using zstd (MySQL 8.0.18+) in
  addition to the existing zlib/deflate support.
- **GTID-aware Read-Your-Own-Writes** — After a write, route subsequent reads to the
  primary until replicas have caught up to the GTID watermark.
- **Embedded MCP server** — Built-in Model Context Protocol server (7 tools) for
  AI assistant integration with the query workload.

### Documentation

- README: added table of contents, active development warning, and MCP auth notes.
