---
slug: /
sidebar_position: 1
---

# Introduction

**TurbineProxy** is an intelligent MySQL & PostgreSQL proxy designed for product teams — not DBAs. It gives you read/write splitting, query analytics, automatic index advice, and a real-time dashboard in under 5 minutes, with zero application code changes.

## Why TurbineProxy?

Most teams struggle with:

- **Scaling reads** — Replication exists, but getting your app to use replicas requires code changes and careful transaction handling
- **Visibility** — Identifying slow queries and getting actionable advice without setting up Prometheus, Grafana, and a DBA
- **Safety** — Preventing runaway queries, connection storms, and accidental writes from read-only contexts

TurbineProxy solves all three by sitting between your application and your database.

```
Your App  →  TurbineProxy :3307  →  Primary (writes)
                                 →  Replica 1 (reads)
                                 →  Replica 2 (reads)
```

## Key Features

| Feature | Description |
|---|---|
| **Read/Write Splitting** | SELECTs go to replicas automatically; writes always go to primary |
| **Connection Pooling** | Persistent backend pools reduce connection overhead |
| **Query Analytics** | Every query fingerprinted, counted, timed, and stored in SQLite |
| **Slow Query Detection** | Configurable threshold, logged with fingerprint and latency |
| **N+1 Detection** | Detects repeated queries within a session (ORM anti-pattern) |
| **Query Routing Rules** | PCRE regex rules to route specific queries to specific backends |
| **Query Rewriting** | Inject LIMIT, timeouts, or transform SQL on the fly |
| **HA & Failover** | Automatic primary failover, replica lag monitoring |
| **SQL Injection Protection** | Pattern-based detection with optional blocking |
| **Audit Log** | Immutable NDJSON append-only log of all queries |
| **Real-time Dashboard** | React web UI with live metrics, heatmap, and cluster view |
| **Grafana Integration** | SimpleJSON datasource for Grafana dashboards |
| **MCP Server** | AI-queryable documentation and config assistance |

## How It Works

TurbineProxy implements the full MySQL wire protocol. Your application connects to TurbineProxy exactly as it would connect to a MySQL server — same host, same port (you choose), same credentials. No driver changes, no connection string changes (except the port number).

Internally, TurbineProxy:

1. **Authenticates** the client using your configured users list
2. **Classifies** each query as Read, Write, Transaction control, or Other
3. **Routes** the query to the appropriate backend (primary or replica)
4. **Fingerprints** the query by normalizing all literal values to `?`
5. **Records** execution time, affected rows, and error state
6. **Forwards** the backend response back to the client unchanged

All of this happens in under 1ms of added latency on a local network.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      TurbineProxy                        │
│                                                          │
│  ┌─────────────┐    ┌──────────────┐   ┌─────────────┐ │
│  │  MySQL Wire │    │    Router    │   │  Analytics  │ │
│  │  Protocol   │───▶│  Classifier  │──▶│  Collector  │ │
│  │  (TCP 3307) │    │  Rule Engine │   │  (SQLite)   │ │
│  └─────────────┘    └──────────────┘   └─────────────┘ │
│                             │                            │
│                    ┌────────┴────────┐                   │
│                    ▼                 ▼                   │
│             ┌────────────┐   ┌────────────┐             │
│             │ Primary    │   │ Replica    │             │
│             │ Pool       │   │ Pool (×N)  │             │
│             └────────────┘   └────────────┘             │
│                                                          │
│  ┌─────────────────────────────────────────────────────┐│
│  │           Dashboard (Axum HTTP :8080)               ││
│  └─────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────┘
```

## Next Steps

- [Install TurbineProxy](./getting-started/installation)
- [Quick Start guide](./getting-started/quick-start)
- [Full configuration reference](./configuration/reference)
