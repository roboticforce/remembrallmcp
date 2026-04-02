# syntax=docker/dockerfile:1.7

# ── Builder ────────────────────────────────────────────────────────────────────
FROM rust:1.88.0-bookworm AS builder

# System packages needed at build time:
#   pkg-config + libssl-dev  - OpenSSL linkage for sqlx / reqwest
#   clang                    - tree-sitter crates compile C grammars via cc crate
#   lzma-dev                 - ort-sys uses lzma-rust2 which may need liblzma
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    clang \
    liblzma-dev \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ── Dependency cache layer ─────────────────────────────────────────────────────
# Copy every Cargo manifest and the lock file first, then create minimal stub
# source files so cargo can compile all dependencies without touching real code.
# This layer is only invalidated when Cargo.toml / Cargo.lock files change.

COPY Cargo.toml Cargo.lock ./

# Workspace member manifests
COPY crates/remembrall-core/Cargo.toml          crates/remembrall-core/Cargo.toml
COPY crates/remembrall-server/Cargo.toml        crates/remembrall-server/Cargo.toml
COPY crates/remembrall-test-harness/Cargo.toml  crates/remembrall-test-harness/Cargo.toml
COPY crates/remembrall-recall-test/Cargo.toml   crates/remembrall-recall-test/Cargo.toml
COPY crates/remembrall-integration-tests/Cargo.toml crates/remembrall-integration-tests/Cargo.toml

# remembrall-core has multiple [[bin]] entries - create a stub for each
RUN mkdir -p crates/remembrall-core/src/bin && \
    echo "fn main() {}" > crates/remembrall-core/src/lib.rs && \
    for bin in spike spike2 spike3 parser_smoke kt_ast_debug bench_index; do \
      echo "fn main() {}" > crates/remembrall-core/src/bin/${bin}.rs; \
    done

# remembrall-server has one [[bin]] - remembrall
RUN mkdir -p crates/remembrall-server/src && \
    echo "fn main() {}" > crates/remembrall-server/src/main.rs

# Test/harness crates - each has one [[bin]]
RUN mkdir -p crates/remembrall-test-harness/src && \
    echo "fn main() {}" > crates/remembrall-test-harness/src/main.rs
RUN mkdir -p crates/remembrall-recall-test/src && \
    echo "fn main() {}" > crates/remembrall-recall-test/src/main.rs
RUN mkdir -p crates/remembrall-integration-tests/src && \
    echo "fn main() {}" > crates/remembrall-integration-tests/src/main.rs

# Compile only the release dependency graph (no source changes will bust this)
RUN cargo build -p remembrall-server --release

# Remove stub artifacts so a real build picks up the correct fingerprints.
# This forces cargo to relink the final binaries without discarding the compiled
# dependency objects (the expensive part).
RUN find target/release -maxdepth 1 -name "remembrall*" -not -name "*.d" -delete && \
    find target/release/deps -name "remembrall*" -delete && \
    find target/release/deps -name "librembrall_*" -delete

# ── Real source compile ────────────────────────────────────────────────────────
COPY crates/ crates/

RUN cargo build -p remembrall-server --release

# ── Runtime ────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# libssl3      - OpenSSL runtime (sqlx TLS)
# libstdc++6   - C++ stdlib (statically-linked ONNX Runtime still needs stdc++)
# ca-certificates - TLS certificate bundle (HTTPS to HuggingFace for model dl)
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 \
    libstdc++6 \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

RUN useradd --uid 1001 --create-home --shell /sbin/nologin remembrall

COPY --from=builder --chown=remembrall:remembrall \
    /build/target/release/remembrall /usr/local/bin/remembrall

USER remembrall
WORKDIR /home/remembrall

ENTRYPOINT ["/usr/local/bin/remembrall"]
# Default: no args = MCP server over stdio
CMD []
