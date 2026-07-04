# homeassistant-mcp

MCP server for **Home Assistant**. Exposes tools to query entity states,
calendars, history, and configuration, as well as mutating tools to call
services and set states so an MCP client can both inspect and control a
smart home.

Two crates:

- `hamcp` — library: Home Assistant REST client + MCP tool definitions and wiring.
- `hamcp-server` — thin binary that serves the tools over streamable HTTP
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

| Variable            | Required | Default           | Description                                                            |
| ------------------- | -------- | ----------------- | --------------------------------------------------------------------- |
| `HA_HOST`           | yes      | —                 | `http://homeassistant.local:8123`, `homeassistant.local:8123`, or `homeassistant.local` |
| `HA_TOKEN`          | yes      | —                 | Long-lived access token from the HA UI                               |
| `HA_INSECURE`       | no       | `false`           | Set `1`/`true` to accept self-signed TLS certificates                |
| `HA_BIND`           | no       | `127.0.0.1:8084`  | Address to bind the streamable-HTTP server (endpoint at `/mcp`)       |
| `HA_ALLOWED_HOSTS`  | no       | loopback only     | Comma-separated allowed `Host` values; set when serving on a hostname |

If no scheme is given, `http://` is assumed; if no port is given, Home
Assistant's default `8123` is used.

By default the server only accepts loopback `Host` headers (DNS-rebinding
protection). When exposing it on a hostname — e.g. behind Tailscale or a
reverse proxy — list that hostname in `HA_ALLOWED_HOSTS`.

### Creating a long-lived access token

In the Home Assistant UI: *Profile → Long-Lived Access Tokens → Create Token*.
Give it a name (e.g. "MCP"), copy the token string, and set it as `HA_TOKEN`.

## Tools

All tool names are explicit (set via `#[tool(name = "…")]`) to preserve the
exact wire-facing names that existing MCP clients may already reference.

| Tool                  | Home Assistant endpoint                              | Mutating |
| --------------------- | --------------------------------------------------- | -------- |
| `health_check`        | `GET /api/`                                         | no       |
| `get_config`          | `GET /api/config`                                   | no       |
| `get_states`          | `GET /api/states`                                   | no       |
| `get_entity`          | `GET /api/states/{entity_id}`                       | no       |
| `call_service`        | `POST /api/services/{domain}/{service}`             | **yes**  |
| `set_state`           | `POST /api/states/{entity_id}`                      | **yes**  |
| `get_services`        | `GET /api/services`                                 | no       |
| `render_template`     | `POST /api/template`                                | no       |
| `get_calendars`       | `GET /api/calendars`                                | no       |
| `get_calendar_events` | `GET /api/calendars/{entity_id}`                    | no       |
| `check_config`        | `POST /api/config/core/check_config`                | no       |
| `get_history`         | `GET /api/history/period/{start}`                   | no       |

## Running

With a `.env` in place:

```sh
cargo run -p hamcp-server
```

Or pass the variables inline:

```sh
HA_HOST=http://homeassistant.local:8123 \
HA_TOKEN='your_long_lived_access_token' \
cargo run -p hamcp-server
```

The MCP endpoint is then served at `http://<HA_BIND>/mcp` (default
`http://127.0.0.1:8084/mcp`). Set `RUST_LOG=debug` for verbose tracing.

### MCP client config

The server uses the streamable-HTTP transport, so point the client at its URL:

```json
{
  "mcpServers": {
    "homeassistant": {
      "type": "http",
      "url": "http://127.0.0.1:8084/mcp"
    }
  }
}
```
