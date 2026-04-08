# === BUILD STAGE ===
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./

# Cache dependencies: create dummy main, build deps, then replace with real source
RUN mkdir src && echo "fn main(){}" > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

COPY src ./src
COPY benches ./benches
RUN cargo build --release

# === RUNTIME STAGE ===
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    iptables \
    iproute2 \
    wireguard-tools \
    unbound \
    qrencode \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
RUN mkdir -p /app/data /app/config

COPY --from=builder /app/target/release/vpn-gateway /usr/local/bin/vpn-gateway
COPY scripts/entrypoint-simple.sh /entrypoint-simple.sh
COPY config/ /app/config/

RUN chmod +x /entrypoint-simple.sh

ENTRYPOINT ["/entrypoint-simple.sh"]
