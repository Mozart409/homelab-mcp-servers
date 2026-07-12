---
created: 2026-06-18
updated: 2026-06-18
tags:
  - mcp
  - server
  - architecture
  - decision
  - monorepo
  - rust
status: research
---

# MCP Monorepo — Should I create one?

**Question:** Should I consolidate all my custom MCP servers (all Rust, Cargo-based) into a single monorepo with a Cargo workspace, or keep them as separate repos?

## Context

I already have one Rust MCP server built and deployed:

- **hamcp-rs** — Home Assistant MCP server, Rust, already deployed on MCP VM at `https://homelab-mcp.dropbear-butterfly.ts.net/mcp`

Planned Rust MCP servers for the homelab:

- **Postgres MCP** — depending on permissions different usecases
- **Forgejo MCP** — read-only repos/issues/PRs
- **Prometheus MCP** — PromQL queries
- **Loki MCP** — LogQL queries
- **Tempo MCP** — trace ID lookups
- **Step-CA MCP** — certificate info
- **NixOS Flake MCP** — config inspection
- **Proxmox MCP** — VM/container status
- **UniFi MCP** — network status

That's potentially 9 servers total. All in Rust using `rmcp` (official MCP Rust SDK from modelcontextprotocol/rust-sdk) or `rust-mcp-sdk`.

## The Rust MCP SDK Landscape

Two main options:

1. **`rmcp`** — Official Rust SDK from modelcontextprotocol/rust-sdk. The SDK repo itself is a Cargo workspace with multiple crates (`rmcp` core, `rmcp-macros`). It's a monorepo — precedent from the protocol developers themselves.
2. **`rust-mcp-sdk`** — Community alternative on crates.io, also async, also supports both stdio and HTTP transport.

Both are async (tokio), which meshes well with Rust's ecosystem. Either way, all servers share essentially the same dependency stack: `tokio`, `serde`, `reqwest`/`hyper`, and the MCP SDK crate.

## Monorepo Advantages (Cargo Workspace)

1. **Shared dependency management** — One `Cargo.lock` at the workspace root. Dependencies like `rmcp`, `serde`, `tokio`, `reqwest` are declared once in the workspace `[workspace.dependencies]` section, inherited by all member crates. No copy-pasting version numbers across 9 `Cargo.toml` files. When you bump a version, you bump it once.

2. **Shared internal library crates** — MCP servers need common utilities:
   - Auth helpers (Tailscale certificate handling, step-CA mTLS, bearer token validation)
   - Shared MCP tool/ resource/ prompt schema helpers
   - Homelab service discovery (how to reach Prometheus, Loki, etc.)
   - Health check endpoint (`/health`, `/metrics`) on every server
   - Error handling and logging wrappers
   A `crates/homelab-mcp-core/` internal crate provides all this. Bug fix in auth? Fix once, rebuild all servers.

3. **Atomic cross-server refactors** — If `rmcp` releases a breaking change, or you redesign the auth layer, you fix everything in one commit. No "fix server A, then remember servers B through F" across separate repos.

4. **Single `cargo build --workspace` / `cargo test --workspace`** — Build and test all servers with one command. Cargo's build cache means unchanged crates use zero rebuild time.

5. **NixOS deployment fits perfectly** — One Nix flake that builds all workspace crates. Each server gets its own `naersk` or `crate2nix` derivation from the same source. `nix build .#hamcp-rs`, `nix build .#forgejo-mcp`, etc. A single `colmena apply` deploys everything.

6. **Unified versioning** — One changelog. `v1.0.0` means "all servers released together."

7. **Cargo workspace is built into Rust** — No external build system (no Turborepo, Nx, pnpm). Just a `[workspace]` in root `Cargo.toml`. It's trivial to set up and the ecosystem standard.

8. **Official Rust SDK is itself a Cargo workspace** — `modelcontextprotocol/rust-sdk` uses `[workspace]` with multiple crates. This is the pattern blessed by the protocol maintainers.

## Monorepo Disadvantages

1. **Single CI failure point** — A broken test in one server blocks the workspace. Mitigation: `cargo test -p specific-crate` and CI matrix jobs per crate, or `cargo test --workspace --no-fail-fast`.

2. **Git history is shared** — A commit fixing the Forgejo server sits next to a commit adding a Prometheus tool. For a solo project this barely matters.

3. **Clone weight** — All crates + `target/` = more disk. Mitigation: `CARGO_TARGET_DIR` outside the repo, or Cargo's shared cache. `cargo clean` per crate when needed. For Rust, `target/` can easily hit gigabytes — but that's true per-repo too, and with a monorepo you only have one `target/`.

