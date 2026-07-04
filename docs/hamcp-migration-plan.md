---
status: plan
date: 2026-07-04
---

# Migration Plan: hamcp-rs → homelab-mcp-servers

Fold the standalone `hamcp-rs` Home Assistant MCP server into the
`homelab-mcp-servers` monorepo, conforming to its conventions.

## Decisions (locked)

| Question | Decision |
|---|---|
| Mutating tools | **Keep all tools** (incl. `call_service`, `set_state`) |
| WebSocket module | **Remove entirely** (O5) — it was a non-functional placeholder |
| NixOS module + crane packaging | **Implement crane in the monorepo now** (new flake outputs + generalized NixOS module) |
| CI workflows | **Port hamcp-rs GitHub Actions to the monorepo** (workspace-wide) |
| Crate naming | `crates/homeassistant-mcp/` → lib `hamcp` + binary `hamcp-server` |
| Env vars | **Align to `HA_` convention**: `HA_HOST`, `HA_TOKEN`, `HA_INSECURE`, `HA_BIND`, `HA_ALLOWED_HOSTS`; default port `127.0.0.1:8084` |
| **O1** Tool names | **Explicit `#[tool(name=…, description=…)]` on every tool** — preserves exact MCP wire names |
| **O2** Healthchecks | **Keep, and make a monorepo-wide convention** — every server must implement them (see Phase 4b) |
| **O3** Release channel | **Harbor-only, but semver tags** (not CalVer) — reconcile `docs/overview.md` |
| **O4** git history | **Clean single-commit import** (no subtree/filter-repo) |
| **O5** WebSocket | **Remove** — drop `websocket/` module, `WebSocketClient` re-export, and its test |
| **O6** Healthcheck sharing | **Shared internal `mcp-common` crate** — the monorepo's first shared crate; all servers depend on it |
| Execution | **Written plan first** (this doc); execute after review |

## Scope callouts

Four decisions expand scope beyond a simple file-move and also **affect every
existing server** (pbs/pg/prom/loki), not just Home Assistant:

1. **Crane packaging** — the monorepo flake is currently dev-shell-only.
   `docs/overview.md` describes this as aspirational. Implementing it means
   authoring per-server `packages.<name>` outputs driven by a server list.
2. **NixOS module** — hamcp-rs ships a single hardened `services.hamcp` module.
   Generalizing it for a multi-server monorepo is a design task (one module
   per server vs. a parameterized `services.homelab-mcp.<name>` family).
3. **CI** — no `.github/` exists in the monorepo today. Porting single-repo
   workflows to a workspace requires rethinking (per-crate vs. workspace-wide,
   matrix over binaries for docker/release).
4. **Healthchecks (O2)** — hamcp-rs's healthcheck pattern (`--healthcheck` CLI
   flag + HTTP `/_healthcheck` route) becomes a **required convention for all
   servers**. pbs/pg/prom/loki currently have none; they must be retrofitted
   (Phase 4b) and the shared `Containerfile`/`compose.yaml`/`overview.md`
   updated to standardize it.

Because of (1)–(4), this migration is effectively **five workstreams**. They can
land incrementally; only Phase 0 + Phases 1–3 are required to get Home Assistant
building.

### O6: first shared crate (`mcp-common`)

Per O6, the healthcheck helper (and, going forward, any other cross-server
plumbing) lives in a new **`crates/mcp-common/`** library — the monorepo's first
shared internal crate. This is an intentional architectural shift: until now
every server was fully self-contained. `mcp-common` is introduced in **Phase 0**
because the healthcheck work (Phase 4b) and the new HA server both depend on it.
Keep it deliberately small and dependency-light so it doesn't become a
dumping ground.

### O3 conflict to resolve

`docs/overview.md` currently mandates **CalVer date-based tags** unified across
the workspace. Your O3 decision is **Harbor-only delivery but semver tags**.
These conflict. Phase 4c updates `overview.md`, `cog.toml` (already `tag_prefix
= ""`, semver-compatible), and `push_harbor.sh` (already strips a leading `v`
and matches `[workspace.package].version`, which is semver `0.2.5`) to codify
semver-over-CalVer. No FlakeHub / GitHub Releases publishing.

---

## Target layout

