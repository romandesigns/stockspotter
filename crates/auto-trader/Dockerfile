# Build context is the repo root — same reasoning as
# crates/ws-server/Dockerfile: this is a Cargo workspace member that pulls
# in market-data + backtest-metrics via path deps, so the whole workspace
# source has to be present for `cargo` to resolve it even though only
# auto-trader's own binary gets built.
# (see ../../ops/vps/docker-compose.yml: build.context: ../..)

FROM rust:1-slim-bookworm AS build
WORKDIR /repo

# native-tls (tokio-tungstenite's TLS backend, see Cargo.toml) is
# OpenSSL-backed on Linux -- needs headers to build, and the runtime lib
# below. Still required at BUILD time even though this service only ever
# opens a plain ws:// connection to ws-server on the internal Docker
# network (never wss://) -- the native-tls feature is a compile-time
# dependency of tokio-tungstenite regardless of which scheme any given
# connection actually uses.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release -p auto-trader

# Runtime — debian-slim rather than distroless, same reasoning as
# ws-server: native-tls needs a real libssl at runtime, not just link time.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /repo/target/release/auto-trader ./auto-trader

CMD ["./auto-trader"]
