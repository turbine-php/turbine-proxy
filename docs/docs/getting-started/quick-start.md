---
sidebar_position: 2
---

# Quick Start

Get TurbineProxy running in front of your MySQL database in under 5 minutes.

## Step 1: Minimal Configuration

Create `turbineproxy.toml` with the bare minimum:

```toml
# Proxy listen address — your app connects here instead of MySQL directly
listen_addr = "0.0.0.0:3307"

[primary]
addr     = "127.0.0.1:3306"
user     = "root"
password = "yourpassword"
database = "myapp"

[analytics]
enabled = true

[dashboard]
enabled    = true
listen_addr = "0.0.0.0:8080"
```

## Step 2: Run

```bash
./target/release/turbineproxy turbineproxy.toml
```

Expected output:

```
INFO turbineproxy: TurbineProxy v0.1.0 starting
INFO turbineproxy: Proxy listening on 0.0.0.0:3307
INFO turbineproxy: Dashboard listening on 0.0.0.0:8080
INFO turbineproxy: Primary: 127.0.0.1:3306
```

## Step 3: Point Your App

Change your database connection string from:

```
mysql://root:password@localhost:3306/myapp
```

To:

```
mysql://root:password@localhost:3307/myapp
```

That's it. Your app now goes through TurbineProxy.

## Step 4: Add a Read Replica (Optional)

Once you have a MySQL replica set up, add it to your config:

```toml
[[replicas]]
addr     = "127.0.0.1:3308"
user     = "root"
password = "yourpassword"
database = "myapp"
weight   = 100
```

Restart TurbineProxy. All SELECT queries will now automatically route to the replica.

## Step 5: Open the Dashboard

Navigate to `http://localhost:8080` to see:

- Live query counts (reads vs. writes)
- Slow query list
- Backend pool utilization
- N+1 detection alerts

## What's Next?

| Goal | See |
|---|---|
| Add multiple replicas with weights | [Backends Configuration](../configuration/backends) |
| Add user-level access control | [Users Configuration](../configuration/users) |
| Create query routing rules | [Routing Rules](../configuration/routing-rules) |
| Set up HA with automatic failover | [HA & Failover](../features/ha-failover) |
| Enable SQL injection protection | [Security](../configuration/security) |
| Expose metrics to Grafana | [Grafana Integration](../features/grafana-integration) |
