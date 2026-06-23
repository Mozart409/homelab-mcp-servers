# syntax=docker/dockerfile:1
#
# One image per MCP server binary. Pick the binary with a build arg:
#   podman build --build-arg BIN=pbsmcp-server -t pbsmcp-server:dev .
#
# The whole workspace is rustls-only (no OpenSSL / native-tls), so we build a
# fully static musl binary and ship it on distroless/static — no libc, no shell,
# runs as a non-root user. cargo-zigbuild handles the musl cross-compile
# (including aws-lc-rs, which reqwest's rustls provider pulls in).

ARG BUILDER_IMAGE=docker.io/messense/cargo-zigbuild:0.20.0
ARG RUNTIME_IMAGE=gcr.io/distroless/static-debian12:nonroot
ARG TARGET=x86_64-unknown-linux-musl
# Keep in sync with the toolchain in flake.nix; the base image's bundled rustc
# is too old for the dependency tree (edition 2024 + recent deps).
ARG RUST_VERSION=1.95.0

# ---- build ------------------------------------------------------------------
FROM ${BUILDER_IMAGE} AS build
ARG BIN
ARG TARGET
# The base image pins an older rustc via RUSTUP_TOOLCHAIN; clear it and install
# the toolchain we want (keep in sync with flake.nix). Hardcoded rather than via
# ARG because the base image's env otherwise shadows it.
ENV RUSTUP_TOOLCHAIN=
WORKDIR /app

# aws-lc-rs (pbs server, via reqwest) builds its crypto with cmake; ring (pg
# server, via sqlx) needs only the C compiler that zig already provides.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install 1.95.0 --profile minimal \
 && rustup default 1.95.0 \
 && rustup target add ${TARGET}

COPY . .
# Cache the cargo registry and target dir across local rebuilds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo zigbuild --locked --release --target ${TARGET} -p ${BIN} \
 && cp "target/${TARGET}/release/${BIN}" /app/server

# ---- runtime ----------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime
COPY --from=build /app/server /usr/local/bin/server

# Streamable-HTTP MCP endpoint. Bind 0.0.0.0 inside the container via the
# server's *_BIND env var (e.g. PBS_BIND=0.0.0.0:8080) — the default 127.0.0.1
# is unreachable from outside the container.
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/server"]