```
crates/mcp-common/             # NEW — first shared internal crate (O6)
├── Cargo.toml                 # name = "mcp-common"; publish = false
└── src/
    └── lib.rs                 # health_router() + run_healthcheck(bind) + health_handler

crates/homeassistant-mcp/
├── README.md
├── hamcp/                      # library crate (was mcp/src lib portion)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # mod client/config/server; pub use; async fn run()
│       ├── config.rs          # Config::from_env()  (HA_* vars, bind, allowed_hosts)
│       ├── client.rs          # HaClient  (was rest/mod.rs; renamed)
│       ├── server.rs          # HaServer  (was main.rs tool_router portion)
│       └── models/
│           ├── mod.rs         # API response types (unchanged)
│           └── inputs.rs      # tool input types (unchanged)
└── hamcp-server/              # binary crate
    ├── Cargo.toml
    └── src/main.rs            # color_eyre + dotenvy + tracing→stderr + hamcp::run()
```

The monorepo lib pattern is 4 flat files (`lib.rs`/`config.rs`/`client.rs`/
`server.rs`). Home Assistant has extra structure (`models/`); that subdir stays
as a nested module under the lib crate. This is a permitted deviation — the
pattern is a convention, not a hard rule, and HA genuinely has more surface area
(12 tools, rich response models). The `websocket/` module is **not** migrated
(O5): it was a non-functional placeholder exposing no tools.

---

## Phase 0 — Create the shared `mcp-common` crate [O6]

The monorepo's first shared internal crate. Small, dependency-light; owns the
healthcheck plumbing so all five servers share one implementation.

**Workspace-glob caveat (must decide):** the root manifest uses
`members = ["crates/*/*"]` — a **two-level** glob (`crates/<service>/<crate>`).
A single-crate `crates/mcp-common/` is only one level deep and **will not be
picked up**. Two options:

- **(a) Nest it:** `crates/common/mcp-common/` so it matches `crates/*/*`.
  Zero root-manifest change; consistent with the existing glob. **Recommended.**
- **(b) Add an explicit member:** keep `crates/mcp-common/` and add it to
  `members`/glob (e.g. `members = ["crates/*/*", "crates/mcp-common"]`).

This plan assumes **(a)** → path `crates/common/mcp-common/`.

**Contents (`crates/common/mcp-common/src/lib.rs`):**
- `pub fn health_router() -> axum::Router` — mounts `/_healthcheck` and `/` GET
  handlers returning `{"status":"ok"}`.
- `pub async fn run_healthcheck(bind: &str) -> color_eyre::Result<()>` — the
  `--healthcheck` CLI path: GET `http://{bind}/_healthcheck`, exit 0/1. Must not
  require dotenv/config beyond the bind address (works in distroless).
- Keep deps minimal: `axum`, `color-eyre`, `reqwest`, `serde` (all workspace).

**`Cargo.toml`:** `name = "mcp-common"`, `version/edition/publish` via workspace,
deps via `.workspace = true`, `description`.

Each server then: `mcp-common = { path = "../../common/mcp-common" }` (adjust
relative path per crate depth) and calls `mcp_common::health_router()` inside
its `run()` and `mcp_common::run_healthcheck(&bind)` from `main.rs`.

## Phase 1 — Scaffold the crate from the template

1. `cp -r templates/server-mcp crates/homeassistant-mcp`
2. `mv crates/homeassistant-mcp/mymcp crates/homeassistant-mcp/hamcp`
3. `mv crates/homeassistant-mcp/mymcp-server crates/homeassistant-mcp/hamcp-server`
4. `sed -i "s/mymcp/hamcp/g"` across the copied `Cargo.toml`s.
5. Set `description` in both Cargo.toml files (drop `TODO:`).

The root workspace glob `crates/*/*` auto-includes the new crates — **no root
`Cargo.toml` members edit needed**.

## Phase 2 — Move & reshape the source

**Source moves** (from `hamcp-rs/mcp/src/` → `crates/homeassistant-mcp/hamcp/src/`):

