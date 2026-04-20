# syntax=docker/dockerfile:1.7
#
# Cellar remote worker image.
#
# Milestone 1.0: ships the `cellar-worker` HTTP daemon only — accepts goals,
# returns stubbed results. Milestone 1.5 will add Chromium + Xvfb for real
# browser automation.

FROM rust:1.80-bookworm AS builder
WORKDIR /app

# Copy workspace manifests + all crates the worker transitively needs.
# (The worker currently depends only on external crates, so we copy lean.
#  Milestone 1.1 will pull in cel-goal-runner and its dependency tree.)
COPY Cargo.toml Cargo.lock ./
COPY cel ./cel
COPY adapters ./adapters
COPY cellar-worker ./cellar-worker

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release -p cellar-worker && \
    cp /app/target/release/cellar-worker /usr/local/bin/cellar-worker

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/cellar-worker /usr/local/bin/cellar-worker

ENV CEL_WORKER_PORT=7777 \
    CEL_WORKER_BIND=0.0.0.0 \
    RUST_LOG=info

EXPOSE 7777

# Basic healthcheck hits /health which is always unauthenticated.
HEALTHCHECK --interval=15s --timeout=3s --retries=3 \
    CMD wget -qO- http://localhost:7777/health >/dev/null 2>&1 || exit 1

ENTRYPOINT ["cellar-worker"]
