# woodpecker-mcp

MCP server for **[Woodpecker CI](https://woodpecker-ci.org/)**. Exposes read-only
tools to inspect repositories, pipelines, build steps, logs, cron schedules, and
agents, so an MCP client can answer questions like *"did the last push to main
pass?"*, *"why did the test step fail?"*, and *"are any agents offline?"* without
being able to change anything.

Two crates:

- **`wpmcp`** — library: Woodpecker REST client + MCP tool definitions and wiring.
- **`wpmcp-server`** — thin binary that serves the tools over streamable HTTP
  (axum), with the MCP endpoint mounted at `/mcp`.

Developed and verified against **Woodpecker 3.16.0**.

## Quick start

```sh
cp .env.example .env        # at the repo root
$EDITOR .env                # set WP_HOST and WP_TOKEN
cargo run -p wpmcp-server
```

The MCP endpoint is then served at `http://127.0.0.1:8085/mcp`. Point your client
at it:

```json
{
  "mcpServers": {
    "woodpecker": { "type": "http", "url": "http://127.0.0.1:8085/mcp" }
  }
}
```

## Configuration

All settings come from environment variables. On startup the binary loads `.env`
from the working directory (via `dotenvy`); real environment variables take
precedence, and a missing `.env` is fine (e.g. when an MCP client injects the
variables itself).

| Variable           | Required | Default          | Description                                                                 |
| ------------------ | -------- | ---------------- | --------------------------------------------------------------------------- |
| `WP_HOST`          | yes      | —                | `https://ci.homelab.local`, `ci.homelab.local:8000`, or `ci.homelab.local`   |
| `WP_TOKEN`         | yes      | —                | Personal access token, sent as `Authorization: Bearer <token>`              |
| `WP_INSECURE`      | no       | `false`          | Set `1`/`true` to accept untrusted TLS certificates — see below              |
| `WP_BIND`          | no       | `127.0.0.1:8085` | Address to bind the streamable-HTTP server (endpoint at `/mcp`)             |
| `WP_ALLOWED_HOSTS` | no       | loopback only    | Comma-separated allowed `Host` values; set when serving on a hostname       |
| `WP_MAX_LOG_LINES` | no       | `500`            | Cap on log entries returned by `step_logs` (`0` falls back to the default)  |

`WP_HOST` assumes `https://` when no scheme is given, and appends **no** default
port — unlike Prometheus or Loki, Woodpecker has no canonical port, and an
instance behind a reverse proxy is normally plain 443.

`WP_TOKEN` is required, and an empty value is rejected at startup rather than
deferred into a runtime 401. See [Authentication](#authentication-and-anonymous-access)
for why a *wrong* token is more dangerous than a missing one.

### TLS: you usually do not need `WP_INSECURE`

Certificates are verified through `rustls-platform-verifier`, which uses the
**host's** trust store. A Woodpecker instance behind a certificate issued by an
internal CA validates normally, provided that CA is installed on the machine
running this server — no flag required.

`WP_INSECURE` is an escape hatch for a genuinely untrusted certificate (a truly
self-signed cert, or an internal CA the host does not know about). It is not a
homelab default, and turning it on disables verification entirely.

### Binding and DNS-rebinding protection

The server binds loopback-only and rejects non-loopback `Host` headers by
default. When exposing it on a hostname — behind Tailscale or a reverse proxy —
list that hostname in `WP_ALLOWED_HOSTS`. Inside a container, override
`WP_BIND=0.0.0.0:8085` via the environment; never as a code default.

### Why the `WP_` prefix and not `WOODPECKER_`

Woodpecker CI injects `WOODPECKER_*` variables into every pipeline step as
built-in context. Because this repository itself builds under Woodpecker CI,
using `WOODPECKER_*` as the config prefix would collide with the runner's own
variables during `just ci`. `WP_` keeps the two namespaces disjoint.

### Obtaining a personal access token

1. Log in to your Woodpecker instance (e.g. `https://ci.homelab.local`).
2. Open your user settings page (avatar menu → **Settings**).
3. Copy the personal access token shown there, or create a new one.
4. Paste it into `.env` as `WP_TOKEN`.

The token carries your own permissions — it is not separately scopeable, so the
server sees exactly what you see in the web UI.

## Tools

All 16 tools are read-only `GET`s. The **Auth** column reflects behaviour
measured against Woodpecker 3.16.0; see
[Authentication](#authentication-and-anonymous-access) for what "public" means.

| Tool                 | Endpoint                                               | Auth              |
| -------------------- | ------------------------------------------------------ | ----------------- |
| `version`            | `GET /version`                                          | none              |
| `healthz`            | `GET /healthz`                                          | none              |
| `queue_info`         | `GET /api/queue/info`                                   | token             |
| `list_repos`         | `GET /api/user/repos`                                   | token             |
| `lookup_repo`        | `GET /api/repos/lookup/{owner}/{name}`                  | public repos: no  |
| `get_repo`           | `GET /api/repos/{repo_id}`                              | public repos: no  |
| `list_branches`      | `GET /api/repos/{repo_id}/branches`                     | public repos: no  |
| `list_pull_requests` | `GET /api/repos/{repo_id}/pull_requests`                | public repos: no  |
| `list_pipelines`     | `GET /api/repos/{repo_id}/pipelines`                    | public repos: no  |
| `get_pipeline`       | `GET /api/repos/{repo_id}/pipelines/{number}`           | public repos: no  |
| `pipeline_config`    | `GET /api/repos/{repo_id}/pipelines/{number}/config`    | public repos: no  |
| `pipeline_metadata`  | `GET /api/repos/{repo_id}/pipelines/{number}/metadata`  | token             |
| `step_logs`          | `GET /api/repos/{repo_id}/logs/{number}/{step_id}`      | public repos: no  |
| `list_crons`         | `GET /api/repos/{repo_id}/cron`                         | token             |
| `list_agents`        | `GET /api/agents`                                       | token + **admin** |
| `pipeline_feed`      | `GET /api/user/feed`                                    | token             |

`list_pipelines` accepts `page`, `per_page`, `branch`, `event`, `status`, `ref`,
`before`, and `after`; the paginated list tools accept `page` and `per_page`.
The MCP-facing `per_page` is sent to Woodpecker as `perPage`.

## Authentication and anonymous access

Woodpecker's authorization model has a quirk worth understanding before you trust
a result from this server.

### Public repositories are readable without any credentials

A repository whose `visibility` is `public` can be read by anyone — no token, no
session. Every repo-scoped tool above marked *"public repos: no"* returns real
data for such a repo even with no `Authorization` header at all.

### An invalid token silently degrades to anonymous access

This is the important one. Woodpecker does **not** reject a malformed or expired
bearer token with a 401 on these routes. It ignores the bad credential and falls
back to anonymous access:

```
GET /api/repos/1              no header  -> 200   bad token -> 200
GET /api/repos/1/pipelines    no header  -> 200   bad token -> 200
GET /api/queue/info           no header  -> 401   bad token -> 401
```

So a typo in `WP_TOKEN` does not produce an obvious failure. Public-repo tools
keep returning correct-looking data, and you only discover the problem when an
authenticated tool fails — or, worse, when `list_repos` returns an empty list and
you conclude you have no repositories rather than no valid token.

**Verify your token explicitly.** `list_repos` is the cheapest check: with a
valid token it lists your repositories; with a broken one it fails with
`401 User not authorized`. `queue_info` works equally well. Do not treat a
successful `get_repo` as proof that authentication is working.

### Private repositories return 401, not 404

An ID that is private and an ID that does not exist both answer `401 User not
authorized`. You cannot distinguish "no permission" from "no such repo", and you
cannot enumerate repositories by walking IDs. Use `list_repos` (authenticated) or
`lookup_repo` with a known `owner`/`name` instead.

### Admin-only endpoints

`list_agents` requires server admin rights and returns `401`/`403` for an ordinary
user's token. This is expected and not a misconfiguration — everything else in the
table works with a normal account.

## API quirks

These are behaviours of Woodpecker's HTTP API that the client works around. They
are documented here because each one produced a confusing failure during
development.

### `version` and `healthz` live outside `/api`

The Swagger document declares `basePath: /api`, but these two routes sit at the
server root. The failure mode is unusually nasty: Woodpecker serves the web UI as
a catch-all, so `/api/version` does **not** 404 — it returns **HTTP 200 with the
single-page app's `index.html`**, which then fails to parse as JSON with a
misleading "invalid JSON" error.

That catch-all is also a handy diagnostic: a real API route answers `401` for an
unauthorized request, whereas a nonexistent one answers `200 text/html`. If a
request returns HTML, the path is wrong.

### `healthz` returns 204 No Content

There is no body, so the tool result is `null`. A successful call with no error
*is* the health signal.

### Log lines arrive base64-encoded — and are decoded for you

Woodpecker models a log line's payload as Go `[]byte`, which marshals to standard
padded base64. On the wire a line looks like `KyBnaXQgaW5pdCA...`, not
`+ git init ...`.

`step_logs` decodes the `data` field of every entry it returns, so the tool hands
back readable text:

```json
{ "id": 22, "step_id": 1, "line": 9, "time": 0, "type": 0,
  "data": "+ git lfs fetch" }
```

Decoding is best-effort by design. A value that is not valid base64 is passed
through exactly as received rather than raising an error — one odd line should
not cost you the whole log, and if a future Woodpecker version stops encoding the
field, the tool degrades to passthrough instead of breaking. Invalid UTF-8 is
decoded lossily, since build logs carry terminal escapes and occasionally raw
bytes.

### Error bodies are plain text, not JSON

Failures come back as `text/plain` (e.g. `User not authorized`), not a JSON error
envelope. The client detects this and surfaces the raw text alongside the status,
so tool errors read as
`Woodpecker API https://…/api/queue/info returned 401 Unauthorized: User not authorized`.

### Large step logs are truncated

Build logs are unbounded, and an unfiltered one can swamp a model's context. When
a step's log exceeds the cap (`max_lines` per call, else `WP_MAX_LOG_LINES`,
default 500), `step_logs` keeps the **last** N entries — a failure's cause is at
the end — and returns:

```json
{
  "truncated": true,
  "total_entries": 4212,
  "returned_entries": 500,
  "note": "showing the last 500 of 4212 log entries; raise max_lines or WP_MAX_LOG_LINES for more",
  "entries": [ … ]
}
```

Below the cap the array is returned unwrapped.

## Read-only guarantee

Every tool issues a `GET`; none can restart, cancel, approve, trigger, or delete
anything. This is enforced by a test, not just convention:
`every_tool_issues_only_get_requests` invokes all 16 tools against a mock server
and asserts that all 16 recorded requests used `GET`. A tool added without a line
in that test fails the assertion.

Woodpecker's mutating endpoints — restart, cancel, approve, decline, trigger,
`cron` run-now, queue pause/resume — are deliberately **not** exposed. See
[`AGENTS.md`](../../AGENTS.md) hard rule §1.

### Deliberately omitted read endpoints

Some `GET`s are excluded even though read-only:

- **`/secrets` and `/registries`** (global, org, and repo scope) return the
  actual credential material — `Secret.value` and `Registry.password` — in
  plaintext. Read-only does not make those safe: exposing them would turn an MCP
  context window into a credential exfiltration path.
- **`/users`, `/orgs`, `/forges`** are account and infrastructure management,
  outside this server's purpose.
- **`/queue/norunningpipelines`** long-polls until a pipeline appears and would
  hang the tool call.
- **`/stream/logs/...`** is a Server-Sent Events stream, not a request/response
  endpoint.
- **`/debug/pprof*`** is Go runtime profiling internals.

## Running

### Locally

```sh
cargo run -p wpmcp-server                      # reads .env
WP_HOST=https://ci.homelab.local WP_TOKEN=… cargo run -p wpmcp-server
RUST_LOG=debug cargo run -p wpmcp-server       # verbose tracing
```

### Container

```sh
just image wpmcp-server     # static musl -> distroless/static:nonroot
just scan wpmcp-server      # trivy vulnerability scan
just up                     # whole compose stack; wpmcp is published on 8085
```

The image has no shell and no `curl`, so the container healthcheck is the binary
probing itself via `--healthcheck`; that path deliberately works without any
config or `.env`. Inside a container set `WP_BIND=0.0.0.0:8085`, as
[`compose.yaml`](../../compose.yaml) does.

## Development

```sh
just test-pkg wpmcp     # this package's tests
just clippy             # -D warnings -D clippy::pedantic
just ci                 # lint + test; must pass before a change is done
```

Tests run against [`wiremock`](https://docs.rs/wiremock) rather than a live
Woodpecker, so they need no credentials and no network. The mock matches on
`any()` so a request built with the wrong path or method still gets served,
letting assertions report what was actually sent instead of failing with an
opaque 404. Coverage includes path construction, optional-parameter omission,
the `perPage`/`ref` wire renames, `lookup_repo`'s multi-segment path, log
truncation, and error mapping for 500s, malformed JSON, and empty bodies.

## Troubleshooting

| Symptom                                                        | Cause                                                                                   |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `invalid JSON from …` and the body looks like HTML              | The path fell through to the web UI catch-all — the route is wrong                       |
| `401 User not authorized`                                       | Missing/invalid token, a private repo, or an admin-only endpoint                          |
| `list_repos` returns `[]` but `get_repo` works                  | Almost certainly an invalid `WP_TOKEN` degrading to anonymous access                      |
| Log text still looks like `KyBnaXQg…`                           | That line was not valid base64, so it was passed through undecoded                        |
| TLS handshake failure                                           | The issuing CA is not in the host trust store; install it rather than setting `WP_INSECURE` |
| Server starts but the client cannot connect                     | Non-loopback `Host` header rejected — add the hostname to `WP_ALLOWED_HOSTS`               |
| `WP_TOKEN must be set` at startup                               | No `.env` in the working directory, or the variable is empty                              |
