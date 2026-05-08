---
sidebar_position: 3
---

# Users Configuration

See the [Full Configuration Reference](./reference#users) for all user fields.

## Example

```toml
# Read-write application user
[[users]]
name         = "app"
password     = "apppassword"
allow_writes = true
max_connections = 100

# Read-only analytics user
[[users]]
name         = "analytics"
password     = "ropass"
allow_writes = false
default_schema = "myapp"

# Admin user with specific isolation level
[[users]]
name                  = "admin"
password              = "adminpass"
transaction_isolation = "READ-COMMITTED"
```

## Transparent Auth Mode

If no `[[users]]` sections are defined, TurbineProxy operates in **transparent auth mode**: it forwards the client's credentials directly to the backend for validation. This is simpler but provides no per-user access control.
