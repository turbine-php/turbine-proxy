---
sidebar_position: 9
---

# TLS Configuration

See the [Full Configuration Reference](./reference#frontend-tls-client--proxy) for all TLS fields.

## Client → Proxy TLS

```toml
[frontend_tls]
enabled = true
cert    = "/etc/turbineproxy/proxy.crt"
key     = "/etc/turbineproxy/proxy.key"
```

## Proxy → Backend TLS

Configured per backend:

```toml
[primary]
tls_mode = "verify-identity"
tls_ca   = "/etc/ssl/certs/rds-ca.pem"
```

### TLS modes

| Value | Behaviour |
|-------|-----------|
| `"off"` | No TLS (default) |
| `"required"` | TLS required; server certificate not validated |
| `"verify-ca"` | Validate certificate against `tls_ca` |
| `"verify-identity"` | Validate certificate + hostname (use for RDS / Cloud SQL) |

## PostgreSQL TLS

TurbineProxy performs a real TLS upgrade for PostgreSQL connections on both directions:

- **Client → Proxy:** When the client sends an `SSLRequest` (e.g. `sslmode=require`), TurbineProxy replies `S` and upgrades the raw socket to TLS before the handshake proceeds. Clients using `sslmode=disable` or `sslmode=prefer` (without TLS configured) continue to work on the plain channel.
- **Proxy → Backend:** TurbineProxy sends `SSLRequest` to the backend and upgrades the raw socket before splitting for async I/O. This enables full TLS with cloud providers that mandate it.

Cloud PostgreSQL example:

```toml
[pgsql.primary]
addr     = "my-db.postgres.database.azure.com:5432"
tls_mode = "verify-identity"
tls_ca   = "/etc/ssl/certs/ca-certificates.crt"
```

## Mutual TLS (mTLS)

```toml
[frontend_tls]
enabled = true
cert    = "/etc/turbineproxy/proxy.crt"
key     = "/etc/turbineproxy/proxy.key"
ca      = "/etc/turbineproxy/client-ca.crt"
require = true   # Reject clients without a valid certificate
```

## SSL Key Log (Debug Only)

TurbineProxy can write TLS session secrets to a file in NSS Key Log Format, compatible with Wireshark and `ssldump`. This is a **debug-only** feature — never enable in production.

```toml
# Frontend (client → proxy)
[frontend_tls]
enabled         = true
cert            = "/etc/turbineproxy/proxy.crt"
key             = "/etc/turbineproxy/proxy.key"
ssl_keylog_file = "/tmp/frontend-keys.log"   # debug only

# Backend (proxy → database)
[primary]
tls_mode        = "verify-identity"
ssl_keylog_file = "/tmp/backend-keys.log"    # debug only
```

See the [SSL Key Log feature guide](../features/ssl-keylog) for security considerations and a Wireshark workflow.
