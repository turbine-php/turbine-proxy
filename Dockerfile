# ─── Stage 1: Build frontend ──────────────────────────────────────────────────
FROM node:22-alpine AS frontend-builder
WORKDIR /build/dashboard
COPY dashboard/package.json dashboard/package-lock.json ./
RUN npm ci --prefer-offline
COPY dashboard/ ./
RUN npm run build

# ─── Stage 2: Build binary (musl static) ──────────────────────────────────────
FROM rust:1.82-bookworm AS rust-builder

# Tools needed: musl-gcc (for SQLite bundled + mimalloc C build), cmake
RUN apt-get update && apt-get install -y --no-install-recommends \
        musl-tools \
        musl-dev \
        cmake \
        && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build

# ── Dependency-cache layer ──
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && printf 'fn main(){}' > src/main.rs \
    && CARGO_INCREMENTAL=0 cargo build --release \
           --target x86_64-unknown-linux-musl \
    && rm -rf src target/x86_64-unknown-linux-musl/release/turbineproxy \
              target/x86_64-unknown-linux-musl/release/.fingerprint/turbineproxy-*

# ── Full source build ──
COPY src ./src
RUN CARGO_INCREMENTAL=0 cargo build --release \
        --target x86_64-unknown-linux-musl

# ─── Stage 3: Runtime image ───────────────────────────────────────────────────
# gcr.io/distroless/static-debian12:nonroot ships CA certs + no shell
FROM gcr.io/distroless/static-debian12:nonroot AS runtime

WORKDIR /app

# Binary
COPY --from=rust-builder \
     /build/target/x86_64-unknown-linux-musl/release/turbineproxy \
     /app/turbineproxy

# Frontend assets (served at runtime via ServeDir)
COPY --from=frontend-builder /build/dashboard/dist /app/dashboard/dist

# Default config (operator should mount a real turbineproxy.toml over this)
COPY turbineproxy.example.toml /app/turbineproxy.toml

# 3307 = MySQL proxy port  |  8080 = dashboard + /metrics
EXPOSE 3307 8080

ENTRYPOINT ["/app/turbineproxy"]
