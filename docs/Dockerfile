# ─── Stage 1: Build Docusaurus ───────────────────────────────────────────────
FROM node:22-alpine AS builder

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --prefer-offline
COPY . .
RUN npm run build

# ─── Stage 2: Install MCP server deps ────────────────────────────────────────
FROM node:22-alpine AS mcp-builder

WORKDIR /mcp
COPY mcp-server/package.json ./
RUN npm install --omit=dev

# ─── Stage 3: Runtime (nginx + MCP node server) ───────────────────────────────
FROM node:22-alpine AS runtime

RUN apk add --no-cache nginx

# Static docs
COPY --from=builder /app/build /usr/share/nginx/html

# MCP server
WORKDIR /mcp
COPY --from=mcp-builder /mcp/node_modules ./node_modules
COPY mcp-server/index.js ./index.js

# nginx config
COPY nginx.conf /etc/nginx/http.d/default.conf

# Entrypoint
COPY docker-entrypoint.sh /docker-entrypoint.sh
RUN chmod +x /docker-entrypoint.sh

EXPOSE 3000

CMD ["/docker-entrypoint.sh"]
