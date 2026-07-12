# postgres-mcp

MCP server for **PostgreSQL**. Exposes read-only tools to introspect a
database's schema (tables, columns, indexes, foreign keys, stats) and to run
arbitrary read-only `SELECT` queries, so an MCP client can answer "what's in
this database / how is this table laid out / how big is it?".

Two crates:

- `pgmcp` — library: sqlx-based DB client + MCP tool definitions and wiring.
- `pgmcp-server` — thin binary that serves the tools over streamable HTTP
  (axum), with the MCP endpoint mounted at `/mcp`.

## Configuration

The server reads its configuration from environment variables. A
[`.env.example`](../../.env.example) lives at the repo root — copy it and fill
it in:

```sh
cp .env.example .env
$EDITOR .env
```

On startup the binary loads `.env` from the working directory (via `dotenvy`);
real environment variables take precedence, and a missing `.env` is fine (e.g.
when an MCP client injects the variables itself).

| Variable                  | Required | Default          | Description                                                            |
| ------------------------- | -------- | ---------------- | --------------------------------------------------------------------- |
| `PG_DATABASE_URL`         | yes\*    | —                | Connection string, e.g. `postgres://user:pass@host:5432/dbname`      |
| `DATABASE_URL`            | yes\*    | —                | Fallback connection string; `PG_DATABASE_URL` takes precedence        |
| `PG_BIND`                 | no       | `127.0.0.1:8081` | Address to bind the streamable-HTTP server (endpoint at `/mcp`)       |
| `PG_ALLOWED_HOSTS`        | no       | loopback only    | Comma-separated allowed `Host` values; set when serving on a hostname |
| `PG_MAX_CONNECTIONS`      | no       | `5`              | Connection pool size                                                   |
| `PG_STATEMENT_TIMEOUT_MS` | no       | `5000`           | Per-query timeout in milliseconds; guards against runaway queries     |
| `PG_MAX_ROWS`             | no       | `1000`           | Hard cap on rows returned per tool call                               |

\* One of `PG_DATABASE_URL` or `DATABASE_URL` must be set; the server errors out
on startup if neither is present.

By default the server only accepts loopback `Host` headers (DNS-rebinding
protection). When exposing it on a hostname — e.g. behind Tailscale or a
reverse proxy — list that hostname in `PG_ALLOWED_HOSTS`.

### Read-only guarantees

`run_query` executes inside a `READ ONLY` transaction with the configured
statement timeout, so writes, DDL, and long-running queries are rejected at the
database level. Results are capped at `PG_MAX_ROWS`. For defense in depth,
point the server at a Postgres role that only has `SELECT` privileges.

## Tools

| Tool                | Description                                                                            |
| ------------------- | ------------------------------------------------------------------------------------- |
| `list_schemas`      | List user schemas (excludes system schemas like `pg_catalog`).                        |
| `list_tables`       | List tables and views with their schema and type. Optionally restrict to one schema.  |
| `describe_table`    | Column details: name, position, data type, nullability, default, length/precision.    |
| `list_indexes`      | List indexes on a table, with their definitions.                                      |
| `list_foreign_keys` | Foreign keys: constraint name, local column, referenced table/column.                 |
| `table_stats`       | Per-table stats: estimated live/dead rows, scan counts, on-disk size, last (auto)vacuum/analyze. Optionally restrict to one schema. |
| `database_size`     | Current database name and total on-disk size (pretty and in bytes).                   |
| `run_query`         | Run an arbitrary read-only `SELECT` and return rows as JSON (see read-only guarantees above). |

## Running

With a `.env` in place:

```sh
cargo run -p pgmcp-server
```

Or pass the variables inline:

```sh
PG_DATABASE_URL='postgres://pgmcp:change-me@127.0.0.1:5432/pgmcp' \
cargo run -p pgmcp-server
```

The repo root ships a [`compose.yaml`](../../compose.yaml) with a Postgres 18
service for local development (`just up`); credentials default to `pgmcp` and
the port is published loopback-only.

The MCP endpoint is then served at `http://<PG_BIND>/mcp` (default
`http://127.0.0.1:8081/mcp`). Set `RUST_LOG=debug` for verbose tracing.

### MCP client config

The server uses the streamable-HTTP transport, so point the client at its URL:

```json
{
  "mcpServers": {
    "postgres": {
      "type": "http",
      "url": "http://127.0.0.1:8081/mcp"
    }
  }
}
```

### Multiple databases (one MCP per database)

The binary reads a single `PG_DATABASE_URL`, so to expose several Postgres
databases you run one `pgmcp-server` instance per database — each with its own
`PG_DATABASE_URL` and `PG_BIND`. The NixOS module supports this via
`serverType` (see the root
[README](../../README.md#multiple-instances--several-postgres-databases)), and
[`compose.yaml`](../../compose.yaml) ships a commented second
`postgres`/`pgmcp` pair as a template.
