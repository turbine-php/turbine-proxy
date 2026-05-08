---
sidebar_position: 2
---

# Backends Configuration

See the [Full Configuration Reference](./reference#primary-backend) for all backend fields.

## Primary

```toml
[primary]
addr     = "db-primary:3306"
user     = "proxyuser"
password = "secret"
database = "myapp"
```

## Replicas

```toml
[[replicas]]
addr     = "db-replica-1:3306"
user     = "proxyuser"
password = "secret"
database = "myapp"
weight   = 100
backup   = false

[[replicas]]
addr     = "db-replica-2:3306"
weight   = 200   # Gets twice the traffic
```

## TLS

```toml
[primary]
tls_mode = "verify-identity"
tls_ca   = "/etc/ssl/certs/ca.crt"
```

| `tls_mode` | Behavior |
|---|---|
| `off` | No TLS (default) |
| `required` | TLS required, certificate not verified |
| `verify-ca` | TLS required, CA verified |
| `verify-identity` | TLS required, CA + hostname verified |
