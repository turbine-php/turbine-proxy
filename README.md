# TurbineProxy

[![CI](https://github.com/turbineproxy/turbineproxy/actions/workflows/ci.yml/badge.svg)](https://github.com/turbineproxy/turbineproxy/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/turbineproxy/turbineproxy/branch/main/graph/badge.svg)](https://codecov.io/gh/turbineproxy/turbineproxy)
[![Crates.io](https://img.shields.io/crates/v/turbineproxy.svg)](https://crates.io/crates/turbineproxy)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

**High-performance MySQL & PostgreSQL proxy** with connection pooling, automatic read/write splitting, query analytics, and an embedded dashboard.

![TurbineProxy Dashboard](docs/static/img/dashboard.png)

```
Client → TurbineProxy → Primary (writes)
                      ↘ Replica 1 (reads)
                        Replica 2 (reads)
```

## Features

- **Protocol support**: MySQL 8.0+, MariaDB 10.6+, PostgreSQL 14+
- **Read/write splitting**: automatic routing based on query intent
- **Connection pooling**: configurable pool per backend with health checks
- **Query analytics**: fingerprinting, slow query log, N+1 detection, index advice
- **Embedded dashboard**: real-time metrics, heatmaps, config editor
- **TLS**: frontend TLS for clients + backend TLS (verify-identity for RDS/Cloud SQL)
- **Authentication**: per-user rules, read-only users, credential cache
- **HA**: automatic failover, Group Replication awareness, PROXY Protocol v1
- **Query rules**: regex routing, rewriting, mirroring, SQL injection protection
- **Zero-downtime reload**: `SIGHUP` or dashboard button

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

After installing the binary, generate a config interactively:

```bash
turbineproxy init
```

Choose a custom output path:

```bash
turbineproxy init --output ./deploy/turbineproxy.toml
```

```bash
# 1. Download the latest binary (Linux x86_64)
curl -Lo turbineproxy https://github.com/turbineproxy/turbineproxy/releases/latest/download/turbineproxy-x86_64-unknown-linux-musl
chmod +x turbineproxy

# 2. Create a minimal config
cat > turbineproxy.toml << 'EOF'
[primary]
addr     = "127.0.0.1:3306"
user     = "root"
password = "secret"
EOF

# 3. Run
./turbineproxy --config turbineproxy.toml
# Dashboard: http://localhost:8080
# Proxy:     localhost:3307
```

## Docker

```bash
docker run -d \
  -v $(pwd)/turbineproxy.toml:/etc/turbineproxy/turbineproxy.toml:ro \
  -p 3307:3307 -p 8080:8080 \
  ghcr.io/turbineproxy/turbineproxy:latest
```

## Configuration

See [turbineproxy.example.toml](turbineproxy.example.toml) for all options, or the [full reference](https://docs.turbineproxy.com/docs/configuration/reference).

```toml
listen_addr     = "0.0.0.0:3307"
max_connections = 1000
pool_size       = 20

[primary]
addr     = "db-primary:3306"
user     = "proxy"
password = "secret"

[[replicas]]
addr     = "db-replica-1:3306"
user     = "proxy"
password = "secret"

[analytics]
enabled       = true
slow_query_ms = 100

[dashboard]
addr   = "0.0.0.0:8080"
secret = "change-me"
```

## Building from Source

```bash
git clone https://github.com/turbineproxy/turbineproxy
cd turbineproxy
cargo build --release
# Binary: target/release/turbineproxy
```

## Testing

```bash
# Unit tests (no database needed)
cargo test --bins

# Integration tests (requires Docker)
docker compose up mysql80 -d
cargo test --test integration_tests -- --test-threads=1

# Benchmarks
cargo bench -- hot_path
```

## Documentation

Full documentation at **[docs.turbineproxy.com](https://docs.turbineproxy.com)**.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see [SECURITY.md](SECURITY.md).

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
