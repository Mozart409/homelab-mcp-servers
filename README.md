# homelab-mcp-servers

A Cargo workspace of [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) servers for a personal homelab. Most servers expose **read-only** tools
over the streamable-HTTP transport so an MCP client (Claude, etc.) can answer
operational questions — "did my backups verify?", "what's in this database?"
— without being able to change anything. The Home Assistant server is the
exception: it also supports mutating tools (`call_service`, `set_state`) because
controlling smart-home devices is its primary purpose.

Built in Rust on [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)
(the official MCP Rust SDK) and [axum](https://github.com/tokio-rs/axum). See
[`docs/overview.md`](docs/overview.md) for the monorepo design rationale.

## Servers

| Server                                  | Target                | Crates                       | Status      |
| --------------------------------------- | --------------------- | ---------------------------- | ----------- |
| [`pbs-mcp`](crates/pbs-mcp/README.md)            | Proxmox Backup Server | `pbsmcp`, `pbsmcp-server`   | implemented |
| [`postgres-mcp`](crates/postgres-mcp/README.md)  | PostgreSQL            | `pgmcp`, `pgmcp-server`     | implemented |
| [`prometheus-mcp`](crates/prometheus-mcp/README.md) | Prometheus         | `prommcp`, `prommcp-server` | implemented |
| [`loki-mcp`](crates/loki-mcp/README.md)          | Grafana Loki          | `lokimcp`, `lokimcp-server` | implemented |
| [`homeassistant-mcp`](crates/homeassistant-mcp/README.md) | Home Assistant | `hamcp`, `hamcp-server` | implemented |

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

Introspect schema and run read-only queries. Tools: `list_schemas`,
`list_tables`, `describe_table`, `list_indexes`, `list_foreign_keys`,
`table_stats`, `database_size`, and `run_query` (arbitrary read-only `SELECT`
in a `READ ONLY` transaction, statement-timed and row-capped). Full
configuration, read-only guarantees, and tool details are in the
[crate README](crates/postgres-mcp/README.md).

### prometheus-mcp — Prometheus

Run PromQL and inspect monitoring state. Tools: `query`, `query_range`,
`series`, `labels`, `label_values`, `targets`, `alerts`, `rules`, `metadata`,
`tsdb_status`, `build_info` — all read-only GETs against the Prometheus HTTP
API. Full configuration and tool/endpoint mapping are in the
[crate README](crates/prometheus-mcp/README.md).

### loki-mcp — Grafana Loki

Search logs with LogQL and inspect labels. Tools: `query`, `query_range`
(the workhorse for log search), `labels`, `label_values`, `series`,
`index_stats` — all read-only GETs against the Loki HTTP API. Supports
multi-tenant `X-Scope-OrgID`. Full configuration and tool details are in the
[crate README](crates/loki-mcp/README.md).

### homeassistant-mcp — Home Assistant

Query and control a Home Assistant instance. Tools: `health_check`,
`get_config`, `get_states`, `get_entity`, `call_service` (mutating),
`set_state` (mutating), `get_services`, `render_template`, `get_calendars`,
`get_calendar_events`, `check_config`, `get_history`. Full configuration,
required token setup, and tool details are in the
[crate README](crates/homeassistant-mcp/README.md).

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
cargo run -p pbsmcp-server    # PBS,        default endpoint http://127.0.0.1:8080/mcp
cargo run -p pgmcp-server     # Postgres,   default endpoint http://127.0.0.1:8081/mcp
cargo run -p prommcp-server   # Prometheus, default endpoint http://127.0.0.1:8082/mcp
cargo run -p lokimcp-server   # Loki,       default endpoint http://127.0.0.1:8083/mcp
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
    "postgres": { "type": "http", "url": "http://127.0.0.1:8081/mcp" },
    "prometheus": { "type": "http", "url": "http://127.0.0.1:8082/mcp" },
    "loki": { "type": "http", "url": "http://127.0.0.1:8083/mcp" }
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

The local stack (the MCP servers + a Postgres 18 instance) runs via
[`compose.yaml`](compose.yaml):

```sh
just up      # podman-compose up --build -d
just down
```

Secrets are read from `.env` and injected at runtime via `env_file` — never
baked into images. [`push_harbor.sh`](push_harbor.sh) pushes images to a Harbor
registry.

### Multiple instances — several Postgres databases

The NixOS module supports running more than one instance of the same server:
each `services.homelab-mcp.servers.<name>` entry is a separate systemd service
with its own environment. Because every pgmcp binary reads `PG_*` env vars, set
`serverType = "pgmcp-server"` on each instance so the module generates `PG_*`
variables (rather than deriving a prefix from the instance name):

```nix
services.homelab-mcp.servers = {
  pg-main = {
    enable = true;
    serverType = "pgmcp-server";
    package = homelab-mcp.packages.${system}.pgmcp-server;
    bind = "127.0.0.1:8081";
    tokenFile = "/run/secrets/pg-main-url";   # -> PG_DATABASE_URL
  };
  pg-warehouse = {
    enable = true;
    serverType = "pgmcp-server";
    package = homelab-mcp.packages.${system}.pgmcp-server;
    bind = "127.0.0.1:8085";
    tokenFile = "/run/secrets/pg-warehouse-url";
  };
};
```

Each `tokenFile` holds that database's connection string (loaded via systemd
`LoadCredential` as `PG_DATABASE_URL`). Give every instance a distinct `bind`
port. For the local compose stack, [`compose.yaml`](compose.yaml) has a
commented `postgres2`/`pgmcp2` pair showing the same one-per-database pattern.

## License

Private — not published (`publish = false`). See [`deny.toml`](deny.toml) for
dependency license/advisory policy.
