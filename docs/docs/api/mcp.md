---
sidebar_position: 3
---

# MCP Server

TurbineProxy has **two MCP server implementations**:

1. **Proxy embedded MCP** — runs inside the `turbineproxy` binary, served at `POST /mcp` on the dashboard port. Exposes live operational data (pool stats, slow queries, backend health, etc.) to AI assistants. No extra process needed.
2. **Docs MCP** — a standalone Node.js server in `docs/mcp-server/` that indexes the documentation site and exposes it as MCP tools. Useful for AI-assisted configuration and rule authoring.

---

## Proxy Embedded MCP

The proxy exposes a [JSON-RPC 2.0](https://www.jsonrpc.org/specification) MCP endpoint at:

```
POST http://<dashboard-host>:<dashboard-port>/mcp
```

It is enabled automatically whenever the dashboard is enabled. Authentication uses the same `username` / `password` configured for the dashboard (HTTP Basic when set).

### Connecting

**VS Code `mcp.json`:**

```json
{
  "servers": {
    "turbineproxy": {
      "type": "http",
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

**Claude Desktop (`claude_desktop_config.json`):**

```json
{
  "mcpServers": {
    "turbineproxy": {
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

**VS Code `settings.json`:**

```json
{
  "mcp": {
    "servers": {
      "turbineproxy": {
        "type": "http",
        "url": "http://localhost:8080/mcp"
      }
    }
  }
}
```

### Available Tools

#### `get_pool_stats`

Returns connection pool utilisation for every backend.

```json
{ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
  "params": { "name": "get_pool_stats", "arguments": {} } }
```

**Response fields per backend:** `addr`, `role` (primary/replica), `idle`, `in_use`, `pool_size`, `created`, `evicted`.

#### `get_slow_queries`

Returns the top slow queries sorted by p99 latency.

**Arguments:** `limit` (integer, default 20)

**Response fields per query:** `fingerprint`, `count`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`, `last_seen`.

#### `get_n1_candidates`

Returns queries flagged as N+1 patterns — the same fingerprint executed many times with different parameters in a short window.

**Response fields:** `fingerprint`, `call_count`, `distinct_params`, `pattern_score`, `last_seen`.

#### `get_index_advice`

Returns index recommendations generated from background `EXPLAIN` analysis of slow queries.

**Response fields:** `table`, `column`, `query_sample`, `estimated_rows`, `suggestion`, `created_at`.

#### `get_backend_health`

Returns the current health state of every configured backend.

**Response fields:** `addr`, `role`, `healthy`, `lag_ms`, `consecutive_failures`, `last_check`.

#### `get_query_rules`

Returns all active routing rules with live hit counters.

**Response fields:** `match_pattern`, `match_digest`, `user`, `schema`, `destination`, `cache_ttl_secs`, `hit_count`, `last_match_secs`, `comment`.

#### `get_rewrite_rules`

Returns all active rewrite rules with live hit counters.

**Response fields:** `match_pattern`, `operation` (replace/add_limit/add_timeout/block), `hit_count`, `last_match_secs`, `comment`.

### Protocol

The endpoint implements a subset of the MCP specification sufficient for tool listing and tool calling:

```jsonc
// List tools
{ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }

// Call a tool
{
  "jsonrpc": "2.0", "id": 2,
  "method": "tools/call",
  "params": { "name": "get_slow_queries", "arguments": { "limit": 10 } }
}
```

All responses follow JSON-RPC 2.0. Errors return a standard `error` object with `code` and `message`.

### Example AI Prompts

Once connected, ask your assistant:

> "Which queries are causing the most replica lag right now?"

> "Are any backends unhealthy or lagging?"

> "Show me the top 5 slowest queries and suggest indexes for them."

> "What routing rules are active and how many times has each one fired?"

---

## Docs MCP (Documentation Server)

The docs MCP server indexes the TurbineProxy documentation and exposes search and lookup tools. It is useful for AI-assisted config authoring without needing a running proxy.

### Running

```bash
cd docs
npm install
npm run mcp
# Listening on http://localhost:3333
```

Configure port and optional live proxy connection:

```bash
MCP_PORT=4000 TURBINEPROXY_API=http://localhost:8080 npm run mcp
```

When `TURBINEPROXY_API` is set, the docs MCP also proxies `get_slow_queries`, `get_backends`, and `get_live_stats` to the running proxy.

### Connecting

**VS Code `mcp.json`:**

```json
{
  "servers": {
    "turbineproxy-docs": {
      "type": "http",
      "url": "http://localhost:3333/mcp"
    }
  }
}
```

**Claude Desktop:**

```json
{
  "mcpServers": {
    "turbineproxy-docs": {
      "command": "node",
      "args": ["/path/to/turbineproxy/docs/mcp-server/index.js"],
      "env": {
        "TURBINEPROXY_API": "http://localhost:8080"
      }
    }
  }
}
```

### Available Tools

#### `search_docs`

Full-text search across all documentation pages.

**Input:** `query` (string)

#### `get_config_option`

Detailed description of a specific configuration key.

**Input:** `key` (string) — e.g. `"max_replica_lag_ms"`

#### `list_config_sections`

List all top-level configuration sections with brief descriptions.

#### `get_live_stats`

Fetch current metrics from the connected proxy (requires `TURBINEPROXY_API`).

#### `get_slow_queries`

Fetch current slow queries from the connected proxy (requires `TURBINEPROXY_API`).

#### `get_backends`

Fetch current backend health from the connected proxy (requires `TURBINEPROXY_API`).

### Example Prompts

> "What does `read_your_own_writes_ms` do?"

> "Write a routing rule to send all queries from user `analytics` to replica 2."

> "What compression algorithms does TurbineProxy support?"
