/**
 * TurbineProxy MCP Server
 *
 * Exposes TurbineProxy documentation and live proxy metrics
 * to AI assistants via the Model Context Protocol (MCP).
 *
 * Usage:
 *   node index.js
 *
 * Environment variables:
 *   MCP_PORT              - HTTP port (default: 3333)
 *   TURBINEPROXY_API      - TurbineProxy API base URL (default: http://localhost:8080)
 *   TURBINEPROXY_TOKEN    - X-Auth-Token for authenticated dashboards
 *   TURBINEPROXY_DOCS_URL - Docs base URL for search result links (default: https://docs.turbineproxy.com/docs)
 */

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
import { StreamableHTTPServerTransport } from '@modelcontextprotocol/sdk/server/streamableHttp.js'
import { z } from 'zod'
import http from 'node:http'

const MCP_PORT = Number(process.env.MCP_PORT ?? 3333)
const API_BASE = (process.env.TURBINEPROXY_API ?? 'http://localhost:8080').replace(/\/$/, '')
const API_TOKEN = process.env.TURBINEPROXY_TOKEN ?? ''
const DOCS_BASE = (process.env.TURBINEPROXY_DOCS_URL ?? 'https://docs.turbineproxy.com/docs').replace(/\/$/, '')

// ---------------------------------------------------------------------------
// Documentation knowledge base
// ---------------------------------------------------------------------------

