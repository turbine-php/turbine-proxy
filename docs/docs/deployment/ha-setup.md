---
sidebar_position: 3
---

# HA Setup

## Two-Instance Active/Active with HAProxy

Run two TurbineProxy instances behind HAProxy for proxy-level redundancy:

```
Clients → HAProxy :3307
           ├── TurbineProxy-1 :3307  →  MySQL Primary
           └── TurbineProxy-2 :3307  →  MySQL Primary
```

Both proxy instances connect to the same MySQL primary and replicas. Use [Cluster Sync](../features/cluster-sync) to keep routing rules in sync.

```toml
# On proxy-1
[cluster]
enabled = true
peers   = ["proxy-2:9090"]
secret  = "shared-secret"

# On proxy-2
[cluster]
enabled = true
peers   = ["proxy-1:9090"]
secret  = "shared-secret"
```

## HAProxy Config

```
frontend mysql_proxy
    bind *:3307
    mode tcp
    default_backend turbineproxy_instances

backend turbineproxy_instances
    mode tcp
    balance roundrobin
    option tcp-check
    server proxy1 proxy-1:3307 check
    server proxy2 proxy-2:3307 check backup
```

Enable PROXY Protocol so TurbineProxy sees real client IPs:

```toml
proxy_protocol = true
```

```
# In HAProxy backend:
server proxy1 proxy-1:3307 check send-proxy
```

## MySQL Primary Failover

TurbineProxy handles MySQL-level failover automatically via `[ha]`. See [HA & Failover](../features/ha-failover) for details.
