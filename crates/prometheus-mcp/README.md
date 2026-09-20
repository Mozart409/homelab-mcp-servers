# prometheus-mcp

MCP server for **Prometheus**. Exposes read-only tools to run PromQL and inspect
monitoring state so an MCP client can answer "is anything firing / what's CPU
doing / are my scrape targets up?".

Two crates:

- `prommcp` — library: Prometheus HTTP API client + MCP tool definitions and wiring.
- `prommcp-server` — thin binary that serves the tools over streamable HTTP
  (axum), with the MCP endpoint mounted at `/mcp`.

## Configuration

The server reads its configuration from environment variables. In this repo
they live encrypted in [`.sops.env`](../../.sops.env) at the root (sops dotenv
store — see the root README) and are injected by the `just` recipes:

```sh
sops .sops.env                 # edit (decrypts into $EDITOR, re-encrypts on save)
just watch-pkg prommcp-server          # run with the variables injected
```

On startup the binary also loads a plain `.env` from the working directory
(via `dotenvy`) if one exists — copy [`.env.example`](../../.env.example) for
that. Real environment variables take precedence, and a missing `.env` is
fine (e.g. when an MCP client injects the variables itself).

| Variable             | Required | Default          | Description                                                            |
| -------------------- | -------- | ---------------- | --------------------------------------------------------------------- |
| `PROM_HOST`          | yes      | —                | `http://prometheus.lan:9090`, `prometheus.lan:9090`, or `prometheus.lan` |
| `PROM_TOKEN`         | no       | none             | Bearer token, sent as `Authorization: Bearer <token>` (use behind an auth proxy) |
| `PROM_INSECURE`      | no       | `false`          | Set `1`/`true` to accept self-signed TLS certificates                 |
| `PROM_BIND`          | no       | `127.0.0.1:8082` | Address to bind the streamable-HTTP server (endpoint at `/mcp`)       |
| `PROM_ALLOWED_HOSTS` | no       | loopback only    | Comma-separated allowed `Host` values; set when serving on a hostname |

If no scheme is given, `http://` is assumed; if no port is given, Prometheus's
default `9090` is used.

By default the server only accepts loopback `Host` headers (DNS-rebinding
protection). When exposing it on a hostname — e.g. behind Tailscale or a
reverse proxy — list that hostname in `PROM_ALLOWED_HOSTS`.

## Tools

All tools are read-only GETs against the
[Prometheus HTTP API](https://prometheus.io/docs/prometheus/latest/querying/api/).

| Tool           | Prometheus endpoint                  |
| -------------- | ------------------------------------ |
| `query`        | `GET /api/v1/query`                  |
| `query_range`  | `GET /api/v1/query_range`            |
| `series`       | `GET /api/v1/series`                 |
| `labels`       | `GET /api/v1/labels`                 |
| `label_values` | `GET /api/v1/label/{name}/values`    |
| `targets`      | `GET /api/v1/targets`                |
| `alerts`       | `GET /api/v1/alerts`                 |
| `rules`        | `GET /api/v1/rules`                  |
| `metadata`     | `GET /api/v1/metadata`               |
| `tsdb_status`  | `GET /api/v1/status/tsdb`            |
| `build_info`   | `GET /api/v1/status/buildinfo`       |

Timestamps (`time`, `start`, `end`) accept RFC3339 or a Unix timestamp in
seconds; `step` accepts a Prometheus duration (`30s`, `5m`, `1h`) or seconds.

## Running

Via the `justfile` (`just watch-pkg prommcp-server`), with a `.env` in place, or by
setting the variables in the environment:

```sh
cargo run -p prommcp-server
```

Or pass the variables inline:

```sh
PROM_HOST=http://prometheus.lan:9090 cargo run -p prommcp-server
```

The MCP endpoint is then served at `http://<PROM_BIND>/mcp` (default
`http://127.0.0.1:8082/mcp`). Set `RUST_LOG=debug` for verbose tracing.

### MCP client config

The server uses the streamable-HTTP transport, so point the client at its URL:

```json
{
  "mcpServers": {
    "prometheus": {
      "type": "http",
      "url": "http://127.0.0.1:8082/mcp"
    }
  }
}
```
