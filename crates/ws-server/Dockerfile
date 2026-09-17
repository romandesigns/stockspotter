# Build context is the repo root — this is a Cargo workspace member, and
# ws-server pulls in market-data + its sibling detection crates (fast-funnel,
# momentum-scorer, ignition-detector, halt-detector, consolidation-breakout)
# via path deps, so the whole workspace source has to be present for `cargo`
# to resolve it even though only ws-server's binary gets built.
# (see ../../docker-compose.yml: build.context: .)

FROM rust:1-slim-bookworm AS build
WORKDIR /repo

# native-tls (reqwest + tokio-tungstenite's TLS backend, see Cargo.toml) is
# OpenSSL-backed on Linux — needs headers to build, and the runtime lib below.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates crates

# Not source, but the test suite below reads it: ws-server's
# `runbook_contract_tests` resolves every JSON path that
# `ops/qualify/session.sh` reads out of /research/completeness, so a renamed
# health field fails the image build instead of silently turning the
# pre-session preflight into a check that reads absent fields as zero.
COPY ops/qualify ops/qualify

# Build-time commit identity (assignment section 30).
#
# `src/provenance.rs` reads this through `option_env!`, so it has to be present
# in the environment when `cargo build` runs -- not at runtime, and not from a
# file the running process could read back. Supplied by whoever runs the build:
# `ops/vps/deploy.sh` passes `git rev-parse HEAD` for the checkout it is
# deploying, so a build cannot invent an identity for itself.
#
# Declared here rather than at the top of the stage so that changing it
# invalidates only this layer. Empty is allowed and means "unstamped", which the
# completeness checker reports as INDETERMINATE rather than as a match -- a
# local or CI build is genuinely unable to prove which commit it came from.
ARG STOCKSPOTTER_COMMIT=""
ENV STOCKSPOTTER_COMMIT=${STOCKSPOTTER_COMMIT}
RUN cargo test --workspace --locked && cargo build --release -p ws-server --locked

# Runtime — debian-slim rather than distroless since native-tls needs a real
# libssl at runtime, not just at link time.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /repo/target/release/ws-server ./ws-server

# DEFAULT_ADDR in main.rs is loopback-only (127.0.0.1:8787) — correct for a
# locally-run dev binary, wrong inside a container. docker-compose.yml sets
# WS_SERVER_ADDR=0.0.0.0:8787 explicitly to override it; this default here
# is just a safety net if the image is ever run standalone without that.
ENV WS_SERVER_ADDR=0.0.0.0:8787
EXPOSE 8787
CMD ["./ws-server"]
