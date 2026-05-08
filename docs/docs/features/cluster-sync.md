---
sidebar_position: 9
---

# Cluster Sync

Run multiple TurbineProxy instances and keep their configuration in sync.

## Configuration

```toml
[cluster]
enabled = true
peers   = ["proxy-2:9090", "proxy-3:9090"]
secret  = "shared-secret-key"
```

When a configuration change is made via the REST API (routing rule create/update/delete), it is automatically propagated to all peers via `POST /api/sync` with Bearer token authentication.

## Use Cases

- **Active/Active load balancing**: Multiple proxies behind a load balancer, all routing the same way
- **Geographic distribution**: Regional proxies with synchronized routing rules
- **Blue/green deployments**: Coordinate rule changes across instances

## Security

Peer communication uses a shared secret (`cluster.secret`). All sync requests are authenticated with `Authorization: Bearer <secret>`. Use a strong random value and keep it secret.