const CONFIG_DOCS = {
  listen_addr: {
    section: 'top-level',
    type: 'string',
    default: '"0.0.0.0:3307"',
    description: 'TCP address for the MySQL proxy listener. Clients connect here instead of directly to MySQL.',
    example: 'listen_addr = "0.0.0.0:3307"',
  },
  max_connections: {
    section: 'top-level',
    type: 'integer',
    default: '1000',
    description: 'Maximum simultaneous client connections. New connections are refused beyond this limit.',
    example: 'max_connections = 500',
  },
  pool_size: {
    section: 'top-level',
    type: 'integer',
    default: '20',
    description: 'Backend connection pool size per backend. Shared across all clients connecting through the proxy.',
    example: 'pool_size = 50',
  },
  auth_cache_ttl_secs: {
    section: 'top-level',
    type: 'integer',
    default: '300',
    description: 'How long to cache successfully authenticated credentials (seconds). Reduces backend auth overhead.',
    example: 'auth_cache_ttl_secs = 600',
  },
  connection_max_idle_secs: {
    section: 'top-level',
    type: 'integer',
    default: '55',
    description: 'Evict idle backend connections older than this (seconds). Prevents stale connection errors from MySQL\'s wait_timeout.',
    example: 'connection_max_idle_secs = 30',
  },
  max_transaction_time_ms: {
    section: 'top-level',
    type: 'integer',
    default: '0',
    description: 'Abort transactions running longer than this (ms). 0 = disabled. Prevents runaway long transactions from holding locks.',
    example: 'max_transaction_time_ms = 30000',
  },
  max_query_time_ms: {
    section: 'top-level',
    type: 'integer',
    default: '0',
    description: 'Kill queries running longer than this via KILL QUERY (ms). 0 = disabled.',
    example: 'max_query_time_ms = 5000',
  },
  max_transaction_idle_ms: {
    section: 'top-level',
    type: 'integer',
    default: '0',
    description: 'Abort transactions that have been idle (no query sent) for this long (ms). 0 = disabled.',
    example: 'max_transaction_idle_ms = 10000',
  },
  read_your_own_writes_ms: {
    section: 'top-level',
    type: 'integer',
    default: '0',
    description: 'After a write, route reads to primary for this many milliseconds. Prevents reading stale replica data immediately after writing. 0 = disabled.',
    example: 'read_your_own_writes_ms = 500',
  },
  select_version_forwarding: {
    section: 'top-level',
    type: 'boolean',
    default: 'true',
    description: 'Respond to SELECT VERSION() locally without a backend round-trip. Reduces unnecessary backend queries from ORMs.',
    example: 'select_version_forwarding = true',
  },
  shutdown_timeout_secs: {
    section: 'top-level',
    type: 'integer',
    default: '30',
    description: 'On SIGTERM, wait up to this many seconds for in-flight queries to finish before exiting.',
    example: 'shutdown_timeout_secs = 60',
  },
  proxy_protocol: {
    section: 'top-level',
    type: 'boolean',
    default: 'false',
    description: 'Enable PROXY Protocol v1 support (HAProxy, AWS NLB). Extracts real client IP from PROXY header.',
    example: 'proxy_protocol = true',
  },
  // [analytics]
  'analytics.enabled': {
    section: 'analytics',
    type: 'boolean',
    default: 'true',
    description: 'Enable query telemetry and analytics storage. When disabled, no fingerprinting, logging, or SQLite writes occur.',
    example: '[analytics]\nenabled = true',
  },
  'analytics.db_path': {
    section: 'analytics',
    type: 'string',
    default: '"turbineproxy_analytics.db"',
    description: 'Path to the SQLite database file for analytics storage.',
    example: '[analytics]\ndb_path = "/var/lib/turbineproxy/analytics.db"',
  },
  'analytics.slow_query_ms': {
    section: 'analytics',
    type: 'integer',
    default: '100',
    description: 'Queries slower than this threshold are logged as slow queries (ms).',
    example: '[analytics]\nslow_query_ms = 50',
  },
  'analytics.retention_days': {
    section: 'analytics',
    type: 'integer',
    default: '30',
    description: 'Analytics data older than this is automatically pruned from SQLite.',
    example: '[analytics]\nretention_days = 90',
  },
  // [dashboard]
  'dashboard.enabled': {
    section: 'dashboard',
    type: 'boolean',
    default: 'true',
    description: 'Enable the web dashboard and REST API.',
    example: '[dashboard]\nenabled = true',
  },
  'dashboard.listen_addr': {
    section: 'dashboard',
    type: 'string',
    default: '"0.0.0.0:8080"',
    description: 'TCP address for the dashboard HTTP server.',
    example: '[dashboard]\nlisten_addr = "0.0.0.0:9090"',
  },
  'dashboard.username': {
    section: 'dashboard',
    type: 'string',
    default: '""',
    description: 'Dashboard login username. Empty string disables authentication.',
    example: '[dashboard]\nusername = "admin"\npassword = "secret"',
  },
  // [ha]
  'ha.enabled': {
    section: 'ha',
    type: 'boolean',
    default: 'true',
    description: 'Enable backend health checking and automatic failover.',
    example: '[ha]\nenabled = true',
  },
  'ha.health_check_interval_secs': {
    section: 'ha',
    type: 'integer',
    default: '5',
    description: 'How often to check backend health (seconds).',
    example: '[ha]\nhealth_check_interval_secs = 10',
  },
  'ha.max_replica_lag_ms': {
    section: 'ha',
    type: 'integer',
    default: '5000',
    description: 'Replicas lagging more than this are marked unhealthy and excluded from read routing.',
    example: '[ha]\nmax_replica_lag_ms = 2000',
  },
  'ha.primary_failover_threshold': {
    section: 'ha',
    type: 'integer',
    default: '3',
    description: 'Consecutive failed health checks before promoting a replica to primary.',
    example: '[ha]\nprimary_failover_threshold = 5',
  },
  'ha.galera_check': {
    section: 'ha',
    type: 'boolean',
    default: 'false',
    description: 'Enable Galera/Percona XtraDB Cluster wsrep_local_state health checks. Nodes not in Synced state are excluded.',
    example: '[ha]\ngalera_check = true',
  },
}