| From (hamcp-rs) | To (monorepo) | Change |
|---|---|---|
| `models/mod.rs` | `models/mod.rs` | as-is |
| `models/inputs.rs` | `models/inputs.rs` | as-is |
| `rest/mod.rs` | `client.rs` | rename `HomeAssistantClient` → `HaClient`; flatten module |
| `websocket/mod.rs` | — | **dropped** (O5); remove `pub mod websocket` + `WebSocketClient` re-export from `lib.rs` |
| `main.rs` (tool_router + HomeAssistantServer) | `server.rs` | rename → `HaServer` |
| `main.rs` (bootstrap) | `hamcp-server/src/main.rs` + lib `run()` | split (see below) |
| — (new) | `config.rs` | new: `Config::from_env()` |

**Code-style reconciliations** (monorepo conventions differ from hamcp-rs):

- **Error type:** hamcp-rs tools return `Result<String, String>`. Monorepo uses
  `Result<String, ErrorData>` (via `ErrorData::internal_error`). Convert all 12
  tools to the `ErrorData` pattern with a shared `call`-style helper mapping
  client errors.
- **Tool attribute (O1 = keep explicit names):** every tool keeps
  `#[tool(name="…", description="…")]`, preserving the exact MCP wire names
  (`health_check`, `get_config`, `call_service`, …). This is a **deliberate
  deviation** from the other monorepo servers (which derive the name from the fn).
  Document it in `crates/homeassistant-mcp/README.md` and, ideally, note in
  `templates/server-mcp` that explicit names are allowed.
- **`get_info()`:** monorepo builds `ServerInfo::default()` then assigns fields
  (because `ServerInfo` is `#[non_exhaustive]`) and sources name/version from
  `env!("CARGO_PKG_NAME")`/`env!("CARGO_PKG_VERSION")`. hamcp-rs uses the
  builder + hardcoded `"hamcp-rs"`. Convert to the monorepo idiom.
- **`run()` in lib:** add `pub async fn run() -> Result<()>` to `lib.rs`
  following the pbs/prom pattern: `Config::from_env()`, build client, build
  `StreamableHttpServerConfig` (apply `allowed_hosts`), `StreamableHttpService`,
  `.nest_service("/mcp", …)`, `.merge(mcp_common::health_router())` for the
  `/_healthcheck` + `/` routes (O2/O6), bind `config.bind`, `axum::serve` with
  graceful shutdown on ctrl-c.
- **Healthcheck (O2/O6):** the `--healthcheck` CLI path and the `/_healthcheck`
  handler come from the shared **`mcp-common`** crate (Phase 0), not a
  lib-local copy. The `--healthcheck` branch in `main.rs` runs **before**
  `dotenvy`/config load so it works in the distroless image with only env.
- **`main.rs`:** the standard bootstrap — but first branch on `--healthcheck`
  (calls `mcp_common::run_healthcheck(&bind)` where `bind` comes from
  `HA_BIND` / default), then color_eyre install, `dotenvy::dotenv().ok()`,
  tracing to **stderr** with EnvFilter, then `hamcp::run().await`.

**`config.rs`** (new, following pbs/prom `config.rs`):
- Fields: `base_url`, `token`, `insecure`, `bind` (default `127.0.0.1:8084`),
  `allowed_hosts: Option<Vec<String>>`.
- `from_env()` reads `HA_HOST` (required, `normalize_base_url`), `HA_TOKEN`
  (required, sensitive), `HA_INSECURE` (optional bool), `HA_BIND` (optional,
  default `127.0.0.1:8084`), `HA_ALLOWED_HOSTS` (optional comma-split).

## Phase 3 — Cargo wiring

**`hamcp/Cargo.toml`** — all deps via `.workspace = true`, `keep-sorted`, plus
the `mcp-common` path dep:
```
axum, color-eyre, mcp-common (path), reqwest, rmcp, schemars, serde,
serde_json, thiserror, tokio, tracing, url
```

**Root `[workspace.dependencies]` additions** (not currently present):
- `schemars` — hamcp uses it directly (monorepo gets it transitively via
  `rmcp` `schemars` feature; needs an explicit entry if used directly).
- `url = "2.5"` — used by hamcp client.
- Verify `reqwest` feature set: monorepo has `default-features=false, rustls`.
  hamcp-rs used default-features (native-tls/OpenSSL). **Switch hamcp to
  rustls** to match the workspace's rustls-only invariant (required for the
  static-musl Containerfile). Confirm no OpenSSL-specific code.

