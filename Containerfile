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
#
# Check the value with:  nix develop .#ci --command rustc --version
#
# Deliberately NOT named RUST_VERSION. The cargo-zigbuild base image sets
# `ENV RUST_VERSION=1.85.0`, and an ENV inherited from the base image SHADOWS a
# same-named ARG when `${...}` is expanded in a RUN instruction. So a build arg
# called RUST_VERSION silently resolves to the image's 1.85.0 no matter what you
# pass — which is why this file previously carried a hardcoded literal instead.
ARG RUST_TOOLCHAIN=1.97.1

# ---- build ------------------------------------------------------------------
FROM ${BUILDER_IMAGE} AS build
ARG BIN
ARG TARGET
# Re-declared because a global ARG (declared above the first FROM) is not
# visible inside a build stage unless it is named again here.
ARG RUST_TOOLCHAIN
# The base image pins its bundled rustc via RUSTUP_TOOLCHAIN; clear it so the
# `rustup default` below is what actually takes effect.
ENV RUSTUP_TOOLCHAIN=
WORKDIR /app

# aws-lc-rs (pbs server, via reqwest) builds its crypto with cmake; ring (pg
# server, via sqlx) needs only the C compiler that zig already provides.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*

# The `rustc --version` assertion is not redundant: if RUST_TOOLCHAIN is ever
# shadowed or empty again, rustup silently falls back to the base image's own
# toolchain and the build proceeds — surfacing much later as confusing MSRV
# errors from unrelated dependencies rather than as a problem with this file.
RUN rustup toolchain install ${RUST_TOOLCHAIN} --profile minimal \
 && rustup default ${RUST_TOOLCHAIN} \
 && rustup target add ${TARGET} \
 && { rustc --version | grep -q "${RUST_TOOLCHAIN}" \
      || { echo "error: active toolchain is not ${RUST_TOOLCHAIN} but $(rustc --version)"; exit 1; }; }

COPY . .
# Cache the cargo registry and target dir across local rebuilds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo zigbuild --locked --release --target ${TARGET} -p ${BIN} \
 && cp "target/${TARGET}/release/${BIN}" /app/server

# ---- runtime ----------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime
COPY --from=build /app/server /usr/local/bin/server

# Already true via the :nonroot base image (UID/GID 65532), but implicit --
# trivy's DS-0002 check reads this file statically and cannot see a user set by
# an upstream image, so it flags a false positive without this line spelled out.
USER nonroot:nonroot

# Streamable-HTTP MCP endpoint. Bind 0.0.0.0 inside the container via the
# server's *_BIND env var (e.g. PBS_BIND=0.0.0.0:8080) — the default 127.0.0.1
# is unreachable from outside the container.
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/server"]
