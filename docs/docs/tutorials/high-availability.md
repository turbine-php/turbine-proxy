---
sidebar_position: 9
---

# High Availability and Automatic Failover

TurbineProxy's HA module continuously monitors all backends and can automatically reroute traffic when a replica falls behind or becomes unavailable. This tutorial covers health checks, lag thresholds, failover triggers, and cluster-specific checks for InnoDB Cluster and Galera/XtraDB.

## How Health Checks Work

TurbineProxy polls each backend on a configurable interval using a lightweight query (`SELECT 1` for connectivity; `SHOW SLAVE STATUS` / `SHOW REPLICA STATUS` for lag). Backends that fail consecutive checks are marked unhealthy and removed from the routing pool until they recover.

## Enabling HA

```toml
[ha]
enabled = true
```

With just this, TurbineProxy:
- Polls each backend every 5 seconds
- Marks a backend unhealthy after 3 consecutive failures
- Excludes lagging replicas from read routing
- Never routes reads to an unhealthy replica

## Configuration Reference

```toml
[ha]
enabled                    = true

# How often to probe each backend (seconds)
health_check_interval_secs = 5

# Replicas with lag above this are excluded from reads (milliseconds)
max_replica_lag_ms         = 5000

# How many consecutive health-check failures before marking primary unhealthy
primary_failover_threshold = 3
```

## Replica Lag Checking

When `[ha].enabled = true`, TurbineProxy queries each replica's lag using `SHOW SLAVE STATUS` (MySQL < 8.0) or `SHOW REPLICA STATUS` (MySQL 8.0+). The `Seconds_Behind_Master` / `Seconds_Behind_Source` field determines lag.

If a replica's lag exceeds `max_replica_lag_ms`, it is **excluded from reads** for that health-check cycle. Reads fall through to other healthy replicas or the primary.

Set a tight threshold for applications that need fresh reads:

```toml
[ha]
enabled            = true
max_replica_lag_ms = 500   # Exclude replicas that are > 500ms behind
```

## Backup Replicas

Mark a replica as `backup = true` to hold it in reserve. Backup replicas only receive traffic when all non-backup replicas are unhealthy:

```toml
[[replicas]]
addr   = "db-replica-primary-region:3306"
weight = 100
backup = false

[[replicas]]
addr   = "db-replica-dr-region:3306"
weight = 100
backup = true   # Only used if the primary-region replica fails
```

## Primary Failover

TurbineProxy performs a **proxy-level soft failover** — it never issues `STOP SLAVE`, `STOP REPLICA`, `FLUSH LOGS`, or any other administrative command on your databases. It only changes where it routes traffic.

When the primary fails `primary_failover_threshold` consecutive health checks:
1. TurbineProxy marks the primary as unhealthy
2. All traffic (reads and writes) stops flowing to it
3. An event is logged and visible in the dashboard

You are responsible for promoting a replica to primary at the database level (e.g., `CHANGE REPLICATION SOURCE TO` or via an orchestrator). Once the new primary is reachable at the configured address, TurbineProxy recovers automatically.

### Manual Failover API

To manually trigger or inspect failover state:

```bash
# Force an immediate health recheck
curl -X POST http://localhost:8080/api/backends/recheck_health

# Trigger proxy-level failover (marks primary as unhealthy)
curl -X POST http://localhost:8080/api/backends/trigger_failover

# Clear failover state after database-level recovery
curl -X POST http://localhost:8080/api/backends/clear_failover
```

## InnoDB Cluster / MySQL Group Replication

If you run MySQL Group Replication (InnoDB Cluster), enable the group replication check:

```toml
[ha]
enabled           = true
group_replication = true
```

With this enabled, TurbineProxy queries `performance_schema.replication_group_members` to determine which member is the PRIMARY and which are SECONDARYs, rather than using `SHOW SLAVE STATUS`. This is the correct check for multi-primary / single-primary Group Replication topologies.

## Galera / Percona XtraDB Cluster

For Galera-based clusters (Percona XtraDB Cluster, MariaDB Galera Cluster):

```toml
[ha]
enabled      = true
galera_check = true
```

With `galera_check = true`, TurbineProxy queries `wsrep_local_state` to verify that each node is in state `4` (Synced) before routing queries to it. Nodes in a non-synced state (joining, donor, desync) are excluded.

## Monitoring Backend Health

Check the current state of all backends from the REST API:

```bash
curl http://localhost:8080/api/backends | jq '.[] | {role, addr, healthy, lag_ms, in_failover}'
```

Or open the **Backends** tab in the dashboard for a visual view.

## Example: Production-Ready HA Configuration

```toml
[ha]
enabled                    = true
health_check_interval_secs = 3      # More frequent checks in production
max_replica_lag_ms         = 2000   # Strict lag threshold
primary_failover_threshold = 2      # Fail faster: only 2 consecutive failures

[[replicas]]
addr   = "db-replica-1:3306"
weight = 100

[[replicas]]
addr   = "db-replica-2:3306"
weight = 100

[[replicas]]
addr   = "db-replica-dr:3306"
backup = true
```

## What's Next?

- [Secrets Encryption](./secrets-encryption)
- [Prometheus and Grafana Integration](./prometheus-grafana)
- [Kubernetes with Helm](./kubernetes-helm)
