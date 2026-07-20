# syntax=docker/dockerfile:1.7

# ── Builder ────────────────────────────────────────────────────────────────────
FROM ubuntu:24.04 AS builder

ENV DEBIAN_FRONTEND=noninteractive

# Rust + system packages needed at build time
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    clang \
    liblzma-dev \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /build

# Copy everything and build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build -p remembrall-server --release

# ── Runtime ────────────────────────────────────────────────────────────────────
FROM ubuntu:24.04 AS runtime

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3t64 \
    libstdc++6 \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

RUN useradd --uid 1001 --create-home --shell /sbin/nologin remembrall

COPY --from=builder --chown=remembrall:remembrall \
    /build/target/release/remembrall /usr/local/bin/remembrall

# Entrypoint runs `remembrall init` (idempotent setup) then hands off to the
# MCP server. Keeps the container alive after setup so `docker compose exec`
# works, and keeps stdout clean for MCP JSON-RPC.
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

USER remembrall
WORKDIR /home/remembrall

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
# Default: no args = entrypoint runs init, then `remembrall` (MCP server over stdio)
CMD []
