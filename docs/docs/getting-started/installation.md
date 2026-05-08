---
sidebar_position: 1
---

# Installation

## Requirements

- **Rust** 1.75 or later (for building from source)
- **MySQL** 5.7+ or **MariaDB** 10.3+ (target backend)
- **Linux** or **macOS** (Windows not currently tested)

## Building from Source

```bash
git clone https://github.com/turbineproxy/turbineproxy.git
cd turbineproxy
cargo build --release
```

The binary will be at `target/release/turbineproxy`.

## Running

```bash
# Copy and edit the example config
cp turbineproxy.example.toml turbineproxy.toml
$EDITOR turbineproxy.toml

# Run
./target/release/turbineproxy turbineproxy.toml
```

You can also run with debug logging:

```bash
RUST_LOG=debug ./target/release/turbineproxy turbineproxy.toml
```

## Verifying the Installation

Once running, TurbineProxy exposes two ports by default:

| Port | Purpose |
|------|---------|
| `3307` | MySQL proxy (your app connects here) |
| `8080` | Web dashboard + REST API |

Check health:

```bash
curl http://localhost:8080/health
# → {"status":"ok"}
```

Connect via MySQL client:

```bash
mysql -h 127.0.0.1 -P 3307 -u myuser -pmypassword mydb
```

## Dashboard

Open `http://localhost:8080` in your browser to access the real-time dashboard.

If you configured `[dashboard].username` and `[dashboard].password`, you will be prompted to log in.

## Development Mode

To run the frontend dev server (hot-reloading UI) alongside the backend:

```bash
# Terminal 1: Backend
RUST_LOG=info cargo run -- turbineproxy.toml

# Terminal 2: Frontend dev server
cd dashboard
npm install
npm run dev
```

The dev server defaults to port `5173`. Configure ports via environment variables:

```bash
FRONTEND_PORT=3000 VITE_API_ORIGIN=http://localhost:8080 npm run dev
```

> **Note:** `VITE_API_ORIGIN` is auto-detected from `turbineproxy.toml` if not set explicitly.
