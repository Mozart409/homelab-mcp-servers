# homelab-mcp-servers

A Cargo workspace of read-only [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) servers for a personal homelab. Each server exposes a focused set of
**read-only** tools over the streamable-HTTP transport so an MCP client (Claude,
etc.) can answer operational questions — "did my backups verify?", "what's in
this database?" — without being able to change anything.

Built in Rust on [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)
(the official MCP Rust SDK) and [axum](https://github.com/tokio-rs/axum). See
[`docs/overview.md`](docs/overview.md) for the monorepo design rationale.

## Servers

| Server                                  | Target                | Crates                       | Status      |
| --------------------------------------- | --------------------- | ---------------------------- | ----------- |
| [`pbs-mcp`](crates/pbs-mcp/README.md)   | Proxmox Backup Server | `pbsmcp`, `pbsmcp-server`    | implemented |
| `postgres-mcp`                          | PostgreSQL            | `pgmcp`, `pgmcp-server`      | implemented |

Each server is split into a **library** crate (REST/DB client + MCP tool
definitions and wiring) and a thin **`-server`** binary that serves the tools
over streamable HTTP, with the MCP endpoint mounted at `/mcp`.

### pbs-mcp — Proxmox Backup Server

Inspect datastores, backup snapshots, GC, and task history. Tools:
`list_datastores`, `datastore_status`, `list_groups`, `list_snapshots`,
`list_tasks`, `task_status`, `task_log`, `gc_status`, `node_status`. Full
configuration, required PBS ACLs, and tool/endpoint mapping are in the
[crate README](crates/pbs-mcp/README.md).

### postgres-mcp — PostgreSQL

Introspect schema and run read-only queries. Tools:

| Tool                | Description                                                                              |
| ------------------- | --------------------------------------------------------------------------------------- |
| `list_schemas`      | List user schemas (excludes system schemas).                                            |
| `list_tables`       | List tables and views with schema and type; optionally restrict to one schema.          |
| `describe_table`    | Column details: name, position, type, nullability, default, length/precision.           |
| `list_indexes`      | Indexes on a table, with their definitions.                                              |
| `list_foreign_keys` | Foreign keys: constraint name, local column, referenced table/column.                    |
| `table_stats`       | Per-table stats: live/dead rows, scan counts, on-disk size, last (auto)vacuum/analyze.   |
| `database_size`     | Current database name and total on-disk size.                                            |
| `run_query`         | Run an arbitrary read-only `SELECT`. Executes in a `READ ONLY` transaction with a statement timeout and a row cap; writes, DDL, and long-running queries are rejected. |

## Quick start

This repo ships a [Nix flake](flake.nix) that pins the Rust toolchain (1.96)
and every dev tool. With [Nix](https://nixos.org) + flakes (and optionally
[direnv](https://direnv.net)):

```sh
nix develop        # or: just dev   — drops you into the dev shell
```

Without Nix you'll need a Rust 1.96+ toolchain (edition 2024) and the tools
referenced by the [`justfile`](justfile) (`just`, `cargo`, `podman` / `podman-compose`).

Configuration is via environment variables, loaded from a `.env` at the repo
root (`dotenvy`); real environment variables take precedence and a missing
`.env` is fine. Copy the example and fill it in:

```sh
cp .env.example .env
$EDITOR .env
```

Then run a server:

```sh
cargo run -p pbsmcp-server   # PBS,      default endpoint http://127.0.0.1:8080/mcp
cargo run -p pgmcp-server    # Postgres, default endpoint http://127.0.0.1:8081/mcp
```

Both bind loopback-only by default and reject non-loopback `Host` headers
(DNS-rebinding protection). To serve on a hostname (behind Tailscale or a
reverse proxy), set the server's `*_BIND` and `*_ALLOWED_HOSTS` variables. See
[`.env.example`](.env.example) for every variable.

### MCP client config

The servers use the streamable-HTTP transport, so point the client at the URL:

```json
{
  "mcpServers": {
    "pbs": { "type": "http", "url": "http://127.0.0.1:8080/mcp" },
    "postgres": { "type": "http", "url": "http://127.0.0.1:8081/mcp" }
  }
}
```

## Development

Common tasks are wrapped in the [`justfile`](justfile) (`just --list` for all):

```sh
just check          # cargo check --workspace
just build          # cargo build --workspace
just test           # cargo test --workspace
just test-pkg pgmcp # test a single package
just watch-pkg pgmcp-server  # watch + re-run a binary

just fmt            # cargo fmt
just clippy         # clippy with -D warnings -D clippy::pedantic
just lint           # fmt + clippy + cargo-deny
just ci             # everything CI runs (lint + test)
```

Commits follow [Conventional Commits](https://www.conventionalcommits.org/);
the changelog is generated by [cocogitto](https://github.com/cocogitto/cocogitto)
(`just changelog`, `just bump <version>`). Git hooks are managed by
[lefthook](https://github.com/evilmartians/lefthook) (installed via the flake's
shell hook).

## Containers & deployment

Each binary builds into a static-musl / distroless image via the shared
[`Containerfile`](Containerfile):

```sh
just image pbsmcp-server          # build one image
just image-all                    # build images for every server
just run-image pbsmcp-server      # run it, loading env from .env
just scan pbsmcp-server           # build, then scan with trivy
```

The local stack (PBS MCP server + a Postgres 18 instance) runs via
[`compose.yaml`](compose.yaml):

```sh
just up      # podman-compose up --build -d
just down
```

Secrets are read from `.env` and injected at runtime via `env_file` — never
baked into images. [`push_harbor.sh`](push_harbor.sh) pushes images to a Harbor
registry.

## License

Private — not published (`publish = false`). See [`deny.toml`](deny.toml) for
dependency license/advisory policy.
