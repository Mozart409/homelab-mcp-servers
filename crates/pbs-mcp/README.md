# pbs-mcp

MCP server for **Proxmox Backup Server (PBS)**. Exposes read-only tools to
inspect datastores, backup snapshots, and task history so an MCP client can
answer "did my backups run / verify / is the datastore filling up?".

Two crates:

- `pbsmcp` — library: PBS REST client + MCP tool definitions and wiring.
- `pbsmcp-server` — thin binary that serves the tools over streamable HTTP
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

| Variable            | Required | Default          | Description                                                            |
| ------------------- | -------- | ---------------- | --------------------------------------------------------------------- |
| `PBS_HOST`          | yes      | —                | `https://pbs.lan:8007`, `pbs.lan:8007`, or `pbs.lan`                  |
| `PBS_API_KEY`       | yes      | —                | API token: `user@realm!tokenname:secret`                              |
| `PBS_NODE`          | no       | `localhost`      | Node name for `/nodes/{node}/...` endpoints                           |
| `PBS_INSECURE`      | no       | `false`          | Set `1`/`true` to accept self-signed TLS certificates                 |
| `PBS_BIND`          | no       | `127.0.0.1:8080` | Address to bind the streamable-HTTP server (endpoint at `/mcp`)       |
| `PBS_ALLOWED_HOSTS` | no       | loopback only    | Comma-separated allowed `Host` values; set when serving on a hostname |

If no scheme is given, `https://` is assumed; if no port is given, PBS's
default `8007` is used.

By default the server only accepts loopback `Host` headers (DNS-rebinding
protection). When exposing it on a hostname — e.g. behind Tailscale or a
reverse proxy — list that hostname in `PBS_ALLOWED_HOSTS`.

### Creating an API token

In the PBS UI: *Configuration → Access Control → API Tokens → Add*, or:

```sh
proxmox-backup-manager user generate-token user@pbs mytoken
```

Save the returned `value` (the secret is shown only once). Set `PBS_API_KEY` to
the token id and secret joined with a colon, e.g. `monitor@pbs!mcp:xxxxxxxx-...`;
the auth header sent is `Authorization: PBSAPIToken=<tokenid>:<secret>`.

#### Required permissions

A valid token still needs ACLs, or PBS silently returns empty lists (`[]`) and
`403 Forbidden: permission check failed` instead of data. Grant the token (not
just its user — see the note below) these two entries:

| Path         | Role            | Unlocks                                                          |
| ------------ | --------------- | --------------------------------------------------------------- |
| `/datastore` | `DatastoreAudit` | `list_datastores`, `datastore_status`, `list_groups`, `list_snapshots`, `gc_status` |
| `/system`    | `Sys.Audit`     | `node_status`, `list_tasks` (and per-task `task_status` / `task_log`) |

In the PBS UI: *Configuration → Access Control → Permissions → Add → API Token
Permission*, pick the token, choose the path + role, and leave **Propagate**
checked. Or via CLI:

```sh
proxmox-backup-manager acl update /datastore DatastoreAudit --auth-id 'monitor@pbs!mcp'
proxmox-backup-manager acl update /system    Sys.Audit      --auth-id 'monitor@pbs!mcp'
```

> **Privilege separation gotcha:** API tokens default to *privilege separation
> enabled*, meaning the token only has privileges granted to the **token id**
> directly — it does **not** inherit its user's permissions. Always target the
> token id (`user@realm!tokenname`) in the ACL, not the bare user.

If you only need backup/datastore status, the `/datastore` grant alone is
enough; add `/system` only when you want node health and task history.

## Tools

| Tool               | PBS endpoint                              |
| ------------------ | ---------------------------------------- |
| `list_datastores`  | `GET /admin/datastore`                   |
| `datastore_status` | `GET /admin/datastore/{store}/status`    |
| `list_groups`      | `GET /admin/datastore/{store}/groups`    |
| `list_snapshots`   | `GET /admin/datastore/{store}/snapshots` |
| `list_tasks`       | `GET /nodes/{node}/tasks`                |
| `task_status`      | `GET /nodes/{node}/tasks/{upid}/status`  |
| `task_log`         | `GET /nodes/{node}/tasks/{upid}/log`     |
| `gc_status`        | `GET /admin/gc`                          |
| `node_status`      | `GET /nodes/{node}/status`               |

## Running

With a `.env` in place:

```sh
cargo run -p pbsmcp-server
```

Or pass the variables inline:

```sh
PBS_HOST=https://pbs.lan:8007 \
PBS_API_KEY='monitor@pbs!mcp:xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx' \
cargo run -p pbsmcp-server
```

The MCP endpoint is then served at `http://<PBS_BIND>/mcp` (default
`http://127.0.0.1:8080/mcp`). Set `RUST_LOG=debug` for verbose tracing.

### MCP client config

The server uses the streamable-HTTP transport, so point the client at its URL:

```json
{
  "mcpServers": {
    "pbs": {
      "type": "http",
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```
