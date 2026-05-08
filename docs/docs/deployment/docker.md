---
sidebar_position: 2
---

# Docker

## Dockerfile

```dockerfile
FROM rust:1.75 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/turbineproxy /usr/local/bin/turbineproxy
EXPOSE 3307 8080
ENTRYPOINT ["turbineproxy", "/config/turbineproxy.toml"]
```

## docker-compose

```yaml
services:
  turbineproxy:
    build: .
    ports:
      - "3307:3307"
      - "8080:8080"
    volumes:
      - ./turbineproxy.toml:/config/turbineproxy.toml:ro
      - turbineproxy_data:/var/lib/turbineproxy
    environment:
      RUST_LOG: info
    restart: unless-stopped

  mysql:
    image: mysql:8.0
    environment:
      MYSQL_ROOT_PASSWORD: secret
      MYSQL_DATABASE: myapp
    ports:
      - "3306:3306"

volumes:
  turbineproxy_data:
```

## Health Check

```yaml
healthcheck:
  test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
  interval: 10s
  timeout: 5s
  retries: 3
```
