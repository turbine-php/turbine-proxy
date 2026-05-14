# Changelog

All notable changes to TurbineProxy are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versioning follows [Semantic Versioning](https://semver.org/).

---

## [0.5.0] - 2026-05-14

### Security & Hardening

- **No lock poisoning** — All internal mutexes and RW-locks migrated from
  `std::sync::{Mutex,RwLock}` to `parking_lot::{Mutex,RwLock}`. Lock poisoning is
  impossible: if a thread panics while holding a lock, the lock is released cleanly
  and subsequent acquires succeed without any `PoisonError` handling. Covered by
  dedicated panic-recovery unit tests.

- **SCRAM-SHA-256 hardening** — PostgreSQL SCRAM-SHA-256 authentication passes a
  full fuzz corpus (15 invariant assertions, randomised per-run) covering malformed
  first/final server messages, short/empty nonce, missing fields, and replay
  variants. No panics or incorrect accept/reject decisions observed.

- **Dashboard auth failure counter** — A new atomic counter
  (`turbineproxy_dashboard_auth_failures_total`) is incremented on every failed
  authentication event: wrong password at login, invalid/expired token in the
  `auth_middleware`, and invalid/expired token presented to `/api/auth/refresh`.
  Exposed via Prometheus.

### Dashboard Auth

- **`POST /api/auth/refresh`** — Endpoint for renewing an authentication token
  without re-entering credentials. The old token is atomically revoked and a new
  UUID token is issued with a fresh TTL. Readonly and admin tokens are both
  supported. Invalid or expired tokens increment the auth failure counter and
  return `401 Unauthorized`.

- **`POST /api/auth/logout`** — Explicitly revoke the current session token.
  The token is removed from the in-memory store immediately. Readonly tokens can
  call this endpoint (previously blocked by the `is_mutating` guard — now
  explicitly exempted alongside `/api/auth/refresh`).

### Reliability

- **Failover flap protection** — Failover events are guarded by a configurable
  cooldown (`failover_cooldown_secs`) and a minimum number of consecutive health
  check passes before a recovered backend is re-admitted
  (`failover_min_recovery_checks`). Prevents oscillation in unstable network
  conditions.

- **Per-backend circuit breaker** — Each backend gets an independent circuit breaker
  (Closed → Open → Half-Open state machine). Once a backend accumulates
  `circuit_breaker_threshold` consecutive failures the circuit opens and requests
  are rejected immediately without hitting the network. After
  `circuit_breaker_timeout_secs` the circuit enters Half-Open and probes with one
  request before fully closing.

- **Bounded connection wait queue** — Connection acquisition from the pool is now
  queued rather than immediately failing. `pool_wait_timeout_ms` caps how long a
  query waits for a connection before returning an error to the client. Prevents
  thundering-herd during traffic spikes.

### Performance

- **Connection multiplexing (Phase A)** — Multiple client sessions can share a
  single backend connection during idle phases. A multiplexing ratio gauge
  (`turbineproxy_multiplex_ratio`) is exported via Prometheus. Values > 1 indicate
  effective connection reuse.

- **PostgreSQL HA parity** — All HA features available for MySQL (health checks,
  replica lag monitoring, weighted read routing, automatic failover, flap
  protection, circuit breakers, bounded wait queue, multiplexing) now apply equally
  to the PostgreSQL listener.

### Observability

- **`turbineproxy_dashboard_auth_failures_total`** — New Prometheus counter for
  monitoring brute-force activity and token lifecycle issues.

- **`turbineproxy_multiplex_ratio`** — New Prometheus gauge for connection
  multiplexing efficiency.

- **`turbineproxy_pg_replica_lag_seconds`** — Prometheus gauge per PostgreSQL
  replica, sourced from `pg_last_xact_replay_timestamp`.

- **`turbineproxy_sessions_pinned_total`** — Counter of sessions pinned to a
  specific backend (user variables, prepared statements, open transactions).

### CI / Quality

- **`clippy::unwrap_used` lint on critical paths** — A dedicated CI step runs
  `clippy` with `-D clippy::unwrap_used` scoped to `src/dashboard/` and
  `src/analytics/`. Any unguarded `.unwrap()` in those paths fails the build.

- **Panic recovery tests** — Two `#[test]` functions in `src/proxy/server.rs`
  assert that `parking_lot::Mutex` and `parking_lot::RwLock` remain usable after
  a thread panics while holding the lock. Run with
  `cargo test --bin turbineproxy parking_lot`.

- **PostgreSQL TLS integration test** — `tests/pg_integration_tests.rs` now
  includes a `pg_tls_connection_with_psql` test that connects with
  `sslmode=require` using the `psql` CLI and asserts `ssl=t` in `pg_stat_ssl`.
  Auto-skips when `psql` is not in `PATH` or when `TEST_PG_SKIP_TLS` is set.

### Dashboard Configuration (breaking)

- **`token_ttl_secs = 0`** no longer means "default 24 h" — it means "tokens
  never expire". Set an explicit positive value to enforce expiry.

---

## [0.3.1] - 2026-05-09

### Fixed

- **Dockerfile**: bump base image from `rust:1.82` to `rust:1.85` so Cargo
  supports `edition2024`, required transitively by `digest 0.11` / `ctutils 0.4.2`.
- **`.deb` packaging**: remove hardcoded cross-compilation path from `assets`
  in `Cargo.toml`; `cargo-deb` now resolves the path dynamically via `--target`.
- **`.rpm` packaging**: same path fix; add `glob = true` for `dashboard/dist`
  to avoid "Is a directory" error in `cargo-generate-rpm`.

---

## [0.3.0] - 2026-05-09

### Features

- **AES-256-GCM at-rest encryption for stored passwords** — Passwords saved through the
  dashboard are encrypted before being written to SQLite when `TURBINEPROXY_SECRET_KEY`
  (64-char hex) is set. On-disk format: `enc:<base64url(nonce || ciphertext)>`. Existing
  plaintext values and `env:`/`file:` references continue to work without migration.

- **External secret references (`env:` / `file:`)** — Any `password` field in TOML or
  SQLite now accepts `env:VAR` (reads environment variable) or `file:/path` (reads file,
  trims whitespace) in addition to literal values. The actual secret never touches SQLite.

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