const FEATURES_DOCS = {
  'read-write-splitting': `TurbineProxy automatically routes SELECT queries to read replicas and write queries to the primary.

Classification:
- READ: SELECT, SHOW, EXPLAIN → routes to replica (round-robin by weight)
- WRITE: INSERT, UPDATE, DELETE, DDL → always primary
- TRANSACTION: BEGIN, COMMIT, ROLLBACK → primary
- OTHER: SET, USE, CALL → primary (safe default)

Special cases:
- SELECT ... FOR UPDATE/FOR SHARE → primary (locking reads)
- Inside a transaction → all queries to primary (sticky connection)
- After SET @var = ... → session pins to same backend
- read_your_own_writes_ms → reads go to primary for N ms after any write`,

  'connection-pooling': `TurbineProxy maintains persistent connection pools to all backends.

- pool_size per backend (default: 20)
- LIFO stack (most recent connection reused first)
- Idle connection eviction (connection_max_idle_secs)
- init_connect SQL executed on every new backend connection
- Counters: idle, in_use, created, reused, evicted per backend`,

  'query-analytics': `Every query is fingerprinted and timed. No setup required.

Fingerprinting: literal values replaced with ? to group identical queries:
  SELECT * FROM users WHERE id = 42 → SELECT * FROM users WHERE id = ?

Stored metrics per fingerprint: count, total_us, min_us, max_us, p95_us, p99_us, last_seen

Architecture: bounded async channel → aggregation background task → SQLite flush every 30s
Never blocks the query hot path.`,

  'ha-failover': `Automatic health monitoring and primary failover.

Health checks run every health_check_interval_secs:
- Primary: ping check
- Replicas: SHOW SLAVE STATUS → Seconds_Behind_Master

Replicas exceeding max_replica_lag_ms are excluded from routing.
Primary failing primary_failover_threshold checks triggers failover.

Priority for write routing:
1. Group Replication primary (if GR monitoring active)
2. HA failover replica (if failover triggered)
3. Configured primary (default)`,

  'sql-injection-protection': `Pattern-based SQL injection detection.

Enable: sql_injection_protection = true in [security]

Blocks common injection patterns before they reach the backend.
Counter available: sqli_blocked in /api/stats`,

  'audit-log': `Immutable append-only NDJSON audit log.

Enable: audit_log = "/var/log/turbineproxy/audit.ndjson" in [security]

Each line is a JSON object with: timestamp, user, client_ip, fingerprint, affected_rows, duration_ms`,
}

const SECTION_DOCS = {
  'top-level': 'Global proxy settings: listener address, connection limits, pool size, timeouts.',
  primary: 'Primary (read-write) database backend configuration.',
  replicas: 'Read replica backends. Multiple [[replicas]] sections allowed. Uses weighted round-robin.',
  users: 'Per-user access control. Defines allowed users, passwords, and permissions.',
  query_rules: 'SQL routing rules. Route specific patterns to specific backends.',
  query_rewrites: 'SQL rewriting rules. Transform, limit, or block queries.',
  analytics: 'Query telemetry, slow query log, and SQLite storage.',
  dashboard: 'Web dashboard and REST API configuration.',
  ha: 'High availability: health checks and automatic failover.',
  security: 'SQL injection protection, audit log, and query whitelist.',
  cluster: 'Multi-instance configuration synchronization.',
  frontend_tls: 'TLS encryption between clients and the proxy.',
  pgsql: 'PostgreSQL proxy configuration (Phase 2).',
}

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

async function fetchProxy(path) {
  const headers = { Accept: 'application/json' }
  if (API_TOKEN) headers['X-Auth-Token'] = API_TOKEN
  const res = await fetch(`${API_BASE}${path}`, { headers })
  if (!res.ok) throw new Error(`HTTP ${res.status} from ${path}`)
  return res.json()
}

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

const server = new McpServer({
  name: 'turbineproxy',
  version: '1.0.0',
})