**`hamcp-server/Cargo.toml`** — `color-eyre, dotenvy, hamcp = { path = "../hamcp" },
tokio, tracing, tracing-subscriber` (all workspace except path dep).

## Phase 4 — Register in monorepo tooling

- `justfile` `image-all`: add `just image hamcp-server`.
- `push_harbor.sh` `SERVERS=(…)`: append `hamcp-server` **and** the missing
  `lokimcp-server` (existing omission — fix as part of this change).
- `compose.yaml`: add a `hamcp` service (BIN `hamcp-server`, `HA_BIND=0.0.0.0:8084`,
  port `8084`, `env_file: .env`) with a `healthcheck:` block invoking
  `["/usr/local/bin/server", "--healthcheck"]` (O2).
- `.env.example`: add `HA_HOST`, `HA_TOKEN`, `HA_INSECURE`, `HA_BIND`,
  `HA_ALLOWED_HOSTS`.
- `README.md` servers table: add the `homeassistant-mcp` row + tool list; note
  the explicit-tool-name convention (O1) and the healthcheck convention (O2).
- `deny.toml`: run `cargo deny check`; add any newly-introduced licenses
  (with keep-sorted comments) if the switch to rustls / added deps trip it.
- Write `crates/homeassistant-mcp/README.md` (per-server doc, matching the
  style of `pbs-mcp/README.md`).

## Phase 4b — Healthcheck convention (monorepo-wide, new) [O2/O6]

Roll the shared `mcp-common` healthcheck (Phase 0) out to every server:

- Retrofit `pbsmcp`, `pgmcp`, `prommcp`, `lokimcp`:
  - add `mcp-common = { path = "../../common/mcp-common" }` to each lib
    `Cargo.toml`;
  - `.merge(mcp_common::health_router())` in each `run()`;
  - add the `--healthcheck` branch calling `mcp_common::run_healthcheck(&bind)`
    in each `-server/src/main.rs` (bind from that server's `*_BIND` var/default).
- Update `templates/server-mcp` so new servers get the `mcp-common` dep + the
  healthcheck wiring (and the `--healthcheck` branch) scaffolded for free; note
  the convention in the template README.
- Update the shared `Containerfile` (add a healthcheck note) and `compose.yaml`
  so **every** service defines a `healthcheck:` using the in-binary
  `--healthcheck` flag — distroless has no shell/curl, so the flag-based probe
  is the correct mechanism.
- Document the convention in `docs/overview.md`.

Verification: `just image <server> && podman run … ` then confirm the container
reports healthy; `cargo test --workspace` for the `mcp-common` unit tests.

## Phase 4c — Semver-over-CalVer reconciliation [O3]

- Edit `docs/overview.md`: replace the "CalVer date-based tags" mandate with
  **semver** (matching `[workspace.package].version = 0.2.5`). Remove FlakeHub /
  GitHub Releases language; delivery is **Harbor-only**.
- Confirm `cog.toml` (`tag_prefix = ""`, `cog bump --auto`) produces semver tags
  — it already does.
- Confirm `push_harbor.sh` version/tag match logic works with semver — it
  already reads `[workspace.package].version` and strips a leading `v`.

## Phase 5 — Crane packaging (monorepo-wide, new)

Rework `flake.nix` to add `packages` outputs. Following hamcp-rs's crane setup
but generalized over the server list:
- Add `crane` (+ keep `rust-overlay`) as a flake input.
- Build `cargoArtifacts` once (deps), then one `craneLib.buildPackage` per
  server with `cargoExtraArgs = "--bin <name>"` and matching `pname`.
- `packages.<system>.{hamcp-server, pbsmcp-server, pgmcp-server,
  prommcp-server, lokimcp-server}` + a sensible `default`.
- Keep the toolchain at the monorepo's **stable 1.96.0** (NOT hamcp-rs's
  nightly — verify hamcp compiles on stable; nothing observed requires nightly).
- Optionally add `dockerTools.buildLayeredImage` OCI outputs per
  `docs/overview.md`.

## Phase 6 — NixOS module (monorepo-wide, new)

Port hamcp-rs's hardened `services.hamcp` module, generalized:
- Either one module per server, or a parameterized
  `services.homelab-mcp.servers.<name>` family.
