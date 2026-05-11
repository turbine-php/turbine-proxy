---
sidebar_position: 2
---

# Install and Run TurbineProxy

This tutorial walks you through installing TurbineProxy and running it in front of a MySQL or PostgreSQL database. You will have a working proxy in under 5 minutes.

## Prerequisites

- A running MySQL 5.7+ (or PostgreSQL 13+) instance
- Linux or macOS (Windows is not currently tested)

## Step 1: Install

### Option A — One-line installer (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/turbineproxy/turbineproxy/main/scripts/install.sh | sh
```

The installer detects your OS and architecture, downloads the matching release binary, and installs it to `/usr/local/bin/turbineproxy`.

To install to a custom directory:

```bash
curl -fsSL https://raw.githubusercontent.com/turbineproxy/turbineproxy/main/scripts/install.sh \
  | TURBINEPROXY_INSTALL_DIR="$HOME/.local/bin" sh
```

### Option B — Docker

```bash
docker pull ghcr.io/turbine-php/turbine-proxy:latest
```

### Option C — Build from source

```bash
git clone https://github.com/turbine-php/turbine-proxy.git
cd turbine-proxy
cargo build --release
# Binary at: target/release/turbineproxy
```

## Step 2: Create a Configuration File

Generate a ready-to-run configuration interactively:

```bash
turbineproxy init
```

Or create `turbineproxy.toml` manually. Here is the minimal configuration for MySQL:

```toml
# Your app will connect to this address instead of MySQL directly
listen_addr = "0.0.0.0:3307"

[primary]
addr     = "127.0.0.1:3306"
user     = "root"
password = "yourpassword"
database = "myapp"

[analytics]
enabled = true

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
```

For PostgreSQL, use the unified format:

```toml
[shared.primary]
addr     = "127.0.0.1:5432"
user     = "postgres"
password = "yourpassword"
database = "myapp"

[pgsql]
enabled     = true
listen_addr = "0.0.0.0:5433"

[analytics]
enabled = true

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
```

## Step 3: Start TurbineProxy

```bash
./turbineproxy turbineproxy.toml
```

You should see output similar to:

```
INFO turbineproxy: TurbineProxy v0.3.x starting
INFO turbineproxy: Proxy listening on 0.0.0.0:3307
INFO turbineproxy: Dashboard listening on 0.0.0.0:8080
INFO turbineproxy: Primary: 127.0.0.1:3306
```

## Step 4: Verify

Check the health endpoint:

```bash
curl http://localhost:8080/health
# Expected: {"status":"ok"}
```

Connect with a MySQL client:

```bash
mysql -h 127.0.0.1 -P 3307 -u root -p myapp
```

For PostgreSQL:

```bash
psql -h 127.0.0.1 -p 5433 -U postgres myapp
```

## Step 5: Enable Debug Logging (Optional)

If you need to troubleshoot, run with verbose logging:

```bash
RUST_LOG=debug ./turbineproxy turbineproxy.toml
```

## Default Ports

| Port | Purpose |
|------|---------|
| `3307` | MySQL proxy — your app connects here |
| `5433` | PostgreSQL proxy (unified format default) |
| `8080` | Web dashboard + REST API |

## What's Next?

- [Connect Your App to TurbineProxy](./connect-your-app)
- [Explore the Dashboard](./explore-the-dashboard)
- [Add a Read Replica for Read/Write Splitting](./read-write-splitting)
