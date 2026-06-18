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
    cargo test -p {{pkg}}

# Watch mode via bacon
watch:
    bacon

# Watch a specific package
watch-pkg pkg:
    bacon -p {{pkg}}

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
    cog bump --{{version}}
