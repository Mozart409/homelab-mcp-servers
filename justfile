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

# Read-only: never rewrites the working tree, so a green local run means the
# same thing a green pipeline does.
#
# This is the fast local path — plain cargo against your existing `target/`.
# CI takes a different route (`just ci-nix`) for caching reasons; both use the
# same fenix toolchain and the same lint flags, so they agree on outcomes.
#
# Run fmt-check, clippy, cargo-deny, and the test suite (fast local path)
ci: lint-ci test

# Runs fmt, clippy, and the test suite through crane, exactly as
# .woodpecker/test.yaml does. Slower on a cold Nix store than `just ci`, because
# it compiles dependencies into the Nix store rather than reusing `target/` —
# but that is precisely what makes the result cacheable in Attic, and it is the
# way to reproduce a CI result locally without pushing.
#
# cargo-deny is absent for the same reason it is absent in CI: it needs network
# access to fetch the advisory DB, and Nix builds are sandboxed. Use `just deny`.
#
# Run the checks the pipeline runs (crane: fmt, clippy, test)
ci-nix:
    nix build --no-link --print-build-logs \
        .#checks.x86_64-linux.fmt \
        .#checks.x86_64-linux.clippy \
        .#checks.x86_64-linux.test

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
    just image wpmcp-server

# Run a built image, loading env from .env (e.g. `just run-image pbsmcp-server`)
run-image bin tag="dev" port="8080":
    podman run --rm -it --env-file .env -e PBS_BIND=0.0.0.0:8080 -p {{ port }}:8080 {{ bin }}:{{ tag }}

# Build an image, then scan it for vulnerabilities with trivy (e.g. `just scan pbsmcp-server`)
scan bin tag="dev": (image bin tag)
    trivy image --skip-version-check {{ bin }}:{{ tag }}

# Build images sequentially (avoids IO storm from parallel podman builds),
# then bring the whole stack up. On a small VM (6 cores, limited RAM),
# parallel builds saturate btrfs IO and choke the system.
up:
    just image-all
    podman-compose up -d

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
    @echo "==> building the compiled dependency tree the crane checks consume"
    nix build --no-link .#legacyPackages.x86_64-linux.cargo-artifacts
    @echo "==> pushing both to {{ attic_target }}"
    # `-j 2` deliberately. The dependency tree's closure is ~325 paths / ~670 MiB,
    # 318 of them small `cargo-package-*` vendored sources. Pushing those at
    # attic's default concurrency made the server return `InternalServerError`
    # for a scattered subset while neighbours succeeded — interleaving like that
    # points at load or a flaky storage backend, not at one bad path. Slower, but
    # a push that completes beats a fast one that leaves the cache half-populated,
    # because a partial closure cannot be substituted and CI recompiles anyway.
    attic push -j 2 {{ attic_target }} ./.ci-profile \
        "$(nix build --no-link --print-out-paths .#legacyPackages.x86_64-linux.cargo-artifacts)"
    @just verify-cache

# Queries the binary-cache URL directly, which is the same request Nix makes when
# substituting — so it tests the URL that actually matters rather than trusting
# `attic cache info`, whose "Binary Cache Endpoint" line renders as
# `...localhomelab` (host and cache name joined with no separator). If this
# passes, that display is cosmetic.
#
# Two things this deliberately does NOT treat as failure:
#
#   * A path the cache does not store. This Attic has `cache.nixos.org-1` in its
#     Upstream Cache Keys, so it SKIPS anything already available upstream —
#     that is the "N in upstream" line from `attic push`. Those paths 404 here
#     and CI still gets them, just from cache.nixos.org. Checking a single path
#     (the profile's own) therefore proves nothing: an earlier version of this
#     recipe did exactly that and reported a false failure right after a
#     successful push.
#   * A timeout on one path. That is a slow negative lookup, not a miss, and it
#     is reported separately because the two have different causes.
#
# The real question is "will CI get hits on the paths only this cache has", so
# sample the closure and require at least one hit.
#
# Check the Attic cache actually serves the seeded CI closure
verify-cache:
    #!/usr/bin/env bash
    set -uo pipefail

    echo "==> checking the cache is reachable"
    if ! curl -fsS --max-time 10 "{{ attic_url }}/nix-cache-info" > /dev/null; then
        echo "==> FAILED: {{ attic_url }} is unreachable." >&2
        echo "    From a container this is usually the step-ca root missing from the" >&2
        echo "    trust store; see .woodpecker/homelab-ca.crt." >&2
        exit 1
    fi

    profile="$(readlink -f ./.ci-profile)"
    mapfile -t paths < <(nix path-info --recursive "$profile" 2>/dev/null | head -40)
    if [ "${#paths[@]}" -eq 0 ]; then
        echo "==> FAILED: could not read the closure of $profile (run 'just seed-cache' first)" >&2
        exit 1
    fi

    echo "==> sampling ${#paths[@]} closure paths"
    served=0; upstream=0; slow=0
    for p in "${paths[@]}"; do
        h="$(basename "$p" | cut -d- -f1)"
        code="$(curl -s -o /dev/null --max-time 8 -w '%{http_code}' "{{ attic_url }}/${h}.narinfo")"
        if [ $? -eq 28 ]; then slow=$((slow + 1))
        elif [ "$code" = "200" ]; then served=$((served + 1))
        else upstream=$((upstream + 1)); fi
    done

    echo "    served by this cache : $served"
    echo "    not stored (upstream): $upstream"
    echo "    timed out            : $slow"

    if [ "$served" -eq 0 ]; then
        echo "==> FAILED: the cache served none of the sampled paths." >&2
        echo "    Reachable but empty — check that 'attic push' actually succeeded." >&2
        exit 1
    fi
    echo "==> OK: cache serves the closure; a cold pipeline will hit it"
    if [ "$slow" -gt 0 ]; then
        echo "==> NOTE: $slow lookup(s) timed out. Attic can be slow answering for paths"
        echo "    it does not hold. Harmless in small numbers; if it is most of them,"
        echo "    substitution in CI will drag and the cache is worth looking at."
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
# Remotes
# ------------------------------------------------------------------------------

# Origin (Forgejo) is the main remote and where other people and agents push,
# so it is pulled first and local work is published straight back to it.
# GitHub is a downstream copy nobody else pushes to — it only receives the
# merged state, never pulls.
#
# Sync all remotes (pull+push origin, then push github)
sync-remotes:
    git pull
    git push
    git push github

# ------------------------------------------------------------------------------
# Versioning / Release
# ------------------------------------------------------------------------------

# Show conventional commits changelog
changelog:
    cog changelog

# Bump version using conventional commits (e.g. `just bump auto`)
bump version:
    cog bump --{{ version }}
