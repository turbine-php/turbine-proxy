---
sidebar_position: 5
---

# HA & Automatic Failover

TurbineProxy includes built-in health monitoring for all backends and can automatically promote a replica to primary when the primary becomes unavailable.

## Enabling HA

```toml
[ha]
enabled                   = true
health_check_interval_secs = 5
max_replica_lag_ms         = 5000
primary_failover_threshold = 3
```

With HA enabled, TurbineProxy spawns a background health checker that periodically connects to all backends and verifies:

1. **Primary**: Can we connect and execute a ping?
2. **Replicas**: Can we connect, and what is `Seconds_Behind_Master`?

## Replica Lag Monitoring

For each replica, TurbineProxy runs:

```sql
SHOW SLAVE STATUS;
-- or
SHOW REPLICA STATUS;  -- MySQL 8.0+
```

And reads `Seconds_Behind_Master`. If the lag exceeds `max_replica_lag_ms`, the replica is marked **unhealthy** and excluded from read routing.

```toml
[ha]
max_replica_lag_ms = 2000   # Exclude replicas lagging more than 2 seconds
```

## Primary Failover

If the primary fails `primary_failover_threshold` consecutive health checks, TurbineProxy will:

1. Mark the primary as unhealthy
2. Select the healthiest replica (lowest lag, highest weight)
3. Promote it as the new primary for write traffic
4. Log the failover event

```toml
[ha]
primary_failover_threshold = 3   # Fail after 3 missed checks (15s at default interval)
```

> **Note**: TurbineProxy performs a **soft failover** at the proxy level — it routes writes to the promoted replica, but does not issue `STOP SLAVE` or `CHANGE MASTER TO` commands. This is safe for use with external orchestrators (Orchestrator, Patroni).

## Manual Cluster Operations

From the **Cluster** panel in the dashboard, you can:

| Action | Description |
|---|---|
| **Recheck Health** | Force an immediate health check on all backends |
| **Trigger Failover** | Manually promote a replica (requires `force: true` if primary is healthy) |
| **Clear Failover** | Restore writes to the original primary after it recovers |

Or via the REST API:

```bash
# Force immediate health check
curl -X POST http://localhost:8080/api/cluster/actions \
  -H 'Content-Type: application/json' \
  -d '{"action": "recheck_health"}'

# Trigger failover (only if primary is already unhealthy)
curl -X POST http://localhost:8080/api/cluster/actions \
  -H 'Content-Type: application/json' \
  -d '{"action": "trigger_failover"}'

# Force failover even if primary is healthy
curl -X POST http://localhost:8080/api/cluster/actions \
  -H 'Content-Type: application/json' \
  -d '{"action": "trigger_failover", "force": true}'

# Clear failover and return to original primary
curl -X POST http://localhost:8080/api/cluster/actions \
  -H 'Content-Type: application/json' \
  -d '{"action": "clear_failover"}'
```

## MySQL Group Replication

TurbineProxy integrates with MySQL InnoDB Cluster (Group Replication) for automatic primary tracking:

```toml
[ha]
enabled          = true
group_replication = true
```

When enabled, TurbineProxy queries `performance_schema.replication_group_members` to track the current primary. If the primary changes due to an election, TurbineProxy routes writes to the new primary automatically within one health check interval.

## Galera / Percona XtraDB Cluster

For Galera-based clusters, enable `wsrep_local_state` checks:

```toml
[ha]
galera_check = true
```

Nodes with `wsrep_local_state` ≠ 4 (Synced) are excluded from routing.

## Backup Replicas

Mark a replica as a backup to use it only as a last resort:

```toml
[[replicas]]
addr   = "replica-dr:3306"
backup = true
```

Backup replicas are only activated when all non-backup replicas are unhealthy.

## Routing Priority (Writes)

TurbineProxy determines the effective primary using this priority order:

1. **Group Replication primary** (if GR monitoring active and elected)
2. **HA failover primary** (if failover has been triggered)
3. **Configured primary** (default)

## Monitoring

View backend health in the dashboard **Backends** tab or via API:

```bash
curl http://localhost:8080/api/backends | jq '.[] | {role, addr, healthy, lag_ms}'
```