4. **Workspace scaling** — Very large Rust workspaces (50+ crates) can slow `cargo check` resolution. For 9 servers this is irrelevant.

5. **Independent release cadences** — You might ship Forgejo MCP in a week and Tempo MCP in 6 months. With a monorepo, version tags cover everything. Mitigation: tag per server (`forgejo-mcp-v1.0.0`, `tempo-mcp-v0.1.0`) or just don't bother tagging individual servers (solo project).

6. **IDE/editor load** — `rust-analyzer` indexes the whole workspace. For 9 small servers this is fine, but worth noting.

## Situation-Specific Analysis (Your Homelab)

### Arguments FOR a monorepo

- **You're the only developer.** No merge conflicts, no coordination. The disadvantages of monorepos (contested CI, messy git history, ownership ambiguity) just don't apply.
- **Rust ecosystem is workspace-native.** Cargo workspaces are first-class, zero-cost, and well-documented. The language's package manager is designed for this.
- **NixOS + Rust = beautiful.** A single Nix flake building all crates via `naersk` or `crane`. Each server is a Nix package derived from the same source tree. This eliminates the biggest pain of separate repos: having to update 9 flakes when you change a shared dependency version.
- **Shared auth layer is a real win.** Every server needs to authenticate to homelab services. A shared `homelab-mcp-core` crate saves you from writing the same curl-with-Tailscale-cert code 9 times.
- **One-clone setup.** Dev machine, MCP VM, CI server: clone once, build everything.
- **You already have 1 server (hamcp-rs).** Migrating it into a workspace is cheap — add `[workspace]` with `members = ["crates/hamcp-rs", "crates/homelab-mcp-core"]`, move the code, done.

### Arguments AGAINST a monorepo

- **hamcp-rs is already deployed and working.** Moving it into a monorepo is overhead for zero functional benefit today. You'd need to update your deployment (Nix flake, systemd unit, CI) to point to the new path. This is minor friction but real.
- **Rust compile times.** A single workspace means `cargo check` on one server checks all servers. The compiler checks all crates on every `cargo build --workspace`. Mitigation: `cargo check -p specific-crate` when working on one server, only `--workspace` for CI.
- **Target directory in the workspace.** `target/` for a workspace with 9 crates can be substantial. Mitigation: `CARGO_TARGET_DIR=/some/other/disk/cargo-target` or a shared NAS mount.
- **Tool-specific dependencies.** If one server needs a heavy crate (e.g. `kube` for Kubernetes, `rusqlite` for SQLite), it's pulled into the workspace lockfile. Doesn't affect compile time of other crates, just the lockfile gets bigger.
- **Over-engineering risk.** For 2-3 servers, separate repos are fine. For 9, workspace wins. The inflection point is around 4-5 servers.

## Decision Paths

### Option A: Cargo Workspace Monorepo (Recommended)

```text
mcp-servers/
├── Cargo.toml             # [workspace] with members
├── Cargo.lock
├── crates/
│   ├── homelab-mcp-core/  # shared lib: auth, health, discovery, error types
│   ├── hamcp-rs/          # existing Home Assistant MCP (moved in)
│   ├── forgejo-mcp/       # planned
│   ├── prometheus-mcp/    # planned
│   ├── loki-mcp/          # planned
│   ├── tempo-mcp/         # planned
│   ├── step-ca-mcp/       # planned
│   ├── nixos-mcp/         # planned
│   ├── proxmox-mcp/       # planned
│   └── unifi-mcp/         # planned
├── nix/
│   └── flake.nix          # builds all crates, produces per-server packages
├── scripts/               # deploy helpers
└── README.md
```

**Root `Cargo.toml`:**

```toml
[workspace]
resolver = "2"
members = [
    "crates/homelab-mcp-core",
    "crates/hamcp-rs",
    "crates/forgejo-mcp",
    "crates/prometheus-mcp",
    "crates/loki-mcp",
    "crates/tempo-mcp",
    "crates/step-ca-mcp",
    "crates/nixos-mcp",
    "crates/proxmox-mcp",
    "crates/unifi-mcp",
]

[workspace.dependencies]
rmcp = "0.5"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json"] }
tracing = "0.1"
tracing-subscriber = "0.3"
anyhow = "1"
thiserror = "2"
```

Each server crate has a minimal `Cargo.toml`:

```toml
[package]
name = "forgejo-mcp"
version.workspace = true
edition = "2024"

[dependencies]
rmcp.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
reqwest.workspace = true
tracing.workspace = true
anyhow.workspace = true
homelab-mcp-core = { path = "../homelab-mcp-core" }
```

### Option B: Separate Repos

