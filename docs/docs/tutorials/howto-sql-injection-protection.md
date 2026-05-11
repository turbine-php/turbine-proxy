---
sidebar_position: 13
---

# How-To: Enable SQL Injection Protection

TurbineProxy can detect and block SQL injection patterns before they reach the backend. This how-to shows you how to enable protection, monitor blocked queries, and optionally add a query whitelist for maximum security.

## Step 1: Enable the Filter

Add a `[security]` section to your `turbineproxy.toml`:

```toml
[security]
sql_injection_protection = true
```

Reload or restart TurbineProxy:

```bash
curl -X POST http://localhost:8080/api/reload
```

That's it. Every query is now checked against a library of injection patterns before it reaches the backend.

## What Gets Blocked

The filter detects common SQL injection payloads, including:

- Classic string termination: `' OR '1'='1`
- `UNION`-based exfiltration: `' UNION SELECT user, password FROM mysql.user --`
- Stacked queries: `'; DROP TABLE users --`
- Comment-based injections: `/**/SELECT/**/`, `/*!SELECT*/`
- Boolean-blind payloads: `' AND 1=1 --`, `' AND SLEEP(5) --`

Blocked queries receive an error response:

```
ERROR 1045 (28000): Query blocked by TurbineProxy security filter
```

No traffic reaches the backend — the block happens entirely at the proxy.

## Step 2: Monitor Blocked Queries

Check the injections-blocked counter:

```bash
curl http://localhost:8080/api/stats | jq .sqli_blocked
```

The number increments on every blocked query. Open the dashboard at `http://localhost:8080` and look at the **Overview** panel — blocked SQL injections are displayed as a live count.

## Step 3: Optional — Add an Audit Log

Log every blocked query to a newline-delimited JSON file:

```toml
[security]
sql_injection_protection = true
audit_log                = "/var/log/turbineproxy/audit.ndjson"
```

Each blocked query is written as a JSON object with timestamp, client IP, username, and the rejected query text.

## Step 4: Optional — Strict Whitelist Mode

For maximum security, define the exact set of query fingerprints your application is allowed to run. All other queries — including legitimate ones not on the list — are rejected:

```toml
[security]
query_whitelist = [
  "SELECT id, name FROM users WHERE id = ?",
  "INSERT INTO orders (user_id, total, status) VALUES (?, ?, ?)",
  "UPDATE orders SET status = ? WHERE id = ? AND user_id = ?"
]
```

Fingerprints use the same normalization as the analytics engine: literal values are replaced with `?`. To find the correct fingerprints for your app:

1. Run your application normally with `sql_injection_protection = true` but no `query_whitelist`
2. Open the dashboard at `http://localhost:8080` → **Queries** tab
3. Copy the **Fingerprint** column values for all queries you want to allow
4. Paste them into `query_whitelist`
5. Reload: `curl -X POST http://localhost:8080/api/reload`

> **Warning:** Whitelist mode will block any query not in the list, including queries added by ORM updates or new application features. Build the whitelist from the full set of application queries before enabling in production.

## Testing the Filter

You can verify the filter is active by running a test injection through a MySQL client:

```sql
-- Connect to the proxy, then run:
SELECT * FROM users WHERE id = 1 OR '1'='1';
```

Expected result:

```
ERROR 1045 (28000): Query blocked by TurbineProxy security filter
```

## What's Next?

- [How-To: Test Query Rules Safely with Dry Run](./howto-query-rules-dry-run)
- [How-To: Hot Reload Configuration](./howto-hot-reload)
- [Secrets Encryption](./secrets-encryption)
