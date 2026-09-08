# 0002 — `flake.nix` is the single registry of server binaries

Status: accepted (2026-09-07)

## Context

A server binary in this workspace has to be named in several unrelated places
before it fully exists: the Nix packages, the NixOS module's per-server defaults,
the local image recipes, the compose stack, and — the one that matters most —
whatever the release workflow iterates over to decide which container images to
publish.

That last list is the one that fails silently. Nothing about a missing entry is
visible at build time or test time; the crate compiles, CI is green, the release
succeeds, and the only symptom is an image that is not in the registry. This has
already happened here: `push_harbor.sh` shipped for two releases without
`wpmcp-server`, because it carried its own hardcoded list.

## Decision

`flake.nix`'s `packages` output is the registry. Anything that needs to know
"which servers does this workspace ship?" derives the list from it rather than
keeping a copy.

`.github/workflows/release.yml` does this literally:

```sh
nix eval --json .#packages.x86_64-linux --apply builtins.attrNames \
  | jq -c '[.[] | select(. != "default" and . != "all")]'
```

The result feeds a build matrix, so adding a `mkServer` line to `serverPkgs` is
what makes a new server get a published image. There is no release-side list to
remember.

## What this does and does not cover

Verified when `alertmanager-mcp` was added:

**Automatic, from the one `serverPkgs` line:**

- The release matrix builds and pushes
  `ghcr.io/<owner>/homelab-mcp-servers/<bin>` at `<version>`, `<minor>`, and
  `latest`.
- The release notes list the new image's `podman pull` line, because they are
  generated from the same enumeration.
- `nix flake check` / `nix-checks.yml` cover the crate's fmt, clippy, and tests,
  because the workspace is `crates/*/*` and the crane checks run
  `--workspace --all-targets --all-features`. A new crate needs no CI entry.

**Still manual, and deliberately so:**

- `just image-all` — a local convenience recipe, not the release path.
- `compose.yaml` — only servers wanted in the local stack belong there.
- `knownServers` in `flake.nix` — the NixOS module's env prefix and default
  port. Separate from `serverPkgs` because it answers a different question
  (how is this deployed) than "does this binary exist".
- `push_harbor.sh` — the internal Harbor push, still hand-maintained. This is
  the one place the original bug can recur.

**A real gap, accepted:** `ci.yml` smoke-builds exactly one container image
(`pbsmcp-server`), not all of them, because every server shares one
`Containerfile` and differs only by `--build-arg BIN=`. A per-server image
failure is therefore not caught on a pull request — only at release, where the
matrix has `fail-fast: false` so one broken server does not block the rest.
Building all seven on every PR would cost seven full dependency compiles to
re-prove the same musl cross-compile. Run `just image <bin>` locally when adding
a server; that is the check this trades away.

## Consequences

- Adding a server is one line in `serverPkgs` plus one in `knownServers`. The
  release needs no edit, which is the whole point.
- The enumeration filters `default` and `all`, which are aliases rather than
  servers. A future non-server entry in `packages` must be added to that filter
  or it will be built as an image — the filter is a denylist, and that is its
  weakness.
- `push_harbor.sh` remains outside this scheme and should be pointed at the same
  enumeration if it is touched again.
