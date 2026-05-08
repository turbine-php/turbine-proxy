---
sidebar_position: 9
---

# TLS Configuration

See the [Full Configuration Reference](./reference#frontend-tls-client--proxy) for TLS fields.

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

## Mutual TLS (mTLS)

```toml
[frontend_tls]
enabled = true
cert    = "/etc/turbineproxy/proxy.crt"
key     = "/etc/turbineproxy/proxy.key"
ca      = "/etc/turbineproxy/client-ca.crt"
require = true   # Reject clients without a valid certificate
```
