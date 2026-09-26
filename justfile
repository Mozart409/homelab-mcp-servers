# https://just.systems

set unstable

# Default recipe — list all available commands
default:
    just --list

# Secrets live encrypted in .sops.env (sops + gpg-agent; values ENC[...], names
# visible, safe to commit) and are decrypted into ONE child process per recipe.
# Nothing is written to disk or exported into the shell -- `set dotenv-load` and
# a plaintext .env are gone on purpose. Edit with `sops .sops.env`.
secrets := "sops exec-env .sops.env"

# Same, for consumers that need an env FILE (podman --env-file, compose
# env_file). `{}` in the command is replaced by a 0600 temp file that exists
# only while the command runs. It lives in XDG_RUNTIME_DIR because that is
# tmpfs and /tmp here is not -- the plaintext never touches the NVMe. A FIFO
# would be cleaner but compose reads the file once per service.
secrets-file := "TMPDIR=" + env("XDG_RUNTIME_DIR", "/dev/shm") + " sops exec-file --no-fifo .sops.env"
# ------------------------------------------------------------------------------
# Development
# ------------------------------------------------------------------------------

# ONE INVOCATION SHAPE, EVERYWHERE.
#
# Cargo keys its artifacts on the resolved feature set and the selected targets,
# so `cargo check --workspace` and `cargo check --workspace --all-targets` are
# two different builds that share nothing but the source. Measured on this
# workspace: populating a second shape costs 70-150s, after which switching back
# and forth is free — the artifacts coexist, they do not clobber each other.
#
# So the flags below are not decoration. `check`, `clippy` and `test` all select
# the same units as the clippy check in flake.nix, which means an edit is
# type-checked once and every subsequent recipe reuses it: after a one-line edit,
# `just check` took 3s and `just clippy` 1s. Change the flags on one of these and
# you silently reintroduce a 70s tax on alternating between them.
#
# Build the entire workspace
check:
    cargo check --workspace --all-targets --all-features

# Build the entire workspace (release mode)
build:
    cargo build --workspace

build-release:
    cargo build --workspace --release

# `--all-features` matches `check` and `clippy` above so all three share
# artifacts. `--all-targets` is deliberately absent: it would silently drop
# doctests from the run.
#
# The cargo invocation is wrapped, not changed: scripts/test-pg.sh brings up a
# throwaway, durability-off Postgres on tmpfs (~1s), hands pgmcp's tests its
# URL, and tears it down afterwards. Nothing is skipped or `#[ignore]`d — the
# read-only guarantee is verified against a real server on every run.
#
# Snapshot tests (insta) write `*.snap.new` on a mismatch and fail; review with
# `cargo insta review`, or accept everything with `INSTA_UPDATE=always just test`.
#
# Test everything
test:
    scripts/test-pg.sh run -- cargo test --workspace --all-features

# Narrowing to one package re-resolves features over just that package, so the
# first run after a `just test` rebuilds a slice of the dependency tree (~16s
# here) into a second, coexisting artifact set. Repeat runs are instant, and
# switching back to `just test` is free — the two sets do not evict each other.
#
# Test a specific package (e.g. `just test-pkg pgmcp`)
test-pkg pkg:
    scripts/test-pg.sh run -- cargo test -p {{ pkg }}

# Watch a specific package and re-run its binary (e.g. `just watch-pkg pbsmcp-server`)
watch-pkg pkg:
    {{ secrets }}  'cargo watch -c -x "run -p {{ pkg }}"'

# sccache hit rate. The dev shell sets RUSTC_WRAPPER, so this reflects real
# usage. Compile requests that are "non-cacheable" are expected and not a
# misconfiguration: proc-macros, anything that invokes the linker, and workspace
# crates built with `-C incremental` are all excluded by sccache's design. The
# number that matters is the hit rate on the ~300 registry dependencies.
#
# Show the dependency-cache hit rate
sccache-stats:
    sccache --show-stats

# Profile a build and open the per-crate breakdown. Use this before optimising
# anything — the answer for this workspace was "aws-lc-sys and linking", which
# is not what you would guess from watching the output scroll.
#
# Profile a build, per crate (writes target/cargo-timings/cargo-timing.html)
timings *args:
    cargo build --workspace --timings {{ args }}
    @echo "==> report: target/cargo-timings/cargo-timing.html"

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
lint-ci: fmt-check clippy deny toolchain-pin

# Fail if the Containerfile's RUST_TOOLCHAIN drifted from the flake's rustc
toolchain-pin:
    scripts/check-toolchain-pin.sh

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

