---
sidebar_position: 15
---

# How-To: Hot Reload Configuration

TurbineProxy supports reloading its configuration without restarting the process. Active connections continue uninterrupted, and the new configuration applies immediately to all new queries.

## What Gets Reloaded

The following configuration is reloaded on a hot reload:

- `[[query_rules]]` — routing rules, rate limits, canary rollouts, dry-run rules
- `[[query_rewrites]]` — rewrite rules (add limit, add timeout, replace, block)
- `[[users]]` — user definitions and per-user limits
- `[security]` — injection protection flag, audit log, query whitelist
- `[ha]` — health check settings, lag thresholds

The following configuration is **not** hot-reloadable (restart required):

- `listen_addr` — the proxy listen address
- Backend addresses (`[primary].addr`, `[[replicas]].addr`)
- Pool size (`pool_size`, `connection_max_idle_secs`)
- Dashboard address (`[dashboard].listen_addr`)

## Method 1: HTTP API

Send a POST request to the reload endpoint:

```bash
curl -X POST http://localhost:8080/api/reload
```

Expected response:

```json
{"status":"ok","message":"Configuration reloaded successfully"}
```

If the new config file has a syntax error, the reload fails and the previous configuration remains active:

```json
{"status":"error","message":"TOML parse error at line 42: expected key"}
```

The error message includes the file location, making it easy to fix.

## Method 2: SIGHUP Signal

Send `SIGHUP` to the TurbineProxy process:

```bash
kill -HUP $(pgrep turbineproxy)

# Or if you know the PID:
kill -HUP 12345
```

In a container environment where you have a shell:

```bash
kill -HUP 1   # PID 1 if TurbineProxy is the init process
```

SIGHUP triggers the same reload as the HTTP API. Errors are logged but the process does not crash — the previous configuration remains active.

## Method 3: Kubernetes ConfigMap Update

When running in Kubernetes with the Helm chart, TurbineProxy's config is stored in a ConfigMap. The chart computes a `checksum/config` annotation — any ConfigMap change triggers a rolling restart rather than a hot reload.

For truly zero-downtime config updates in Kubernetes, use the API method inside an init container or a sidecar that watches for ConfigMap changes. This is an advanced pattern; for most teams, a rolling restart (the default Helm behavior) is sufficient.

## Workflow: Safe Config Change in Production

Use this sequence to safely update rules in production:

```bash
# 1. Edit the config file
nano turbineproxy.toml

# 2. Validate the TOML syntax before reloading
turbineproxy validate turbineproxy.toml
# Output: "Configuration is valid" or a parse error

# 3. Hot reload
curl -X POST http://localhost:8080/api/reload

# 4. Verify the new rules are active
curl http://localhost:8080/api/stats | jq .
```

## Verifying the Reload Worked

After a successful reload, check the **Config** tab in the dashboard at `http://localhost:8080`. It shows the currently active configuration, including all query rules and their dry-run status.

To see the exact moment the reload happened, check logs:

```bash
# If using journald:
journalctl -u turbineproxy -n 50

# If running directly:
RUST_LOG=info ./turbineproxy turbineproxy.toml 2>&1 | grep reload
```

Expected log line:

```
INFO turbineproxy::config: Configuration reloaded successfully
```

## What's Next?

- [How-To: Test Query Rules Safely with Dry Run](./howto-query-rules-dry-run)
- [Query Routing and Rate Limiting](./rate-limiting-and-routing)
