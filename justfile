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

# Run all linting checks (format, clippy, deny)
lint: fmt clippy deny

# Run keep-sorted on staged files or all tracked files
sort:
    git ls-files | keep-sorted

# ------------------------------------------------------------------------------
# CI Simulation
# ------------------------------------------------------------------------------

# Run everything CI would run (pre-commit + pre-push)
ci: lint test

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
