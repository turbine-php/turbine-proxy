---
sidebar_position: 3
---

# Cluster Panel

The **Cluster** panel provides visibility into MySQL Group Replication or HA state and allows operational actions.

## Cluster Members

Lists all detected Group Replication or HA cluster members:

| Column | Description |
|---|---|
| Address | `host:port` |
| Role | `PRIMARY` or `SECONDARY` |
| State | `ONLINE`, `RECOVERING`, `ERROR`, `OFFLINE`, `UNREACHABLE` |
| Version | MySQL server version |

## Operational Actions

| Action | Description |
|---|---|
| **Recheck Health** | Force immediate health check on all backends |
| **Trigger Failover** | Promote healthiest replica to primary |
| **Clear Failover** | Restore writes to original primary after recovery |

All actions require confirmation. Triggering failover when the primary is healthy requires explicit force confirmation to prevent accidental promotions.

See [HA & Failover](../features/ha-failover) for full documentation.
