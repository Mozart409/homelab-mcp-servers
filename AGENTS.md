# AGENTS.md

Guidance for AI coding agents working in this repository. Humans should read
[`README.md`](README.md) first; this file captures the conventions an agent must
follow to make changes that fit the codebase.

## What this is

A Cargo workspace of [MCP](https://modelcontextprotocol.io) servers for a
personal homelab. Each server exposes a focused set of **read-only** tools over
the streamable-HTTP transport (mounted at `/mcp`) so an MCP client can answer
operational questions without being able to change anything — with one
deliberate exception (`homeassistant-mcp`; see Hard rules §1).

- **Language/edition:** Rust, edition 2024, toolchain pinned via [`flake.nix`](flake.nix).
- **Core deps:** [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) (MCP SDK),
  [`axum`](https://github.com/tokio-rs/axum), `tokio`, `serde`, `color-eyre`,
  `reqwest` (rustls) for REST targets, `sqlx` (rustls) for DB targets.
- **Design rationale:** [`docs/overview.md`](docs/overview.md).

## Layout

```
crates/<service>-mcp/
  <svc>mcp/         library crate: config + client + server (MCP tool wiring)
  <svc>mcp-server/  thin binary: calls the lib's run()
crates/common/mcp-common/  shared plumbing every server depends on
templates/server-mcp/  scaffold for a new server (copy this to start)
```

Existing servers: `crates/pbs-mcp` (Proxmox Backup Server, REST, port `8080`),
`crates/postgres-mcp` (PostgreSQL, sqlx, `8081`), `crates/prometheus-mcp`
(Prometheus, REST, `8082`), `crates/loki-mcp` (Grafana Loki, REST, `8083`),
`crates/homeassistant-mcp` (Home Assistant, REST, `8084` — **not read-only**,
see Hard rules §1), and `crates/woodpecker-mcp` (Woodpecker CI, REST, `8085`).
The library crate is the unit of substance; the `-server` binary is a near-empty
`main` that calls `run()`. The Prometheus/Loki REST servers are the closest
clone of `pbs-mcp` — copy that one when adding another REST-backed server.

Alongside the servers, `crates/common/mcp-common` (package `mcp-common`) holds
the cross-server plumbing that would otherwise be copy-pasted into every crate:
`health_router()` — an axum router mounting `GET /_healthcheck` and `GET /` —
and `run_healthcheck(bind)`, the client-side probe behind every binary's
`--healthcheck` flag. The distroless images have no shell and no `curl`, so the
container healthcheck is the binary probing itself; that is why the probe must
work without config or dotenv. A new server merges `health_router()` into its
own router in `run()` and wires the `--healthcheck` flag in `main` — don't
reimplement either.

Within a library crate the module split is consistent:
- `config.rs` — `Config` struct + `Config::from_env()`, all settings from env vars.
- `client.rs` — the REST/DB client wrapper.
- `server.rs` — the `*Server` struct, tool parameter types, and `#[tool]` methods.
- `lib.rs` — re-exports + the `run()` function that builds config/client/service
  and serves over streamable HTTP.

## Hard rules

1. **Read-only only — hamcp excepted.** Every tool must be incapable of
   mutating the target. Postgres runs queries in a `READ ONLY` transaction,
   statement-timed and row-capped; REST servers only issue GETs. Never add a
   write/DDL/mutating tool. **Exception:** `homeassistant-mcp` deliberately
   exposes mutating tools (`set_state`, `call_service` — POSTs) because
   controlling smart-home devices is its primary purpose; the owner opted it
   out of this rule on purpose. The exception is per-server, not a precedent:
   don't add mutating tools to any other server, and don't add more to hamcp
   unless explicitly asked.
2. **Loopback by default.** Servers bind `127.0.0.1` and rely on rmcp's
   DNS-rebinding protection (`allowed_hosts`). Don't change defaults to bind
   non-loopback; that's opt-in via `*_BIND` / `*_ALLOWED_HOSTS` env vars.
3. **rustls everywhere — no OpenSSL/native-tls.** `reqwest` uses
   `default-features = false, features = ["json","query","rustls"]`; `sqlx` uses
   `tls-rustls-ring-webpki`. This is what keeps the static-musl/distroless image
   possible. Don't pull in a dep that drags in OpenSSL.
4. **Config is runtime env only.** All settings come from env vars (`<SVC>_*`),
   loaded from `.env` via `dotenvy`. Secrets are never baked into images. Update
   [`.env.example`](.env.example) when adding a variable.
5. **Workspace-inherited deps.** Add shared deps to root `[workspace.dependencies]`
   and reference them with `<dep>.workspace = true`. Keep the `# keep-sorted`
   blocks sorted (`just sort`).
6. **Use this workspace's crates, not the usual alternatives.** Errors:
   `color_eyre` (`color_eyre::eyre::{Result, WrapErr, bail, eyre}`) — **never**
   `anyhow`. IDs: `ulid` — **never** `uuid`. These are already in
   `[workspace.dependencies]`; reach for them rather than pulling in a parallel
   crate that does the same job.
7. **No panicking shortcuts in production code.** `[workspace.lints.clippy]` in
   the root `Cargo.toml` denies `unwrap_used`, `expect_used`, `panic`,
   `unwrap_in_result`, and `indexing_slicing`; every member crate opts in with
   `[lints] workspace = true`. These are long-running daemons — a panic kills
   every in-flight MCP request, not just the one that tripped it. Propagate with
   `color_eyre`'s `Result` + `WrapErr` in `run()`/lib code, and map into
   `rmcp::ErrorData` inside tool methods. Tests are exempt via the root
   [`clippy.toml`](clippy.toml) (`allow-unwrap-in-tests` et al.) — a failed
   `unwrap()` in a test *is* the assertion — but note the exemption keys off the
   enclosing `#[test]` function, so a free helper in `tests/` is still subject to
   the lint. Reach for `#[allow]` only when there is genuinely no alternative,
   at the narrowest possible scope, with a comment saying why.

## Build / test / lint

Use the [`justfile`](justfile) (`just --list`):

```sh
just check              # cargo check --workspace
just test               # cargo test --workspace
just test-pkg pgmcp     # one package
just watch-pkg pgmcp-server
just fmt                # cargo fmt
just clippy             # clippy -D warnings -D clippy::pedantic  (must pass clean)
just lint               # fmt + clippy + cargo-deny
just ci                 # what CI runs: lint + test
```

Before considering a change done: `just ci` must pass. Clippy runs with
`-D warnings -D clippy::pedantic`, so pedantic lints are errors — write
`# Errors`/`# Panics` doc sections, `#[must_use]`, etc. as the existing code does.

## Conventions

- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/) in
  the form `type(scope): title` (`feat(pgmcp): ...`, `docs(readme): ...`). Git
  hooks run via lefthook.
  - **Subject line only — no body.** Every non-merge commit in this repository's
    history has an empty body, and new commits must match. Say it in the title
    or don't say it: if a change needs a paragraph of justification, that
    paragraph belongs in a code comment or the crate README, where it stays next
    to the thing it explains instead of being buried in `git log`.
  - Keep the subject under ~77 characters, the longest this history uses.
  - Scope is the crate name (`pgmcp`, `wpmcp`, `mcp-common`) or the area
    (`deps`, `nix`, `ci`, `readme`, `agents`). Use `servers` when a change lands
    across several server crates at once.
- **Releases / version bumps:** cut releases with cocogitto, never by hand. The
  workflow is `cog bump --patch` (or `--minor` / `--major` / `--auto`), wrapped
  as `just bump patch` / `just bump minor` / `just bump major` / `just bump auto`.
  This is the canonical way to release — it bumps `[workspace.package] version`,
  regenerates the changelog, and creates the tag in one step. **Never** hand-edit
  `CHANGELOG.md` or the `version` in `Cargo.toml`, and don't create a
  `chore(version): ...` commit or git tag manually — `cog bump` owns all of that.
  Use `just changelog` (`cog changelog`) to preview unreleased changes.
- **Errors:** `color-eyre`'s `Result` in lib/`run()` code; map into
  `rmcp::ErrorData` (e.g. `ErrorData::internal_error(format!("{e:#}"), None)`)
  inside tool methods.
- **Tool params:** a `#[derive(Debug, Deserialize, schemars::JsonSchema)]` struct
  per tool, doc-commented (the docs become the tool's JSON schema), taken via
  `Parameters<T>`.
- **Tests:** env-reading config tests serialize on a `Mutex` and clear env keys
  before/after (see `config.rs`); follow that pattern. Integration tests live in
  the lib crate's `tests/`.
- **Tone of docs:** module/`run()`/config doc comments are thorough and explain
  *why* (see `config.rs`, `lib.rs`). Match that density.

## Adding a new server

1. Copy `templates/server-mcp/` to `crates/<service>-mcp/`, rename the
   `mymcp`/`mymcp-server` crates to `<svc>mcp`/`<svc>mcp-server`.
2. Implement `config.rs` (env vars prefixed `<SVC>_`, pick the next free default
   port — pbs `8080`, postgres `8081`, prometheus `8082`, loki `8083`, ha `8084`,
   wp `8085`),
   `client.rs`, `server.rs` (read-only tools), and `run()` in `lib.rs`.
3. Register the crate (workspace `members` is `crates/*/*`, so it's automatic),
   add deps to `[workspace.dependencies]` if new.
4. Add the binary to `just image-all` and, if it should run in the local stack,
   to [`compose.yaml`](compose.yaml).
5. Add a crate `README.md`, a row in the root README's Servers table, and an
   entry in `.env.example`.
6. `just ci`.

## Containers

Each `-server` binary builds via the shared [`Containerfile`](Containerfile)
(static musl → `distroless/static:nonroot`, built with `cargo-zigbuild`):
`just image <bin>`, `just image-all`, `just scan <bin>` (trivy), `just up`/`down`
for the compose stack. Bind must be `0.0.0.0` *inside* a container (env override),
never as a code default.