- One repo per server, each with its own `Cargo.toml`, `Cargo.lock`, Nix flake
- Shared code via `homelab-mcp-core` published to a private crate registry (or vendored via git dependency: `homelab-mcp-core = { git = "https://forgejo.homelab.local/amadeus/homelab-mcp-core" }`)
- More git repos to manage, more CI configs, more Nix flakes to update

### Option C: Hybrid — Workspace for future, hamcp-rs stays separate

- Start a new workspace for all planned servers
- Keep hamcp-rs in its own repo as-is (it already works)
- If hamcp-rs ever needs shared `homelab-mcp-core` features, migrate it into the workspace then

## Recommendation

**Go with Option A: Cargo workspace monorepo.** Here's why:

1. **Cargo workspace costs are near-zero.** Adding `[workspace]` to a `Cargo.toml` is a 2-line change. Workspaces are Rust's standard, not an add-on build system.

2. **You only have 1 server right now.** Migrating `hamcp-rs` into `crates/hamcp-rs/` is a `git mv` + path update in your deployment config. Minimal friction.

3. **Shared `homelab-mcp-core` crate avoids 8x code duplication.** Every server needs auth → you'd either copy-paste the same `authenticate_with_tailscale()` function or maintain a separate shared-crate repo. A workspace internal crate is the cleanest solution.

4. **Nix + Cargo workspace is a dream.** One `flake.nix` builds everything. `nix build .#hamcp-rs` builds just that crate; `nix build .#all` builds everything. Deploy each server as a separate systemd unit from the same source tree.

5. **You're already using Rust.** This isn't "should I use a monorepo for a polyglot project" — it's one language, one build system, one lockfile. Rust is uniquely well-suited to monorepos among compiled languages.

### Migration Plan (quick sketch)

1. Create root repo `mcp-servers` (or whatever name)
2. Add `Cargo.toml` with `[workspace]` and initial members
3. Move `hamcp-rs` into `crates/hamcp-rs/`
4. Create `crates/homelab-mcp-core/` with initial shared auth module
5. Generate `Cargo.lock` via `cargo generate-lockfile`
6. Update Nix flake to build from workspace
7. Update deployment (systemd unit paths on MCP VM, axon-gateway config)
8. Add next server crate when ready

## Releases & Artifacts

**Decision: no crates.io.** Ship complete source + built artifacts (OCI images, NixOS modules) from Harbor registry, versioned with **semver tags** (e.g. `0.2.5`, `v0.2.5`). One tag covers the whole workspace — every artifact released together. The git tag must match `[workspace.package].version` in `Cargo.toml`.

This vindicates the original unified-versioning instinct. Because nobody depends on individual crates through Cargo's registry, the per-crate semver burden disappears: internal crate splits (`hamcp-core`, `hamcp-util`, `pgmcp-core`, …) are pure organization and can be refactored freely, with no namespacing, per-crate metadata, or `release-plz` machinery needed.

### Sub-crate layout (per server)

Each server can be split into as many internal crates as helps readability — there's no external semver cost:

```text
crates/
├── shared/
│   └── homelab-mcp-core/     # cross-server: auth, health, discovery, errors
├── hamcp/
│   ├── hamcp-core/           # domain/protocol types
│   ├── hamcp/                # reusable lib
│   ├── hamcp-util/
│   └── hamcp-server/         # binary
├── postgres-mcp/
│   ├── pgmcp-core/
│   ├── pgmcp/
│   ├── pgmcp-util/
│   └── pgmcp-server/
└── ...                       # same pattern per server
```

### Build graph (one source → many outputs)

Use **`crane`** so the dependency closure compiles **once** and is shared across all crates — the marginal cost of server #10 is compiling one crate, not rebuilding the world. From the same build:

```text
workspace source
   └─ cargoArtifacts (deps compiled ONCE, shared)
       ├─ crane.buildPackage per crate  →  N binaries
       │     ├─ dockerTools.buildLayeredImage  →  N OCI images (shared base layers)
       │     └─ (binary referenced by)         →  N NixOS modules
```

Define the server list **once** and `genAttrs`/fold over it (ideally with `flake-parts`) so the same list drives `packages.*`, `dockerImages.*`, and `nixosModules.*`. Adding a server is one list entry that lands in every artifact type.

### Semver in `Cargo.toml`

Cargo's `version` must be valid semver — `major.minor.patch`. The `[workspace.package]` version is the single source of truth for the entire workspace, inherited by every crate via `version.workspace = true`.

Set `[workspace.package] version = "0.2.5"` and bump it with `cog bump --auto` (reads conventional commits) or `cog bump --minor`/`--major`. The version commit is tagged by cog with a bare semver tag (no `v` prefix: `0.2.5`). `push_harbor.sh` strips a leading `v` if present and validates the tag matches the workspace version before pushing images.

