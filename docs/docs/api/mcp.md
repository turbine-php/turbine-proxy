---
sidebar_position: 3
---

# MCP Server

TurbineProxy ships with a **Model Context Protocol (MCP)** server that exposes documentation and live proxy data to AI assistants like GitHub Copilot, Claude, and others.

## What Is MCP?

MCP (Model Context Protocol) is an open standard that allows AI tools to query external context sources. By running the TurbineProxy MCP server, your AI assistant can:

- Look up configuration options by name
- Get explanations of features
- Query live metrics from your running proxy
- Help you write routing rules and rewrite rules

## Running the MCP Server

```bash
cd docs
npm install
npm run mcp
```

The MCP server runs on `http://localhost:3333` by default.

Configure the port:

```bash
MCP_PORT=4000 npm run mcp
```

To connect it to a live TurbineProxy instance:

```bash
TURBINEPROXY_API=http://localhost:8080 npm run mcp
```

## VS Code / GitHub Copilot Integration

Add to your `.vscode/mcp.json`:

```json
{
  "servers": {
    "turbineproxy": {
      "type": "http",
      "url": "http://localhost:3333/mcp"
    }
  }
}
```

Or add to your VS Code `settings.json`:

```json
{
  "mcp": {
    "servers": {
      "turbineproxy": {
        "type": "http",
        "url": "http://localhost:3333/mcp"
      }
    }
  }
}
```

After adding, you can ask Copilot questions like:

> "What does `read_your_own_writes_ms` do in TurbineProxy?"
> "Write a routing rule to send all queries from user `analytics` to replica 2"
> "What are the current slow queries on my proxy?"

## Available Tools

The MCP server exposes the following tools:

### `search_docs`

Search the TurbineProxy documentation.

**Input:** `query` (string)

**Example:** `search_docs("connection pool eviction")`

### `get_config_option`

Get detailed information about a specific configuration key.

**Input:** `key` (string)

**Example:** `get_config_option("max_replica_lag_ms")`

### `list_config_sections`

List all top-level configuration sections.

### `get_live_stats`

Fetch current metrics from a running TurbineProxy instance.

**Requires:** `TURBINEPROXY_API` environment variable set.

### `get_slow_queries`

Fetch the current slow query list from a running proxy.

**Requires:** `TURBINEPROXY_API` environment variable set.

### `get_backends`

Fetch current backend health status.

**Requires:** `TURBINEPROXY_API` environment variable set.

## Claude Desktop Integration

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "turbineproxy": {
      "command": "node",
      "args": ["/path/to/turbineproxy/docs/mcp-server/index.js"],
      "env": {
        "TURBINEPROXY_API": "http://localhost:8080"
      }
    }
  }
}
```
