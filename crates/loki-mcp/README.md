# loki-mcp

MCP server for **Grafana Loki**. Exposes read-only tools to search logs with
LogQL and inspect labels so an MCP client can answer "show me errors in service
X over the last hour / which jobs are logging?".

Two crates:

- `lokimcp` — library: Loki HTTP API client + MCP tool definitions and wiring.
- `lokimcp-server` — thin binary that serves the tools over streamable HTTP
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

| Variable             | Required | Default          | Description                                                            |
| -------------------- | -------- | ---------------- | --------------------------------------------------------------------- |
| `LOKI_HOST`          | yes      | —                | `http://loki.lan:3100`, `loki.lan:3100`, or `loki.lan`                |
| `LOKI_TOKEN`         | no       | none             | Bearer token, sent as `Authorization: Bearer <token>` (use behind an auth proxy) |
| `LOKI_ORG_ID`        | no       | none             | Tenant ID, sent as `X-Scope-OrgID` (required in multi-tenant mode)    |
| `LOKI_INSECURE`      | no       | `false`          | Set `1`/`true` to accept self-signed TLS certificates                 |
| `LOKI_BIND`          | no       | `127.0.0.1:8083` | Address to bind the streamable-HTTP server (endpoint at `/mcp`)       |
| `LOKI_ALLOWED_HOSTS` | no       | loopback only    | Comma-separated allowed `Host` values; set when serving on a hostname |

If no scheme is given, `http://` is assumed; if no port is given, Loki's
default `3100` is used.

By default the server only accepts loopback `Host` headers (DNS-rebinding
protection). When exposing it on a hostname — e.g. behind Tailscale or a
reverse proxy — list that hostname in `LOKI_ALLOWED_HOSTS`.

## Tools

All tools are read-only GETs against the
[Loki HTTP API](https://grafana.com/docs/loki/latest/reference/loki-http-api/).

| Tool           | Loki endpoint                          |
| -------------- | -------------------------------------- |
| `query`        | `GET /loki/api/v1/query`               |
| `query_range`  | `GET /loki/api/v1/query_range`         |
| `labels`       | `GET /loki/api/v1/labels`              |
| `label_values` | `GET /loki/api/v1/label/{name}/values` |
| `series`       | `GET /loki/api/v1/series`              |
| `index_stats`  | `GET /loki/api/v1/index/stats`         |

`query_range` is the workhorse for searching log lines over a window; `query`
(instant) is best for metric queries like `count_over_time(...)`. Timestamps
(`time`, `start`, `end`) accept RFC3339 or a Unix timestamp in nanoseconds;
`direction` is `forward` or `backward` (default `backward`, newest first).

## Running

With a `.env` in place:

```sh
cargo run -p lokimcp-server
```

Or pass the variables inline:

```sh
LOKI_HOST=http://loki.lan:3100 cargo run -p lokimcp-server
```

The MCP endpoint is then served at `http://<LOKI_BIND>/mcp` (default
`http://127.0.0.1:8083/mcp`). Set `RUST_LOG=debug` for verbose tracing.

### MCP client config

The server uses the streamable-HTTP transport, so point the client at its URL:

```json
{
  "mcpServers": {
    "loki": {
      "type": "http",
      "url": "http://127.0.0.1:8083/mcp"
    }
  }
}
```
