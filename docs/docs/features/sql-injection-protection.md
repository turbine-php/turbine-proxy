---
sidebar_position: 6
---

# SQL Injection Protection

TurbineProxy can detect and block common SQL injection patterns **before they reach the backend**.

## Enabling

```toml
[security]
sql_injection_protection = true
```

When enabled, every query is checked against a library of injection patterns. Blocked queries receive an error response; no traffic reaches the backend.

## Threat Model

The filter is **defense in depth**, not a replacement for parameterized queries in your application.

| Threat | Protected? |
|---|---|
| Script-kiddie scanners and automated probers | ✅ Yes |
| Misconfigured apps sending unsanitized input | ✅ Yes |
| Payload evades pattern matching | ❌ No — use parameterized queries |
| Attacker controls the DB user's permissions | ❌ No — use per-user `allow_writes` |

**The correct security model:** parameterized queries in your app + this filter as a second layer. The filter catches known bad payloads; prepared statements prevent the class of attack entirely.

## What Gets Blocked

The built-in pattern library covers:

- `UNION SELECT` / `UNION ALL SELECT` (data exfiltration)
- Stacked queries via `;` (e.g., `'; DROP TABLE users --`)
- Tautologies: `OR 1=1`, `AND 1=1`
- Comment truncation: `--`, `/* */`
- Time-delay probes: `SLEEP()`, `BENCHMARK()`, `WAITFOR DELAY`
- File system access: `INTO OUTFILE`, `INTO DUMPFILE`, `LOAD_FILE()`
- System commands: `xp_cmdshell`, `sp_executesql`
- Encoding evasion: hex literals (`0x...`), `CHAR()` sequences
- System table probing: `information_schema.`, `performance_schema.`

Blocked queries return a MySQL/PostgreSQL error to the client and increment `sqli_blocked` in `/api/stats`.

## Monitoring

```bash
curl http://localhost:8080/api/stats | jq .sqli_blocked
```

The dashboard Overview panel displays the blocked count in real time.

## Query Whitelist (Maximum Security)

For the strictest posture, define an allowlist of permitted query fingerprints. **All queries not on the list are rejected** — regardless of whether they look malicious:

```toml
[security]
query_whitelist = [
  "SELECT id, name FROM users WHERE id = ?",
  "INSERT INTO orders (user_id, total, status) VALUES (?, ?, ?)",
  "UPDATE orders SET status = ? WHERE id = ? AND user_id = ?"
]
```

Fingerprints use the same normalization as analytics (literals → `?`). Use the dashboard **Queries** tab to find the correct fingerprints for your application's queries.

The whitelist is the only mechanism that provides a strong security guarantee at the proxy layer. The injection filter is additive on top of it.