- Preserve the security hardening (`DynamicUser`, `LoadCredential`,
  `ProtectSystem=strict`, `MemoryDenyWriteExecute`, etc.).
- Map options to the new env var names (`HA_HOST`, `HA_BIND`, token via
  `LoadCredential`).
- Export via `nixosModules.default` / `nixosModules.hamcp`.

## Phase 7 — CI (monorepo-wide, new `.github/workflows/`)

Port and adapt hamcp-rs's workflows to workspace scope:
- **pr-checks**: `cargo fmt --check`, `cargo clippy --all-targets --all-features
  -- -D warnings -D clippy::pedantic`, `cargo test --workspace`, typos,
  actionlint. Run via the Nix dev shell (DeterminateSystems installer).
- **nix-build**: `nix build .#<each package>` + `nix flake check` (depends on
  Phase 5 existing).
- **security-audit**: `cargo deny check` (advisories/bans/licenses/sources).
- **docker**: matrix over all server binaries (`hamcp-server`, `pbsmcp-server`,
  `pgmcp-server`, `prommcp-server`, `lokimcp-server`), build+push to **Harbor**
  via podman using the `Containerfile` (mirrors `push_harbor.sh`). Triggered on
  **semver tags** (O3). No ghcr, no FlakeHub, no GitHub Releases.
- **healthcheck (O2)** is validated implicitly by the docker/compose smoke and
  by `cargo test`.
- **Do NOT port** hamcp-rs's `flakehub-publish-tagged.yml`, `documentation.yml`
  (GitHub Pages), or `release-builds.yml` (binary artifacts) — out of scope per
  O3 (Harbor-only).

## Phase 8 — Verify

```sh
cargo check -p mcp-common
cargo check -p hamcp
cargo check -p hamcp-server
cargo check --workspace       # confirms retrofitted pbs/pg/prom/loki still build
cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic
cargo test --workspace
cargo deny check
git ls-files | keep-sorted    # on edited files
just ci
nix build .#hamcp-server      # after Phase 5
nix flake check               # after Phase 5/6
```

---

## Things being dropped / changed (confirm acceptable)

- **hamcp-rs `Dockerfile` (alpine→scratch, OpenSSL-static)** — discarded in
  favor of the monorepo's parameterized `Containerfile` (zigbuild musl →
  distroless, rustls). The `--healthcheck` flag + `/_healthcheck` route are
  **kept** (O2) and generalized to all servers (Phase 4b).
- **hamcp-rs `bacon.toml`** — monorepo has no bacon; uses `cargo-watch` via
  `just watch-pkg`. Dropped.
- **hamcp-rs `websocket/` module** — **removed** (O5); non-functional placeholder.
- **Independent version `0.1.3`** — collapses into workspace `0.2.5`.
- **Toolchain nightly 2026-02-15** → **stable 1.96.0**.
- **`MCP_ADDR` / `HA_URL`** env vars → `HA_BIND` / `HA_HOST`. **Breaking** for
  any existing hamcp-rs deployment.
- **`#![warn(clippy::pedantic)]` crate attribute** — monorepo enforces pedantic
  via CLI (`just clippy`), so the crate-level attribute is redundant; drop for
  consistency (optional).
- **hamcp-rs workflows not ported:** `flakehub-publish-tagged.yml`,
  `documentation.yml`, `release-builds.yml` (Harbor-only per O3).
- **git history:** **clean single-commit import** (O4) — hamcp-rs history is
  not preserved; the migration lands as one commit
  (`feat: add home assistant mcp server`).

## Resolved decisions

- **O1 Tool names** → explicit `#[tool(name=…)]` on all tools.
- **O2 Healthcheck** → keep + make monorepo-wide convention (Phase 4b).
- **O3 Release channel** → Harbor-only, semver tags; reconcile `overview.md` (Phase 4c).
- **O4 git history** → clean single-commit import.
- **O5 WebSocket** → removed.
- **O6 Healthcheck sharing** → new shared **`mcp-common`** crate (Phase 0), at
  `crates/common/mcp-common/` to satisfy the `crates/*/*` glob; all servers
  depend on it. First shared internal crate in the monorepo.

**All decisions resolved — plan is ready to execute.**