### How consumers pull a release (no registry)

All paths key off the same tag:

| Consumer | How they pin `0.2.5` |
|---|---|
| **Nix / NixOS** | `inputs.homelab-mcp.url = "github:you/homelab-mcp/0.2.5";` → gets `nixosModules.*` and `packages.*` |
| **OCI** | `homelab-harbor.dropbear-butterfly.ts.net/mcp-servers/hamcp-server:0.2.5` (also `:latest`) |
| **Rust devs** | git dependency: `hamcp = { git = "https://github.com/you/homelab-mcp", tag = "0.2.5" }` |
| **Anyone** | `git clone` + `nix build` or `cargo build --release` from source |

### Release flow (manual via `push_harbor.sh`)

1. `cog bump --auto` → bumps `[workspace.package].version`, tags the commit (e.g. `0.2.5`)
2. `./push_harbor.sh` → validates tag matches workspace version, builds all server images with `podman build`, pushes to Harbor registry with the version tag + `latest`

No publish step to crates.io, no GitHub/Forgejo Releases, no FlakeHub. Harbor is the single artifact registry.

> **Note:** if everything deploys to NixOS VMs internally, OCI images are optional — NixOS modules + systemd are the native path. Build images only for non-Nix consumers (k8s, Podman, public pulls). A self-hosted Nix binary cache (`attic`/`harmonia`) is worth standing up to keep CI and VM rebuilds fast at 10 servers.

### OCI images today (podman / Containerfile)

Ahead of the Nix `dockerTools` path, there's a working **podman** build for local dev — one image per `*-server` binary from a single parameterized [`Containerfile`](../Containerfile):

```sh
just image pbsmcp-server      # podman build --build-arg BIN=pbsmcp-server -t pbsmcp-server:dev .
just run-image pbsmcp-server  # podman run with .env, PBS_BIND=0.0.0.0:8080
just up                       # podman-compose up --build (whole stack)
```

Decisions baked in (validated end-to-end against live PBS — 13.2 MB image, rustls→HTTPS works):

- **rustls everywhere, no OpenSSL/native-tls.** `reqwest` pins `default-features = false, features = ["json","query","rustls"]`; `sqlx` uses `tls-rustls-ring-webpki`. This is what makes a static, libc-free image possible. Per binary: `pbsmcp-server` carries the **aws-lc-rs** provider (via reqwest), `pgmcp-server` carries **ring** (via sqlx).
- **Static musl → `distroless/static:nonroot`.** Built with `cargo-zigbuild` targeting `x86_64-unknown-linux-musl`; `cmake` is installed in the builder for aws-lc-rs. distroless/static (not `scratch`) because it bundles the CA cert bundle reqwest's platform-verifier needs, plus a non-root UID and tzdata.
- **Builder toolchain pinned to 1.95.0** (matches `flake.nix`) — the `cargo-zigbuild` base image's bundled rustc (1.85) is too old for the dep tree.
- **Config is runtime env only.** Secrets come from `.env` via `env_file`/`--env-file`, never baked into the image; bind must be `0.0.0.0` inside the container.

`pgmcp-server` serves over streamable HTTP in [`compose.yaml`](../compose.yaml), which also includes a commented second `postgres`/`pgmcp` pair as a template for one-MCP-per-database deployments.

## Crates

- ulid
- sqlx
- serde
- axum
- utoipa
- utoipa-scalar
- utoipa-axum
- color_eyre
- chrono
- rmcp
- reqwest

## Open Questions

- Repo name: `mcp-servers`, `homelab-mcp`, `homelab-mcp-rs`?
- Host it on Forgejo (self-hosted) or GitHub? (affects where Releases + OCI registry live)
- Auth approach for shared crate: step-CA mTLS, Tailscale auth keys, long-lived bearer tokens?
- Should hamcp-rs be moved in immediately or left separate until the shared crate has something it needs?
- CI: Forgejo Actions, Gitea Actions, or just Nix build locally?
- Do we build OCI images at all, or NixOS-modules-only? (only needed for non-Nix consumers — see Releases & Artifacts)
- Stand up a self-hosted Nix binary cache (`attic`/`harmonia`) for fast CI/VM rebuilds at ~10 servers?

**Resolved:**

- No crates.io — release source + OCI images via Harbor registry only.
- Versioning: **semver** tags (e.g. `0.2.5`), unified across the whole workspace via `cog bump`.
- Harbor-only delivery; no GitHub Releases, no FlakeHub, no ghcr.
- Internal sub-crate splits (`core`/`lib`/`util` per server) are free — no external semver cost.

