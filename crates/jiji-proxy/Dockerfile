# Build from the repository root so the whole Cargo workspace (Cargo.toml,
# Cargo.lock, every crate manifest) is available to resolve jiji-proxy's own
# dependency graph:
#
#   docker build -f crates/jiji-proxy/Dockerfile -t jiji-proxy .

# 1.85 (phase 1's original pin) is no longer enough once phase 4 pulled in
# instant-acme's dependency tree (rcgen 0.14/time 0.3/icu_* need 1.86-1.88).
FROM rust:1.90-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    cmake \
    perl \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p jiji-proxy

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/jiji-proxy /usr/local/bin/jiji-proxy

# 80/443 are published from the host, DNATed to these via jiji_proxy_ingress
# or normal Docker port publishing depending on the bridge's masquerade
# setting.
EXPOSE 8080 8443

ENTRYPOINT ["/usr/local/bin/jiji-proxy"]
CMD ["run", "--config", "/etc/jiji/proxy/config.yml"]