// Tool: search_docs
server.tool(
  'search_docs',
  'Search TurbineProxy documentation. Returns relevant config options, feature descriptions, and guides.',
  { query: z.string().describe('Search query, e.g. "connection pool eviction" or "routing rules"') },
  async ({ query }) => {
    const q = query.toLowerCase()
    const results = []

    // Search config options
    for (const [key, doc] of Object.entries(CONFIG_DOCS)) {
      if (
        key.toLowerCase().includes(q) ||
        doc.description.toLowerCase().includes(q) ||
        doc.section.toLowerCase().includes(q)
      ) {
        results.push(`**Config: \`${key}\`** (section: ${doc.section})\n${doc.description}\nDefault: \`${doc.default}\`\nExample:\n\`\`\`toml\n${doc.example}\n\`\`\`\nDocs: ${DOCS_BASE}/configuration/reference`)
      }
    }

    // Search features
    for (const [feature, desc] of Object.entries(FEATURES_DOCS)) {
      if (feature.toLowerCase().includes(q) || desc.toLowerCase().includes(q)) {
        results.push(`**Feature: ${feature}**\n${desc}\nDocs: ${DOCS_BASE}/features/${feature}`)
      }
    }

    // Search sections
    for (const [section, desc] of Object.entries(SECTION_DOCS)) {
      if (section.toLowerCase().includes(q) || desc.toLowerCase().includes(q)) {
        results.push(`**Section: [${section}]**\n${desc}\nDocs: ${DOCS_BASE}/configuration/reference`)
      }
    }

    if (results.length === 0) {
      return { content: [{ type: 'text', text: `No documentation found for query: "${query}". Try searching for a config key name, feature name, or section name.` }] }
    }

    return {
      content: [{
        type: 'text',
        text: `Found ${results.length} result(s) for "${query}":\n\n${results.slice(0, 5).join('\n\n---\n\n')}`,
      }],
    }
  },
)

// Tool: get_config_option
server.tool(
  'get_config_option',
  'Get full documentation for a specific TurbineProxy configuration key.',
  { key: z.string().describe('Config key name, e.g. "max_replica_lag_ms" or "analytics.slow_query_ms"') },
  async ({ key }) => {
    const doc = CONFIG_DOCS[key] ?? CONFIG_DOCS[key.toLowerCase()]
    if (!doc) {
      const similar = Object.keys(CONFIG_DOCS).filter(k => k.includes(key.toLowerCase()) || key.toLowerCase().includes(k))
      const hint = similar.length > 0 ? `\n\nSimilar keys: ${similar.join(', ')}` : ''
      return { content: [{ type: 'text', text: `Unknown config key: "${key}"${hint}` }] }
    }
    return {
      content: [{
        type: 'text',
        text: `## \`${key}\`\n\n**Section:** ${doc.section}\n**Type:** ${doc.type}\n**Default:** \`${doc.default}\`\n\n${doc.description}\n\n**Example:**\n\`\`\`toml\n${doc.example}\n\`\`\`\n\n**Docs:** ${DOCS_BASE}/configuration/reference`,
      }],
    }
  },
)

// Tool: list_config_sections
server.tool(
  'list_config_sections',
  'List all TurbineProxy configuration sections with descriptions.',
  {},
  async () => {
    const lines = Object.entries(SECTION_DOCS).map(([s, d]) => `- **[${s}]**: ${d}`)
    return { content: [{ type: 'text', text: `## Configuration Sections\n\n${lines.join('\n')}` }] }
  },
)