# Runs fmt, clippy, the test suite, and the toolchain-pin check through crane,
# exactly as the GitHub `nix checks` workflow does. Slower on a cold Nix store than `just ci`, because
# it compiles dependencies into the Nix store rather than reusing `target/` —
# but that is precisely what makes the result cacheable, and it is the
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
        .#checks.x86_64-linux.test \
        .#checks.x86_64-linux.actionlint \
        .#checks.x86_64-linux.toolchain-pin

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
    just image alertmanagermcp-server
    just image tempomcp-server

# Run a built image, loading env from .sops.env (e.g. `just run-image pbsmcp-server`)
run-image bin tag="dev" port="8080":
    {{ secrets-file }} 'podman run --rm -it --env-file {} -e PBS_BIND=0.0.0.0:8080 -p {{ port }}:8080 {{ bin }}:{{ tag }}'

# Build an image, then scan it for vulnerabilities with trivy (e.g. `just scan pbsmcp-server`)
scan bin tag="dev": (image bin tag)
    trivy image --skip-version-check {{ bin }}:{{ tag }}

# Build an image, then smoke-test the container offline (e.g. `just smoke wpmcp-server`)
smoke bin tag="dev": (image bin tag)
    ./scripts/smoke.sh {{ bin }} {{ tag }}

# As `smoke`, but loads .sops.env and calls one tool for real (needs the backend reachable)
smoke-live bin tag="dev": (image bin tag)
    {{ secrets-file }} 'ENV_FILE={} ./scripts/smoke.sh {{ bin }} {{ tag }} --live'

# Build images sequentially (avoids IO storm from parallel podman builds),
# then bring the whole stack up. On a small VM (6 cores, limited RAM),
# parallel builds saturate btrfs IO and choke the system.
#
# compose.yaml reads `env_file: ${ENV_FILE:-.env}`, so the decrypted temp file
# serves both the per-service env_file and the `${POSTGRES_PASSWORD:?}`
# interpolation (--env-file). That `:?` is deliberate: without it a missing
# secrets file would boot postgres with a placeholder password.
up:
    just image-all
    {{ secrets-file }} 'ENV_FILE={} podman-compose --env-file {} up -d'

# Tear the stack down. Interpolation runs on every compose command, but `down`
# never uses the values, so a dummy satisfies `:?` without touching secrets.
down:
    POSTGRES_PASSWORD=unused podman-compose down

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

# Clean Cargo build artifacts. This does NOT clear the sccache cache, which is
# the point: the next build re-checks out the dependency tree from cache rather
# than recompiling it. `sccache --zero-stats` resets counters;
# `rm -rf ~/.cache/sccache` is the nuclear option.
#
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
# so it is the only one pulled from. Every other remote — GitHub today — is a
# downstream copy that receives the merged state and is never pulled.
#
# The remotes are ENUMERATED, not named. `git remote` becomes the single place a
# remote is declared, so adding one is `git remote add` and nothing else, and a
# clone that has no `github` remote does not fail on a hardcoded name. That
# second half matters because cog.toml's post-bump hook calls this recipe: a
# release must not die halfway through on a workstation whose remotes differ.
#
# Push the current branch and ALL tags to every configured remote
push-all:
    #!/usr/bin/env bash
    set -uo pipefail

    failed=()
    for remote in $(git remote); do
        echo "==> $remote"
        # `git push <remote>` with no refspec pushes the CURRENT branch to the
        # branch of the same name there: push.default=simple falls back to
        # `current` for a remote that is not this branch's upstream. So the
        # downstream copies need no tracking branches set up.
        #
        # `--tags` is a SEPARATE push on purpose. `--follow-tags` carries only
        # annotated tags reachable from the pushed commit, and cog creates
        # lightweight ones (`tag_prefix = ""`, no -a), so it would silently
        # push none of them. This is what makes "every remote has every tag"
        # true rather than approximately true.
        if git push "$remote" && git push "$remote" --tags; then
            continue
        fi
        # Keep going rather than aborting on the first failure. One unreachable
        # remote must not leave the reachable ones un-pushed — the point of this
        # recipe is that they all end up holding the same refs, and a partial
        # sync that stops early is the outcome hardest to reason about later.
        failed+=("$remote")
    done

    if [ "${#failed[@]}" -gt 0 ]; then
        echo "error: push failed for: ${failed[*]}" >&2
        exit 1
    fi

# Pull from origin, then publish the branch and every tag to every remote
sync-remotes:
    git pull
    just push-all

# ------------------------------------------------------------------------------
# Versioning / Release
# ------------------------------------------------------------------------------

# Show conventional commits changelog
changelog:
    cog changelog

# Bump version using conventional commits (e.g. `just bump auto`)
bump version:
    cog bump --{{ version }}
