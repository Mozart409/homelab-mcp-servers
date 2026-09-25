#!/usr/bin/env bash
# Fail when the Containerfile's RUST_TOOLCHAIN differs from the rustc on PATH.
#
#   scripts/check-toolchain-pin.sh [Containerfile]
#
# The Containerfile carries the only rustc version literal in the repo: the
# cargo-zigbuild base image ships a rustc too old for edition 2024, so the image
# installs a toolchain by number and cannot read `flake.lock`. Everything else
# (dev shell, CI shell, crane checks) takes rustc from fenix via the lock, so a
# `nix flake update` moves them all and leaves the image behind. Left alone, the
# drift surfaces at release time as MSRV errors from unrelated dependencies,
# which points at the wrong thing entirely.
#
# One script, three callers, so there is exactly one definition of "in sync":
#   - `checks.toolchain-pin` in flake.nix (so `just ci-nix` and GitHub CI run it)
#   - `just toolchain-pin`, part of `just ci` (so it fails on the workstation,
#     before a push — the old GitHub-only step let a patch-level drift sit
#     unnoticed locally)
# The caller puts the flake's rustc on PATH; this script does not look for one.
set -euo pipefail

containerfile="${1:-Containerfile}"

flake_rustc="$(rustc --version | cut -d' ' -f2)"
pinned="$(awk -F= '/^ARG RUST_TOOLCHAIN=/ { print $2; exit }' "$containerfile")"

if [ -z "$pinned" ]; then
    echo "error: no 'ARG RUST_TOOLCHAIN=' line in $containerfile" >&2
    exit 1
fi

if [ "$flake_rustc" != "$pinned" ]; then
    echo "error: Containerfile pins RUST_TOOLCHAIN=$pinned but the flake ships rustc $flake_rustc." >&2
    echo "       Update the ARG in Containerfile to $flake_rustc." >&2
    exit 1
fi

echo "toolchain pin OK: $pinned"
