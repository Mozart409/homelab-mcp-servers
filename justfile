# https://just.systems

set unstable
set dotenv-load

# Default recipe — list all available commands
default:
    just --list

# ------------------------------------------------------------------------------
# Development
# ------------------------------------------------------------------------------

# Build the entire workspace
check:
    cargo check --workspace

# Build the entire workspace (release mode)
build:
    cargo build --workspace

build-release:
    cargo build --workspace --release

# Test everything
test:
    cargo test --workspace

# Test a specific package (e.g. `just test-pkg pgmcp`)
test-pkg pkg:
    cargo test -p {{ pkg }}

# Run the pgmcp DB integration tests (needs PGMCP_TEST_DATABASE_URL; `just test` marks them ignored)
test-db:
    cargo test -p pgmcp --test integration -- --ignored

# Watch a specific package and re-run its binary (e.g. `just watch-pkg pbsmcp-server`)
watch-pkg pkg:
    cargo watch -c -x "run -p {{ pkg }}"

# Clear terminal
clear:
    clear

# ------------------------------------------------------------------------------
# Lint / Format / Fix
# ------------------------------------------------------------------------------

# Format all Rust code
fmt:
    cargo fmt

# Verify formatting without touching the tree (fails on unformatted code)
fmt-check:
    cargo fmt --all --check

# Run clippy with pedantic lints
clippy:
    cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic

# Auto-fix lints and formatting
fix:
    cargo fix --allow-dirty --allow-staged
    cargo fmt

# Run cargo-deny license/advisory check
deny:
    cargo deny check

# Run cargo audit for security advisories
audit:
    cargo audit

# Run all linting checks (format, clippy, deny) — reformats in place
lint: fmt clippy deny

# Same checks as `lint`, but read-only — this is the variant CI must use, since
# `fmt` rewrites files and would let unformatted code pass a pipeline silently.
lint-ci: fmt-check clippy deny

# Run keep-sorted on staged files or all tracked files
sort:
    git ls-files | keep-sorted

# ------------------------------------------------------------------------------
# CI Simulation
# ------------------------------------------------------------------------------

# Run everything CI would run. Read-only: never rewrites the working tree, so a
# green local run means the same thing a green pipeline does.
ci: lint-ci test

# Run pre-commit hooks manually
pre-commit:
    lefthook run pre-commit

# Run pre-push hooks manually
pre-push:
    lefthook run pre-push

# ------------------------------------------------------------------------------
# OCI images (podman)
# ------------------------------------------------------------------------------

# Build a static-musl/distroless image for one server (e.g. `just image pbsmcp-server`)
image bin tag="dev":
    podman build --build-arg BIN={{ bin }} -t {{ bin }}:{{ tag }} -f Containerfile .

# Build images for every server binary
image-all:
    just image pbsmcp-server
    just image pgmcp-server
    just image prommcp-server
    just image lokimcp-server
    just image hamcp-server

# Run a built image, loading env from .env (e.g. `just run-image pbsmcp-server`)
run-image bin tag="dev" port="8080":
    podman run --rm -it --env-file .env -e PBS_BIND=0.0.0.0:8080 -p {{ port }}:8080 {{ bin }}:{{ tag }}

# Build an image, then scan it for vulnerabilities with trivy (e.g. `just scan pbsmcp-server`)
scan bin tag="dev": (image bin tag)
    trivy image --skip-version-check {{ bin }}:{{ tag }}

# Bring the whole stack up with podman-compose
up:
    podman-compose up --build -d

# Tear the stack down
down:
    podman-compose down

# ------------------------------------------------------------------------------
# Nix
# ------------------------------------------------------------------------------

# Enter the Nix development shell
dev:
    nix develop

# Update flake inputs
update:
    nix flake update

# ------------------------------------------------------------------------------
# Binary cache (Attic)
# ------------------------------------------------------------------------------

# Attic push target, as <server>:<cache>. Writes need a JWT (`attic login`);
# reads are public, which is why CI can substitute without any secret.
attic_target := "homelab:homelab"

# The binary-cache URL Nix substitutes from — must match `extra-substituters`
# in .woodpecker/test.yaml and `substituters` in /etc/nix/nix.conf.
attic_url := "https://cache.homelab.local/homelab"

# The profile path is load-bearing: .woodpecker/test.yaml runs
# `nix develop --profile ./.ci-profile .#ci`, so pushing that same profile gives
# CI exact-path hits rather than near-misses. Building `.#ci` (not the default
# shell) matters too — the default shell drags in podman, trivy, claude-code and
# the rest of the workstation toolbox, none of which CI ever asks for.
#
# Requires `attic login` beforehand. Worth re-running after `just update`, since
# new flake inputs are exactly when CI would otherwise face a cold cache.
#
# Push the CI shell closure to the Attic cache so a cold pipeline doesn't realise it
seed-cache:
    @echo "==> realising the CI shell closure (same command the pipeline runs)"
    nix develop --profile ./.ci-profile .#ci --command true
    @echo "==> pushing closure to {{ attic_target }}"
    attic push {{ attic_target }} ./.ci-profile
    @just verify-cache

# Queries the binary-cache URL directly for the profile's .narinfo — the same
# request Nix makes when substituting. It therefore tests the URL that actually
# matters, rather than trusting `attic cache info`, whose "Binary Cache Endpoint"
# line renders as `...localhomelab`, joining host and cache name with no
# separator. If this recipe passes, that display is cosmetic; if it fails, the
# endpoint really is misconfigured.
#
# Check the Attic cache actually serves the seeded CI closure
verify-cache:
    #!/usr/bin/env bash
    set -euo pipefail
    store_path="$(readlink -f ./.ci-profile)"
    hash="$(basename "$store_path" | cut -d- -f1)"
    url="{{ attic_url }}/${hash}.narinfo"
    echo "==> GET ${url}"
    if curl -fsS --max-time 20 "$url" > /dev/null; then
        echo "==> OK: cache serves the CI closure; a cold pipeline will hit it"
    else
        echo "==> FAILED: cache did not serve ${hash}.narinfo" >&2
        echo "    Check that '{{ attic_url }}' is the right binary-cache URL and" >&2
        echo "    that 'attic login' used an endpoint with a trailing slash." >&2
        exit 1
    fi

# ------------------------------------------------------------------------------
# Clean
# ------------------------------------------------------------------------------

# Clean Cargo build artifacts
clean:
    cargo clean

# Clean everything including Nix result links and direnv
clean-all: clean
    rm -f result result-*
    rm -rf .direnv

# ------------------------------------------------------------------------------
# Versioning / Release
# ------------------------------------------------------------------------------

# Show conventional commits changelog
changelog:
    cog changelog

# Bump version using conventional commits (e.g. `just bump auto`)
bump version:
    cog bump --{{ version }}
