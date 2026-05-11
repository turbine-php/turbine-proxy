#!/bin/sh
set -e

# Start MCP server in background
echo "Starting TurbineProxy MCP server on :3333..."
node /mcp/index.js &

# Start nginx in foreground
echo "Starting nginx on :3000..."
exec nginx -g "daemon off;"
