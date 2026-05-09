# ─── Stage 1: Build frontend ──────────────────────────────────────────────────
FROM node:22-alpine AS frontend-builder
WORKDIR /build/dashboard
COPY dashboard/package.json dashboard/package-lock.json ./
RUN npm ci --prefer-offline
COPY dashboard/ ./
RUN npm run build

# ─── Stage 2: Build binary (musl static) ──────────────────────────────────────
FROM rust:1.87-bookworm AS rust-builder

# TARGETARCH is injected by buildx: "amd64" or "arm64"
ARG TARGETARCH

# Tools needed: musl-gcc (for SQLite bundled + mimalloc C build), cmake
RUN apt-get update && apt-get install -y --no-install-recommends \
        musl-tools \
        musl-dev \
        cmake \
        && rm -rf /var/lib/apt/lists/*

# Map Docker arch → Rust musl triple and install the toolchain.
# musl-gcc on each arch is the native wrapper — set it explicitly for both
# targets so Cargo picks it up without additional cargo config files.
RUN case "$TARGETARCH" in \
      amd64) printf 'x86_64-unknown-linux-musl'  > /rust_target ;; \
      arm64) printf 'aarch64-unknown-linux-musl' > /rust_target ;; \
      *)     echo "Unsupported TARGETARCH: $TARGETARCH" && exit 1 ;; \
    esac \
    && rustup target add "$(cat /rust_target)"

ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    TURBINEPROXY_SKIP_DASHBOARD_BUILD=1

WORKDIR /build

# ── Dependency-cache layer ──
COPY Cargo.toml Cargo.lock ./
RUN RUST_TARGET="$(cat /rust_target)" \
    && mkdir -p src benches \
    && printf 'fn main(){}' > src/main.rs \
    && printf 'fn main(){}' > benches/proxy_bench.rs \
    && CARGO_INCREMENTAL=0 cargo build --release \
           --target "$RUST_TARGET" \
    && rm -rf src benches \
              "target/$RUST_TARGET/release/turbineproxy" \
              "target/$RUST_TARGET/release/.fingerprint/turbineproxy-*"

# ── Full source build ──
COPY src ./src
COPY benches ./benches
RUN CARGO_INCREMENTAL=0 cargo build --release \
        --target "$(cat /rust_target)"

# ── Copy binary to a fixed path so the runtime stage doesn't need TARGETARCH ──
RUN cp "target/$(cat /rust_target)/release/turbineproxy" /turbineproxy

# ─── Stage 3: Runtime image ───────────────────────────────────────────────────
# gcr.io/distroless/static-debian12:nonroot ships CA certs + no shell
FROM gcr.io/distroless/static-debian12:nonroot AS runtime

WORKDIR /app

# Binary (copied from the arch-neutral /turbineproxy path)
COPY --from=rust-builder /turbineproxy /app/turbineproxy

# Frontend assets (served at runtime via ServeDir)
COPY --from=frontend-builder /build/dashboard/dist /app/dashboard/dist

# Default config (operator should mount a real turbineproxy.toml over this)
COPY turbineproxy.example.toml /app/turbineproxy.toml

# 3307 = MySQL proxy port  |  8080 = dashboard + /metrics
EXPOSE 3307 8080

ENTRYPOINT ["/app/turbineproxy"]