// Tool: get_live_stats
server.tool(
  'get_live_stats',
  'Fetch current metrics from a running TurbineProxy instance. Requires TURBINEPROXY_API env var.',
  {},
  async () => {
    try {
      const stats = await fetchProxy('/api/stats')
      const text = [
        '## Live TurbineProxy Stats',
        `- Connections active: ${stats.connections_active}`,
        `- Connections total: ${stats.connections_total}`,
        `- Queries total: ${stats.queries_total}`,
        `- Reads: ${stats.queries_read}`,
        `- Writes: ${stats.queries_write}`,
        `- Queries killed (timeout): ${stats.queries_killed ?? 0}`,
        `- Transactions killed: ${stats.transactions_killed ?? 0}`,
        `- SQLi blocked: ${stats.sqli_blocked ?? 0}`,
      ].join('\n')
      return { content: [{ type: 'text', text }] }
    } catch (err) {
      return { content: [{ type: 'text', text: `Failed to fetch stats: ${err.message}\n\nMake sure TURBINEPROXY_API is set to your proxy's dashboard address.` }] }
    }
  },
)

// Tool: get_slow_queries
server.tool(
  'get_slow_queries',
  'Fetch the current slow query list from a running TurbineProxy instance. Requires TURBINEPROXY_API env var.',
  { limit: z.number().optional().default(10).describe('Number of slow queries to return') },
  async ({ limit }) => {
    try {
      const queries = await fetchProxy(`/api/slow-queries?limit=${limit}`)
      if (!queries.length) return { content: [{ type: 'text', text: 'No slow queries recorded yet.' }] }
      const rows = queries.map((q, i) =>
        `${i + 1}. **${q.fingerprint}**\n   Count: ${q.count} | P95: ${((q.p95_us ?? 0) / 1000).toFixed(1)}ms | Max: ${((q.max_us ?? 0) / 1000).toFixed(1)}ms`
      )
      return { content: [{ type: 'text', text: `## Top ${queries.length} Slow Queries\n\n${rows.join('\n\n')}` }] }
    } catch (err) {
      return { content: [{ type: 'text', text: `Failed to fetch slow queries: ${err.message}` }] }
    }
  },
)

// Tool: get_backends
server.tool(
  'get_backends',
  'Fetch current backend health status from a running TurbineProxy instance. Requires TURBINEPROXY_API env var.',
  {},
  async () => {
    try {
      const backends = await fetchProxy('/api/backends')
      const rows = backends.map(b =>
        `- **${b.role}** ${b.addr}: ${b.healthy ? '✓ healthy' : '✗ unhealthy'} | lag: ${b.lag_ms}ms | failures: ${b.consecutive_failures}`
      )
      return { content: [{ type: 'text', text: `## Backend Health\n\n${rows.join('\n')}` }] }
    } catch (err) {
      return { content: [{ type: 'text', text: `Failed to fetch backends: ${err.message}` }] }
    }
  },
)

// ---------------------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------------------

const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined })

const httpServer = http.createServer(async (req, res) => {
  if (req.url === '/mcp' || req.url?.startsWith('/mcp?') || req.url?.startsWith('/mcp/')) {
    await transport.handleRequest(req, res)
  } else if (req.url === '/health') {
    res.writeHead(200, { 'Content-Type': 'application/json' })
    res.end(JSON.stringify({ status: 'ok', server: 'turbineproxy-mcp' }))
  } else if (req.url === '/' || req.url === '') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' })
    res.end(`<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>TurbineProxy MCP Server</title></head>
<body style="font-family:sans-serif;max-width:480px;margin:60px auto;padding:0 16px">
  <h1>TurbineProxy MCP Server</h1>
  <p>MCP endpoint: <code>/mcp</code></p>
  <p>
    <a href="https://turbineproxy.com">turbineproxy.com</a> &middot;
    <a href="https://docs.turbineproxy.com">Documentation</a>
  </p>
</body>
</html>`)
  } else {
    res.writeHead(404)
    res.end('Not found. MCP endpoint is at /mcp')
  }
})

await server.connect(transport)

httpServer.listen(MCP_PORT, () => {
  console.log(`TurbineProxy MCP server listening on http://localhost:${MCP_PORT}/mcp`)
  console.log(`Connected to proxy API: ${API_BASE}`)
  if (!API_TOKEN) console.log('No auth token set (unauthenticated dashboard assumed)')
})
